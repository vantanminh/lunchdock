//! `web/openapi.json` feeds `openapi-typescript`; keep it in sync with the server.
//! Regenerate with `cargo run -p rudder-server -- openapi > web/openapi.json && pnpm --dir web gen:api`.

#[test]
fn web_openapi_snapshot_is_current() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/openapi.json");
    let committed = std::fs::read_to_string(path).unwrap_or_default();
    let current = rudder_server::openapi_json();
    assert!(
        committed.trim() == current.trim(),
        "web/openapi.json is stale; run `cargo run -p rudder-server -- openapi > web/openapi.json && pnpm --dir web gen:api`"
    );
}

/// `.route("/path", get(a).post(b))` calls in `src/routes`, as (method, path).
fn registered_routes() -> Vec<(String, String)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/routes");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let source = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        let mut rest = source.as_str();
        while let Some(start) = rest.find(".route(") {
            rest = &rest[start + ".route(".len()..];
            let segment = &rest[..rest.find(".route(").unwrap_or(rest.len())];
            let segment = &segment[..segment.find(';').unwrap_or(segment.len())];
            let Some(path) = segment.split('"').nth(1) else {
                continue;
            };
            for method in ["get", "post", "put", "patch", "delete"] {
                let call = format!("{method}(");
                let found = segment.match_indices(&call).any(|(at, _)| {
                    !segment[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_')
                });
                if found {
                    out.push((method.to_owned(), path.to_owned()));
                }
            }
        }
    }
    out
}

#[test]
fn every_route_is_documented() {
    let doc: serde_json::Value = serde_json::from_str(&rudder_server::openapi_json()).unwrap();
    let routes = registered_routes();
    assert!(routes.len() > 100, "found only {} routes", routes.len());
    let missing: Vec<String> = routes
        .iter()
        .filter(|(method, path)| doc["paths"][path.as_str()][method.as_str()].is_null())
        .map(|(method, path)| format!("{} {path}", method.to_uppercase()))
        .collect();
    assert!(
        missing.is_empty(),
        "routes missing from openapi.rs ROUTES: {missing:#?}"
    );
}
