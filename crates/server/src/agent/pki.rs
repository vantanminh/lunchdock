//! The panel's agent certificate authority.
//!
//! The CA is created on first start and kept in Postgres with its key sealed by the
//! master key, so a database backup restores agent trust too. It signs agent
//! client certificates at enrollment and a fresh panel server certificate on every
//! start. Agents verify the panel against the CA and the fixed name
//! [`PANEL_TLS_NAME`], so they can dial any address the panel is reachable on.

use chrono::{Datelike, Duration, Utc};
use rcgen::{
    BasicConstraints, CertificateParams, CertificateSigningRequestParams, DnType,
    ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose, SanType,
};
use rudder_core::Cipher;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

/// Name in the panel's gRPC certificate. Agents set it as the TLS server name.
pub const PANEL_TLS_NAME: &str = "rudder-panel";
const CLIENT_CERT_YEARS: i32 = 10;

pub struct AgentCa {
    pub cert_pem: String,
    key_pem: String,
}

impl std::fmt::Debug for AgentCa {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentCa").finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PkiError {
    #[error("database: {0}")]
    Database(#[from] sqlx::Error),
    #[error("certificate: {0}")]
    Certificate(#[from] rcgen::Error),
    #[error("the CA key could not be unsealed")]
    Sealed,
}

fn ymd(at: chrono::DateTime<Utc>) -> time::OffsetDateTime {
    rcgen::date_time_ymd(at.year(), at.month() as u8, at.day() as u8)
}

impl AgentCa {
    /// Load the CA, creating it on first use. Concurrent first starts agree on one row.
    pub async fn load_or_create(pool: &PgPool, cipher: &Cipher) -> Result<Self, PkiError> {
        if let Some(ca) = Self::load(pool, cipher).await? {
            return Ok(ca);
        }
        let key = KeyPair::generate()?;
        let mut params = CertificateParams::new(Vec::new())?;
        params
            .distinguished_name
            .push(DnType::CommonName, "Rudder agent CA");
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        params.not_before = ymd(Utc::now() - Duration::days(1));
        params.not_after = ymd(Utc::now() + Duration::days(365 * 30));
        let cert = params.self_signed(&key)?;
        let sealed = cipher
            .encrypt(key.serialize_pem().as_bytes())
            .map_err(|_| PkiError::Sealed)?;
        sqlx::query!(
            "INSERT INTO agent_ca (cert_pem, key_sealed) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            cert.pem(),
            sealed
        )
        .execute(pool)
        .await?;
        Self::load(pool, cipher)
            .await?
            .ok_or(PkiError::Database(sqlx::Error::RowNotFound))
    }

    async fn load(pool: &PgPool, cipher: &Cipher) -> Result<Option<Self>, PkiError> {
        let Some(row) = sqlx::query!("SELECT cert_pem, key_sealed FROM agent_ca WHERE id")
            .fetch_optional(pool)
            .await?
        else {
            return Ok(None);
        };
        let key = cipher
            .decrypt(&row.key_sealed)
            .map_err(|_| PkiError::Sealed)?;
        let key_pem = String::from_utf8(key).map_err(|_| PkiError::Sealed)?;
        Ok(Some(Self {
            cert_pem: row.cert_pem,
            key_pem,
        }))
    }

    fn issuer(&self) -> Result<Issuer<'static, KeyPair>, PkiError> {
        let key = KeyPair::from_pem(&self.key_pem)?;
        Ok(Issuer::from_ca_cert_pem(&self.cert_pem, key)?)
    }

    /// Sign an agent's CSR. Only the public key is taken from the request; the
    /// subject and extensions are set here so an agent cannot ask for more.
    pub fn sign_agent_csr(&self, csr_pem: &str, server_id: Uuid) -> Result<String, PkiError> {
        let csr = CertificateSigningRequestParams::from_pem(csr_pem)?;
        let mut params = CertificateParams::new(Vec::new())?;
        params
            .distinguished_name
            .push(DnType::CommonName, server_id.to_string());
        params.subject_alt_names = vec![SanType::DnsName(
            format!("{server_id}.agent.rudder").try_into()?,
        )];
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        params.use_authority_key_identifier_extension = true;
        params.not_before = ymd(Utc::now() - Duration::days(1));
        params.not_after = ymd(Utc::now() + Duration::days(365 * i64::from(CLIENT_CERT_YEARS)));
        let request = CertificateSigningRequestParams {
            params,
            public_key: csr.public_key,
        };
        Ok(request.signed_by(&self.issuer()?)?.pem())
    }

    /// A new server certificate for the gRPC listener: (cert PEM, key PEM).
    pub fn panel_identity(&self) -> Result<(String, String), PkiError> {
        let key = KeyPair::generate()?;
        let mut params = CertificateParams::new(vec![PANEL_TLS_NAME.to_owned()])?;
        params
            .distinguished_name
            .push(DnType::CommonName, PANEL_TLS_NAME);
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.use_authority_key_identifier_extension = true;
        params.not_before = ymd(Utc::now() - Duration::days(1));
        params.not_after = ymd(Utc::now() + Duration::days(365));
        let cert = params.signed_by(&key, &self.issuer()?)?;
        Ok((cert.pem(), key.serialize_pem()))
    }
}

/// SHA-256 of a DER certificate, hex. Agents are looked up by this value.
pub fn fingerprint(der: &[u8]) -> String {
    hex::encode(Sha256::digest(der))
}

/// Fingerprint of the first certificate in a PEM string.
pub fn pem_fingerprint(pem: &str) -> Option<String> {
    let block = pem
        .split("-----BEGIN CERTIFICATE-----")
        .nth(1)?
        .split("-----END CERTIFICATE-----")
        .next()?;
    let body: String = block.chars().filter(|c| !c.is_whitespace()).collect();
    use base64::Engine;
    let der = base64::engine::general_purpose::STANDARD
        .decode(body)
        .ok()?;
    Some(fingerprint(&der))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ca() -> AgentCa {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(Vec::new()).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let cert = params.self_signed(&key).unwrap();
        AgentCa {
            cert_pem: cert.pem(),
            key_pem: key.serialize_pem(),
        }
    }

    #[test]
    fn signs_csrs_with_the_server_id_and_ignores_requested_names() {
        let ca = ca();
        let agent_key = KeyPair::generate().unwrap();
        let mut asked = CertificateParams::new(vec!["panel.example.com".into()]).unwrap();
        asked.distinguished_name.push(DnType::CommonName, "admin");
        let csr = asked.serialize_request(&agent_key).unwrap().pem().unwrap();
        let id = Uuid::new_v4();
        let pem = ca.sign_agent_csr(&csr, id).unwrap();
        let fingerprint = pem_fingerprint(&pem).unwrap();
        assert_eq!(fingerprint.len(), 64);
        let (_, parsed) = x509_parser::pem::parse_x509_pem(pem.as_bytes()).unwrap();
        let cert = parsed.parse_x509().unwrap();
        assert_eq!(
            cert.subject()
                .iter_common_name()
                .next()
                .unwrap()
                .as_str()
                .unwrap(),
            id.to_string()
        );
        let text = format!("{:?}", cert.subject_alternative_name().unwrap());
        assert!(!text.contains("panel.example.com"), "{text}");
        assert!(text.contains(&format!("{id}.agent.rudder")), "{text}");
    }

    #[test]
    fn rejects_garbage_csrs() {
        assert!(ca().sign_agent_csr("not a csr", Uuid::new_v4()).is_err());
    }

    #[test]
    fn panel_identity_carries_the_fixed_tls_name() {
        let (cert, key) = ca().panel_identity().unwrap();
        assert!(cert.contains("BEGIN CERTIFICATE"));
        assert!(key.contains("PRIVATE KEY"));
        let (_, parsed) = x509_parser::pem::parse_x509_pem(cert.as_bytes()).unwrap();
        let text = format!(
            "{:?}",
            parsed
                .parse_x509()
                .unwrap()
                .subject_alternative_name()
                .unwrap()
        );
        assert!(text.contains(PANEL_TLS_NAME));
    }
}
