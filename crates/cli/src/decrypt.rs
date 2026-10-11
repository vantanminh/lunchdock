//! `rudder backup decrypt`: undo client-side backup encryption offline.

use std::fs::File;
use std::io::{self, BufRead, Read, Write};

use anyhow::{bail, Context};
use rudder_core::Decryptor;

pub const PASSPHRASE_ENV: &str = "RUDDER_BACKUP_PASSPHRASE";

pub struct Args {
    pub input: Option<String>,
    pub output: Option<String>,
}

pub fn parse(mut args: impl Iterator<Item = String>) -> anyhow::Result<Args> {
    let mut parsed = Args {
        input: None,
        output: None,
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-i" | "--in" => parsed.input = Some(args.next().context("--in needs a path")?),
            "-o" | "--out" => parsed.output = Some(args.next().context("--out needs a path")?),
            other => bail!("unknown option {other}"),
        }
    }
    Ok(parsed)
}

/// Streams `input` (or stdin) to `output` (or stdout). The passphrase comes from
/// `RUDDER_BACKUP_PASSPHRASE`, or the first stdin line when a file is given.
pub fn run(args: Args) -> anyhow::Result<()> {
    let passphrase = match std::env::var(PASSPHRASE_ENV) {
        Ok(value) if !value.is_empty() => value,
        _ if args.input.is_some() => {
            eprint!("passphrase: ");
            let mut line = String::new();
            io::stdin().lock().read_line(&mut line)?;
            line.trim_end_matches(['\r', '\n']).to_owned()
        }
        _ => bail!("set {PASSPHRASE_ENV} when reading the backup from stdin"),
    };
    let mut reader: Box<dyn Read> = match &args.input {
        Some(path) => Box::new(File::open(path).with_context(|| format!("open {path}"))?),
        None => Box::new(io::stdin().lock()),
    };
    let mut writer: Box<dyn Write> = match &args.output {
        Some(path) => Box::new(File::create(path).with_context(|| format!("create {path}"))?),
        None => Box::new(io::stdout().lock()),
    };
    decrypt(&passphrase, &mut reader, &mut writer)
}

pub fn decrypt(
    passphrase: &str,
    reader: &mut dyn Read,
    writer: &mut dyn Write,
) -> anyhow::Result<()> {
    let mut decryptor = Decryptor::new(passphrase);
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let plain = decryptor
            .update(&buffer[..read])
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        writer.write_all(&plain)?;
    }
    decryptor
        .finish()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use rudder_core::Encryptor;

    use super::decrypt;

    #[test]
    fn decrypts_an_envelope() {
        let plain: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let mut encryptor = Encryptor::new("correct horse").unwrap();
        let mut sealed = encryptor.update(&plain).unwrap();
        sealed.extend(encryptor.finish().unwrap());
        let mut out = Vec::new();
        decrypt("correct horse", &mut sealed.as_slice(), &mut out).unwrap();
        assert_eq!(out, plain);
        let mut wrong = Vec::new();
        assert!(decrypt("wrong", &mut sealed.as_slice(), &mut wrong).is_err());
        let truncated = &sealed[..sealed.len() - 10];
        let mut cut = Vec::new();
        assert!(decrypt("correct horse", &mut &truncated[..], &mut cut).is_err());
    }
}
