//! Stream transforms between a dump tool and S3: compression, encryption,
//! bandwidth limits, hashing, and fan-out to several destinations.
//!
//! Every stage works chunk by chunk, so a backup never needs a temp file.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_compression::tokio::bufread::{GzipDecoder, GzipEncoder, ZstdDecoder, ZstdEncoder};
use async_compression::Level;
use bytes::Bytes;
use futures_util::{StreamExt, TryStreamExt};
use rudder_core::{ByteStream, Compression, Decryptor, Encryptor, Error};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio_util::io::{ReaderStream, StreamReader};

const IO_CHUNK: usize = 64 * 1024;

fn to_io(error: Error) -> std::io::Error {
    std::io::Error::other(error.to_string())
}

fn from_io(error: std::io::Error) -> Error {
    Error::Node(error.to_string())
}

fn reader(
    stream: ByteStream,
) -> StreamReader<impl futures_util::Stream<Item = std::io::Result<Bytes>>, Bytes> {
    StreamReader::new(stream.map_err(to_io))
}

pub fn compress(stream: ByteStream, compression: Compression, level: i32) -> ByteStream {
    match compression {
        Compression::None => stream,
        Compression::Gzip => {
            let encoder = GzipEncoder::with_quality(reader(stream), Level::Precise(level));
            Box::pin(ReaderStream::with_capacity(encoder, IO_CHUNK).map_err(from_io))
        }
        Compression::Zstd => {
            let encoder = ZstdEncoder::with_quality(reader(stream), Level::Precise(level));
            Box::pin(ReaderStream::with_capacity(encoder, IO_CHUNK).map_err(from_io))
        }
    }
}

pub fn decompress(stream: ByteStream, compression: Compression) -> ByteStream {
    match compression {
        Compression::None => stream,
        Compression::Gzip => {
            let decoder = GzipDecoder::new(reader(stream));
            Box::pin(ReaderStream::with_capacity(decoder, IO_CHUNK).map_err(from_io))
        }
        Compression::Zstd => {
            let decoder = ZstdDecoder::new(reader(stream));
            Box::pin(ReaderStream::with_capacity(decoder, IO_CHUNK).map_err(from_io))
        }
    }
}

enum Sealing {
    Open(ByteStream, Box<Encryptor>),
    Done,
}

/// Wrap `stream` in the Rudder envelope. Key derivation happens here, once.
pub fn encrypt(stream: ByteStream, passphrase: &str) -> Result<ByteStream, Error> {
    let encryptor = Encryptor::new(passphrase)?;
    let state = Sealing::Open(stream, Box::new(encryptor));
    Ok(Box::pin(futures_util::stream::unfold(
        state,
        |state| async move {
            let Sealing::Open(mut inner, mut encryptor) = state else {
                return None;
            };
            loop {
                match inner.next().await {
                    Some(Ok(chunk)) => match encryptor.update(&chunk) {
                        Ok(out) if out.is_empty() => continue,
                        Ok(out) => {
                            return Some((Ok(Bytes::from(out)), Sealing::Open(inner, encryptor)))
                        }
                        Err(error) => return Some((Err(error), Sealing::Done)),
                    },
                    Some(Err(error)) => return Some((Err(error), Sealing::Done)),
                    None => {
                        return Some((encryptor.finish().map(Bytes::from), Sealing::Done));
                    }
                }
            }
        },
    )))
}

enum Opening {
    Open(ByteStream, Box<Decryptor>),
    Done,
}

pub fn decrypt(stream: ByteStream, passphrase: &str) -> ByteStream {
    let state = Opening::Open(stream, Box::new(Decryptor::new(passphrase)));
    Box::pin(futures_util::stream::unfold(state, |state| async move {
        let Opening::Open(mut inner, mut decryptor) = state else {
            return None;
        };
        loop {
            match inner.next().await {
                Some(Ok(chunk)) => match decryptor.update(&chunk) {
                    Ok(out) if out.is_empty() => continue,
                    Ok(out) => {
                        return Some((Ok(Bytes::from(out)), Opening::Open(inner, decryptor)))
                    }
                    Err(error) => return Some((Err(error), Opening::Done)),
                },
                Some(Err(error)) => return Some((Err(error), Opening::Done)),
                None => {
                    return match decryptor.finish() {
                        Ok(()) => None,
                        Err(error) => Some((Err(error), Opening::Done)),
                    };
                }
            }
        }
    }))
}

/// Sleep so the average rate stays at or below `bytes_per_sec`.
pub fn throttle(stream: ByteStream, bytes_per_sec: Option<u64>) -> ByteStream {
    let Some(limit) = bytes_per_sec.filter(|limit| *limit > 0) else {
        return stream;
    };
    let started = Instant::now();
    let mut sent = 0u64;
    Box::pin(stream.then(move |item| {
        if let Ok(chunk) = &item {
            sent += chunk.len() as u64;
        }
        let due = Duration::from_secs_f64(sent as f64 / limit as f64);
        let elapsed = started.elapsed();
        async move {
            if due > elapsed {
                tokio::time::sleep(due - elapsed).await;
            }
            item
        }
    }))
}

/// Running SHA-256 and byte count of what passed through.
#[derive(Clone, Default)]
pub struct Meter(Arc<Mutex<(Sha256, u64)>>);

impl Meter {
    pub fn wrap(&self, stream: ByteStream) -> ByteStream {
        let meter = self.clone();
        Box::pin(stream.inspect_ok(move |chunk| {
            if let Ok(mut guard) = meter.0.lock() {
                guard.0.update(chunk);
                guard.1 += chunk.len() as u64;
            }
        }))
    }

    pub fn finish(&self) -> (String, u64) {
        let guard = self.0.lock().map(|guard| guard.clone());
        match guard {
            Ok((hasher, size)) => (hex::encode(hasher.finalize()), size),
            Err(_) => (String::new(), 0),
        }
    }
}

pub async fn sha256_of(mut stream: ByteStream) -> Result<(String, u64), Error> {
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        size += chunk.len() as u64;
        hasher.update(&chunk);
    }
    Ok((hex::encode(hasher.finalize()), size))
}

pub type FanOutReceiver = mpsc::Receiver<Result<Bytes, String>>;

/// Copy `stream` into `count` bounded channels. A source error is forwarded
/// to every receiver so each upload aborts instead of completing a partial object.
pub fn fan_out(
    mut stream: ByteStream,
    count: usize,
) -> (
    Vec<FanOutReceiver>,
    tokio::task::JoinHandle<Result<(), Error>>,
) {
    let mut senders = Vec::with_capacity(count);
    let mut receivers = Vec::with_capacity(count);
    for _ in 0..count {
        let (tx, rx) = mpsc::channel(8);
        senders.push(Some(tx));
        receivers.push(rx);
    }
    let task = tokio::spawn(async move {
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => {
                    let mut alive = 0;
                    for slot in &mut senders {
                        if let Some(tx) = slot {
                            if tx.send(Ok(chunk.clone())).await.is_err() {
                                *slot = None;
                            } else {
                                alive += 1;
                            }
                        }
                    }
                    if alive == 0 {
                        return Err(Error::Node("every upload stopped early".into()));
                    }
                }
                Err(error) => {
                    for tx in senders.iter().flatten() {
                        let _ = tx.send(Err(error.to_string())).await;
                    }
                    return Err(error);
                }
            }
        }
        Ok(())
    });
    (receivers, task)
}

#[cfg(test)]
pub fn from_bytes(data: Vec<u8>) -> ByteStream {
    let chunks: Vec<Result<Bytes, Error>> = data
        .chunks(7_919)
        .map(|chunk| Ok(Bytes::copy_from_slice(chunk)))
        .collect();
    Box::pin(futures_util::stream::iter(chunks))
}

pub async fn collect(mut stream: ByteStream) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        (0..300_000u32)
            .flat_map(|i| format!("row {i} value {}\n", i % 97).into_bytes())
            .collect()
    }

    #[tokio::test]
    async fn compress_encrypt_and_back() {
        let data = sample();
        for (compression, level) in [
            (Compression::None, 0),
            (Compression::Gzip, 6),
            (Compression::Zstd, 3),
        ] {
            let meter = Meter::default();
            let sealed = meter.wrap(
                encrypt(
                    compress(from_bytes(data.clone()), compression, level),
                    "job passphrase",
                )
                .unwrap(),
            );
            let stored = collect(sealed).await.unwrap();
            let (sum, size) = meter.finish();
            assert_eq!(size, stored.len() as u64);
            assert_eq!(sha256_of(from_bytes(stored.clone())).await.unwrap().0, sum);
            if compression != Compression::None {
                assert!(stored.len() < data.len() / 2);
            }
            let restored = collect(decompress(
                decrypt(from_bytes(stored.clone()), "job passphrase"),
                compression,
            ))
            .await
            .unwrap();
            assert_eq!(restored, data);
            assert!(collect(decrypt(from_bytes(stored), "other passphrase"))
                .await
                .is_err());
        }
    }

    #[tokio::test]
    async fn fan_out_copies_and_forwards_errors() {
        let (mut receivers, task) = fan_out(from_bytes(vec![1u8; 50_000]), 2);
        let mut totals = Vec::new();
        for rx in &mut receivers {
            let mut total = 0;
            while let Some(item) = rx.recv().await {
                total += item.unwrap().len();
            }
            totals.push(total);
        }
        task.await.unwrap().unwrap();
        assert_eq!(totals, [50_000, 50_000]);

        let failing: ByteStream = Box::pin(futures_util::stream::iter(vec![
            Ok(Bytes::from_static(b"abc")),
            Err(Error::Node("pg_dump exited with code 1".into())),
        ]));
        let (mut receivers, task) = fan_out(failing, 1);
        assert!(receivers[0].recv().await.unwrap().is_ok());
        assert!(receivers[0].recv().await.unwrap().is_err());
        assert!(task.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn throttle_limits_the_rate() {
        let started = Instant::now();
        let out = collect(throttle(from_bytes(vec![0u8; 40_000]), Some(100_000)))
            .await
            .unwrap();
        assert_eq!(out.len(), 40_000);
        assert!(started.elapsed() >= Duration::from_millis(300));
    }
}
