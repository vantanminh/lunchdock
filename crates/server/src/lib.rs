//! Rudder control plane library.
//!
//! The binary in `main.rs` loads configuration, initializes tracing, and calls
//! [`run`]. Tests construct an [`AppState`] and the [`router`] directly.

mod config;
mod db;
mod error;
mod routes;
mod state;
pub mod telemetry;

use std::sync::Arc;

use anyhow::Context;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;

pub use config::{Config, ConfigError, LogFormat};
pub use db::ensure_partitions;
pub use error::ApiError;
pub use state::AppState;

/// Bind the HTTP server, run migrations, and serve until shutdown.
pub async fn run(config: Config) -> anyhow::Result<()> {
    let pool = db::connect(&config).await?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("apply database migrations")?;
    db::ensure_partitions(&pool)
        .await
        .context("ensure daily partitions")?;
    let settings = db::load_panel_settings(&pool).await?;
    tracing::info!(
        github_repo = %settings.github_repo,
        update_channel = %settings.update_channel,
        auto_update = settings.auto_update,
        "panel settings loaded"
    );

    let addr = config.bind;
    let state = AppState {
        pool,
        config: Arc::new(config),
    };
    let app = router(state);
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    let bound = listener.local_addr().context("local address")?;
    tracing::info!(%bound, "rudder control plane listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("http server")?;
    tracing::info!("rudder control plane stopped");
    Ok(())
}

/// HTTP router. Additional route groups are nested here as features land.
pub fn router(state: AppState) -> axum::Router {
    axum::Router::new()
        .merge(routes::health::router())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "failed to listen for ctrl-c");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => tracing::error!(%error, "failed to listen for sigterm"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
    tracing::info!("shutdown signal received");
}
