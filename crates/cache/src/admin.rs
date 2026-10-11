//! Admin listener: health, stats, Prometheus metrics, purge, and reload behind a bearer token.

use async_trait::async_trait;
use http::{Method, Response, StatusCode};
use pingora_core::apps::http_app::ServeHttp;
use pingora_core::protocols::http::ServerSession;
use rudder_core::{CachePurge, CachePurgeResult};

use crate::state::Shared;

const MAX_BODY: usize = 1024 * 1024;

pub struct Admin {
    pub shared: &'static Shared,
}

fn reply(status: StatusCode, content_type: &str, body: Vec<u8>) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, content_type)
        .header(http::header::CONTENT_LENGTH, body.len())
        .body(body)
        .unwrap_or_default()
}

fn json(status: StatusCode, value: &impl serde::Serialize) -> Response<Vec<u8>> {
    reply(
        status,
        "application/json",
        serde_json::to_vec(value).unwrap_or_default(),
    )
}

fn error(status: StatusCode, message: &str) -> Response<Vec<u8>> {
    json(status, &serde_json::json!({ "error": message }))
}

/// Compare without stopping at the first differing byte.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn read_body(session: &mut ServerSession) -> Result<Vec<u8>, StatusCode> {
    let mut body = Vec::new();
    loop {
        match session.read_request_body().await {
            Ok(Some(chunk)) => {
                if body.len() + chunk.len() > MAX_BODY {
                    return Err(StatusCode::PAYLOAD_TOO_LARGE);
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => return Ok(body),
            Err(_) => return Err(StatusCode::BAD_REQUEST),
        }
    }
}

#[async_trait]
impl ServeHttp for Admin {
    async fn response(&self, session: &mut ServerSession) -> Response<Vec<u8>> {
        let header = session.req_header();
        let method = header.method.clone();
        let path = header.uri.path().to_owned();
        if method == Method::GET && path == "/health" {
            return reply(StatusCode::OK, "text/plain", b"ok\n".to_vec());
        }
        let presented = header
            .headers
            .get(http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .unwrap_or_default();
        if !same(presented.as_bytes(), self.shared.token.as_bytes()) {
            return error(StatusCode::UNAUTHORIZED, "missing or wrong admin token");
        }
        match (method, path.as_str()) {
            (Method::GET, "/stats") => json(StatusCode::OK, &self.shared.stats()),
            (Method::GET, "/metrics") => reply(
                StatusCode::OK,
                "text/plain; version=0.0.4",
                self.shared.metrics().into_bytes(),
            ),
            (Method::POST, "/purge") => {
                let body = match read_body(session).await {
                    Ok(body) => body,
                    Err(status) => return error(status, "could not read the request body"),
                };
                let request: CachePurge = match serde_json::from_slice(&body) {
                    Ok(request) => request,
                    Err(parse) => {
                        return error(StatusCode::BAD_REQUEST, &format!("invalid purge: {parse}"))
                    }
                };
                if let Err(invalid) = request.validate() {
                    return error(StatusCode::BAD_REQUEST, &invalid.to_string());
                }
                let removed = self.shared.purge(&request);
                tracing::info!(host = %request.host, removed, "cache purge");
                json(StatusCode::OK, &CachePurgeResult { removed })
            }
            (Method::POST, "/reload") => match self.shared.reload() {
                Ok(changed) => json(StatusCode::OK, &serde_json::json!({ "changed": changed })),
                Err(message) => error(StatusCode::UNPROCESSABLE_ENTITY, &message),
            },
            _ => error(StatusCode::NOT_FOUND, "not found"),
        }
    }
}
