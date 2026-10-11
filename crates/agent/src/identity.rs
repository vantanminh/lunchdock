//! Enrollment and the agent's on-disk identity.
//!
//! The private key is generated here and never leaves the machine; the panel
//! only sees a certificate request.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

const KEY_FILE: &str = "key.pem";
const CERT_FILE: &str = "cert.pem";
const CA_FILE: &str = "ca.pem";
const META_FILE: &str = "agent.json";

/// Where and how to reach the panel, written at enrollment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMeta {
    pub server_id: String,
    pub panel_url: String,
    pub grpc_url: String,
    pub tls_name: String,
}

/// Everything needed to open the mTLS session.
#[derive(Clone)]
pub struct Identity {
    pub meta: AgentMeta,
    pub key_pem: String,
    pub cert_pem: String,
    pub ca_pem: String,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("meta", &self.meta)
            .field("key_pem", &"[redacted]")
            .finish()
    }
}

impl Identity {
    pub fn load(data_dir: &Path) -> anyhow::Result<Self> {
        let read = |name: &str| -> anyhow::Result<String> {
            let path = data_dir.join(name);
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))
        };
        let meta: AgentMeta =
            serde_json::from_str(&read(META_FILE)?).context("parse agent.json")?;
        Ok(Self {
            meta,
            key_pem: read(KEY_FILE)?,
            cert_pem: read(CERT_FILE)?,
            ca_pem: read(CA_FILE)?,
        })
    }

    pub fn save(&self, data_dir: &Path) -> anyhow::Result<()> {
        create_private_dir(data_dir)?;
        write_private(&data_dir.join(KEY_FILE), self.key_pem.as_bytes())?;
        write_private(&data_dir.join(CERT_FILE), self.cert_pem.as_bytes())?;
        write_private(&data_dir.join(CA_FILE), self.ca_pem.as_bytes())?;
        let meta = serde_json::to_vec_pretty(&self.meta)?;
        write_private(&data_dir.join(META_FILE), &meta)
    }
}

#[derive(Serialize)]
struct EnrollRequest<'a> {
    join_token: &'a str,
    csr_pem: &'a str,
    hostname: &'a str,
}

#[derive(Deserialize)]
struct EnrollResponse {
    server_id: String,
    client_cert_pem: String,
    ca_pem: String,
    grpc_url: String,
    tls_name: String,
}

#[derive(Deserialize)]
struct ApiError {
    error: ApiErrorBody,
}

#[derive(Deserialize)]
struct ApiErrorBody {
    message: String,
}

/// Exchange a join token for a client certificate and save the identity.
pub async fn enroll(panel_url: &str, token: &str, data_dir: &Path) -> anyhow::Result<Identity> {
    let panel_url = panel_url.trim_end_matches('/');
    if !(panel_url.starts_with("https://") || panel_url.starts_with("http://")) {
        anyhow::bail!("--server must start with https:// or http://");
    }
    let key = rcgen::KeyPair::generate().context("generate key")?;
    let csr = rcgen::CertificateParams::new(Vec::<String>::new())
        .context("certificate request")?
        .serialize_request(&key)
        .context("sign certificate request")?
        .pem()
        .context("encode certificate request")?;
    let hostname = crate::host::hostname();
    let response = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?
        .post(format!("{panel_url}/api/v1/agent/enroll"))
        .json(&EnrollRequest {
            join_token: token.trim(),
            csr_pem: &csr,
            hostname: &hostname,
        })
        .send()
        .await
        .with_context(|| format!("reach {panel_url}"))?;
    let status = response.status();
    if !status.is_success() {
        let message = response
            .json::<ApiError>()
            .await
            .map(|body| body.error.message)
            .unwrap_or_else(|_| status.to_string());
        anyhow::bail!("enrollment rejected: {message}");
    }
    let body: EnrollResponse = response.json().await.context("parse enrollment reply")?;
    if !body.grpc_url.starts_with("https://") {
        anyhow::bail!("the panel returned a non-TLS agent address");
    }
    let identity = Identity {
        meta: AgentMeta {
            server_id: body.server_id,
            panel_url: panel_url.to_owned(),
            grpc_url: body.grpc_url,
            tls_name: body.tls_name,
        },
        key_pem: key.serialize_pem(),
        cert_pem: body.client_cert_pem,
        ca_pem: body.ca_pem,
    };
    identity.save(data_dir)?;
    Ok(identity)
}

pub fn create_private_dir(dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Write a file readable only by the agent, replacing it atomically.
pub fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let tmp: PathBuf = path.with_extension("tmp");
    {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&tmp)
            .with_context(|| format!("write {}", tmp.display()))?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))
}
