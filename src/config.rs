//! Persistent configuration: named contexts (which installation and tenant to
//! talk to) and the credentials obtained for each of them.

use anyhow::{bail, Context as _, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// One graal installation + tenant, as seen by the CLI.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Context {
    /// Base URL of the REST API, including `/api/v1`.
    pub api_url: String,
    /// Base URL of the OpenID Connect server (Keycloak), without `/realms/...`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_url: Option<String>,
    /// Tenant identifier, sent as the `X-Tenant` header.
    pub tenant: String,
    /// Authentication realm. Discovered from the API when not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realm: Option<String>,
    /// OIDC client used for interactive logins (password or device flow).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Infrastructure used to start runs when none is given on the command line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub infrastructure: Option<String>,
    /// PEM file with the CA certificates to trust (internal PKI). Replaces the
    /// system trust store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_file: Option<PathBuf>,
    /// Skip TLS certificate verification. Never use in production.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub insecure: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_context: Option<String>,
    #[serde(default)]
    pub contexts: BTreeMap<String, Context>,
}

/// Tokens and client credentials, stored separately from the configuration
/// with owner-only permissions.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Credentials {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Unix timestamp (seconds) after which the access token is expired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Only stored for application (client credentials) logins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
}

pub fn config_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("GRAALCTL_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let base = dirs::config_dir().context("cannot determine the user configuration directory")?;
    Ok(base.join("graalctl"))
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        Ok(config_dir()?.join("config.toml"))
    }

    pub fn load() -> Result<Self> {
        Self::load_from(&Self::path()?)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s)
                .with_context(|| format!("invalid configuration file {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::path()?)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        write_private(path, toml::to_string_pretty(self)?.as_bytes())
    }

    /// Returns the name and value of the context to use: `requested` if given,
    /// otherwise the current context.
    pub fn resolve(&self, requested: Option<&str>) -> Result<Option<(String, Context)>> {
        let name = match requested.or(self.current_context.as_deref()) {
            Some(n) => n,
            None => return Ok(None),
        };
        match self.contexts.get(name) {
            Some(c) => Ok(Some((name.to_string(), c.clone()))),
            None => bail!("context '{name}' does not exist (see `graalctl config get-contexts`)"),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CredentialStore {
    #[serde(default)]
    pub contexts: BTreeMap<String, Credentials>,
}

impl CredentialStore {
    pub fn path() -> Result<PathBuf> {
        Ok(config_dir()?.join("credentials.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        match fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s)
                .with_context(|| format!("invalid credentials file {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        write_private(
            &Self::path()?,
            serde_json::to_string_pretty(self)?.as_bytes(),
        )
    }
}

/// Writes a file readable by its owner only (0600 on Unix).
fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&tmp)
            .with_context(|| format!("cannot write {}", tmp.display()))?;
        std::io::Write::write_all(&mut f, contents)?;
    }
    fs::rename(&tmp, path).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_resolve() {
        let dir = std::env::temp_dir().join(format!("graalctl-test-{}", std::process::id()));
        let path = dir.join("config.toml");
        let mut cfg = Config::default();
        cfg.contexts.insert(
            "prod".into(),
            Context {
                api_url: "https://graal.example.com/api/v1".into(),
                tenant: "acme".into(),
                ..Default::default()
            },
        );
        cfg.current_context = Some("prod".into());
        cfg.save_to(&path).unwrap();

        let loaded = Config::load_from(&path).unwrap();
        let (name, ctx) = loaded.resolve(None).unwrap().unwrap();
        assert_eq!(name, "prod");
        assert_eq!(ctx.tenant, "acme");
        assert!(loaded.resolve(Some("missing")).is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_file_is_empty_config() {
        let cfg = Config::load_from(Path::new("/nonexistent/graalctl/config.toml")).unwrap();
        assert!(cfg.contexts.is_empty());
    }
}
