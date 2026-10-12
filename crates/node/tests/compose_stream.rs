//! `docker compose` runs: streamed output, registry logins, and stopping a run.
//!
//! These put a fake `docker` first on `PATH`, so they run on any Unix machine
//! without an engine. Each test file is its own process, so changing `PATH` here
//! does not reach other tests.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rudder_core::{ComposeAction, ComposeOptions, LogSink, NodeDriver, RegistryAuth};
use rudder_node::LocalDriver;

/// Tests share one `PATH`, so they take turns.
static TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static ORIGINAL_PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// A `docker` that runs `script` with the compose arguments.
fn fake_docker(dir: &Path, script: &str) {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let path = bin.join("docker");
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let old = ORIGINAL_PATH.get_or_init(|| std::env::var("PATH").unwrap_or_default());
    std::env::set_var("PATH", format!("{}:{old}", bin.display()));
}

async fn setup(
    name: &str,
    script: &str,
) -> (
    tokio::sync::MutexGuard<'static, ()>,
    std::path::PathBuf,
    LocalDriver,
) {
    let turn = TURN.lock().await;
    let root = std::env::temp_dir().join(format!("rudder-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("stacks/blog")).unwrap();
    std::fs::write(root.join("stacks/blog/compose.yml"), "services: {}\n").unwrap();
    fake_docker(&root, script);
    let driver = LocalDriver::connect(&root, None);
    (turn, root, driver)
}

fn collector() -> (LogSink, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink: LogSink = {
        let seen = seen.clone();
        Arc::new(move |text: &str| seen.lock().unwrap().push(text.to_owned()))
    };
    (sink, seen)
}

#[tokio::test]
async fn output_reaches_the_sink_while_the_command_runs() {
    let (_turn, root, driver) = setup(
        "stream",
        r#"echo "step one"; echo "warning on stderr" >&2; sleep 1; echo "step two""#,
    )
    .await;
    let (sink, seen) = collector();
    let options = ComposeOptions::default();
    let run = driver.compose_run(
        "blog",
        "stacks/blog/compose.yml",
        ComposeAction::Up,
        &options,
        Some(&sink),
    );
    tokio::pin!(run);
    // The first lines arrive before the command has finished.
    let early = tokio::time::timeout(Duration::from_millis(600), &mut run).await;
    assert!(early.is_err(), "the command should still be running");
    {
        let lines = seen.lock().unwrap().join("");
        assert!(lines.contains("step one"), "{lines}");
        assert!(lines.contains("warning on stderr"), "{lines}");
        assert!(!lines.contains("step two"), "{lines}");
    }
    let output = run.await.unwrap();
    assert!(output.contains("step two"));
    assert!(seen.lock().unwrap().join("").contains("step two"));
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_failed_run_reports_the_end_of_its_output() {
    let (_turn, root, driver) = setup(
        "fail",
        r#"echo "pulling"; echo "pull access denied for private/app" >&2; exit 3"#,
    )
    .await;
    let error = driver
        .compose("blog", "stacks/blog/compose.yml", ComposeAction::Pull)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("exited"), "{error}");
    assert!(error.contains("pull access denied"), "{error}");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn logins_exist_only_for_the_run() {
    let (_turn, root, driver) = setup(
        "login",
        r#"echo "config=$DOCKER_CONFIG"; cat "$DOCKER_CONFIG/config.json""#,
    )
    .await;
    let options = ComposeOptions {
        logins: vec![RegistryAuth {
            server: "ghcr.io".into(),
            username: "octo".into(),
            password: "s3cret".into(),
        }],
    };
    let output = driver
        .compose_run(
            "blog",
            "stacks/blog/compose.yml",
            ComposeAction::UpPull,
            &options,
            None,
        )
        .await
        .unwrap();
    // base64("octo:s3cret")
    assert!(output.contains("b2N0bzpzM2NyZXQ="), "{output}");
    let dir = output
        .lines()
        .find_map(|line| line.strip_prefix("config="))
        .unwrap();
    assert!(
        !Path::new(dir).exists(),
        "the login directory was left behind"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn dropping_the_future_stops_the_command() {
    let (_turn, root, driver) =
        setup("cancel", r#"echo $$ > "$(dirname "$0")/pid"; sleep 60"#).await;
    let pid_file = root.join("bin/pid");
    let run = driver.compose("blog", "stacks/blog/compose.yml", ComposeAction::UpBuild);
    let _ = tokio::time::timeout(Duration::from_millis(700), run).await;
    let pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .to_owned();
    // The process is gone shortly after the future was dropped.
    let mut gone = false;
    for _ in 0..20 {
        if !Path::new(&format!("/proc/{pid}")).exists() {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(gone, "docker compose kept running after it was cancelled");
    let _ = std::fs::remove_dir_all(root);
}
