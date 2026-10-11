//! Serves the built React panel from the binary.

use axum::body::Body;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::Response;
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "../../web/dist"]
struct Assets;

pub async fn spa(uri: axum::http::Uri) -> Response {
    spa_response(uri.path())
}

pub fn spa_response(path: &str) -> Response {
    if path.starts_with("/api/") || path == "/api" {
        return StatusCode::NOT_FOUND.into_response_empty();
    }
    let relative = path.trim_start_matches('/');
    let asset_path = if relative.is_empty() || Assets::get(relative).is_none() {
        if relative.contains('.') {
            return StatusCode::NOT_FOUND.into_response_empty();
        }
        "index.html"
    } else {
        relative
    };
    let Some(asset) = Assets::get(asset_path) else {
        return StatusCode::NOT_FOUND.into_response_empty();
    };
    let mime = mime_guess::from_path(asset_path).first_or_octet_stream();
    let cache = if asset_path == "index.html" {
        "no-cache"
    } else {
        "public, max-age=31536000, immutable"
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime.as_ref())
        .header(header::CACHE_CONTROL, cache)
        .body(Body::from(asset.data.into_owned()))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response_empty())
}

trait EmptyResponse {
    fn into_response_empty(self) -> Response;
}

impl EmptyResponse for StatusCode {
    fn into_response_empty(self) -> Response {
        Response::builder()
            .status(self)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            )
            .body(Body::from(self.canonical_reason().unwrap_or("error")))
            .expect("status response")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_routes_receive_the_embedded_shell() {
        assert_eq!(spa_response("/").status(), StatusCode::OK);
        let index = Assets::get("index.html").expect("index");
        let text = String::from_utf8_lossy(&index.data);
        assert!(text.contains("Rudder"));
        assert_eq!(spa_response("/api/missing").status(), StatusCode::NOT_FOUND);
        assert_eq!(spa_response("/dashboard").status(), StatusCode::OK);
    }
}
