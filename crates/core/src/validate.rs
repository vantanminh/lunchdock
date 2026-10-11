use crate::Error;

/// Docker-safe resource name: lowercase ASCII, digits, and hyphens.
///
/// Pattern: `^[a-z0-9][a-z0-9-]{0,62}$` (1 to 63 characters).
pub fn validate_name(name: &str) -> Result<(), Error> {
    if is_valid_name(name) {
        Ok(())
    } else {
        Err(Error::InvalidName(name.to_owned()))
    }
}

pub fn is_valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 63 {
        return false;
    }
    let first = bytes[0];
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    bytes
        .iter()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

/// Image references are passed as a single Docker argument, never through a shell.
pub fn validate_image_ref(image: &str) -> Result<(), Error> {
    let ok = !image.is_empty()
        && image.len() <= 512
        && !image.chars().any(|ch| {
            ch.is_whitespace()
                || ch.is_control()
                || matches!(ch, ';' | '|' | '&' | '$' | '`' | '\'' | '"' | '\\')
        });
    if ok {
        Ok(())
    } else {
        Err(Error::Validation("invalid image reference".into()))
    }
}

/// Environment variable names accepted by Docker.
pub fn validate_env_key(key: &str) -> Result<(), Error> {
    let bytes = key.as_bytes();
    let ok = !bytes.is_empty()
        && bytes.len() <= 128
        && (bytes[0].is_ascii_alphabetic() || bytes[0] == b'_')
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_');
    if ok {
        Ok(())
    } else {
        Err(Error::Validation(format!(
            "invalid environment variable name '{key}'"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_short_and_max_length_names() {
        assert!(validate_name("a").is_ok());
        assert!(validate_name("web-1").is_ok());
        assert!(validate_name(&"a".repeat(63)).is_ok());
        assert!(validate_name("0db").is_ok());
    }

    #[test]
    fn rejects_empty_uppercase_underscore_and_too_long() {
        assert_eq!(validate_name(""), Err(Error::InvalidName(String::new())));
        assert!(validate_name("Web").is_err());
        assert!(validate_name("-web").is_err());
        assert!(validate_name("web_app").is_err());
        assert!(validate_name("web app").is_err());
        assert!(validate_name(&"a".repeat(64)).is_err());
    }

    #[test]
    fn image_refs_reject_shell_metacharacters() {
        assert!(validate_image_ref("ghcr.io/acme/app:1.2").is_ok());
        assert!(validate_image_ref("redis:7").is_ok());
        assert!(validate_image_ref("nginx:1;rm -rf /").is_err());
        assert!(validate_image_ref("has space").is_err());
        assert!(validate_image_ref("").is_err());
    }

    #[test]
    fn env_keys_are_identifiers() {
        assert!(validate_env_key("DATABASE_URL").is_ok());
        assert!(validate_env_key("1URL").is_err());
        assert!(validate_env_key("MY-VAR").is_err());
    }
}
