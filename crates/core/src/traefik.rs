//! Traefik v3 dynamic configuration.
//!
//! Routes are file-provider routers. Custom certificates are listed in the
//! TLS store so Traefik selects them by SNI, including wildcards.

use crate::Error;

/// Docker network shared by Traefik and routed containers.
pub const PROXY_NETWORK: &str = "rudder-proxy";

/// Path inside the Traefik container. The install mounts the panel data directory here.
pub const TRAEFIK_CERT_DIR: &str = "/etc/rudder/traefik/certs";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsMode {
    None,
    LetsEncrypt,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyRoute {
    pub name: String,
    pub host: String,
    pub path_prefix: String,
    pub container: String,
    pub port: u16,
    pub https_redirect: bool,
    pub tls: TlsMode,
    pub basic_auth_user: Option<String>,
    pub basic_auth_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsMaterial {
    pub cert_file: String,
    pub key_file: String,
}

pub fn validate_host(host: &str) -> Result<(), Error> {
    if host.len() > 253 || host.is_empty() || host.contains('*') {
        return Err(Error::Validation("invalid host".into()));
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return Err(Error::Validation("host needs a dot".into()));
    }
    for label in labels {
        if label.is_empty() || label.len() > 63 {
            return Err(Error::Validation("invalid host".into()));
        }
        let bytes = label.as_bytes();
        if bytes[0] == b'-' || bytes[bytes.len() - 1] == b'-' {
            return Err(Error::Validation("invalid host".into()));
        }
        if !bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        {
            return Err(Error::Validation("invalid host".into()));
        }
    }
    Ok(())
}

pub fn validate_path_prefix(path: &str) -> Result<(), Error> {
    if path == "/" {
        return Ok(());
    }
    if path.len() > 200
        || !path.starts_with('/')
        || path.contains("//")
        || path.split('/').any(|part| part == "..")
        || !path.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~')
        })
    {
        return Err(Error::Validation("invalid path prefix".into()));
    }
    Ok(())
}

pub fn preview_hostname(service: &str, nonce: &str, public_ip: &str) -> Result<String, Error> {
    crate::validate_name(service)?;
    if nonce.len() != 8 || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::Validation(
            "preview nonce must be 8 hex characters".into(),
        ));
    }
    let ip = if public_ip.is_empty() {
        "127.0.0.1"
    } else {
        public_ip
    };
    if !is_ipv4(ip) {
        return Err(Error::Validation(
            "preview domains need an IPv4 public address".into(),
        ));
    }
    let prefix: String = service.chars().take(20).collect();
    let nonce = nonce.to_ascii_lowercase();
    let host = format!("{prefix}-{nonce}.{ip}.sslip.io");
    validate_host(&host)?;
    Ok(host)
}

fn is_ipv4(value: &str) -> bool {
    let octets: Vec<&str> = value.split('.').collect();
    if octets.len() != 4 {
        return false;
    }
    octets.iter().all(|octet| {
        !octet.is_empty()
            && octet.len() <= 3
            && octet.bytes().all(|byte| byte.is_ascii_digit())
            && octet.parse::<u8>().is_ok()
            && !(octet.len() > 1 && octet.starts_with('0'))
    })
}

/// Render the file-provider document. Inputs are validated so the YAML cannot be injected.
pub fn render_dynamic_config(
    routes: &[ProxyRoute],
    certs: &[TlsMaterial],
) -> Result<String, Error> {
    let mut yaml = String::from("http:\n  routers:\n");
    if routes.is_empty() {
        yaml.push_str("    {}\n");
    }
    for route in routes {
        validate_route(route)?;
        let rule = format!(
            "Host(`{}`) && PathPrefix(`{}`)",
            route.host, route.path_prefix
        );
        yaml.push_str(&format!("    {}:\n", route.name));
        yaml.push_str(&format!("      rule: \"{rule}\"\n"));
        yaml.push_str("      service: ");
        yaml.push_str(&route.name);
        yaml.push('\n');
        match route.tls {
            TlsMode::None => {
                yaml.push_str("      entryPoints:\n        - web\n");
            }
            TlsMode::LetsEncrypt => {
                yaml.push_str("      entryPoints:\n        - websecure\n");
                yaml.push_str("      tls:\n        certResolver: letsencrypt\n");
            }
            TlsMode::Custom => {
                yaml.push_str("      entryPoints:\n        - websecure\n");
                yaml.push_str("      tls: {}\n");
            }
        }
        let mut middlewares = Vec::new();
        if route.basic_auth_user.is_some() {
            middlewares.push(format!("{}-auth", route.name));
        }
        if !middlewares.is_empty() {
            yaml.push_str("      middlewares:\n");
            for name in &middlewares {
                yaml.push_str(&format!("        - {name}\n"));
            }
        }
        if route.https_redirect && route.tls != TlsMode::None {
            yaml.push_str(&format!("    {}-http:\n", route.name));
            yaml.push_str(&format!("      rule: \"{rule}\"\n"));
            yaml.push_str("      entryPoints:\n        - web\n");
            yaml.push_str(&format!(
                "      middlewares:\n        - {}-https\n",
                route.name
            ));
            yaml.push_str("      service: ");
            yaml.push_str(&route.name);
            yaml.push('\n');
        }
    }
    yaml.push_str("  services:\n");
    if routes.is_empty() {
        yaml.push_str("    {}\n");
    }
    for route in routes {
        yaml.push_str(&format!("    {}:\n", route.name));
        yaml.push_str("      loadBalancer:\n        servers:\n");
        yaml.push_str(&format!(
            "          - url: \"http://{}:{}\"\n",
            route.container, route.port
        ));
    }
    yaml.push_str("  middlewares:\n");
    let mut any_middleware = false;
    for route in routes {
        if route.https_redirect && route.tls != TlsMode::None {
            any_middleware = true;
            yaml.push_str(&format!("    {}-https:\n", route.name));
            yaml.push_str(
                "      redirectScheme:\n        scheme: https\n        permanent: true\n",
            );
        }
        if let (Some(user), Some(hash)) = (&route.basic_auth_user, &route.basic_auth_hash) {
            any_middleware = true;
            yaml.push_str(&format!("    {}-auth:\n", route.name));
            yaml.push_str("      basicAuth:\n        users:\n");
            yaml.push_str(&format!("          - \"{user}:{hash}\"\n"));
        }
    }
    if !any_middleware {
        yaml.push_str("    {}\n");
    }
    yaml.push_str("tls:\n  certificates:\n");
    if certs.is_empty() {
        yaml.push_str("    []\n");
    }
    for cert in certs {
        validate_cert_path(&cert.cert_file)?;
        validate_cert_path(&cert.key_file)?;
        yaml.push_str(&format!("    - certFile: {}\n", cert.cert_file));
        yaml.push_str(&format!("      keyFile: {}\n", cert.key_file));
    }
    Ok(yaml)
}

fn validate_route(route: &ProxyRoute) -> Result<(), Error> {
    crate::validate_name(&route.name)?;
    crate::validate_name(&route.container)?;
    validate_host(&route.host)?;
    validate_path_prefix(&route.path_prefix)?;
    if route.port == 0 {
        return Err(Error::Validation(
            "target port must be between 1 and 65535".into(),
        ));
    }
    if route.basic_auth_user.is_some() != route.basic_auth_hash.is_some() {
        return Err(Error::Validation(
            "basic auth needs both a user and a hash".into(),
        ));
    }
    if let Some(user) = &route.basic_auth_user {
        if user.is_empty()
            || user.len() > 64
            || !user
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(Error::Validation("invalid basic auth user".into()));
        }
    }
    if let Some(hash) = &route.basic_auth_hash {
        if hash.len() < 20
            || hash.len() > 128
            || !hash
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'/' | b'$'))
        {
            return Err(Error::Validation("invalid basic auth hash".into()));
        }
    }
    Ok(())
}

fn validate_cert_path(path: &str) -> Result<(), Error> {
    if path.starts_with(TRAEFIK_CERT_DIR)
        && !path.contains("..")
        && path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'_'))
    {
        Ok(())
    } else {
        Err(Error::Validation("invalid certificate path".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(tls: TlsMode) -> ProxyRoute {
        ProxyRoute {
            name: "rapp".into(),
            host: "app.example.com".into(),
            path_prefix: "/api".into(),
            container: "app-1".into(),
            port: 8080,
            https_redirect: true,
            tls,
            basic_auth_user: None,
            basic_auth_hash: None,
        }
    }

    #[test]
    fn letsencrypt_uses_the_resolver_and_redirects() {
        let yaml = render_dynamic_config(&[route(TlsMode::LetsEncrypt)], &[]).unwrap();
        assert!(yaml.contains("Host(`app.example.com`) && PathPrefix(`/api`)"));
        assert!(yaml.contains("certResolver: letsencrypt"));
        assert!(yaml.contains("redirectScheme:"));
        assert!(yaml.contains("http://app-1:8080"));
        assert!(!yaml.contains("certFile:"));
    }

    #[test]
    fn custom_certificates_are_listed_for_sni() {
        let yaml = render_dynamic_config(
            &[route(TlsMode::Custom)],
            &[TlsMaterial {
                cert_file: format!("{TRAEFIK_CERT_DIR}/cert.crt"),
                key_file: format!("{TRAEFIK_CERT_DIR}/cert.key"),
            }],
        )
        .unwrap();
        assert!(yaml.contains("tls: {}"));
        assert!(yaml.contains(&format!("certFile: {TRAEFIK_CERT_DIR}/cert.crt")));
        assert!(!yaml.contains("certResolver"));
    }

    #[test]
    fn hosts_and_paths_reject_injection() {
        assert!(validate_host("app.example.com").is_ok());
        assert!(validate_host("bad`host.example").is_err());
        assert!(validate_host("*.example.com").is_err());
        assert!(validate_path_prefix("/").is_ok());
        assert!(validate_path_prefix("/api").is_ok());
        assert!(validate_path_prefix("/../etc").is_err());
    }

    #[test]
    fn preview_host_uses_sslip() {
        let host = preview_hostname("web", "ab12cd34", "203.0.113.8").unwrap();
        assert_eq!(host, "web-ab12cd34.203.0.113.8.sslip.io");
        assert!(preview_hostname("web", "short", "203.0.113.8").is_err());
        assert!(preview_hostname("web", "ab12cd34", "not-an-ip").is_err());
    }
}
