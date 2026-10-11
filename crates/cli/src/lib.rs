//! Rudder command line client.
//!
//! API commands authenticate with a scoped API token (`Authorization: Bearer`).
//! `rudder login` checks the token and saves it with the panel URL; `RUDDER_URL`
//! and `RUDDER_TOKEN` override the saved values.

pub mod config;
pub mod decrypt;

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Context as _};
use serde::Deserialize;
use serde_json::{json, Value};

pub const HELP: &str = "\
Usage:
  rudder login --url <panel URL>        save an API token (read from RUDDER_TOKEN or stdin)
  rudder logout                         forget the saved token
  rudder services                       list services
  rudder deploy <service> [--image IMAGE] [--no-wait] [--timeout SECS]
  rudder logs <service> [-f] [-n LINES] [--since 15m|RFC3339] [--level L] [--source S] [--grep TEXT]
  rudder cache purge <service> (--all | --path P... | --prefix P) [--domain ID]
  rudder backup decrypt [--in FILE] [--out FILE]   (passphrase from RUDDER_BACKUP_PASSPHRASE)
  rudder --version

<service> is a service id, its name, project/name, or project/environment/name.
Create tokens under Account > API tokens; deploy and purge need the write scope.";

/// Where the CLI reads its settings from; `main` fills it from the environment.
pub struct Context {
    pub config_path: PathBuf,
    pub url: Option<String>,
    pub token: Option<String>,
    /// Reads a token when `RUDDER_TOKEN` is not set (stdin in the binary).
    pub read_token: Box<dyn Fn() -> std::io::Result<String> + Send + Sync>,
    /// Poll interval for `deploy` and `logs -f`.
    pub poll: Duration,
}

impl Context {
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        Self {
            config_path: config::default_path(),
            url: var("RUDDER_URL"),
            token: var("RUDDER_TOKEN"),
            read_token: Box::new(|| {
                use std::io::{BufRead, IsTerminal};
                if std::io::stdin().is_terminal() {
                    eprint!("API token: ");
                }
                let mut line = String::new();
                std::io::stdin().lock().read_line(&mut line)?;
                Ok(line.trim().to_owned())
            }),
            poll: Duration::from_secs(2),
        }
    }
}

pub async fn run(
    args: Vec<String>,
    ctx: &Context,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let mut args = args.into_iter();
    match args.next().as_deref() {
        None | Some("--help" | "-h" | "help") => {
            writeln!(out, "rudder {}\n\n{HELP}", rudder_core::VERSION)?;
            Ok(())
        }
        Some("--version" | "-V") => {
            writeln!(out, "rudder {}", rudder_core::VERSION)?;
            Ok(())
        }
        Some("backup") => match args.next().as_deref() {
            Some("decrypt") => decrypt::run(decrypt::parse(args)?),
            _ => bail!("usage: rudder backup decrypt [--in FILE] [--out FILE]"),
        },
        Some("login") => login(args.collect(), ctx, out).await,
        Some("logout") => {
            match std::fs::remove_file(&ctx.config_path) {
                Ok(()) => writeln!(out, "Removed {}", ctx.config_path.display())?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    writeln!(out, "Not logged in")?
                }
                Err(error) => return Err(error.into()),
            }
            Ok(())
        }
        Some("services") => services(&Api::from_context(ctx)?, out).await,
        Some("deploy") => deploy(&Api::from_context(ctx)?, args.collect(), ctx, out).await,
        Some("logs") => logs(&Api::from_context(ctx)?, args.collect(), ctx, out).await,
        Some("cache") => match args.next().as_deref() {
            Some("purge") => purge(&Api::from_context(ctx)?, args.collect(), out).await,
            _ => bail!("usage: rudder cache purge <service> (--all | --path P... | --prefix P)"),
        },
        Some(other) => bail!("unknown command '{other}'; see rudder --help"),
    }
}

struct Api {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Api {
    fn new(base: &str, token: &str) -> anyhow::Result<Self> {
        if !token.starts_with("rdr_") {
            bail!("that is not a Rudder API token (they start with rdr_)");
        }
        Ok(Self {
            http: reqwest::Client::builder()
                .user_agent(format!("rudder-cli/{}", rudder_core::VERSION))
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(900))
                .build()?,
            base: config::normalize_url(base)?,
            token: token.to_owned(),
        })
    }

    fn from_context(ctx: &Context) -> anyhow::Result<Self> {
        let saved = config::load(&ctx.config_path)?;
        let url = ctx
            .url
            .clone()
            .or_else(|| saved.as_ref().map(|s| s.url.clone()))
            .ok_or_else(|| anyhow!("not logged in; run `rudder login --url <panel URL>`"))?;
        let token = ctx
            .token
            .clone()
            .or_else(|| saved.map(|s| s.token))
            .ok_or_else(|| anyhow!("no API token; run `rudder login` or set RUDDER_TOKEN"))?;
        Self::new(&url, &token)
    }

    fn url(&self, path: &str, query: &[(&str, String)]) -> anyhow::Result<url::Url> {
        let mut url = url::Url::parse(&format!("{}{path}", self.base))?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        Ok(url)
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> anyhow::Result<Value> {
        let url = self.url(path, query)?;
        let mut request = self
            .http
            .request(method.clone(), url)
            .bearer_auth(&self.token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("{method} {}{path}", self.base))?;
        let status = response.status();
        let bytes = response.bytes().await?;
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(value);
        }
        let message = value["error"]["message"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| String::from_utf8_lossy(&bytes).trim().to_owned());
        match status.as_u16() {
            401 => bail!("the panel rejected the token: {message}"),
            403 => bail!("not allowed: {message}"),
            _ => bail!("{message} ({status})"),
        }
    }

    async fn get(&self, path: &str, query: &[(&str, String)]) -> anyhow::Result<Value> {
        self.call(reqwest::Method::GET, path, query, None).await
    }

    async fn post(&self, path: &str, body: Value) -> anyhow::Result<Value> {
        self.call(reqwest::Method::POST, path, &[], Some(body))
            .await
    }
}

async fn login(
    args: Vec<String>,
    ctx: &Context,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let mut url = ctx.url.clone();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--url" => url = Some(args.next().context("--url needs a value")?),
            "--token" => bail!(
                "pass the token in RUDDER_TOKEN or on stdin, not as an argument, so it stays out of shell history and ps"
            ),
            other => bail!("unknown option {other}"),
        }
    }
    let url = url
        .or_else(|| config::load(&ctx.config_path).ok().flatten().map(|s| s.url))
        .context("usage: rudder login --url <panel URL>")?;
    let token = match &ctx.token {
        Some(token) => token.clone(),
        None => (ctx.read_token)()?,
    };
    let api = Api::new(&url, token.trim())?;
    let me = api.get("/api/v1/me", &[]).await?;
    config::save(
        &ctx.config_path,
        &config::Saved {
            url: api.base.clone(),
            token: api.token.clone(),
        },
    )?;
    writeln!(
        out,
        "Logged in to {} as {} ({}). Saved to {}",
        api.base,
        me["user"]["email"].as_str().unwrap_or("?"),
        me["user"]["role"].as_str().unwrap_or("?"),
        ctx.config_path.display()
    )?;
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
struct Service {
    id: String,
    name: String,
    kind: String,
    status: String,
    project_name: String,
    environment_name: String,
}

impl Service {
    fn path(&self) -> String {
        format!(
            "{}/{}/{}",
            self.project_name, self.environment_name, self.name
        )
    }
}

async fn list_services(api: &Api) -> anyhow::Result<Vec<Service>> {
    Ok(serde_json::from_value(
        api.get("/api/v1/services", &[]).await?,
    )?)
}

async fn services(api: &Api, out: &mut (dyn Write + Send)) -> anyhow::Result<()> {
    let list = list_services(api).await?;
    let width = list
        .iter()
        .map(|s| s.path().len())
        .max()
        .unwrap_or(4)
        .max(4);
    writeln!(
        out,
        "{:<36}  {:<width$}  {:<11}  STATUS",
        "ID", "NAME", "KIND"
    )?;
    for service in list {
        writeln!(
            out,
            "{:<36}  {:<width$}  {:<11}  {}",
            service.id,
            service.path(),
            service.kind,
            service.status
        )?;
    }
    Ok(())
}

/// Find a service by id, name, `project/name`, or `project/environment/name`.
async fn resolve(api: &Api, spec: &str) -> anyhow::Result<Service> {
    let list = list_services(api).await?;
    let parts: Vec<&str> = spec.split('/').collect();
    let matches: Vec<&Service> = list
        .iter()
        .filter(|s| match parts.as_slice() {
            [one] => s.id == *one || s.name == *one,
            [project, name] => s.project_name == *project && s.name == *name,
            [project, env, name] => {
                s.project_name == *project && s.environment_name == *env && s.name == *name
            }
            _ => false,
        })
        .collect();
    match matches.as_slice() {
        [one] => Ok((*one).clone()),
        [] => bail!("no service matches '{spec}'; see `rudder services`"),
        many => bail!(
            "'{spec}' matches {} services ({}); use project/environment/name or the id",
            many.len(),
            many.iter().map(|s| s.path()).collect::<Vec<_>>().join(", ")
        ),
    }
}

async fn deploy(
    api: &Api,
    args: Vec<String>,
    ctx: &Context,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let mut spec = None;
    let mut image = None;
    let mut wait = true;
    let mut timeout = Duration::from_secs(1800);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--image" => image = Some(args.next().context("--image needs a value")?),
            "--no-wait" => wait = false,
            "--timeout" => {
                timeout = Duration::from_secs(
                    args.next()
                        .context("--timeout needs seconds")?
                        .parse()
                        .context("--timeout needs seconds")?,
                )
            }
            other if other.starts_with('-') => bail!("unknown option {other}"),
            other if spec.is_none() => spec = Some(other.to_owned()),
            other => bail!("unexpected argument {other}"),
        }
    }
    let spec = spec.context("usage: rudder deploy <service> [--image IMAGE]")?;
    let service = resolve(api, &spec).await?;
    let id = &service.id;
    let deployment = match service.kind.as_str() {
        "compose" => {
            if image.is_some() {
                bail!("--image does not apply to compose stacks");
            }
            writeln!(out, "Starting stack {}", service.path())?;
            api.post(&format!("/api/v1/services/{id}/stack/up"), json!({}))
                .await?["id"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        }
        "application" => {
            let detail = api.get(&format!("/api/v1/services/{id}"), &[]).await?;
            let from_git = image.is_none() && detail["spec"]["git"]["repo"].is_string();
            if from_git {
                writeln!(
                    out,
                    "Building {} from {}",
                    service.path(),
                    detail["spec"]["git"]["repo"].as_str().unwrap_or_default()
                )?;
                api.post(&format!("/api/v1/services/{id}/build"), json!({}))
                    .await?["deployment_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            } else {
                writeln!(
                    out,
                    "Deploying {} ({})",
                    service.path(),
                    image
                        .as_deref()
                        .or(detail["spec"]["image"].as_str())
                        .unwrap_or("current image")
                )?;
                let done = api
                    .post(
                        &format!("/api/v1/services/{id}/deploy"),
                        json!({ "image": image }),
                    )
                    .await?;
                return report(&done, &mut 0, out);
            }
        }
        other => bail!(
            "{} is a {other} service; deploy applies to applications and compose stacks",
            service.path()
        ),
    };
    if !wait {
        writeln!(out, "Queued deployment {deployment}")?;
        return Ok(());
    }
    let deadline = tokio::time::Instant::now() + timeout;
    let mut printed = 0;
    loop {
        let list = api
            .get(&format!("/api/v1/services/{id}/deployments"), &[])
            .await?;
        if let Some(current) = list
            .as_array()
            .and_then(|items| items.iter().find(|d| d["id"] == deployment.as_str()))
        {
            if current["finished_at"].is_string() {
                return report(current, &mut printed, out);
            }
            print_new_logs(current, &mut printed, out)?;
        }
        if tokio::time::Instant::now() > deadline {
            bail!(
                "deployment {deployment} is still running after {}s",
                timeout.as_secs()
            );
        }
        tokio::time::sleep(ctx.poll).await;
    }
}

fn print_new_logs(
    deployment: &Value,
    printed: &mut usize,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let logs = deployment["logs"].as_str().unwrap_or_default();
    if let Some(new) = logs.get(*printed..) {
        out.write_all(new.as_bytes())?;
        if !new.is_empty() && !new.ends_with('\n') {
            writeln!(out)?;
        }
    }
    *printed = logs.len();
    Ok(())
}

fn report(
    deployment: &Value,
    printed: &mut usize,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    print_new_logs(deployment, printed, out)?;
    let status = deployment["status"].as_str().unwrap_or("unknown");
    if status == "succeeded" {
        writeln!(
            out,
            "Deployment {} succeeded{}",
            deployment["id"].as_str().unwrap_or_default(),
            deployment["image_ref"]
                .as_str()
                .map(|image| format!(" ({image})"))
                .unwrap_or_default()
        )?;
        return Ok(());
    }
    bail!(
        "deployment {} {status}{}",
        deployment["id"].as_str().unwrap_or_default(),
        deployment["error"]
            .as_str()
            .map(|error| format!(": {error}"))
            .unwrap_or_default()
    )
}

/// `15m`, `2h`, `7d`, `30s`, or an RFC 3339 time.
fn parse_since(value: &str) -> anyhow::Result<String> {
    if chrono::DateTime::parse_from_rfc3339(value).is_ok() {
        return Ok(value.to_owned());
    }
    let (number, unit) = value.split_at(value.len().saturating_sub(1));
    let amount: i64 = number.parse().with_context(|| {
        format!("--since '{value}' is not like 15m, 2h, 7d, or an RFC 3339 time")
    })?;
    let seconds = match unit {
        "s" => amount,
        "m" => amount * 60,
        "h" => amount * 3600,
        "d" => amount * 86_400,
        _ => bail!("--since '{value}' is not like 15m, 2h, 7d, or an RFC 3339 time"),
    };
    Ok((chrono::Utc::now() - chrono::Duration::seconds(seconds)).to_rfc3339())
}

async fn logs(
    api: &Api,
    args: Vec<String>,
    ctx: &Context,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let mut spec = None;
    let mut follow = false;
    let mut lines: usize = 100;
    let mut filters: Vec<(&str, String)> = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().with_context(|| format!("{name} needs a value"));
        match arg.as_str() {
            "-f" | "--follow" => follow = true,
            "-n" | "--lines" => {
                lines = value("-n")?.parse().context("-n needs a number")?;
                if !(1..=1000).contains(&lines) {
                    bail!("-n must be between 1 and 1000");
                }
            }
            "--since" => filters.push(("since", parse_since(&value("--since")?)?)),
            "--level" => filters.push(("level", value("--level")?)),
            "--source" => filters.push(("source", value("--source")?)),
            "--grep" => filters.push(("q", value("--grep")?)),
            other if other.starts_with('-') => bail!("unknown option {other}"),
            other if spec.is_none() => spec = Some(other.to_owned()),
            other => bail!("unexpected argument {other}"),
        }
    }
    let spec = spec.context("usage: rudder logs <service> [-f] [-n LINES]")?;
    let service = resolve(api, &spec).await?;
    let mut query = filters.clone();
    query.push(("service_id", service.id.clone()));
    query.push(("limit", lines.to_string()));
    let page = api.get("/api/v1/logs", &query).await?;
    let mut entries: Vec<Value> = page["entries"].as_array().cloned().unwrap_or_default();
    entries.reverse();
    let mut last_id = 0;
    let mut last_time = None;
    for entry in &entries {
        print_entry(entry, out)?;
        last_id = last_id.max(entry["id"].as_i64().unwrap_or(0));
        last_time = entry["logged_at"].as_str().map(str::to_owned);
    }
    if !follow {
        return Ok(());
    }
    out.flush()?;
    loop {
        tokio::time::sleep(ctx.poll).await;
        let mut query: Vec<(&str, String)> = filters
            .iter()
            .filter(|(key, _)| *key != "since")
            .cloned()
            .collect();
        query.push(("service_id", service.id.clone()));
        query.push(("limit", "1000".into()));
        if let Some(time) = &last_time {
            query.push(("since", time.clone()));
        }
        let page = api.get("/api/v1/logs", &query).await?;
        let mut fresh: Vec<Value> = page["entries"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| entry["id"].as_i64().unwrap_or(0) > last_id)
            .collect();
        fresh.reverse();
        for entry in &fresh {
            print_entry(entry, out)?;
            last_id = last_id.max(entry["id"].as_i64().unwrap_or(0));
            last_time = entry["logged_at"].as_str().map(str::to_owned);
        }
        out.flush()?;
    }
}

fn print_entry(entry: &Value, out: &mut (dyn Write + Send)) -> anyhow::Result<()> {
    writeln!(
        out,
        "{} {:<5} [{}] {}",
        entry["logged_at"].as_str().unwrap_or_default(),
        entry["level"].as_str().unwrap_or_default().to_uppercase(),
        entry["source"].as_str().unwrap_or_default(),
        entry["message"].as_str().unwrap_or_default()
    )?;
    Ok(())
}

async fn purge(api: &Api, args: Vec<String>, out: &mut (dyn Write + Send)) -> anyhow::Result<()> {
    let mut spec = None;
    let mut all = false;
    let mut paths = Vec::new();
    let mut prefix = None;
    let mut domain = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--all" => all = true,
            "--path" => paths.push(args.next().context("--path needs a value")?),
            "--prefix" => prefix = Some(args.next().context("--prefix needs a value")?),
            "--domain" => domain = Some(args.next().context("--domain needs a domain id")?),
            other if other.starts_with('-') => bail!("unknown option {other}"),
            other if spec.is_none() => spec = Some(other.to_owned()),
            other => bail!("unexpected argument {other}"),
        }
    }
    let spec =
        spec.context("usage: rudder cache purge <service> (--all | --path P... | --prefix P)")?;
    if !all && paths.is_empty() && prefix.is_none() {
        bail!("say what to purge: --all, --path P (repeatable), or --prefix P");
    }
    let service = resolve(api, &spec).await?;
    let result = api
        .post(
            &format!("/api/v1/services/{}/cache/purge", service.id),
            json!({
                "all": all,
                "paths": if paths.is_empty() { None } else { Some(paths) },
                "prefix": prefix,
                "domain_id": domain,
            }),
        )
        .await?;
    writeln!(
        out,
        "Purged {} cached objects for {}",
        result["removed"].as_u64().unwrap_or(0),
        service.path()
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn since_accepts_durations_and_timestamps() {
        assert_eq!(
            parse_since("2026-10-11T10:00:00Z").unwrap(),
            "2026-10-11T10:00:00Z"
        );
        let fifteen = chrono::DateTime::parse_from_rfc3339(&parse_since("15m").unwrap()).unwrap();
        let ago = chrono::Utc::now().signed_duration_since(fifteen);
        assert!((899..=901).contains(&ago.num_seconds()), "{ago:?}");
        assert!(parse_since("15").is_err());
        assert!(parse_since("tomorrow").is_err());
        assert!(parse_since("5w").is_err());
    }

    #[test]
    fn tokens_must_look_like_rudder_tokens() {
        assert!(Api::new("http://127.0.0.1:3000", "rdr_abc").is_ok());
        assert!(Api::new("http://127.0.0.1:3000", "ghp_abc").is_err());
        assert!(Api::new("not a url", "rdr_abc").is_err());
    }

    #[test]
    fn version_constant_matches_the_product() {
        assert_eq!(rudder_core::PRODUCT, "rudder");
        assert!(!rudder_core::VERSION.is_empty());
    }
}
