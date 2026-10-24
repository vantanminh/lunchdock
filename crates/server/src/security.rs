//! Headers that apply to every response, including errors and the panel pages.
//!
//! The API docs page is exempt from the script policy because Swagger UI needs
//! its own inline scripts. Everything else only runs scripts the panel served.

use axum::extract::{Request, State};
use axum::http::{header, HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;

use crate::state::AppState;

const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self'; connect-src 'self'; worker-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'";

pub async fn middleware(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let secure = state.config.cookie_secure;
    let path = request.uri().path().to_owned();
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    insert(headers, header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    insert(headers, header::X_FRAME_OPTIONS, "DENY");
    insert(headers, header::REFERRER_POLICY, "no-referrer");
    insert(
        headers,
        HeaderName::from_static("permissions-policy"),
        "camera=(), microphone=(), geolocation=()",
    );
    if !path.starts_with("/api/docs") {
        insert(headers, header::CONTENT_SECURITY_POLICY, CSP);
    }
    if secure {
        insert(
            headers,
            header::STRICT_TRANSPORT_SECURITY,
            "max-age=15552000; includeSubDomains",
        );
    }
    response
}

fn insert(headers: &mut axum::http::HeaderMap, name: impl Into<HeaderName>, value: &'static str) {
    if let Ok(value) = HeaderValue::from_str(value) {
        headers.insert(name.into(), value);
    }
}
