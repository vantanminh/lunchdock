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

#[test]
fn releases_publish_the_changelog_as_their_notes() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let release =
        std::fs::read_to_string(root.join(".github/workflows/release.yml")).expect("read release");
    assert!(
        release.contains("deploy/release/changelog.sh"),
        "release must read its notes from CHANGELOG.md"
    );
    assert!(
        release.contains("--notes-file") && !release.contains("--generate-notes"),
        "release notes come from the changelog, not from commit titles"
    );
    let ci = std::fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("read ci");
    assert!(
        ci.contains("CHANGELOG.md"),
        "pull requests must be checked for a changelog entry"
    );
    let changelog = std::fs::read_to_string(root.join("CHANGELOG.md")).expect("read changelog");
    assert!(
        changelog.contains("## [Unreleased]"),
        "CHANGELOG.md needs an [Unreleased] section"
    );
}
