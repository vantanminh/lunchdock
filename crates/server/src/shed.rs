//! Request priority: a flood of anonymous requests (webhooks, sign-in attempts,
//! scanners) cannot take the panel away from the people who are signed in.
//!
//! Requests that carry a session cookie or a bearer token always pass. Other
//! requests share a fixed number of slots; when every slot is busy the panel
//! answers 503 at once instead of queueing work it cannot finish.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tokio::sync::Semaphore;

use crate::auth::SESSION_COOKIE;
use crate::error::ApiError;

/// Anonymous requests the panel works on at the same time.
pub const ANONYMOUS_SLOTS: usize = 64;

/// Slots shared by anonymous requests.
pub fn slots() -> Arc<Semaphore> {
    Arc::new(Semaphore::new(ANONYMOUS_SLOTS))
}

pub async fn middleware(
    State(slots): State<Arc<Semaphore>>,
    request: Request,
    next: Next,
) -> Response {
    if signed_in(request.headers()) || request.uri().path() == "/api/health" {
        return next.run(request).await;
    }
    match slots.try_acquire_owned() {
        Ok(permit) => {
            let response = next.run(request).await;
            drop(permit);
            response
        }
        Err(_) => {
            let mut response = ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "busy",
                "the panel is busy with other requests; try again in a few seconds",
            )
            .into_response();
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
            response
        }
    }
}

/// The request says who it is: a session cookie or a bearer token. Whether the
/// credential is valid is checked later by the handler, as before.
fn signed_in(headers: &HeaderMap) -> bool {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("Bearer ") && value.len() > 7);
    bearer
        || headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(';'))
            .any(|pair| {
                pair.trim()
                    .split_once('=')
                    .is_some_and(|(name, value)| name == SESSION_COOKIE && !value.is_empty())
            })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::routing::get;
    use tower::ServiceExt;

    fn app(slots: Arc<Semaphore>) -> axum::Router {
        axum::Router::new()
            .route("/api/v1/x", get(|| async { "ok" }))
            .route("/api/health", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(slots, middleware))
    }

    fn request(path: &str, header: Option<(header::HeaderName, &str)>) -> Request {
        let mut builder = Request::builder().uri(path);
        if let Some((name, value)) = header {
            builder = builder.header(name, value);
        }
        builder.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn anonymous_requests_are_turned_away_when_slots_run_out() {
        let full = Arc::new(Semaphore::new(0));
        let response = app(full.clone())
            .oneshot(request("/api/v1/x", None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()[header::RETRY_AFTER], "5");

        let health = app(full.clone())
            .oneshot(request("/api/health", None))
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);

        let cookie = app(full.clone())
            .oneshot(request(
                "/api/v1/x",
                Some((header::COOKIE, "theme=dark; rudder_session=abc")),
            ))
            .await
            .unwrap();
        assert_eq!(cookie.status(), StatusCode::OK);

        let token = app(full)
            .oneshot(request(
                "/api/v1/x",
                Some((header::AUTHORIZATION, "Bearer rdr_x")),
            ))
            .await
            .unwrap();
        assert_eq!(token.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn anonymous_requests_pass_while_slots_are_free() {
        let response = app(slots())
            .oneshot(request("/api/v1/x", None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn empty_credentials_do_not_count() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_static("rudder_session="));
        assert!(!signed_in(&headers));
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer "));
        assert!(!signed_in(&headers));
    }
}
