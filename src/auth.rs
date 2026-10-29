//! OpenID Connect token handling against the installation's Keycloak.
//!
//! Supported grants:
//! - client credentials (graal Applications — recommended for scripts and CI),
//! - device authorization (interactive login that works with SSO),
//! - resource owner password (interactive login, local accounts only),
//! - refresh token.

use crate::api::{self, encode};
use crate::config::Credentials;
use anyhow::{anyhow, bail, Context as _, Result};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::Value;
use std::io::Read;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Seconds of margin before expiry at which a token is considered stale.
const EXPIRY_MARGIN: u64 = 30;

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
}

impl TokenResponse {
    /// Copies the tokens into `creds`, keeping the client identity.
    pub fn store_into(self, creds: &mut Credentials) {
        creds.expires_at = self
            .expires_in
            .map(|s| now() + s)
            .or_else(|| jwt_exp(&self.access_token));
        creds.access_token = Some(self.access_token);
        if self.refresh_token.is_some() {
            creds.refresh_token = self.refresh_token;
        }
    }
}

pub struct Oidc<'a> {
    pub agent: &'a ureq::Agent,
    pub auth_url: String,
    pub realm: String,
}

impl Oidc<'_> {
    fn endpoint(&self, suffix: &str) -> String {
        format!(
            "{}/realms/{}/protocol/openid-connect/{suffix}",
            self.auth_url.trim_end_matches('/'),
            encode(&self.realm)
        )
    }

    pub fn token_endpoint(&self) -> String {
        self.endpoint("token")
    }

    fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<(u16, Value)> {
        let body = form
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let req = ureq::http::Request::builder()
            .method("POST")
            .uri(url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .body(body.into_bytes())?;
        let resp = self
            .agent
            .run(req)
            .with_context(|| format!("request to {url} failed"))?;
        let status = resp.status().as_u16();
        let mut buf = Vec::new();
        resp.into_body().into_reader().read_to_end(&mut buf)?;
        let value = serde_json::from_slice(&buf).unwrap_or(Value::Null);
        if !(200..300).contains(&status) && !(status == 400 && value.get("error").is_some()) {
            return Err(api::api_error(status, &buf).into());
        }
        Ok((status, value))
    }

    fn token(&self, form: &[(&str, &str)]) -> Result<TokenResponse> {
        let url = self.token_endpoint();
        let (status, value) = self.post_form(&url, form)?;
        if status >= 300 {
            return Err(api::api_error(status, value.to_string().as_bytes()).into());
        }
        serde_json::from_value(value).context("unexpected token response")
    }

    pub fn client_credentials(
        &self,
        client_id: &str,
        client_secret: &str,
    ) -> Result<TokenResponse> {
        self.token(&[
            ("grant_type", "client_credentials"),
            ("client_id", client_id),
            ("client_secret", client_secret),
        ])
    }

    pub fn password(
        &self,
        client_id: &str,
        username: &str,
        password: &str,
    ) -> Result<TokenResponse> {
        self.token(&[
            ("grant_type", "password"),
            ("client_id", client_id),
            ("username", username),
            ("password", password),
            ("scope", "openid offline_access"),
        ])
    }

    pub fn refresh(
        &self,
        client_id: &str,
        client_secret: Option<&str>,
        refresh_token: &str,
    ) -> Result<TokenResponse> {
        let mut form = vec![
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh_token),
        ];
        if let Some(secret) = client_secret {
            form.push(("client_secret", secret));
        }
        self.token(&form)
    }

    /// OAuth 2.0 device authorization grant (RFC 8628). The client must have
    /// "OAuth 2.0 Device Authorization Grant" enabled in Keycloak.
    pub fn device(
        &self,
        client_id: &str,
        prompt: impl Fn(&str, &str, Option<&str>),
    ) -> Result<TokenResponse> {
        let (status, start) = self.post_form(
            &self.endpoint("auth/device"),
            &[("client_id", client_id), ("scope", "openid offline_access")],
        )?;
        if status >= 300 {
            bail!(
                "device login is not enabled for client '{client_id}': {}",
                start
            );
        }
        let field = |k: &str| start.get(k).and_then(Value::as_str);
        let device_code = field("device_code").context("missing device_code")?;
        let user_code = field("user_code").context("missing user_code")?;
        let uri = field("verification_uri").context("missing verification_uri")?;
        prompt(uri, user_code, field("verification_uri_complete"));

        let mut interval = start.get("interval").and_then(Value::as_u64).unwrap_or(5);
        let deadline = now()
            + start
                .get("expires_in")
                .and_then(Value::as_u64)
                .unwrap_or(600);
        while now() < deadline {
            std::thread::sleep(Duration::from_secs(interval));
            let (status, value) = self.post_form(
                &self.token_endpoint(),
                &[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("client_id", client_id),
                    ("device_code", device_code),
                ],
            )?;
            if status < 300 {
                return serde_json::from_value(value).context("unexpected token response");
            }
            match value.get("error").and_then(Value::as_str) {
                Some("authorization_pending") => {}
                Some("slow_down") => interval += 5,
                Some(other) => bail!("device login failed: {other}"),
                None => bail!("device login failed: {value}"),
            }
        }
        Err(anyhow!("device login timed out"))
    }
}

/// Finds the authentication realm of a tenant (`GET /tenants/{id}?attribute=realm`,
/// which does not require authentication). Falls back to the tenant id.
pub fn discover_realm(client: &api::Client, tenant: &str) -> String {
    let path = format!("/tenants/{}?attribute=realm", encode(tenant));
    client
        .get(&path, "tenant", &[])
        .ok()
        .and_then(|v| v.get("realm").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| tenant.to_string())
}

/// Returns a valid access token, refreshing or re-requesting it if needed.
/// The boolean tells whether `creds` changed and should be persisted.
pub fn ensure_token(oidc: Option<&Oidc>, creds: &mut Credentials) -> Result<(String, bool)> {
    if let Some(token) = &creds.access_token {
        let fresh = creds
            .expires_at
            .or_else(|| jwt_exp(token))
            .is_none_or(|exp| exp > now() + EXPIRY_MARGIN);
        if fresh {
            return Ok((token.clone(), false));
        }
    }
    let oidc = oidc.ok_or_else(|| {
        anyhow!("the session has expired and no auth URL is configured; run `graalctl login`")
    })?;
    let client_id = creds.client_id.clone().unwrap_or_else(|| "graal-ui".into());
    let response = if let Some(refresh) = creds.refresh_token.clone() {
        match oidc.refresh(&client_id, creds.client_secret.as_deref(), &refresh) {
            Ok(r) => r,
            Err(_) if creds.client_secret.is_some() => oidc.client_credentials(
                &client_id,
                creds.client_secret.as_deref().unwrap_or_default(),
            )?,
            Err(e) => return Err(e.context("the session has expired; run `graalctl login`")),
        }
    } else if let Some(secret) = creds.client_secret.clone() {
        oidc.client_credentials(&client_id, &secret)?
    } else if creds.access_token.is_some() {
        bail!("the access token has expired; run `graalctl login`")
    } else {
        bail!("not logged in; run `graalctl login`")
    };
    response.store_into(creds);
    Ok((creds.access_token.clone().unwrap_or_default(), true))
}

/// Decodes the claims of a JWT without verifying it (display purposes only).
pub fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn jwt_exp(token: &str) -> Option<u64> {
    jwt_claims(token)?.get("exp")?.as_u64()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(claims: &str) -> String {
        let e = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!("{}.{}.sig", e.encode(r#"{"alg":"none"}"#), e.encode(claims))
    }

    #[test]
    fn decodes_claims() {
        let token = jwt(r#"{"preferred_username":"ada","exp":4102444800}"#);
        let claims = jwt_claims(&token).unwrap();
        assert_eq!(claims["preferred_username"], "ada");
        assert_eq!(jwt_exp(&token), Some(4102444800));
        assert!(jwt_claims("not-a-jwt").is_none());
    }

    #[test]
    fn fresh_token_is_reused_without_network() {
        let mut creds = Credentials {
            access_token: Some(jwt(r#"{"exp":4102444800}"#)),
            ..Default::default()
        };
        let (token, changed) = ensure_token(None, &mut creds).unwrap();
        assert_eq!(Some(token), creds.access_token);
        assert!(!changed);
    }

    #[test]
    fn expired_token_without_oidc_asks_for_login() {
        let mut creds = Credentials {
            access_token: Some(jwt(r#"{"exp":1}"#)),
            ..Default::default()
        };
        let err = ensure_token(None, &mut creds).unwrap_err().to_string();
        assert!(err.contains("graalctl login"), "{err}");
    }
}
