//! Stops an agent token from calling anything outside its scopes.
//!
//! Cookie sessions and ordinary API tokens are unchanged. An agent token that
//! hits a route with no scope, or a scope the approval does not include, is
//! refused before the handler runs.

use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::error::ApiError;
use crate::state::AppState;

use super::{agent_access, tokens, AgentAccess, ACCESS_PREFIX};

pub async fn middleware(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let Some(token) = bearer(request.headers()) else {
        return next.run(request).await;
    };
    if !token.starts_with(ACCESS_PREFIX) {
        return next.run(request).await;
    }
    let path = request.uri().path().to_owned();
    let method = request.method().as_str().to_owned();
    match agent_access(&method, &path) {
        AgentAccess::Pass => next.run(request).await,
        AgentAccess::Allow(scope) => match tokens::load_access(&state, &token).await {
            Ok(user) if user.scopes.iter().any(|have| have == &scope) => next.run(request).await,
            Ok(_) => ApiError::new(
                StatusCode::FORBIDDEN,
                "forbidden",
                format!("this agent approval does not include {scope}"),
            )
            .into_response(),
            Err(error) => error.into_response(),
        },
        AgentAccess::Deny => {
            let mut response = ApiError::new(
                StatusCode::FORBIDDEN,
                "forbidden",
                "agents cannot call this endpoint",
            )
            .into_response();
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-store"),
            );
            response
        }
    }
}

fn bearer(headers: &axum::http::HeaderMap) -> Option<String> {
    let value = headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
}
