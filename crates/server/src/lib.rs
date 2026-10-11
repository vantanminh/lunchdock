//! Rudder control plane library.
//!
//! The binary in `main.rs` loads configuration, initializes tracing, and calls
//! [`run`]. Tests construct an [`AppState`] and the [`router`] directly.

mod agents;
mod audit;
mod auth;
mod backup;
mod build_queue;
mod config;
mod db;
mod deploy;
mod driver;
mod embed;
mod error;
mod events;
mod gitkey;
mod nodes;
mod openapi;
mod publish;
mod routes;
mod s3;
mod state;
pub mod telemetry;

use anyhow::Context;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;

pub use build_queue::build_once;
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
    db::ensure_local_server(&pool)
        .await
        .context("ensure local server")?;
    db::recover_interrupted(&pool)
        .await
        .context("recover interrupted work")?;
    let settings = db::load_panel_settings(&pool).await?;
    tracing::info!(
        github_repo = %settings.github_repo,
        update_channel = %settings.update_channel,
        auto_update = settings.auto_update,
        "panel settings loaded"
    );

    let addr = config.bind;
    let state = AppState::new(pool, config);
    let watch_state = state.clone();
    let backup_state = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            match backup::schedule_once(&backup_state).await {
                Ok(0) => {}
                Ok(started) => tracing::info!(started, "scheduled backups started"),
                Err(error) => tracing::warn!(%error, "backup scheduler query failed"),
            }
        }
    });
    let build_state = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            match build_queue::build_once(&build_state).await {
                Ok(true) => tracing::debug!("build queue claimed a job"),
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(message = %error.public_message(), "build queue failed")
                }
            }
        }
    });
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
            if let Err(error) = deploy::watch_once(&watch_state).await {
                tracing::warn!(%error, "image watch query failed");
            }
        }
    });
    let app = router(state);
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    let bound = listener.local_addr().context("local address")?;
    tracing::info!(%bound, "rudder control plane listening");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("http server")?;
    tracing::info!("rudder control plane stopped");
    Ok(())
}

/// The OpenAPI document as pretty JSON, used to generate frontend types.
pub fn openapi_json() -> String {
    openapi::document()
        .to_pretty_json()
        .expect("OpenAPI document serializes")
}

/// HTTP router. Additional route groups are nested here as features land.
pub fn router(state: AppState) -> axum::Router {
    axum::Router::new()
        .merge(routes::health::router())
        .merge(routes::account::router())
        .merge(routes::catalog::router())
        .merge(routes::edge::router())
        .merge(routes::git::router())
        .merge(routes::databases::router())
        .merge(routes::backups::router())
        .merge(
            utoipa_swagger_ui::SwaggerUi::new("/api/docs")
                .url("/api/openapi.json", openapi::document()),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
        .fallback(embed::spa)
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
