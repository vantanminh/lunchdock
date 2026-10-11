//! AES-256-GCM blobs. Layout is `nonce (12 bytes) || ciphertext || tag`.
//!
//! Callers must not log plaintext. Decryption failure is a single error so
//! padding and authentication failures are not distinguishable.

use aes_gcm::aead::array::Array;
use aes_gcm::aead::Aead;
use aes_gcm::aead::KeyInit;
use aes_gcm::Aes256Gcm;

use crate::Error;

const NONCE_LEN: usize = 12;

/// Master-key cipher. The key is zeroized only when the process drops it.
pub struct Cipher {
    key: [u8; 32],
}

impl Cipher {
    pub fn new(key: [u8; 32]) -> Self {
        Self { key }
    }

    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>, Error> {
        let cipher = Aes256Gcm::new_from_slice(&self.key).map_err(|_| Error::Crypto)?;
        let nonce = Array::<u8, _>::from(crate::random_bytes::<NONCE_LEN>());
        let ciphertext = cipher
            .encrypt(&nonce, plaintext)
            .map_err(|_| Error::Crypto)?;
        let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        out.extend_from_slice(nonce.as_slice());
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    pub fn decrypt(&self, blob: &[u8]) -> Result<Vec<u8>, Error> {
        if blob.len() < NONCE_LEN + 16 {
            return Err(Error::Crypto);
        }
        let (nonce_bytes, ciphertext) = blob.split_at(NONCE_LEN);
        let cipher = Aes256Gcm::new_from_slice(&self.key).map_err(|_| Error::Crypto)?;
        let nonce = Array::<u8, _>::try_from(nonce_bytes).map_err(|_| Error::Crypto)?;
        cipher
            .decrypt(&nonce, ciphertext)
            .map_err(|_| Error::Crypto)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_tamper_detection() {
        let cipher = Cipher::new([9u8; 32]);
        let blob = cipher.encrypt(b"postgres://app:secret@db/app").unwrap();
        assert_eq!(
            cipher.decrypt(&blob).unwrap(),
            b"postgres://app:secret@db/app"
        );
        let mut tampered = blob.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0x01;
        assert_eq!(cipher.decrypt(&tampered), Err(Error::Crypto));
        assert!(cipher.decrypt(&[1, 2, 3]).is_err());
    }
}
