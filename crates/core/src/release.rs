//! Release discovery and verification for self-update.
//!
//! A release carries three assets the updater trusts in order:
//! `SHA256SUMS.minisig` signs `SHA256SUMS`, which lists the SHA-256 of every
//! other asset including `release.json`, which pins the panel and cache images
//! by digest and names the agent binaries' checksums.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Error;

pub const RELEASE_MANIFEST: &str = "release.json";
pub const RELEASE_SUMS: &str = "SHA256SUMS";
pub const RELEASE_SIGNATURE: &str = "SHA256SUMS.minisig";
pub const RELEASE_FORMAT: &str = "rudder-release";
/// Agent builds the panel image bundles and the release publishes.
pub const AGENT_TARGETS: [&str; 2] = ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"];

/// Which releases the panel offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    Stable,
    Prerelease,
}

impl UpdateChannel {
    pub fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "stable" => Ok(Self::Stable),
            "prerelease" => Ok(Self::Prerelease),
            _ => Err(Error::Validation(
                "update channel must be stable or prerelease".into(),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Prerelease => "prerelease",
        }
    }
}

/// The parts of a GitHub release the updater reads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubRelease {
    pub tag_name: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub html_url: Option<String>,
    #[serde(default)]
    pub assets: Vec<GithubAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubAsset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub size: u64,
}

impl GithubRelease {
    pub fn asset(&self, name: &str) -> Option<&GithubAsset> {
        self.assets.iter().find(|asset| asset.name == name)
    }
}

/// `v1.2.3` or `1.2.3`, with optional pre-release and build parts.
pub fn parse_version(tag: &str) -> Result<semver::Version, Error> {
    let text = tag.strip_prefix('v').unwrap_or(tag);
    semver::Version::parse(text)
        .map_err(|error| Error::Validation(format!("invalid version '{tag}': {error}")))
}

/// The newest published release newer than `current` on `channel`.
///
/// Drafts and tags that are not versions are ignored. A semver pre-release counts
/// as a pre-release even when GitHub does not mark it as one.
pub fn pick_release<'a>(
    releases: &'a [GithubRelease],
    current: &semver::Version,
    channel: UpdateChannel,
) -> Option<(&'a GithubRelease, semver::Version)> {
    newer_releases(releases, current, channel).pop()
}

/// Every release of the channel above `current`, oldest first. The last one is
/// what [`pick_release`] returns; the rest are what an update brings in as well.
pub fn newer_releases<'a>(
    releases: &'a [GithubRelease],
    current: &semver::Version,
    channel: UpdateChannel,
) -> Vec<(&'a GithubRelease, semver::Version)> {
    let mut found: Vec<_> = releases
        .iter()
        .filter(|release| !release.draft)
        .filter_map(|release| {
            parse_version(&release.tag_name)
                .ok()
                .map(|version| (release, version))
        })
        .filter(|(release, version)| {
            channel == UpdateChannel::Prerelease || (!release.prerelease && version.pre.is_empty())
        })
        .filter(|(_, version)| version > current)
        .collect();
    found.sort_by(|(_, a), (_, b)| a.cmp(b));
    found
}

/// Parse `sha256sum` output: `<64 hex>  <name>` or `<64 hex> *<name>` per line.
pub fn parse_sha256sums(text: &str) -> Result<BTreeMap<String, String>, Error> {
    let mut sums = BTreeMap::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let invalid = || Error::Validation(format!("SHA256SUMS line {} is invalid", number + 1));
        let (hash, rest) = line.split_once(' ').ok_or_else(invalid)?;
        let name = rest
            .strip_prefix(' ')
            .or_else(|| rest.strip_prefix('*'))
            .ok_or_else(invalid)?;
        let hash_ok = hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit());
        let name_ok = !name.is_empty()
            && name.len() <= 200
            && !name.starts_with('.')
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
        if !hash_ok || !name_ok {
            return Err(invalid());
        }
        if sums
            .insert(name.to_owned(), hash.to_ascii_lowercase())
            .is_some()
        {
            return Err(Error::Validation(format!("SHA256SUMS lists {name} twice")));
        }
    }
    Ok(sums)
}

/// `release.json`: what one release installs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseManifest {
    pub format: String,
    pub version: String,
    pub images: ReleaseImages,
    /// Agent target triple to the SHA-256 of `rudder-agent-<target>`.
    #[serde(default)]
    pub agents: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseImages {
    pub panel: String,
    pub cache: String,
}

impl ReleaseManifest {
    pub fn validate(&self) -> Result<(), Error> {
        if self.format != RELEASE_FORMAT {
            return Err(Error::Validation(format!(
                "release.json format must be {RELEASE_FORMAT}"
            )));
        }
        parse_version(&self.version)?;
        for image in [&self.images.panel, &self.images.cache] {
            validate_pinned_image(image)?;
        }
        for (target, hash) in &self.agents {
            if !AGENT_TARGETS.contains(&target.as_str()) {
                return Err(Error::Validation(format!("unknown agent target {target}")));
            }
            if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(Error::Validation(format!(
                    "agent checksum for {target} is invalid"
                )));
            }
        }
        Ok(())
    }
}

/// An image reference must name its content: `repo[:tag]@sha256:<64 hex>`.
pub fn validate_pinned_image(image: &str) -> Result<(), Error> {
    crate::validate_image_ref(image)?;
    let pinned = image.rsplit_once("@sha256:").is_some_and(|(repo, digest)| {
        !repo.is_empty() && digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
    });
    if pinned {
        Ok(())
    } else {
        Err(Error::Validation(format!(
            "image {image} must be pinned by digest"
        )))
    }
}

/// Check a minisign signature. `public_key` is the base64 key or a whole `.pub` file.
pub fn verify_minisign(public_key: &str, data: &[u8], signature: &str) -> Result<(), Error> {
    let key = public_key.trim();
    let key = if key.contains('\n') {
        minisign_verify::PublicKey::decode(key)
    } else {
        minisign_verify::PublicKey::from_base64(key)
    }
    .map_err(|error| Error::Validation(format!("update signing key is invalid: {error}")))?;
    let signature = minisign_verify::Signature::decode(signature)
        .map_err(|error| Error::Validation(format!("release signature is malformed: {error}")))?;
    key.verify(data, &signature, false)
        .map_err(|_| Error::Validation("release signature does not match the signing key".into()))
}

/// Verify a release's signed checksum list and return its manifest.
///
/// `expected_version` is the tag the assets were downloaded from; a manifest for
/// another version is refused so an old signed release cannot be replayed.
pub fn verify_release(
    public_key: &str,
    sums: &[u8],
    signature: &str,
    manifest: &[u8],
    expected_version: &semver::Version,
) -> Result<(ReleaseManifest, BTreeMap<String, String>), Error> {
    verify_minisign(public_key, sums, signature)?;
    let text = std::str::from_utf8(sums)
        .map_err(|_| Error::Validation("SHA256SUMS is not UTF-8".into()))?;
    let sums = parse_sha256sums(text)?;
    let listed = sums
        .get(RELEASE_MANIFEST)
        .ok_or_else(|| Error::Validation("SHA256SUMS does not list release.json".into()))?;
    if *listed != sha256_hex(manifest) {
        return Err(Error::Validation(
            "release.json does not match its signed checksum".into(),
        ));
    }
    let parsed: ReleaseManifest = serde_json::from_slice(manifest)
        .map_err(|error| Error::Validation(format!("release.json is invalid: {error}")))?;
    parsed.validate()?;
    if parse_version(&parsed.version)? != *expected_version {
        return Err(Error::Validation(format!(
            "release.json is for {} but the release is {expected_version}",
            parsed.version
        )));
    }
    for (target, hash) in &parsed.agents {
        if let Some(listed) = sums.get(&format!("rudder-agent-{target}")) {
            if listed != hash {
                return Err(Error::Validation(format!(
                    "agent checksum for {target} differs between release.json and SHA256SUMS"
                )));
            }
        }
    }
    Ok((parsed, sums))
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(data))
}

/// `HH:MM-HH:MM` in UTC. The end may be earlier than the start to span midnight.
pub fn parse_maintenance_window(window: &str) -> Result<(u16, u16), Error> {
    let invalid =
        || Error::Validation("maintenance window must look like 02:00-05:00 (UTC)".into());
    let (start, end) = window.trim().split_once('-').ok_or_else(invalid)?;
    let minutes = |text: &str| -> Result<u16, Error> {
        let (hours, mins) = text.trim().split_once(':').ok_or_else(invalid)?;
        if hours.len() != 2 || mins.len() != 2 {
            return Err(invalid());
        }
        let hours: u16 = hours.parse().map_err(|_| invalid())?;
        let mins: u16 = mins.parse().map_err(|_| invalid())?;
        if hours > 23 || mins > 59 {
            return Err(invalid());
        }
        Ok(hours * 60 + mins)
    };
    let (start, end) = (minutes(start)?, minutes(end)?);
    if start == end {
        return Err(invalid());
    }
    Ok((start, end))
}

/// Whether `minute_of_day` (UTC) falls inside the window. No window means always.
pub fn in_maintenance_window(window: Option<&str>, minute_of_day: u16) -> Result<bool, Error> {
    let Some(window) = window.filter(|w| !w.trim().is_empty()) else {
        return Ok(true);
    };
    let (start, end) = parse_maintenance_window(window)?;
    Ok(if start < end {
        (start..end).contains(&minute_of_day)
    } else {
        minute_of_day >= start || minute_of_day < end
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, prerelease: bool) -> GithubRelease {
        GithubRelease {
            tag_name: tag.into(),
            name: None,
            body: None,
            draft: false,
            prerelease,
            published_at: None,
            html_url: None,
            assets: Vec::new(),
        }
    }

    #[test]
    fn picks_the_newest_release_for_the_channel() {
        let mut draft = release("v9.0.0", false);
        draft.draft = true;
        let releases = vec![
            release("v0.1.0", false),
            release("v0.3.0-rc.1", false),
            release("v0.2.1", false),
            release("v0.4.0", true),
            release("nightly", false),
            draft,
        ];
        let current = parse_version("0.1.0").unwrap();
        let (stable, version) = pick_release(&releases, &current, UpdateChannel::Stable).unwrap();
        assert_eq!(stable.tag_name, "v0.2.1");
        assert_eq!(version.to_string(), "0.2.1");
        let (pre, _) = pick_release(&releases, &current, UpdateChannel::Prerelease).unwrap();
        assert_eq!(pre.tag_name, "v0.4.0");
        let newest = parse_version("v0.2.1").unwrap();
        assert!(pick_release(&releases, &newest, UpdateChannel::Stable).is_none());
    }

    #[test]
    fn newer_releases_are_listed_oldest_first() {
        let releases = vec![
            release("v0.3.0", false),
            release("v0.1.0", false),
            release("v0.2.0", false),
            release("v0.4.0-rc.1", true),
        ];
        let current = parse_version("0.1.0").unwrap();
        let tags: Vec<_> = newer_releases(&releases, &current, UpdateChannel::Stable)
            .iter()
            .map(|(release, _)| release.tag_name.as_str())
            .collect();
        assert_eq!(tags, ["v0.2.0", "v0.3.0"]);
        let tags: Vec<_> = newer_releases(&releases, &current, UpdateChannel::Prerelease)
            .iter()
            .map(|(release, _)| release.tag_name.as_str())
            .collect();
        assert_eq!(tags, ["v0.2.0", "v0.3.0", "v0.4.0-rc.1"]);
    }

    #[test]
    fn sha256sums_parse_both_styles_and_reject_paths() {
        let hash = "a".repeat(64);
        let sums =
            parse_sha256sums(&format!("{hash}  release.json\n{hash} *rudder-agent-x\n")).unwrap();
        assert_eq!(sums.len(), 2);
        assert!(sums.contains_key("rudder-agent-x"));
        assert!(parse_sha256sums(&format!("{hash}  ../etc/passwd\n")).is_err());
        assert!(parse_sha256sums(&format!("{hash}  dir/file\n")).is_err());
        assert!(parse_sha256sums("abc  file\n").is_err());
        assert!(parse_sha256sums(&format!("{hash}  a\n{hash}  a\n")).is_err());
    }

    fn manifest(version: &str) -> ReleaseManifest {
        let digest = "b".repeat(64);
        ReleaseManifest {
            format: RELEASE_FORMAT.into(),
            version: version.into(),
            images: ReleaseImages {
                panel: format!("ghcr.io/vantanminh/rudder:{version}@sha256:{digest}"),
                cache: format!("ghcr.io/vantanminh/rudder-cache@sha256:{digest}"),
            },
            agents: BTreeMap::from([("x86_64-unknown-linux-musl".into(), "c".repeat(64))]),
        }
    }

    #[test]
    fn manifests_must_pin_images_by_digest() {
        assert!(manifest("0.2.0").validate().is_ok());
        let mut tag_only = manifest("0.2.0");
        tag_only.images.cache = "ghcr.io/vantanminh/rudder-cache:0.2.0".into();
        assert!(tag_only.validate().is_err());
        let mut odd_target = manifest("0.2.0");
        odd_target
            .agents
            .insert("x86_64-pc-windows-msvc".into(), "c".repeat(64));
        assert!(odd_target.validate().is_err());
    }

    struct Signer {
        public: String,
        secret: minisign::SecretKey,
    }

    impl Signer {
        fn new() -> Self {
            let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
            Self {
                public: pair.pk.to_base64(),
                secret: pair.sk,
            }
        }

        fn sign(&self, data: &[u8]) -> String {
            minisign::sign(None, &self.secret, data, None, None)
                .unwrap()
                .into_string()
        }
    }

    fn signed_release(signer: &Signer, manifest: &ReleaseManifest) -> (Vec<u8>, String, Vec<u8>) {
        let manifest = serde_json::to_vec(manifest).unwrap();
        let agent = format!(
            "{}  rudder-agent-x86_64-unknown-linux-musl\n",
            "c".repeat(64)
        );
        let sums = format!("{}  release.json\n{agent}", sha256_hex(&manifest)).into_bytes();
        let signature = signer.sign(&sums);
        (sums, signature, manifest)
    }

    #[test]
    fn verify_release_checks_signature_checksum_and_version() {
        let signer = Signer::new();
        let version = parse_version("0.2.0").unwrap();
        let (sums, signature, body) = signed_release(&signer, &manifest("0.2.0"));
        let (parsed, listed) =
            verify_release(&signer.public, &sums, &signature, &body, &version).unwrap();
        assert_eq!(parsed.version, "0.2.0");
        assert_eq!(listed.len(), 2);

        let other = Signer::new();
        let error = verify_release(&other.public, &sums, &signature, &body, &version).unwrap_err();
        assert!(error.to_string().contains("signing key"), "{error}");

        let mut tampered = body.clone();
        tampered.push(b' ');
        let error =
            verify_release(&signer.public, &sums, &signature, &tampered, &version).unwrap_err();
        assert!(error.to_string().contains("checksum"), "{error}");

        let mut forged = sums.clone();
        forged.extend_from_slice(format!("{}  extra\n", "d".repeat(64)).as_bytes());
        assert!(verify_release(&signer.public, &forged, &signature, &body, &version).is_err());

        let newer = parse_version("0.3.0").unwrap();
        let error = verify_release(&signer.public, &sums, &signature, &body, &newer).unwrap_err();
        assert!(error.to_string().contains("release is 0.3.0"), "{error}");
    }

    #[test]
    fn maintenance_windows_wrap_midnight() {
        assert!(in_maintenance_window(None, 700).unwrap());
        assert!(in_maintenance_window(Some("02:00-05:00"), 150).unwrap());
        assert!(!in_maintenance_window(Some("02:00-05:00"), 300).unwrap());
        assert!(in_maintenance_window(Some("23:00-01:30"), 1430).unwrap());
        assert!(in_maintenance_window(Some("23:00-01:30"), 30).unwrap());
        assert!(!in_maintenance_window(Some("23:00-01:30"), 120).unwrap());
        assert!(parse_maintenance_window("2:00-5:00").is_err());
        assert!(parse_maintenance_window("02:00-02:00").is_err());
        assert!(parse_maintenance_window("24:00-01:00").is_err());
    }
}
