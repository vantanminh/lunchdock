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
}
