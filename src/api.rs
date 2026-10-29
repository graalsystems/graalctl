//! Thin HTTP client for the graal REST API (`/api/v1`).
//!
//! Every request carries `Authorization: Bearer <token>` and `X-Tenant`.
//! Resources use vendor media types (`application/vnd.graal.systems.v1.<type>+json`)
//! and edits use JSON Patch (RFC 6902).

use anyhow::{anyhow, Context as _, Result};
use serde_json::Value;
use std::fmt;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use ureq::http;
use ureq::tls::{Certificate, PemItem, RootCerts, TlsConfig};

pub const JSON_PATCH: &str = "application/json-patch+json;charset=UTF-8";

/// Vendor media type for a resource type, e.g. `project`, `job`, `jobrun`.
pub fn media_type(kind: &str) -> String {
    format!("application/vnd.graal.systems.v1.{kind}+json")
}

pub fn accept(kind: &str) -> String {
    format!(
        "{}, {}, application/json;q=0.9, */*;q=0.1",
        media_type(kind),
        media_type("error")
    )
}

/// Error returned by the API: `{ code, message, errors }`.
///
/// `code` is the stable part; authorization problems may come back as HTTP 400,
/// so callers should look at `code` rather than at the status.
#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub code: Option<String>,
    pub message: Option<String>,
    pub errors: Option<Value>,
    pub body: String,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HTTP {}", self.status)?;
        if let Some(code) = &self.code {
            write!(f, " {code}")?;
        }
        match &self.message {
            Some(m) => write!(f, ": {m}")?,
            None if self.code.is_none() && !self.body.trim().is_empty() => {
                write!(f, ": {}", truncate(self.body.trim(), 500))?
            }
            None => {}
        }
        if let Some(errors) = &self.errors {
            if !errors.is_null() && errors.as_array().is_none_or(|a| !a.is_empty()) {
                write!(
                    f,
                    "\n{}",
                    serde_json::to_string_pretty(errors).unwrap_or_default()
                )?;
            }
        }
        match self.code.as_deref() {
            Some("feature_disabled") => write!(f, "\nhint: this feature is disabled on this installation; retrying will not help")?,
            Some("TENANT_CONFIGURATION_ERROR") => write!(f, "\nhint: the tenant is missing required configuration (often the storage internal address); contact your platform administrator")?,
            Some("MISSING_OWNER") => write!(f, "\nhint: this resource needs an explicit owner")?,
            _ if self.status == 401 => write!(f, "\nhint: your session may have expired; run `graalctl login`")?,
            _ => {}
        }
        Ok(())
    }
}

impl std::error::Error for ApiError {}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// TLS settings for a context.
#[derive(Debug, Clone, Default)]
pub struct Tls {
    pub ca_file: Option<std::path::PathBuf>,
    pub insecure: bool,
}

pub fn agent(tls: &Tls, timeout: Duration) -> Result<ureq::Agent> {
    let roots = match &tls.ca_file {
        Some(path) => RootCerts::Specific(Arc::new(load_pem(path)?)),
        None => RootCerts::PlatformVerifier,
    };
    let tls_config = TlsConfig::builder()
        .root_certs(roots)
        .disable_verification(tls.insecure)
        .build();
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .user_agent(format!("graalctl/{}", env!("CARGO_PKG_VERSION")))
        .timeout_global(Some(timeout))
        .proxy(ureq::Proxy::try_from_env())
        .tls_config(tls_config)
        .build();
    Ok(config.into())
}

fn load_pem(path: &Path) -> Result<Vec<Certificate<'static>>> {
    let pem =
        std::fs::read(path).with_context(|| format!("cannot read CA file {}", path.display()))?;
    let mut certs = Vec::new();
    for item in ureq::tls::parse_pem(&pem) {
        if let PemItem::Certificate(c) =
            item.with_context(|| format!("invalid PEM in {}", path.display()))?
        {
            certs.push(c.to_owned());
        }
    }
    if certs.is_empty() {
        return Err(anyhow!("no certificate found in {}", path.display()));
    }
    Ok(certs)
}

/// A response whose status has already been checked.
pub struct Response {
    pub status: u16,
    pub headers: http::HeaderMap,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// Parses the body as JSON; an empty body is `null`.
    pub fn json(&self) -> Result<Value> {
        if self.body.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&self.body).context("the API returned a body that is not valid JSON")
    }
}

pub struct Client {
    agent: ureq::Agent,
    base: String,
    tenant: String,
    token: Option<String>,
    verbose: bool,
}

/// Percent-encodes a query string component or path segment.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

impl Client {
    pub fn new(
        agent: ureq::Agent,
        api_url: &str,
        tenant: &str,
        token: Option<String>,
        verbose: bool,
    ) -> Self {
        Client {
            agent,
            base: api_url.trim_end_matches('/').to_string(),
            tenant: tenant.to_string(),
            token,
            verbose,
        }
    }

    pub fn url(&self, path: &str, query: &[(&str, String)]) -> String {
        let mut url = if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else {
            format!("{}/{}", self.base, path.trim_start_matches('/'))
        };
        let mut sep = if url.contains('?') { '&' } else { '?' };
        for (k, v) in query {
            url.push(sep);
            url.push_str(&encode(k));
            url.push('=');
            url.push_str(&encode(v));
            sep = '&';
        }
        url
    }

    /// Sends a request and returns the response if its status is 2xx, or an
    /// [`ApiError`] otherwise.
    pub fn send(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, String)],
        accept: &str,
        body: Option<(&str, Vec<u8>)>,
    ) -> Result<Response> {
        let url = self.url(path, query);
        if self.verbose {
            eprintln!("> {method} {url}");
        }
        let mut builder = http::Request::builder()
            .method(method)
            .uri(&url)
            .header("Accept", accept)
            .header("X-Tenant", &self.tenant);
        if let Some(token) = &self.token {
            builder = builder.header("Authorization", format!("Bearer {token}"));
        }
        let response = match body {
            Some((content_type, bytes)) => {
                let req = builder.header("Content-Type", content_type).body(bytes)?;
                self.agent.run(req)
            }
            None => self.agent.run(builder.body(())?),
        }
        .with_context(|| format!("request to {url} failed"))?;

        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let mut body = Vec::new();
        response
            .into_body()
            .into_reader()
            .read_to_end(&mut body)
            .with_context(|| format!("cannot read the response of {url}"))?;
        if self.verbose {
            eprintln!("< {status} ({} bytes)", body.len());
        }
        if !(200..300).contains(&status) {
            return Err(api_error(status, &body).into());
        }
        Ok(Response {
            status,
            headers,
            body,
        })
    }

    pub fn get(&self, path: &str, kind: &str, query: &[(&str, String)]) -> Result<Value> {
        self.send("GET", path, query, &accept(kind), None)?.json()
    }

    pub fn post(&self, path: &str, kind: &str, body: &Value) -> Result<Value> {
        let mt = media_type(kind);
        let bytes = serde_json::to_vec(body)?;
        self.send(
            "POST",
            path,
            &[],
            &accept(kind),
            Some((&format!("{mt};charset=UTF-8"), bytes)),
        )?
        .json()
    }

    /// Applies a JSON Patch document; there is no full `PUT` in the API.
    pub fn patch(&self, path: &str, kind: &str, ops: &Value) -> Result<Value> {
        let bytes = serde_json::to_vec(ops)?;
        self.send("PATCH", path, &[], &accept(kind), Some((JSON_PATCH, bytes)))?
            .json()
    }

    pub fn delete(&self, path: &str) -> Result<()> {
        self.send("DELETE", path, &[], &accept("error"), None)?;
        Ok(())
    }

    /// Uploads a file as `multipart/form-data` (field `file`).
    pub fn upload(&self, path: &str, kind: &str, file: &Path) -> Result<Value> {
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .context("the file name is not valid UTF-8")?
            .replace('"', "_");
        let data =
            std::fs::read(file).with_context(|| format!("cannot read {}", file.display()))?;
        let boundary = format!("graalctl-{:x}", now_nanos());
        let mut body = Vec::with_capacity(data.len() + 256);
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(&data);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let ct = format!("multipart/form-data; boundary={boundary}");
        self.send("POST", path, &[], &accept(kind), Some((&ct, body)))?
            .json()
    }
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

pub fn api_error(status: u16, body: &[u8]) -> ApiError {
    let text = String::from_utf8_lossy(body).into_owned();
    let parsed: Option<Value> = serde_json::from_slice(body).ok();
    let field = |name: &str| -> Option<String> {
        parsed.as_ref()?.get(name).and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Null => None,
            other => Some(other.to_string()),
        })
    };
    ApiError {
        status,
        code: field("code").or_else(|| field("error")),
        message: field("message").or_else(|| field("error_description")),
        errors: parsed.as_ref().and_then(|v| v.get("errors").cloned()),
        body: text,
    }
}

/// Extracts the list of items from a list response, which may be a bare array
/// or a page object (`content`, `items`, `data` or `results`).
pub fn items(value: &Value) -> Vec<Value> {
    match value {
        Value::Array(a) => a.clone(),
        Value::Object(o) => {
            for key in ["content", "items", "data", "results"] {
                if let Some(Value::Array(a)) = o.get(key) {
                    return a.clone();
                }
            }
            vec![value.clone()]
        }
        Value::Null => vec![],
        other => vec![other.clone()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn media_types() {
        assert_eq!(
            media_type("instance-type"),
            "application/vnd.graal.systems.v1.instance-type+json"
        );
        assert!(accept("job").starts_with("application/vnd.graal.systems.v1.job+json, application/vnd.graal.systems.v1.error+json"));
    }

    #[test]
    fn extracts_items_from_pages_and_arrays() {
        assert_eq!(items(&json!([1, 2])).len(), 2);
        assert_eq!(items(&json!({"content": [1, 2, 3], "total": 3})).len(), 3);
        assert_eq!(items(&json!({"id": "a"})).len(), 1);
        assert!(items(&Value::Null).is_empty());
    }

    #[test]
    fn parses_error_bodies() {
        let e = api_error(
            501,
            br#"{"code":"feature_disabled","message":"dbt is not enabled","errors":[]}"#,
        );
        assert_eq!(e.code.as_deref(), Some("feature_disabled"));
        let text = e.to_string();
        assert!(text.starts_with("HTTP 501 feature_disabled: dbt is not enabled"));
        assert!(text.contains("retrying will not help"));

        let e = api_error(502, b"<html>bad gateway</html>");
        assert_eq!(e.to_string(), "HTTP 502: <html>bad gateway</html>");
    }

    #[test]
    fn encodes_query_components() {
        assert_eq!(encode("a b/c?d=é"), "a%20b%2Fc%3Fd%3D%C3%A9");
    }
}
