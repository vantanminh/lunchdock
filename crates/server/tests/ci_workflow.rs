use std::path::PathBuf;

#[test]
fn ci_workflow_runs_format_clippy_tests_and_frontend() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/ci.yml");
    let text = std::fs::read_to_string(&path).expect("read ci workflow");
    assert!(
        text.contains("cargo fmt --all --check"),
        "fmt check missing"
    );
    assert!(
        text.contains("cargo clippy --workspace --all-targets -- -D warnings"),
        "clippy gate missing"
    );
    assert!(text.contains("cargo test --workspace"), "test gate missing");
    assert!(text.contains("SQLX_OFFLINE"), "offline sqlx flag missing");
    assert!(text.contains("pnpm build"), "frontend build missing");
}
