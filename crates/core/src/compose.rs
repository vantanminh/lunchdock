//! Arguments for `docker compose`. The file path is one argv element.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{normalize_registry, validate_name, Error, RegistryAuth};

/// What `docker compose` should do to a stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComposeAction {
    Up,
    /// `up -d --build`: rebuild images that have a `build:` section, then start.
    UpBuild,
    /// `up -d --pull always`: fetch newer images for every service, then start.
    UpPull,
    Down,
    Restart,
    Pull,
    /// `down --volumes --remove-orphans`: removes containers and named volumes.
    Destroy,
}

/// Receives the output of a running command, a few lines at a time.
pub type LogSink = Arc<dyn Fn(&str) + Send + Sync>;

/// What a `docker compose` run needs besides the project and the action.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComposeOptions {
    /// Logins for the registries the project's images come from. The node writes
    /// them to a Docker config that exists only for the run.
    #[serde(default)]
    pub logins: Vec<RegistryAuth>,
}

/// Build the argv for `docker compose`, excluding the `docker` binary itself.
pub fn compose_args(
    project: &str,
    file: &str,
    action: ComposeAction,
) -> Result<Vec<String>, Error> {
    validate_name(project)?;
    if file.is_empty() || file.contains('\0') || file.contains('\n') || file.contains('\r') {
        return Err(Error::Validation("invalid compose file path".into()));
    }
    let mut args = vec![
        "compose".to_owned(),
        "-p".to_owned(),
        project.to_owned(),
        "-f".to_owned(),
        file.to_owned(),
        match action {
            ComposeAction::Up | ComposeAction::UpBuild | ComposeAction::UpPull => "up",
            ComposeAction::Down | ComposeAction::Destroy => "down",
            ComposeAction::Restart => "restart",
            ComposeAction::Pull => "pull",
        }
        .to_owned(),
    ];
    match action {
        ComposeAction::Up => {
            args.push("-d".to_owned());
            args.push("--remove-orphans".to_owned());
        }
        ComposeAction::UpBuild => {
            args.extend(["-d", "--build", "--remove-orphans"].map(str::to_owned));
        }
        ComposeAction::UpPull => {
            args.extend(["-d", "--pull", "always", "--remove-orphans"].map(str::to_owned));
        }
        ComposeAction::Destroy => {
            args.push("--volumes".to_owned());
            args.push("--remove-orphans".to_owned());
        }
        _ => {}
    }
    Ok(args)
}

/// The key `docker` uses for a registry in `config.json`.
fn config_key(server: &str) -> String {
    let host = normalize_registry(server);
    if matches!(
        host.as_str(),
        "docker.io" | "index.docker.io" | "registry-1.docker.io"
    ) {
        "https://index.docker.io/v1/".to_owned()
    } else {
        host
    }
}

/// A Docker `config.json` holding `logins`, for one command through `DOCKER_CONFIG`.
///
/// `existing` is the user's own config, kept so credential helpers and contexts
/// still work; our logins replace entries for the same registry. Moving
/// `DOCKER_CONFIG` hides the plugins in the user's `~/.docker/cli-plugins`, so
/// `plugin_dirs` is added as `cliPluginsExtraDirs`.
pub fn docker_config_with_logins(
    existing: Option<&str>,
    logins: &[RegistryAuth],
    plugin_dirs: &[String],
) -> Result<String, Error> {
    use base64::Engine;
    let mut config: serde_json::Value = match existing {
        Some(text) if !text.trim().is_empty() => serde_json::from_str(text)
            .map_err(|error| Error::Node(format!("docker config.json is not JSON: {error}")))?,
        _ => serde_json::json!({}),
    };
    let root = config
        .as_object_mut()
        .ok_or_else(|| Error::Node("docker config.json is not an object".into()))?;
    let auths = root
        .entry("auths")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| Error::Node("docker config.json auths is not an object".into()))?;
    for login in logins {
        let token = base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", login.username, login.password));
        auths.insert(
            config_key(&login.server),
            serde_json::json!({ "auth": token }),
        );
    }
    if !plugin_dirs.is_empty() {
        let dirs = root
            .entry("cliPluginsExtraDirs")
            .or_insert_with(|| serde_json::json!([]));
        if let Some(list) = dirs.as_array_mut() {
            for dir in plugin_dirs {
                if !list.iter().any(|entry| entry.as_str() == Some(dir)) {
                    list.push(serde_json::Value::String(dir.clone()));
                }
            }
        }
    }
    serde_json::to_string_pretty(&config).map_err(|error| Error::Node(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn up_is_detached_and_paths_stay_one_argument() {
        let args = compose_args(
            "blog",
            "/var/lib/rudder/stacks/blog/compose.yml",
            ComposeAction::Up,
        )
        .unwrap();
        assert_eq!(
            args,
            vec![
                "compose",
                "-p",
                "blog",
                "-f",
                "/var/lib/rudder/stacks/blog/compose.yml",
                "up",
                "-d",
                "--remove-orphans",
            ]
        );
    }

    #[test]
    fn rejects_bad_project_names_and_newlines_in_paths() {
        assert!(compose_args("Blog", "compose.yml", ComposeAction::Down).is_err());
        assert!(compose_args("blog", "compose.yml\n-f", ComposeAction::Pull).is_err());
        let pull = compose_args("blog", "compose.yml", ComposeAction::Pull).unwrap();
        assert_eq!(pull.last().map(String::as_str), Some("pull"));
        let destroy = compose_args("blog", "compose.yml", ComposeAction::Destroy).unwrap();
        assert_eq!(&destroy[5..], ["down", "--volumes", "--remove-orphans"]);
    }

    #[test]
    fn rebuild_and_pull_variants_extend_up() {
        let build = compose_args("blog", "compose.yml", ComposeAction::UpBuild).unwrap();
        assert_eq!(&build[5..], ["up", "-d", "--build", "--remove-orphans"]);
        let pull = compose_args("blog", "compose.yml", ComposeAction::UpPull).unwrap();
        assert_eq!(
            &pull[5..],
            ["up", "-d", "--pull", "always", "--remove-orphans"]
        );
    }

    fn login(server: &str) -> RegistryAuth {
        RegistryAuth {
            server: server.into(),
            username: "octo".into(),
            password: "s3cret".into(),
        }
    }

    #[test]
    fn docker_config_carries_logins_and_keeps_the_users_settings() {
        let existing = r#"{"credsStore":"desktop","auths":{"quay.io":{"auth":"x"}}}"#;
        let text = docker_config_with_logins(
            Some(existing),
            &[login("https://ghcr.io"), login("docker.io")],
            &["/root/.docker/cli-plugins".into()],
        )
        .unwrap();
        let config: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(config["credsStore"], "desktop");
        assert_eq!(config["auths"]["quay.io"]["auth"], "x");
        // base64("octo:s3cret")
        assert_eq!(config["auths"]["ghcr.io"]["auth"], "b2N0bzpzM2NyZXQ=");
        assert!(config["auths"]["https://index.docker.io/v1/"].is_object());
        assert_eq!(
            config["cliPluginsExtraDirs"][0],
            "/root/.docker/cli-plugins"
        );
        assert!(docker_config_with_logins(Some("[]"), &[], &[]).is_err());
        let fresh = docker_config_with_logins(None, &[login("ghcr.io")], &[]).unwrap();
        assert!(!fresh.contains("cliPluginsExtraDirs"));
    }
}
