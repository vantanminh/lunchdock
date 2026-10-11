//! Image reference helpers that never shell out.

use crate::Error;

/// Registry host Docker would use for `image`.
///
/// `nginx:1` is `docker.io`. `ghcr.io/acme/app:1` is `ghcr.io`.
/// `localhost:5000/app:1` keeps the port.
pub fn image_registry(image: &str) -> &str {
    match image.split_once('/') {
        Some((remote, _))
            if remote == "localhost" || remote.contains('.') || remote.contains(':') =>
        {
            remote
        }
        _ => "docker.io",
    }
}

/// `{service}-{generation}`, still a Docker name.
pub fn container_name(service: &str, generation: i32) -> Result<String, Error> {
    crate::validate_name(service)?;
    if generation < 1 {
        return Err(Error::Validation("generation must be positive".into()));
    }
    let name = format!("{service}-{generation}");
    crate::validate_name(&name)?;
    Ok(name)
}

/// Saved registry, without the secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryRecord {
    pub name: String,
    pub url: String,
}

/// Pick an explicit registry, otherwise the one whose host matches the image.
pub fn select_registry<'a>(
    image: &str,
    explicit: Option<&'a RegistryRecord>,
    registries: &'a [RegistryRecord],
) -> Option<&'a RegistryRecord> {
    if let Some(explicit) = explicit {
        return Some(explicit);
    }
    let host = image_registry(image);
    registries
        .iter()
        .find(|registry| registry_hosts_match(&registry.url, host))
}

fn registry_hosts_match(stored: &str, image_host: &str) -> bool {
    let stored = normalize_registry(stored);
    let image_host = normalize_registry(image_host);
    stored == image_host
        || (image_host == "docker.io" && matches!(stored.as_str(), "docker.io" | "index.docker.io"))
}

/// Accept `ghcr.io`, `https://ghcr.io`, or `localhost:5000` and store the host.
pub fn validate_registry_url(value: &str) -> Result<String, Error> {
    let host = normalize_registry(value);
    let ok = !host.is_empty()
        && host.len() <= 253
        && !host.contains('/')
        && !host.contains('@')
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':'));
    if ok {
        Ok(host)
    } else {
        Err(Error::Validation("invalid registry url".into()))
    }
}

/// Strip a scheme and trailing slash so stored URLs compare with image hosts.
pub fn normalize_registry(value: &str) -> String {
    let trimmed = value.trim().trim_end_matches('/');
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    without_scheme.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_host_follows_docker_rules() {
        assert_eq!(image_registry("nginx:1.27"), "docker.io");
        assert_eq!(image_registry("ghcr.io/acme/app:1"), "ghcr.io");
        assert_eq!(
            image_registry("localhost:5000/app:latest"),
            "localhost:5000"
        );
        assert_eq!(image_registry("redis"), "docker.io");
    }

    #[test]
    fn explicit_registry_wins_and_ghcr_matches_by_host() {
        let ghcr = RegistryRecord {
            name: "ghcr".into(),
            url: "https://ghcr.io".into(),
        };
        let hub = RegistryRecord {
            name: "hub".into(),
            url: "docker.io".into(),
        };
        let all = [ghcr.clone(), hub.clone()];
        assert_eq!(
            select_registry("ghcr.io/acme/private:1", None, &all).map(|item| item.name.as_str()),
            Some("ghcr")
        );
        assert_eq!(
            select_registry("nginx:1", None, &all).map(|item| item.name.as_str()),
            Some("hub")
        );
        assert_eq!(
            select_registry("nginx:1", Some(&ghcr), &all).map(|item| item.name.as_str()),
            Some("ghcr")
        );
    }

    #[test]
    fn container_names_stay_inside_the_docker_limit() {
        assert_eq!(container_name("web", 2).unwrap(), "web-2");
        assert!(container_name(&"a".repeat(63), 1).is_err());
        assert!(container_name("web", 0).is_err());
    }
}
