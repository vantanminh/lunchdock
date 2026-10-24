//! MCP server and the OAuth login agents use to reach it.
//!
//! Every documented HTTP route becomes one tool unless [`skip_reason`] says it
//! cannot (browser login, webhooks, WebSockets, public downloads). A new route
//! is therefore controllable as soon as it is added to the OpenAPI table.

mod gate;
pub(crate) mod http;
pub(crate) mod tokens;

use utoipa::openapi::path::HttpMethod;

use crate::openapi::{self, Route};

pub use gate::middleware as gate;
pub use http::router;

pub const ACCESS_PREFIX: &str = "rdr_mca_";

/// Why a documented route is not an agent tool. `None` means it is a tool.
pub(crate) fn skip_reason(route: &Route) -> Option<&'static str> {
    if route.status == 101 {
        return Some("websocket");
    }
    match (method_name(&route.method), route.path) {
        ("POST", "/api/v1/login" | "/api/v1/logout" | "/api/v1/setup") => Some("browser session"),
        ("GET", "/api/v1/setup/status") => Some("browser session"),
        ("POST", "/api/v1/agent/enroll") => Some("enrollment secret"),
        ("POST", "/api/v1/mcp/consent") => Some("browser approval"),
        ("GET", "/install-agent.sh" | "/downloads/{file}") => Some("public download"),
        _ if route.path.ends_with("/webhook") => Some("inbound webhook"),
        _ if route.path == "/mcp"
            || route.path.starts_with("/oauth/")
            || route.path.starts_with("/.well-known/") =>
        {
            Some("agent login")
        }
        _ => None,
    }
}

pub(crate) enum AgentAccess {
    /// Call is allowed when the grant has this scope.
    Allow(String),
    /// The path exists but agents must not call it, or it is not an API route.
    Deny,
    /// Login and the MCP endpoint itself. The handler checks the token.
    Pass,
}

pub(crate) fn agent_access(method: &str, path: &str) -> AgentAccess {
    if path == "/mcp" || path.starts_with("/oauth/") || path.starts_with("/.well-known/") {
        return AgentAccess::Pass;
    }
    for route in openapi::ROUTES {
        if method_name(&route.method) == method && path_matches(route.path, path) {
            return if skip_reason(route).is_some() {
                AgentAccess::Deny
            } else {
                AgentAccess::Allow(required_scope(route))
            };
        }
    }
    AgentAccess::Deny
}

pub(crate) struct Tool {
    pub name: String,
    pub description: String,
    pub scope: String,
    pub method: &'static str,
    pub path: &'static str,
    pub body_schema: Option<&'static str>,
}

pub(crate) fn tools() -> Vec<Tool> {
    openapi::ROUTES
        .iter()
        .filter(|route| skip_reason(route).is_none())
        .map(|route| {
            let scope = required_scope(route);
            Tool {
                name: openapi::operation_id(route),
                description: format!("{}. Requires scope {scope}.", route.summary),
                scope,
                method: method_name(&route.method),
                path: route.path,
                body_schema: route.request,
            }
        })
        .collect()
}

pub(crate) fn required_scope(route: &Route) -> String {
    let kind = if is_write(&route.method) {
        "write"
    } else {
        "read"
    };
    format!("{}:{kind}", route.tag)
}

fn method_name(method: &HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
        HttpMethod::Put => "PUT",
        HttpMethod::Patch => "PATCH",
        HttpMethod::Delete => "DELETE",
        HttpMethod::Options => "OPTIONS",
        HttpMethod::Head => "HEAD",
        HttpMethod::Trace => "TRACE",
    }
}

fn is_write(method: &HttpMethod) -> bool {
    matches!(
        method,
        HttpMethod::Post | HttpMethod::Put | HttpMethod::Patch | HttpMethod::Delete
    )
}

fn path_matches(pattern: &str, path: &str) -> bool {
    let mut pattern = pattern.split('/');
    let mut path = path.split('/');
    loop {
        match (pattern.next(), path.next()) {
            (None, None) => return true,
            (Some(expected), Some(actual)) => {
                let placeholder =
                    expected.starts_with('{') && expected.ends_with('}') && expected.len() > 2;
                if placeholder {
                    if actual.is_empty() {
                        return false;
                    }
                } else if expected != actual {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

pub(crate) async fn maintain(state: &crate::state::AppState) -> Result<(), crate::error::ApiError> {
    tokens::maintain(state).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_covers_every_documented_route() {
        let tools = tools();
        let mut names = std::collections::BTreeSet::new();
        for tool in &tools {
            assert!(
                names.insert(tool.name.clone()),
                "duplicate tool {}",
                tool.name
            );
            assert!(
                rudder_core::scope_known(&tool.scope),
                "add {} to MCP_SCOPES and the full-control template",
                tool.scope
            );
        }
        let full = rudder_core::scopes_for_template("full").unwrap();
        for route in openapi::ROUTES {
            let name = openapi::operation_id(route);
            let skipped = skip_reason(route);
            let listed = names.contains(&name);
            assert!(
                skipped.is_some() != listed,
                "{name} skipped={skipped:?} listed={listed}"
            );
            if listed {
                let scope = required_scope(route);
                assert!(
                    full.iter().any(|id| *id == scope),
                    "full control is missing {scope}"
                );
            }
        }
        assert!(tools.len() > 100, "only {} tools", tools.len());
        assert!(names.contains("get_v1_projects"));
        assert!(!names.contains("post_v1_login"));
    }
}
