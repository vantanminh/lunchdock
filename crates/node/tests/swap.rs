//! Container replacement against a real Docker Engine. Runs only with
//! `RUDDER_DOCKER_TESTS=1`.

use rudder_core::{ContainerSpec, NodeDriver, ReadyProbe, ReplaceContainer};
use rudder_node::LocalDriver;

fn docker_tests() -> bool {
    let enabled = std::env::var("RUDDER_DOCKER_TESTS").as_deref() == Ok("1");
    if !enabled {
        eprintln!("skipping: set RUDDER_DOCKER_TESTS=1 to run Docker tests");
    }
    enabled
}

#[tokio::test]
async fn replace_keeps_settings_and_rolls_back_a_failed_probe() {
    if !docker_tests() {
        return;
    }
    let root = std::env::temp_dir().join(format!("rudder-swap-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let driver = LocalDriver::connect(&root, None);
    let name = format!("rudder-swap-test-{}", std::process::id());
    for image in ["alpine:3.20", "alpine:3.22", "busybox:1.36"] {
        if driver.image_digest(image).await.is_err() {
            driver.pull_image(image, None).await.unwrap();
        }
    }
    let _ = driver.remove_container(&name, true).await;
    let _ = driver
        .remove_container(&format!("{name}-previous"), true)
        .await;
    driver
        .create_container(&ContainerSpec {
            name: name.clone(),
            image: "alpine:3.20".into(),
            env: vec![("GREETING".into(), "kept-across-updates".into())],
            command: vec![
                "sh".into(),
                "-c".into(),
                "trap 'exit 0' TERM; sleep 600 & wait".into(),
            ],
            ports: Vec::new(),
            volumes: Vec::new(),
            memory_bytes: Some(64 * 1024 * 1024),
            nano_cpus: None,
            restart: "unless-stopped".into(),
            network: None,
            labels: vec![("rudder.test".into(), "swap".into())],
        })
        .await
        .unwrap();
    driver.start_container(&name).await.unwrap();

    let probe = ReadyProbe::Exec {
        command: vec!["sh".into(), "-c".into(), "echo $GREETING".into()],
        expect: Some("kept-across-updates".into()),
    };
    let report = driver
        .replace_container(&ReplaceContainer {
            name: name.clone(),
            image: "alpine:3.22".into(),
            probe: probe.clone(),
            timeout_secs: 30,
        })
        .await
        .unwrap();
    assert!(!report.rolled_back, "{}", report.log);
    assert_eq!(report.previous_image, "alpine:3.20");
    assert_eq!(
        driver.container_image(&name).await.unwrap().as_deref(),
        Some("alpine:3.22")
    );
    assert!(driver.container_running(&name).await.unwrap());
    assert_eq!(
        driver
            .container_image(&format!("{name}-previous"))
            .await
            .unwrap(),
        None
    );
    let release = driver
        .exec(&name, vec!["cat".into(), "/etc/alpine-release".into()])
        .await
        .unwrap();
    assert!(release.output.starts_with("3.22"), "{}", release.output);

    // The new image runs, but never passes its probe: the old container returns.
    let report = driver
        .replace_container(&ReplaceContainer {
            name: name.clone(),
            image: "busybox:1.36".into(),
            probe: ReadyProbe::Exec {
                command: vec!["sh".into(), "-c".into(), "echo $GREETING".into()],
                expect: Some("never-printed".into()),
            },
            timeout_secs: 10,
        })
        .await
        .unwrap();
    assert!(report.rolled_back, "{}", report.log);
    assert!(report.log.contains("rolling back"), "{}", report.log);
    assert_eq!(
        driver.container_image(&name).await.unwrap().as_deref(),
        Some("alpine:3.22")
    );
    assert!(driver.container_running(&name).await.unwrap());
    let greeting = driver
        .exec(
            &name,
            vec!["sh".into(), "-c".into(), "echo $GREETING".into()],
        )
        .await
        .unwrap();
    assert_eq!(greeting.output.trim(), "kept-across-updates");
    assert_eq!(
        driver
            .container_image(&format!("{name}-previous"))
            .await
            .unwrap(),
        None
    );

    driver.remove_container(&name, true).await.unwrap();
    let _ = std::fs::remove_dir_all(root);
}
