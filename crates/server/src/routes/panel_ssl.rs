//! Checks the certificate the panel domain serves to the public.
//!
//! The panel connects to its own domain over HTTPS, the way a browser or an AI
//! agent would, and reports what came back. Nothing is changed.

use std::time::Duration;

use axum::extract::State;
use axum::routing::get;
use axum::Json;
use chrono::{DateTime, Utc};
use rudder_core::{certificate_matches_host, describe_certificate};
use serde::Serialize;
use utoipa::ToSchema;

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

const TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Serialize, ToSchema)]
pub struct PanelSslCheck {
    pub domain: Option<String>,
    pub reachable: bool,
    pub trusted: bool,
    pub hostname_matches: bool,
    pub common_name: Option<String>,
    pub sans: Vec<String>,
    pub issuer: Option<String>,
    pub not_before: Option<DateTime<Utc>>,
    pub not_after: Option<DateTime<Utc>>,
    pub days_left: Option<i64>,
    pub error: Option<String>,
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new().route("/api/v1/panel/ssl-check", get(ssl_check))
}

async fn ssl_check(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<PanelSslCheck>, ApiError> {
    let domain: Option<String> =
        sqlx::query_scalar("SELECT panel_domain FROM panel_settings WHERE id = TRUE")
            .fetch_one(&state.pool)
            .await
            .map_err(ApiError::database)?;
    let domain = domain.filter(|domain| !domain.is_empty());
    let mut check = PanelSslCheck {
        domain: domain.clone(),
        reachable: false,
        trusted: false,
        hostname_matches: false,
        common_name: None,
        sans: Vec::new(),
        issuer: None,
        not_before: None,
        not_after: None,
        days_left: None,
        error: None,
    };
    let Some(domain) = domain else {
        check.error = Some("no panel domain is set".into());
        return Ok(Json(check));
    };
    probe(&domain, &mut check).await;
    Ok(Json(check))
}

async fn probe(domain: &str, check: &mut PanelSslCheck) {
    let url = format!("https://{domain}/api/health");
    let strict = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build();
    let lenient = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .tls_danger_accept_invalid_certs(true)
        .tls_info(true)
        .build();
    let (Ok(strict), Ok(lenient)) = (strict, lenient) else {
        check.error = Some("could not start the TLS check".into());
        return;
    };
    // The lenient request reads the certificate even when it is not trusted.
    let response = match lenient.get(&url).send().await {
        Ok(response) => response,
        Err(error) => {
            check.error = Some(connect_error(&error));
            return;
        }
    };
    check.reachable = true;
    let der = response
        .extensions()
        .get::<reqwest::tls::TlsInfo>()
        .and_then(|info| info.peer_certificate())
        .map(<[u8]>::to_vec);
    if let Some(details) = der.as_deref().and_then(|der| describe_certificate(der).ok()) {
        let mut names: Vec<&str> = details.sans.iter().map(String::as_str).collect();
        if names.is_empty() {
            names.extend(details.common_name.as_deref());
        }
        check.hostname_matches = certificate_matches_host(domain, &names);
        check.common_name = details.common_name;
        check.sans = details.sans;
        check.issuer = Some(details.issuer);
        check.not_before = DateTime::from_timestamp(details.not_before_unix, 0);
        check.not_after = DateTime::from_timestamp(details.not_after_unix, 0);
        check.days_left = check
            .not_after
            .map(|after| (after - Utc::now()).num_days());
    }
    match strict.get(&url).send().await {
        Ok(_) => check.trusted = true,
        Err(error) => check.error = Some(trust_error(&error, check)),
    }
}

fn connect_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        return "the domain did not answer on port 443 in time; check the DNS record and the firewall"
            .into();
    }
    if error.is_connect() {
        return "could not connect to the domain on port 443; check that its DNS record points at this server and that the port is open".into();
    }
    format!("HTTPS request failed: {error}")
}

fn trust_error(error: &reqwest::Error, check: &PanelSslCheck) -> String {
    if check.days_left.is_some_and(|days| days < 0) {
        return "the certificate has expired".into();
    }
    if !check.hostname_matches {
        return "the certificate does not cover this domain".into();
    }
    if error.is_timeout() {
        return "the second HTTPS request timed out".into();
    }
    "the certificate is not trusted: it is self-signed, still being issued, or its chain is incomplete".into()
}
