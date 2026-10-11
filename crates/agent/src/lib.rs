//! Node agent library. The control stream is added with enrollment.

use std::path::PathBuf;

use anyhow::Context;

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub server: String,
    pub data_dir: PathBuf,
    pub join_token: Option<String>,
}

impl AgentConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.server.trim().is_empty() {
            anyhow::bail!("server URL is empty");
        }
        if !(self.server.starts_with("https://") || self.server.starts_with("http://")) {
            anyhow::bail!("server URL must start with https:// or http://");
        }
        Ok(())
    }
}

/// Prepare the data directory and block until the process is asked to stop.
///
/// Certificate enrollment and the gRPC session replace the wait once the
/// control-plane listener exists. This entrypoint still creates the on-disk
/// buffer directory and refuses to start without a join token or client cert.
pub async fn run(config: AgentConfig) -> anyhow::Result<()> {
    config.validate()?;
    tokio::fs::create_dir_all(&config.data_dir)
        .await
        .with_context(|| format!("create {}", config.data_dir.display()))?;
    let cert_path = config.data_dir.join("client.pem");
    let has_cert = tokio::fs::try_exists(&cert_path).await.unwrap_or(false);
    if !has_cert && config.join_token.as_deref().unwrap_or("").is_empty() {
        anyhow::bail!(
            "provide --join-token for enrollment or install a client certificate at {}",
            cert_path.display()
        );
    }
    tracing::info!(
        server = %config.server,
        data_dir = %config.data_dir.display(),
        enrolled = has_cert,
        "rudder-agent waiting for shutdown"
    );
    shutdown_signal().await;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_server_url_without_a_scheme() {
        let config = AgentConfig {
            server: "panel.example.com:7443".into(),
            data_dir: PathBuf::from("/tmp/rudder-agent"),
            join_token: None,
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn accepts_an_https_server_url() {
        let config = AgentConfig {
            server: "https://panel.example.com:7443".into(),
            data_dir: PathBuf::from("/tmp/rudder-agent"),
            join_token: Some("join".into()),
        };
        assert!(config.validate().is_ok());
    }
}
