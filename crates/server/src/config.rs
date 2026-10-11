//! Process configuration loaded from the environment.
//!
//! Secrets are never included in [`Debug`] output.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use base64::Engine;
use thiserror::Error;

const DEFAULT_BIND: &str = "0.0.0.0:3000";
const DEFAULT_DATA_DIR: &str = "/var/lib/rudder";
const DEFAULT_GITHUB_REPO: &str = "vantanminh/lunchdock";

/// How tracing lines are encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// One JSON object per event, suitable for the log explorer.
    Json,
    /// Human-readable text for local terminals.
    Pretty,
}

/// Control-plane configuration.
pub struct Config {
    pub database_url: String,
    pub bind: SocketAddr,
    master_key: [u8; 32],
    pub data_dir: PathBuf,
    pub public_url: Option<String>,
    pub github_repo: String,
    pub log_format: LogFormat,
    pub cookie_secure: bool,
    /// When true, the first `X-Forwarded-For` hop is the client IP.
    pub trust_proxy: bool,
}

impl Config {
    /// Load configuration from process environment variables.
    pub fn load() -> Result<Self, ConfigError> {
        let vars: HashMap<String, String> = std::env::vars().collect();
        Self::from_map(&vars, |path| fs::read(path))
    }

    /// AES-256-GCM master key. Callers must not log the returned bytes.
    pub fn master_key(&self) -> &[u8; 32] {
        &self.master_key
    }

    pub fn from_map(
        vars: &HashMap<String, String>,
        read_file: impl Fn(&Path) -> std::io::Result<Vec<u8>>,
    ) -> Result<Self, ConfigError> {
        let database_url = required(vars, "DATABASE_URL")?;
        let bind = vars
            .get("RUDDER_BIND")
            .map(String::as_str)
            .unwrap_or(DEFAULT_BIND)
            .parse::<std::net::SocketAddr>()
            .map_err(|error| ConfigError::Invalid {
                name: "RUDDER_BIND",
                reason: error.to_string(),
            })?;
        let data_dir = PathBuf::from(
            vars.get("RUDDER_DATA_DIR")
                .map(String::as_str)
                .unwrap_or(DEFAULT_DATA_DIR),
        );
        let public_url = vars
            .get("RUDDER_PUBLIC_URL")
            .cloned()
            .filter(|v| !v.is_empty());
        let github_repo = vars
            .get("RUDDER_GITHUB_REPO")
            .cloned()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_GITHUB_REPO.to_owned());
        if !github_repo.contains('/') || github_repo.contains(' ') {
            return Err(ConfigError::Invalid {
                name: "RUDDER_GITHUB_REPO",
                reason: "expected owner/name".into(),
            });
        }
        let log_format = match vars
            .get("RUDDER_LOG_FORMAT")
            .map(String::as_str)
            .unwrap_or("json")
        {
            "json" => LogFormat::Json,
            "pretty" => LogFormat::Pretty,
            other => {
                return Err(ConfigError::Invalid {
                    name: "RUDDER_LOG_FORMAT",
                    reason: format!("unknown format '{other}'"),
                });
            }
        };
        let cookie_secure = parse_bool(vars, "RUDDER_COOKIE_SECURE", true)?;
        let trust_proxy = parse_bool(vars, "RUDDER_TRUST_PROXY", true)?;
        let master_key = load_master_key(vars, read_file)?;
        Ok(Self {
            database_url,
            bind,
            master_key,
            data_dir,
            public_url,
            github_repo,
            log_format,
            cookie_secure,
            trust_proxy,
        })
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Config")
            .field("database_url", &redact_database_url(&self.database_url))
            .field("bind", &self.bind)
            .field("master_key", &"[redacted]")
            .field("data_dir", &self.data_dir)
            .field("public_url", &self.public_url)
            .field("github_repo", &self.github_repo)
            .field("log_format", &self.log_format)
            .field("cookie_secure", &self.cookie_secure)
            .field("trust_proxy", &self.trust_proxy)
            .finish()
    }
}

fn parse_bool(
    vars: &HashMap<String, String>,
    name: &'static str,
    default: bool,
) -> Result<bool, ConfigError> {
    match vars.get(name).map(String::as_str) {
        None => Ok(default),
        Some("1") | Some("true") | Some("TRUE") => Ok(true),
        Some("0") | Some("false") | Some("FALSE") => Ok(false),
        Some(other) => Err(ConfigError::Invalid {
            name,
            reason: format!("expected true or false, got '{other}'"),
        }),
    }
}

fn required(vars: &HashMap<String, String>, name: &'static str) -> Result<String, ConfigError> {
    vars.get(name)
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or(ConfigError::Missing(name))
}

fn load_master_key(
    vars: &HashMap<String, String>,
    read_file: impl Fn(&Path) -> std::io::Result<Vec<u8>>,
) -> Result<[u8; 32], ConfigError> {
    if let Some(path) = vars.get("RUDDER_MASTER_KEY_FILE") {
        let bytes = read_file(Path::new(path)).map_err(ConfigError::Io)?;
        return decode_master_key_bytes(&bytes);
    }
    if let Some(raw) = vars.get("RUDDER_MASTER_KEY") {
        return decode_master_key_text(raw);
    }
    Err(ConfigError::Missing("RUDDER_MASTER_KEY"))
}

fn decode_master_key_bytes(bytes: &[u8]) -> Result<[u8; 32], ConfigError> {
    if bytes.len() == 32 {
        let mut key = [0u8; 32];
        key.copy_from_slice(bytes);
        return Ok(key);
    }
    let text = String::from_utf8_lossy(bytes);
    decode_master_key_text(text.trim())
}

fn decode_master_key_text(raw: &str) -> Result<[u8; 32], ConfigError> {
    let trimmed = raw.trim();
    if let Some(key) = decode_hex_key(trimmed) {
        return Ok(key);
    }
    if let Some(key) = decode_base64_key(trimmed) {
        return Ok(key);
    }
    Err(ConfigError::Invalid {
        name: "RUDDER_MASTER_KEY",
        reason: "expected 32 raw bytes, 64 hex characters, or standard base64".into(),
    })
}

fn decode_hex_key(raw: &str) -> Option<[u8; 32]> {
    if raw.len() != 64 || !raw.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut key = [0u8; 32];
    for (index, chunk) in raw.as_bytes().chunks(2).enumerate() {
        let text = std::str::from_utf8(chunk).ok()?;
        key[index] = u8::from_str_radix(text, 16).ok()?;
    }
    Some(key)
}

fn decode_base64_key(raw: &str) -> Option<[u8; 32]> {
    let engines = [
        base64::engine::general_purpose::STANDARD,
        base64::engine::general_purpose::STANDARD_NO_PAD,
        base64::engine::general_purpose::URL_SAFE,
        base64::engine::general_purpose::URL_SAFE_NO_PAD,
    ];
    for engine in engines {
        if let Ok(bytes) = engine.decode(raw) {
            if bytes.len() == 32 {
                let mut key = [0u8; 32];
                key.copy_from_slice(&bytes);
                return Some(key);
            }
        }
    }
    None
}

fn redact_database_url(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => match rest.split_once('@') {
            Some((_, host)) => format!("{scheme}://[redacted]@{host}"),
            None => format!("{scheme}://[redacted]"),
        },
        None => "[redacted]".to_owned(),
    }
}

/// Configuration failure. The display text never includes key material.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("missing required environment variable {0}")]
    Missing(&'static str),
    #[error("invalid {name}: {reason}")]
    Invalid { name: &'static str, reason: String },
    #[error("failed to read master key file")]
    Io(#[source] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_vars() -> HashMap<String, String> {
        HashMap::from([
            (
                "DATABASE_URL".into(),
                "postgres://rudder:secret@localhost/rudder".into(),
            ),
            ("RUDDER_MASTER_KEY".into(), "11".repeat(32)),
        ])
    }

    #[test]
    fn loads_defaults_and_redacts_secrets() {
        let config = Config::from_map(&base_vars(), |_| unreachable!()).unwrap();
        assert_eq!(config.bind, "0.0.0.0:3000".parse().unwrap());
        assert_eq!(config.github_repo, DEFAULT_GITHUB_REPO);
        assert_eq!(config.log_format, LogFormat::Json);
        assert!(config.cookie_secure);
        assert_eq!(config.master_key()[0], 0x11);
        let rendered = format!("{config:?}");
        assert!(rendered.contains("[redacted]"));
        assert!(!rendered.contains("secret"));
        assert!(!rendered.contains("1111"));
    }

    #[test]
    fn accepts_standard_base64_master_key() {
        let encoded = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
        let mut vars = base_vars();
        vars.insert("RUDDER_MASTER_KEY".into(), encoded);
        vars.insert("RUDDER_BIND".into(), "127.0.0.1:8080".into());
        vars.insert("RUDDER_LOG_FORMAT".into(), "pretty".into());
        vars.insert("RUDDER_COOKIE_SECURE".into(), "false".into());
        let config = Config::from_map(&vars, |_| unreachable!()).unwrap();
        assert_eq!(config.master_key(), &[7u8; 32]);
        assert_eq!(config.bind, "127.0.0.1:8080".parse().unwrap());
        assert_eq!(config.log_format, LogFormat::Pretty);
        assert!(!config.cookie_secure);
    }

    #[test]
    fn reads_master_key_file_as_raw_bytes() {
        let mut vars = base_vars();
        vars.remove("RUDDER_MASTER_KEY");
        vars.insert("RUDDER_MASTER_KEY_FILE".into(), "/keys/master".into());
        let config = Config::from_map(&vars, |path| {
            assert_eq!(path, Path::new("/keys/master"));
            Ok(vec![9u8; 32])
        })
        .unwrap();
        assert_eq!(config.master_key(), &[9u8; 32]);
    }

    #[test]
    fn rejects_missing_database_url_and_short_keys() {
        let mut vars = base_vars();
        vars.remove("DATABASE_URL");
        let error = Config::from_map(&vars, |_| unreachable!()).unwrap_err();
        assert!(matches!(error, ConfigError::Missing("DATABASE_URL")));

        let mut vars = base_vars();
        vars.insert("RUDDER_MASTER_KEY".into(), "abcd".into());
        let error = Config::from_map(&vars, |_| unreachable!()).unwrap_err();
        assert!(matches!(
            error,
            ConfigError::Invalid {
                name: "RUDDER_MASTER_KEY",
                ..
            }
        ));
    }
}
