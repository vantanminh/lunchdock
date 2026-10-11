//! Include/exclude globs applied to a tar stream as it passes through.
//!
//! Entries are re-written one at a time on a blocking thread, so memory stays
//! bounded by one entry buffer regardless of archive size.

use bytes::Bytes;
use futures_util::TryStreamExt;
use globset::{Glob, GlobSet, GlobSetBuilder};
use rudder_core::{ByteStream, Error};
use tokio::sync::mpsc;
use tokio_util::io::{StreamReader, SyncIoBridge};

const FLUSH_AT: usize = 64 * 1024;

pub struct Filter {
    include: Option<GlobSet>,
    exclude: Option<GlobSet>,
}

fn build(globs: &[String]) -> Result<Option<GlobSet>, Error> {
    if globs.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for glob in globs {
        builder.add(
            Glob::new(glob)
                .map_err(|error| Error::Validation(format!("glob '{glob}': {error}")))?,
        );
    }
    builder
        .build()
        .map(Some)
        .map_err(|error| Error::Validation(error.to_string()))
}

impl Filter {
    pub fn new(include: &[String], exclude: &[String]) -> Result<Self, Error> {
        Ok(Self {
            include: build(include)?,
            exclude: build(exclude)?,
        })
    }

    pub fn is_noop(&self) -> bool {
        self.include.is_none() && self.exclude.is_none()
    }

    /// `path` is relative, without a leading `./`.
    pub fn keeps(&self, path: &str, is_dir: bool) -> bool {
        let matches = |set: &GlobSet| {
            set.is_match(path)
                || (is_dir && set.is_match(format!("{path}/")))
                || ancestors(path).any(|parent| set.is_match(parent))
        };
        if let Some(exclude) = &self.exclude {
            if matches(exclude) {
                return false;
            }
        }
        match &self.include {
            None => true,
            Some(include) => {
                if path.is_empty() {
                    return false;
                }
                matches(include)
                    || (is_dir && include.is_match(format!("{path}/x")) && !include.is_match(path))
            }
        }
    }
}

fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/')
        .map(move |(index, _)| &path[..index])
}

struct ChannelWriter {
    tx: mpsc::Sender<Result<Bytes, Error>>,
    buffer: Vec<u8>,
}

impl ChannelWriter {
    fn send(&mut self) -> std::io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let chunk = Bytes::from(std::mem::take(&mut self.buffer));
        self.tx
            .blocking_send(Ok(chunk))
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "reader stopped"))
    }
}

impl std::io::Write for ChannelWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buffer.extend_from_slice(data);
        if self.buffer.len() >= FLUSH_AT {
            self.send()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.send()
    }
}

pub fn filter_tar(stream: ByteStream, filter: Filter) -> ByteStream {
    if filter.is_noop() {
        return stream;
    }
    let (tx, rx) = mpsc::channel::<Result<Bytes, Error>>(8);
    let input = SyncIoBridge::new(StreamReader::new(
        stream.map_err(|error| std::io::Error::other(error.to_string())),
    ));
    let error_tx = tx.clone();
    tokio::task::spawn_blocking(move || {
        let writer = ChannelWriter {
            tx,
            buffer: Vec::with_capacity(FLUSH_AT),
        };
        if let Err(error) = rewrite(input, writer, &filter) {
            let _ = error_tx.blocking_send(Err(Error::Node(format!("tar filter: {error}"))));
        }
    });
    Box::pin(tokio_stream_from(rx))
}

fn tokio_stream_from(
    rx: mpsc::Receiver<Result<Bytes, Error>>,
) -> impl futures_util::Stream<Item = Result<Bytes, Error>> {
    futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    })
}

fn rewrite(
    input: impl std::io::Read,
    writer: ChannelWriter,
    filter: &Filter,
) -> std::io::Result<()> {
    let mut archive = tar::Archive::new(input);
    let mut builder = tar::Builder::new(writer);
    builder.follow_symlinks(false);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let text = path.to_string_lossy();
        let relative = text
            .trim_start_matches("./")
            .trim_end_matches('/')
            .to_owned();
        let kind = entry.header().entry_type();
        if !filter.keeps(&relative, kind.is_dir()) {
            continue;
        }
        let mut header = entry.header().clone();
        if kind.is_symlink() || kind.is_hard_link() {
            let target = entry.link_name()?.map(|link| link.into_owned());
            if let Some(target) = target {
                builder.append_link(&mut header, &path, target)?;
            }
        } else {
            builder.append_data(&mut header, &path, &mut entry)?;
        }
    }
    let mut writer = builder.into_inner()?;
    std::io::Write::flush(&mut writer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::pipeline::{collect, from_bytes};

    fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let mut dir = tar::Header::new_gnu();
        dir.set_entry_type(tar::EntryType::Directory);
        dir.set_mode(0o755);
        dir.set_size(0);
        builder
            .append_data(&mut dir.clone(), "./", &[][..])
            .unwrap();
        for name in ["./uploads/", "./cache/", "./uploads/2026/"] {
            builder
                .append_data(&mut dir.clone(), name, &[][..])
                .unwrap();
        }
        for (name, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            builder.append_data(&mut header, name, *data).unwrap();
        }
        builder.into_inner().unwrap()
    }

    fn names(bytes: &[u8]) -> Vec<String> {
        let mut archive = tar::Archive::new(bytes);
        archive
            .entries()
            .unwrap()
            .map(|entry| {
                entry
                    .unwrap()
                    .path()
                    .unwrap()
                    .to_string_lossy()
                    .trim_start_matches("./")
                    .trim_end_matches('/')
                    .to_owned()
            })
            .collect()
    }

    #[tokio::test]
    async fn excludes_and_includes_filter_entries() {
        let data = archive(&[
            ("./uploads/2026/a.jpg", b"jpeg"),
            ("./uploads/notes.txt", b"text"),
            ("./cache/blob.bin", b"cache"),
            ("./config.yml", b"yaml"),
        ]);
        let excluded = collect(filter_tar(
            from_bytes(data.clone()),
            Filter::new(&[], &["cache/**".into(), "*.txt".into()]).unwrap(),
        ))
        .await
        .unwrap();
        let kept = names(&excluded);
        assert!(kept.contains(&"uploads/2026/a.jpg".to_owned()));
        assert!(kept.contains(&"config.yml".to_owned()));
        assert!(!kept.iter().any(|name| name.starts_with("cache")));
        assert!(!kept.contains(&"uploads/notes.txt".to_owned()));

        let included = collect(filter_tar(
            from_bytes(data),
            Filter::new(&["uploads/**/*.jpg".into()], &[]).unwrap(),
        ))
        .await
        .unwrap();
        let kept = names(&included);
        assert!(kept.contains(&"uploads/2026/a.jpg".to_owned()));
        assert!(!kept.contains(&"config.yml".to_owned()));
        assert!(!kept.contains(&"uploads/notes.txt".to_owned()));
        let mut archive = tar::Archive::new(&included[..]);
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            if entry.path().unwrap().ends_with("a.jpg") {
                let mut body = String::new();
                std::io::Read::read_to_string(&mut entry, &mut body).unwrap();
                assert_eq!(body, "jpeg");
            }
        }
    }

    #[test]
    fn no_filters_is_a_passthrough() {
        assert!(Filter::new(&[], &[]).unwrap().is_noop());
        assert!(Filter::new(&["[".into()], &[]).is_err());
    }
}
