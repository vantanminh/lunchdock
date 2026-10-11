use thiserror::Error;

/// Domain error returned by validation and shared helpers.
#[derive(Debug, Error)]
pub enum Error {
    /// Resource name failed [`crate::validate_name`].
    #[error("invalid name '{0}'")]
    InvalidName(String),

    /// Caller sent a value the domain rejects.
    #[error("{0}")]
    Validation(String),

    /// AES-GCM rejected the key or the ciphertext.
    #[error("crypto failure")]
    Crypto,

    /// A node driver could not complete the requested operation.
    #[error("{0}")]
    Node(String),
}

impl PartialEq for Error {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::InvalidName(a), Self::InvalidName(b)) => a == b,
            (Self::Validation(a), Self::Validation(b)) => a == b,
            (Self::Crypto, Self::Crypto) => true,
            (Self::Node(a), Self::Node(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for Error {}
