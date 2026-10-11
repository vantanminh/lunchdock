//! Arguments for `docker compose`. The file path is one argv element.

use crate::{validate_name, Error};

/// What `docker compose` should do to a stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeAction {
    Up,
    Down,
    Restart,
    Pull,
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
            ComposeAction::Up => "up",
            ComposeAction::Down => "down",
            ComposeAction::Restart => "restart",
            ComposeAction::Pull => "pull",
        }
        .to_owned(),
    ];
    if action == ComposeAction::Up {
        args.push("-d".to_owned());
    }
    Ok(args)
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
            ]
        );
    }

    #[test]
    fn rejects_bad_project_names_and_newlines_in_paths() {
        assert!(compose_args("Blog", "compose.yml", ComposeAction::Down).is_err());
        assert!(compose_args("blog", "compose.yml\n-f", ComposeAction::Pull).is_err());
        let pull = compose_args("blog", "compose.yml", ComposeAction::Pull).unwrap();
        assert_eq!(pull.last().map(String::as_str), Some("pull"));
    }
}
