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
