//! Durable non-secret tunnel settings + keyring-backed secrets.
//!
//! The JSON file never stores Cloudflare / FRP tokens. Those live in the OS
//! keyring under stable account names. The renderer only sees `*TokenSet`
//! booleans.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::acp::atomic_file;
use crate::secure_storage;

pub(crate) const CF_NAMED_TOKEN_ACCOUNT: &str = "tunnel-cf-named-token-v1";
pub(crate) const FRP_TOKEN_ACCOUNT: &str = "tunnel-frp-token-v1";

/// Default loopback port for named Cloudflare tunnels so the operator can
/// point a remotely-managed ingress at a stable `http://127.0.0.1:18787`.
pub const DEFAULT_NAMED_LOCAL_PORT: u16 = 18787;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum TunnelProviderKind {
    #[default]
    CloudflareQuick,
    CloudflareNamed,
    Frp,
}

impl TunnelProviderKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CloudflareQuick => "cloudflareQuick",
            Self::CloudflareNamed => "cloudflareNamed",
            Self::Frp => "frp",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelConfig {
    pub provider: TunnelProviderKind,
    /// Hostname only (`termul.example.com`), never a URL with credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloudflare_named_hostname: Option<String>,
    /// Loopback port the named-tunnel ingress should target. Ignored by Quick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloudflare_named_local_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frp_server_addr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frp_server_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frp_custom_domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frp_remote_port: Option<u16>,
    /// When true the QR uses `https://`; FRP itself may still speak HTTP to origin.
    #[serde(default = "default_true")]
    pub frp_public_https: bool,
}

fn default_true() -> bool {
    true
}

impl Default for TunnelConfig {
    fn default() -> Self {
        Self {
            provider: TunnelProviderKind::CloudflareQuick,
            cloudflare_named_hostname: None,
            cloudflare_named_local_port: None,
            frp_server_addr: None,
            frp_server_port: None,
            frp_custom_domain: None,
            frp_remote_port: None,
            frp_public_https: true,
        }
    }
}

impl TunnelConfig {
    /// Named Cloudflare needs a stable local port; other providers keep OS-assigned `0`.
    #[must_use]
    pub fn preferred_bind_port(&self) -> Option<u16> {
        match self.provider {
            TunnelProviderKind::CloudflareNamed => Some(
                self.cloudflare_named_local_port
                    .unwrap_or(DEFAULT_NAMED_LOCAL_PORT),
            ),
            TunnelProviderKind::CloudflareQuick | TunnelProviderKind::Frp => None,
        }
    }

    pub fn validate_for_start(&self) -> Result<(), String> {
        match self.provider {
            TunnelProviderKind::CloudflareQuick => Ok(()),
            TunnelProviderKind::CloudflareNamed => {
                normalize_hostname(self.cloudflare_named_hostname.as_deref().unwrap_or(""))?;
                if let Some(port) = self.cloudflare_named_local_port {
                    if port == 0 {
                        return Err("named Cloudflare local port must be 1..=65535".to_string());
                    }
                }
                Ok(())
            }
            TunnelProviderKind::Frp => {
                let addr = self
                    .frp_server_addr
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "FRP server address is required".to_string())?;
                if addr.contains('/') || addr.contains(' ') {
                    return Err("FRP server address must be a host or IP, not a URL".to_string());
                }
                if self.frp_server_port == Some(0) {
                    return Err("FRP server port must be 1..=65535".to_string());
                }
                let domain = self
                    .frp_custom_domain
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                if let Some(domain) = domain {
                    normalize_hostname(domain)?;
                }
                if domain.is_none() && self.frp_remote_port.unwrap_or(0) == 0 {
                    return Err(
                        "FRP requires a custom domain or a remote port for the public URL"
                            .to_string(),
                    );
                }
                Ok(())
            }
        }
    }

    pub fn public_origin(&self) -> Result<String, String> {
        match self.provider {
            TunnelProviderKind::CloudflareQuick => {
                Err("quick tunnel Origin is produced by cloudflared".to_string())
            }
            TunnelProviderKind::CloudflareNamed => {
                let host =
                    normalize_hostname(self.cloudflare_named_hostname.as_deref().unwrap_or(""))?;
                Ok(format!("https://{host}"))
            }
            TunnelProviderKind::Frp => {
                let scheme = if self.frp_public_https {
                    "https"
                } else {
                    "http"
                };
                if let Some(domain) = self
                    .frp_custom_domain
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    let host = normalize_hostname(domain)?;
                    return Ok(format!("{scheme}://{host}"));
                }
                let addr = self
                    .frp_server_addr
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "FRP server address is required".to_string())?;
                let port = self.frp_remote_port.filter(|p| *p > 0).ok_or_else(|| {
                    "FRP remote port is required without a custom domain".to_string()
                })?;
                Ok(format!("{scheme}://{addr}:{port}"))
            }
        }
    }
}

/// Renderer-facing view: never includes raw secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelConfigView {
    pub provider: TunnelProviderKind,
    pub cloudflare_named_hostname: Option<String>,
    pub cloudflare_named_local_port: Option<u16>,
    pub cloudflare_named_token_set: bool,
    pub frp_server_addr: Option<String>,
    pub frp_server_port: Option<u16>,
    pub frp_custom_domain: Option<String>,
    pub frp_remote_port: Option<u16>,
    pub frp_public_https: bool,
    pub frp_token_set: bool,
}

/// Partial update from the renderer. `None` on a secret field means "leave as-is";
/// `Some("")` clears the keyring entry; any other `Some` replaces it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelConfigUpdate {
    pub provider: TunnelProviderKind,
    #[serde(default)]
    pub cloudflare_named_hostname: Option<String>,
    #[serde(default)]
    pub cloudflare_named_local_port: Option<u16>,
    #[serde(default)]
    pub cloudflare_named_token: Option<String>,
    #[serde(default)]
    pub frp_server_addr: Option<String>,
    #[serde(default)]
    pub frp_server_port: Option<u16>,
    #[serde(default)]
    pub frp_custom_domain: Option<String>,
    #[serde(default)]
    pub frp_remote_port: Option<u16>,
    #[serde(default)]
    pub frp_public_https: Option<bool>,
    #[serde(default)]
    pub frp_token: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TunnelConfigStore {
    path: PathBuf,
}

impl TunnelConfigStore {
    #[must_use]
    pub fn new(app_data_dir: PathBuf) -> Self {
        Self {
            path: app_data_dir.join("remote-tunnel").join("config.json"),
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn for_path(path: PathBuf) -> Self {
        Self { path }
    }

    #[must_use]
    pub(crate) fn parent_dir(&self) -> PathBuf {
        self.path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    pub fn load(&self) -> Result<TunnelConfig, String> {
        if !self.path.exists() {
            return Ok(TunnelConfig::default());
        }
        let bytes =
            std::fs::read(&self.path).map_err(|e| format!("failed to read tunnel config: {e}"))?;
        serde_json::from_slice(&bytes).map_err(|e| format!("tunnel config is invalid: {e}"))
    }

    pub fn save(&self, config: &TunnelConfig) -> Result<(), String> {
        // Named/FRP may be saved incomplete while the operator fills fields.
        // `validate_for_start` is the hard gate at tunnel start.
        let bytes = serde_json::to_vec_pretty(config)
            .map_err(|e| format!("failed to serialize tunnel config: {e}"))?;
        atomic_file::replace(&self.path, &bytes)
            .map_err(|e| format!("failed to write tunnel config: {e}"))
    }

    pub fn apply_update(&self, update: TunnelConfigUpdate) -> Result<TunnelConfig, String> {
        let mut config = self.load()?;
        config.provider = update.provider;
        config.cloudflare_named_hostname = empty_to_none(update.cloudflare_named_hostname);
        config.cloudflare_named_local_port = update.cloudflare_named_local_port.filter(|p| *p > 0);
        config.frp_server_addr = empty_to_none(update.frp_server_addr);
        config.frp_server_port = update.frp_server_port.filter(|p| *p > 0);
        config.frp_custom_domain = empty_to_none(update.frp_custom_domain);
        config.frp_remote_port = update.frp_remote_port.filter(|p| *p > 0);
        if let Some(https) = update.frp_public_https {
            config.frp_public_https = https;
        }
        apply_secret(CF_NAMED_TOKEN_ACCOUNT, update.cloudflare_named_token)?;
        apply_secret(FRP_TOKEN_ACCOUNT, update.frp_token)?;
        self.save(&config)?;
        log::info!(
            target: "termul::remote::tunnel",
            "operation=tunnel_config_save provider={} stable_code=OK",
            config.provider.as_str()
        );
        Ok(config)
    }

    pub fn view(&self) -> Result<TunnelConfigView, String> {
        let config = self.load()?;
        Ok(TunnelConfigView {
            provider: config.provider,
            cloudflare_named_hostname: config.cloudflare_named_hostname,
            cloudflare_named_local_port: config.cloudflare_named_local_port,
            cloudflare_named_token_set: secret_is_set(CF_NAMED_TOKEN_ACCOUNT)?,
            frp_server_addr: config.frp_server_addr,
            frp_server_port: config.frp_server_port,
            frp_custom_domain: config.frp_custom_domain,
            frp_remote_port: config.frp_remote_port,
            frp_public_https: config.frp_public_https,
            frp_token_set: secret_is_set(FRP_TOKEN_ACCOUNT)?,
        })
    }

    pub fn named_token(&self) -> Result<String, String> {
        required_secret(CF_NAMED_TOKEN_ACCOUNT, "Cloudflare named-tunnel token")
    }

    pub fn require_frp_token(&self) -> Result<String, String> {
        required_secret(FRP_TOKEN_ACCOUNT, "FRP auth token")
    }
}

fn empty_to_none(value: Option<String>) -> Option<String> {
    value.and_then(|s| {
        let trimmed = s.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

fn apply_secret(account: &str, value: Option<String>) -> Result<(), String> {
    match value {
        None => Ok(()),
        Some(raw) if raw.is_empty() => secure_storage::keyring_delete(account),
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return secure_storage::keyring_delete(account);
            }
            secure_storage::keyring_set(account, trimmed)
        }
    }
}

fn secret_is_set(account: &str) -> Result<bool, String> {
    Ok(secure_storage::keyring_get(account)?
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty()))
}

fn optional_secret(account: &str) -> Result<Option<String>, String> {
    Ok(secure_storage::keyring_get(account)?.and_then(|s| {
        let trimmed = s.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }))
}

fn required_secret(account: &str, label: &str) -> Result<String, String> {
    optional_secret(account)?.ok_or_else(|| format!("{label} is not set"))
}

/// Accept `example.com` or `https://example.com`; reject credentials and paths.
pub fn normalize_hostname(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("public hostname is required".to_string());
    }
    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let parsed =
        Url::parse(&candidate).map_err(|_| "public hostname is not a valid host".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("public hostname must use http or https".to_string());
    }
    if parsed.username() != "" || parsed.password().is_some() {
        return Err("public hostname must not include credentials".to_string());
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "public hostname is not a valid host".to_string())?;
    if host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1" {
        return Err("public hostname must not be a loopback address".to_string());
    }
    let path = parsed.path();
    if !path.is_empty() && path != "/" {
        return Err("public hostname must not include a path".to_string());
    }
    Ok(host.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_provider_is_quick_tunnel() {
        assert_eq!(
            TunnelConfig::default().provider,
            TunnelProviderKind::CloudflareQuick
        );
        assert_eq!(TunnelConfig::default().preferred_bind_port(), None);
    }

    #[test]
    fn named_provider_uses_stable_local_port() {
        let mut config = TunnelConfig::default();
        config.provider = TunnelProviderKind::CloudflareNamed;
        assert_eq!(config.preferred_bind_port(), Some(DEFAULT_NAMED_LOCAL_PORT));
        config.cloudflare_named_local_port = Some(8787);
        assert_eq!(config.preferred_bind_port(), Some(8787));
    }

    #[test]
    fn named_origin_normalizes_hostname() {
        let config = TunnelConfig {
            provider: TunnelProviderKind::CloudflareNamed,
            cloudflare_named_hostname: Some("https://Termul.Example.com/".to_string()),
            ..TunnelConfig::default()
        };
        let origin = config.public_origin().unwrap();
        assert!(origin.eq_ignore_ascii_case("https://Termul.Example.com"));
    }

    #[test]
    fn hostname_rejects_credentials_and_loopback() {
        assert!(normalize_hostname("https://user:pass@example.com").is_err());
        assert!(normalize_hostname("localhost").is_err());
        assert!(normalize_hostname("https://example.com/path").is_err());
        assert_eq!(normalize_hostname("example.com").unwrap(), "example.com");
    }

    #[test]
    fn frp_requires_domain_or_remote_port() {
        let mut config = TunnelConfig {
            provider: TunnelProviderKind::Frp,
            frp_server_addr: Some("1.2.3.4".to_string()),
            ..TunnelConfig::default()
        };
        assert!(config.validate_for_start().is_err());
        config.frp_remote_port = Some(8443);
        assert!(config.validate_for_start().is_ok());
        assert_eq!(config.public_origin().unwrap(), "https://1.2.3.4:8443");
        config.frp_public_https = false;
        config.frp_custom_domain = Some("termul.example.com".to_string());
        assert_eq!(config.public_origin().unwrap(), "http://termul.example.com");
    }

    #[test]
    fn view_never_serializes_token_field_names_as_values() {
        let view = TunnelConfigView {
            provider: TunnelProviderKind::CloudflareQuick,
            cloudflare_named_hostname: None,
            cloudflare_named_local_port: None,
            cloudflare_named_token_set: false,
            frp_server_addr: None,
            frp_server_port: None,
            frp_custom_domain: None,
            frp_remote_port: None,
            frp_public_https: true,
            frp_token_set: false,
        };
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains("eyJ"));
        assert!(json.contains("cloudflareNamedTokenSet"));
        assert!(!json.contains("cloudflareNamedToken\""));
    }

    #[test]
    fn persist_roundtrip_omits_none_fields() {
        let dir = tempfile::tempdir().unwrap();
        let store = TunnelConfigStore::for_path(dir.path().join("config.json"));
        let config = TunnelConfig::default();
        store.save(&config).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded, config);
    }
}
