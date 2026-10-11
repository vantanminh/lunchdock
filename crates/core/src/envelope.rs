//! Client-side backup encryption with a per-job passphrase.
//!
//! Layout (all integers big-endian):
//!
//! ```text
//! magic "RDRENC01" (8) | salt (16) | nonce prefix (8) | chunk size u32 (4)
//! frame*: ciphertext length u32 (4) | AES-256-GCM ciphertext+tag
//! ```
//!
//! The key is Argon2id(passphrase, salt) with m=19456 KiB, t=2, p=1. Frame `n`
//! uses nonce `prefix || n as u32`, and the associated data is one byte: 1 for
//! the final frame, 0 otherwise. A stream that ends without a final frame is
//! rejected, so truncation is detected. `rudder backup decrypt` reverses this.

use aes_gcm::aead::array::Array;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::Aes256Gcm;

use crate::Error;

pub const MAGIC: &[u8; 8] = b"RDRENC01";
pub const HEADER_LEN: usize = 8 + 16 + 8 + 4;
pub const DEFAULT_CHUNK: usize = 64 * 1024;
const TAG_LEN: usize = 16;

/// Derive the 32-byte key. Slow on purpose; call once per stream.
pub fn derive_key(passphrase: &str, salt: &[u8; 16]) -> Result<[u8; 32], Error> {
    if passphrase.len() < 8 {
        return Err(Error::Validation(
            "encryption passphrase must be at least 8 characters".into(),
        ));
    }
    let params = argon2::Params::new(19_456, 2, 1, Some(32)).map_err(|_| Error::Crypto)?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = [0u8; 32];
    argon
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|_| Error::Crypto)?;
    Ok(key)
}

/// Incremental encryptor. Feed plaintext with [`Encryptor::update`], then call
/// [`Encryptor::finish`] exactly once.
pub struct Encryptor {
    cipher: Aes256Gcm,
    prefix: [u8; 8],
    counter: u32,
    chunk: usize,
    pending: Vec<u8>,
    header: Option<Vec<u8>>,
}

impl Encryptor {
    pub fn new(passphrase: &str) -> Result<Self, Error> {
        Self::with_parts(
            passphrase,
            crate::random_bytes::<16>(),
            crate::random_bytes::<8>(),
            DEFAULT_CHUNK,
        )
    }

    pub fn with_parts(
        passphrase: &str,
        salt: [u8; 16],
        prefix: [u8; 8],
        chunk: usize,
    ) -> Result<Self, Error> {
        if chunk == 0 || chunk > 16 * 1024 * 1024 {
            return Err(Error::Validation("invalid chunk size".into()));
        }
        let key = derive_key(passphrase, &salt)?;
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| Error::Crypto)?;
        let mut header = Vec::with_capacity(HEADER_LEN);
        header.extend_from_slice(MAGIC);
        header.extend_from_slice(&salt);
        header.extend_from_slice(&prefix);
        header.extend_from_slice(&(chunk as u32).to_be_bytes());
        Ok(Self {
            cipher,
            prefix,
            counter: 0,
            chunk,
            pending: Vec::with_capacity(chunk),
            header: Some(header),
        })
    }

    /// Returns ciphertext ready to write. May be empty while a chunk fills.
    pub fn update(&mut self, mut data: &[u8]) -> Result<Vec<u8>, Error> {
        let mut out = self.header.take().unwrap_or_default();
        while !data.is_empty() {
            let room = self.chunk - self.pending.len();
            let take = room.min(data.len());
            self.pending.extend_from_slice(&data[..take]);
            data = &data[take..];
            // Keep a full chunk buffered so the final frame is never empty
            // unless the whole stream is empty.
            if self.pending.len() == self.chunk && !data.is_empty() {
                let frame = std::mem::take(&mut self.pending);
                self.seal(&frame, false, &mut out)?;
            }
        }
        Ok(out)
    }

    pub fn finish(mut self) -> Result<Vec<u8>, Error> {
        let mut out = self.header.take().unwrap_or_default();
        let frame = std::mem::take(&mut self.pending);
        self.seal(&frame, true, &mut out)?;
        Ok(out)
    }

    fn seal(&mut self, plaintext: &[u8], last: bool, out: &mut Vec<u8>) -> Result<(), Error> {
        let nonce = nonce(&self.prefix, self.counter);
        self.counter = self
            .counter
            .checked_add(1)
            .ok_or_else(|| Error::Validation("backup is too large to encrypt".into()))?;
        let aad = [u8::from(last)];
        let sealed = self
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| Error::Crypto)?;
        out.extend_from_slice(&(sealed.len() as u32).to_be_bytes());
        out.extend_from_slice(&sealed);
        Ok(())
    }
}

/// Incremental decryptor. Errors are generic so a wrong passphrase and
/// tampering look the same.
pub struct Decryptor {
    passphrase: String,
    state: DecryptState,
    buffer: Vec<u8>,
    counter: u32,
}

enum DecryptState {
    Header,
    Frames {
        cipher: Box<Aes256Gcm>,
        prefix: [u8; 8],
        max_frame: usize,
    },
    Done,
}

impl Decryptor {
    pub fn new(passphrase: &str) -> Self {
        Self {
            passphrase: passphrase.to_owned(),
            state: DecryptState::Header,
            buffer: Vec::new(),
            counter: 0,
        }
    }

    pub fn update(&mut self, data: &[u8]) -> Result<Vec<u8>, Error> {
        self.buffer.extend_from_slice(data);
        let mut out = Vec::new();
        loop {
            match &self.state {
                DecryptState::Header => {
                    if self.buffer.len() < HEADER_LEN {
                        return Ok(out);
                    }
                    if &self.buffer[..8] != MAGIC {
                        return Err(Error::Validation(
                            "object is not a Rudder encrypted backup".into(),
                        ));
                    }
                    let mut salt = [0u8; 16];
                    salt.copy_from_slice(&self.buffer[8..24]);
                    let mut prefix = [0u8; 8];
                    prefix.copy_from_slice(&self.buffer[24..32]);
                    let chunk = u32::from_be_bytes([
                        self.buffer[32],
                        self.buffer[33],
                        self.buffer[34],
                        self.buffer[35],
                    ]) as usize;
                    if chunk == 0 || chunk > 16 * 1024 * 1024 {
                        return Err(Error::Crypto);
                    }
                    let key = derive_key(&self.passphrase, &salt)?;
                    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| Error::Crypto)?;
                    self.buffer.drain(..HEADER_LEN);
                    self.state = DecryptState::Frames {
                        cipher: Box::new(cipher),
                        prefix,
                        max_frame: chunk + TAG_LEN,
                    };
                }
                DecryptState::Frames {
                    cipher,
                    prefix,
                    max_frame,
                } => {
                    if self.buffer.len() < 4 {
                        return Ok(out);
                    }
                    let len = u32::from_be_bytes([
                        self.buffer[0],
                        self.buffer[1],
                        self.buffer[2],
                        self.buffer[3],
                    ]) as usize;
                    if len < TAG_LEN || len > *max_frame {
                        return Err(Error::Crypto);
                    }
                    if self.buffer.len() < 4 + len {
                        return Ok(out);
                    }
                    let nonce = nonce(prefix, self.counter);
                    let frame = &self.buffer[4..4 + len];
                    let (plain, last) = match cipher.decrypt(
                        &nonce,
                        Payload {
                            msg: frame,
                            aad: &[0],
                        },
                    ) {
                        Ok(plain) => (plain, false),
                        Err(_) => (
                            cipher
                                .decrypt(
                                    &nonce,
                                    Payload {
                                        msg: frame,
                                        aad: &[1],
                                    },
                                )
                                .map_err(|_| Error::Crypto)?,
                            true,
                        ),
                    };
                    self.counter = self.counter.checked_add(1).ok_or(Error::Crypto)?;
                    self.buffer.drain(..4 + len);
                    out.extend_from_slice(&plain);
                    if last {
                        self.state = DecryptState::Done;
                    }
                }
                DecryptState::Done => {
                    if self.buffer.is_empty() {
                        return Ok(out);
                    }
                    return Err(Error::Crypto);
                }
            }
        }
    }

    /// Fails when the final frame never arrived.
    pub fn finish(self) -> Result<(), Error> {
        match self.state {
            DecryptState::Done if self.buffer.is_empty() => Ok(()),
            _ => Err(Error::Validation(
                "encrypted backup is truncated or the passphrase is wrong".into(),
            )),
        }
    }
}

fn nonce(prefix: &[u8; 8], counter: u32) -> Array<u8, aes_gcm::aead::consts::U12> {
    let mut bytes = [0u8; 12];
    bytes[..8].copy_from_slice(prefix);
    bytes[8..].copy_from_slice(&counter.to_be_bytes());
    Array::from(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encrypt_all(passphrase: &str, data: &[u8], chunk: usize, feed: usize) -> Vec<u8> {
        let mut enc = Encryptor::with_parts(passphrase, [1; 16], [2; 8], chunk).unwrap();
        let mut out = Vec::new();
        for part in data.chunks(feed.max(1)) {
            out.extend(enc.update(part).unwrap());
        }
        out.extend(enc.finish().unwrap());
        out
    }

    fn decrypt_all(passphrase: &str, data: &[u8], feed: usize) -> Result<Vec<u8>, Error> {
        let mut dec = Decryptor::new(passphrase);
        let mut out = Vec::new();
        for part in data.chunks(feed.max(1)) {
            out.extend(dec.update(part)?);
        }
        dec.finish()?;
        Ok(out)
    }

    #[test]
    fn round_trip_across_chunk_boundaries() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        for (chunk, feed) in [(16, 7), (1024, 1024), (4096, 1), (10_000, 333)] {
            let sealed = encrypt_all("correct horse", &data, chunk, feed);
            assert_eq!(&sealed[..8], MAGIC);
            assert_eq!(decrypt_all("correct horse", &sealed, 97).unwrap(), data);
        }
        let empty = encrypt_all("correct horse", b"", 64, 1);
        assert_eq!(decrypt_all("correct horse", &empty, 5).unwrap(), b"");
    }

    #[test]
    fn wrong_passphrase_tamper_and_truncation_fail() {
        let data = vec![7u8; 5000];
        let sealed = encrypt_all("correct horse", &data, 1024, 512);
        assert!(decrypt_all("wrong horse!", &sealed, 512).is_err());
        let mut tampered = sealed.clone();
        tampered[HEADER_LEN + 10] ^= 1;
        assert!(decrypt_all("correct horse", &tampered, 512).is_err());
        let frame = 4 + 1024 + TAG_LEN;
        let truncated = &sealed[..HEADER_LEN + frame];
        assert!(decrypt_all("correct horse", truncated, 512).is_err());
        assert!(derive_key("short", &[0; 16]).is_err());
    }
}
