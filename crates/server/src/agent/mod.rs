//! Remote servers: enrollment, the mTLS gRPC listener, and live sessions.

pub mod pki;
mod registry;
pub mod service;

use chrono::Utc;
use rudder_proto::NodeAgentServer;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use uuid::Uuid;

pub use registry::AgentRegistry;

use crate::auth;
use crate::AppState;

pub const AGENT_PORT: u16 = 7443;

/// Serve the agent gRPC API on `listener` until the process stops.
pub async fn serve(state: AppState, listener: TcpListener) -> anyhow::Result<()> {
    install_crypto_provider();
    let ca = pki::AgentCa::load_or_create(&state.pool, &state.cipher).await?;
    let (cert, key) = ca.panel_identity()?;
    let tls = ServerTlsConfig::new()
        .identity(Identity::from_pem(cert, key))
        .client_ca_root(Certificate::from_pem(&ca.cert_pem));
    tracing::info!(bound = %listener.local_addr()?, "agent listener ready");
    Server::builder()
        .tls_config(tls)?
        .http2_keepalive_interval(Some(std::time::Duration::from_secs(30)))
        .http2_keepalive_timeout(Some(std::time::Duration::from_secs(20)))
        .add_service(NodeAgentServer::new(service::AgentService::new(state)))
        .serve_with_incoming(TcpListenerStream::new(listener))
        .await?;
    Ok(())
}

/// Several crates enable both rustls backends, so tonic cannot pick one by itself.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

#[derive(Debug)]
pub struct Enrolled {
    pub server_id: Uuid,
    pub client_cert_pem: String,
    pub ca_pem: String,
}

#[derive(Debug, thiserror::Error)]
pub enum EnrollError {
    #[error("the join token is unknown, used, or expired")]
    BadToken,
    #[error("the certificate request is invalid")]
    BadCsr,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Pki(#[from] pki::PkiError),
}

/// Exchange a one-time join token and a CSR for a client certificate.
pub async fn enroll(
    state: &AppState,
    join_token: &str,
    csr_pem: &str,
    hostname: &str,
) -> Result<Enrolled, EnrollError> {
    let hash = auth::sha256_hex(join_token.trim());
    let mut tx = state.pool.begin().await?;
    let server_id = sqlx::query_scalar!(
        r#"
        SELECT id FROM servers
        WHERE kind = 'remote' AND join_token_hash = $1 AND join_token_expires_at > now()
        FOR UPDATE
        "#,
        hash
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(EnrollError::BadToken)?;
    let ca = pki::AgentCa::load_or_create(&state.pool, &state.cipher).await?;
    let cert = ca
        .sign_agent_csr(csr_pem, server_id)
        .map_err(|_| EnrollError::BadCsr)?;
    let fingerprint = pki::pem_fingerprint(&cert).ok_or(EnrollError::BadCsr)?;
    sqlx::query!(
        r#"
        UPDATE servers
        SET client_cert_pem = $2, client_cert_fingerprint = $3, join_token_hash = NULL,
            join_token_expires_at = NULL, enrolled_at = $4, hostname = NULLIF($5, ''),
            status = 'offline', updated_at = now()
        WHERE id = $1
        "#,
        server_id,
        cert,
        fingerprint,
        Utc::now(),
        hostname.chars().take(253).collect::<String>(),
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    // A re-enrolled server must not keep a session from its previous certificate.
    state.agents.disconnect(server_id);
    Ok(Enrolled {
        server_id,
        client_cert_pem: cert,
        ca_pem: ca.cert_pem,
    })
}
