//! Build plans as argument vectors.
//!
//! Callers run these with `Command::new` and a working directory. Nothing here
//! is joined into a shell string.

use crate::{validate_env_key, validate_image_ref, Error};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildStrategy {
    Dockerfile,
    Nixpacks,
    Static,
}

impl BuildStrategy {
    pub fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "dockerfile" => Ok(Self::Dockerfile),
            "nixpacks" => Ok(Self::Nixpacks),
            "static" => Ok(Self::Static),
            _ => Err(Error::Validation(
                "strategy must be dockerfile, nixpacks, or static".into(),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dockerfile => "dockerfile",
            Self::Nixpacks => "nixpacks",
            Self::Static => "static",
        }
    }
}

pub struct BuildPlan {
    pub strategy: BuildStrategy,
    pub context: String,
    pub dockerfile: String,
    pub publish_dir: String,
    pub image: String,
    pub build_args: Vec<(String, String)>,
}

pub fn can_start(running: i64, limit: i64) -> bool {
    limit > 0 && running < limit
}

pub fn validate_rel_path(path: &str) -> Result<(), Error> {
    if path == "." {
        return Ok(());
    }
    if path.is_empty() || path.len() > 200 || path.starts_with('/') || path.contains('\\') {
        return Err(Error::Validation("build path must be relative".into()));
    }
    let ok = path.split('/').all(|part| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && !part.contains(':')
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    });
    if ok {
        Ok(())
    } else {
        Err(Error::Validation(
            "build path contains an invalid component".into(),
        ))
    }
}

pub fn validate_git_url(url: &str) -> Result<(), Error> {
    let graphic = !url.is_empty()
        && url.len() <= 512
        && url.bytes().all(|byte| {
            byte.is_ascii_graphic()
                && !matches!(
                    byte,
                    b';' | b'|' | b'&' | b'$' | b'`' | b'\'' | b'"' | b'\\' | b'<' | b'>'
                )
        });
    if !graphic {
        return Err(Error::Validation("invalid git url".into()));
    }
    if let Some(rest) = url.strip_prefix("https://") {
        if rest.contains('@') || rest.is_empty() {
            return Err(Error::Validation(
                "https git url must not include credentials".into(),
            ));
        }
        return Ok(());
    }
    if url.starts_with("ssh://") || url.starts_with("git@") {
        return Ok(());
    }
    Err(Error::Validation(
        "git url must be https, ssh, or git@".into(),
    ))
}

pub fn validate_branch(branch: &str) -> Result<(), Error> {
    crate::webhook::validate_branch(branch)
}

pub fn git_clone_argv(branch: &str, url: &str, dest: &str) -> Result<Vec<String>, Error> {
    validate_branch(branch)?;
    validate_git_url(url)?;
    if dest.is_empty()
        || dest.len() > 512
        || dest.contains("..")
        || dest.bytes().any(|byte| {
            byte.is_ascii_control()
                || matches!(
                    byte,
                    b';' | b'|' | b'&' | b'$' | b'`' | b'\'' | b'"' | b'\\' | b' '
                )
        })
    {
        return Err(Error::Validation("invalid clone destination".into()));
    }
    Ok(vec![
        "git".into(),
        "clone".into(),
        "--depth".into(),
        "1".into(),
        "--branch".into(),
        branch.into(),
        "--".into(),
        url.into(),
        dest.into(),
    ])
}

pub fn docker_build_argv(plan: &BuildPlan) -> Result<Vec<String>, Error> {
    validate_rel_path(&plan.context)?;
    validate_image_ref(&plan.image)?;
    let dockerfile = dockerfile_path(plan)?;
    let mut argv = vec![
        "docker".into(),
        "build".into(),
        "-f".into(),
        dockerfile,
        "-t".into(),
        plan.image.clone(),
    ];
    push_args(&mut argv, plan, "--build-arg")?;
    argv.push(plan.context.clone());
    Ok(argv)
}

pub fn nixpacks_argv(plan: &BuildPlan) -> Result<Vec<String>, Error> {
    validate_rel_path(&plan.context)?;
    validate_image_ref(&plan.image)?;
    let mut argv = vec![
        "nixpacks".into(),
        "build".into(),
        plan.context.clone(),
        "--name".into(),
        plan.image.clone(),
    ];
    push_args(&mut argv, plan, "--env")?;
    Ok(argv)
}

pub fn static_dockerfile(publish_dir: &str) -> Result<String, Error> {
    validate_rel_path(publish_dir)?;
    Ok(format!(
        "FROM nginx:1.27-alpine\nCOPY {publish_dir} /usr/share/nginx/html\n"
    ))
}

pub fn redact(text: &str, secrets: &[&str]) -> String {
    let mut out = text.to_owned();
    for secret in secrets {
        if secret.len() >= 4 {
            out = out.replace(secret, "***");
        }
    }
    out
}

fn dockerfile_path(plan: &BuildPlan) -> Result<String, Error> {
    if plan.strategy == BuildStrategy::Static {
        return Ok(if plan.context == "." {
            "Dockerfile.rudder".into()
        } else {
            format!("{}/Dockerfile.rudder", plan.context)
        });
    }
    validate_rel_path(&plan.dockerfile)?;
    Ok(plan.dockerfile.clone())
}

fn push_args(argv: &mut Vec<String>, plan: &BuildPlan, flag: &str) -> Result<(), Error> {
    for (key, value) in &plan.build_args {
        validate_env_key(key)?;
        if value.is_empty()
            || value.len() > 4096
            || value.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(Error::Validation("invalid build argument".into()));
        }
        argv.push(flag.into());
        argv.push(format!("{key}={value}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(strategy: BuildStrategy) -> BuildPlan {
        BuildPlan {
            strategy,
            context: ".".into(),
            dockerfile: "Dockerfile".into(),
            publish_dir: "dist".into(),
            image: "rudder-web:abc123".into(),
            build_args: vec![("NODE_ENV".into(), "production".into())],
        }
    }

    #[test]
    fn docker_argv_keeps_args_separate() {
        let argv = docker_build_argv(&plan(BuildStrategy::Dockerfile)).unwrap();
        assert_eq!(argv[0], "docker");
        assert!(argv.windows(2).any(|pair| pair == ["-f", "Dockerfile"]));
        assert!(argv
            .windows(2)
            .any(|pair| pair == ["--build-arg", "NODE_ENV=production"]));
        assert_eq!(argv.last().map(String::as_str), Some("."));
        assert!(!argv
            .iter()
            .any(|arg| arg.contains(';') || arg.contains("&&")));
    }

    #[test]
    fn docker_argv_rejects_parent_segments_and_shell_characters() {
        let mut bad = plan(BuildStrategy::Dockerfile);
        bad.dockerfile = "../Dockerfile".into();
        assert!(docker_build_argv(&bad).is_err());
        bad.dockerfile = "Dockerfile".into();
        bad.context = "src;rm".into();
        assert!(docker_build_argv(&bad).is_err());
        bad.context = ".".into();
        bad.image = "nginx:1;id".into();
        assert!(docker_build_argv(&bad).is_err());
    }

    #[test]
    fn nixpacks_and_static_plans() {
        let argv = nixpacks_argv(&plan(BuildStrategy::Nixpacks)).unwrap();
        assert_eq!(&argv[..3], ["nixpacks", "build", "."]);
        assert!(argv.iter().any(|arg| arg == "--name"));
        let file = static_dockerfile("dist").unwrap();
        assert!(file.contains("COPY dist /usr/share/nginx/html"));
        assert!(static_dockerfile("../public").is_err());
    }

    #[test]
    fn concurrency_gate_and_git_url() {
        assert!(can_start(0, 1));
        assert!(!can_start(1, 1));
        assert!(!can_start(0, 0));
        assert!(validate_git_url("https://github.com/acme/web.git").is_ok());
        assert!(validate_git_url("git@github.com:acme/web.git").is_ok());
        assert!(validate_git_url("https://user:token@github.com/acme/web.git").is_err());
        assert!(validate_git_url("https://github.com/acme/web.git;rm").is_err());
        let argv = git_clone_argv(
            "main",
            "https://github.com/acme/web.git",
            "/tmp/rudder-build",
        )
        .unwrap();
        assert_eq!(argv[0], "git");
        assert!(argv.iter().any(|arg| arg == "--"));
    }
}
