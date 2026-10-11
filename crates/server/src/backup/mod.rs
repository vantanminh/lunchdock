//! Backup and restore execution.
//!
//! A run is: prepare the source (hooks, optional stop) → stream a dump or tar
//! → filter → compress → encrypt → hash → throttle → fan out to every S3
//! destination → optional verify → retention. Restores reverse the chain.
//! Runs are rows in `backup_runs`; their logs are redacted before storage.

pub mod pipeline;
pub mod tarfilter;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use rudder_core::{
    dump_command, dump_extension, expired_ids, filter_databases, is_due,
    mysql_list_databases_command, object_key, redis_bgsave_command, redis_persistence_command,
    restore_command, retry_backoff_secs, validate_host_path, ArchiveSource, BackupPoint,
    ByteStream, Compression, Consistency, Engine, Error, ExecSpec, KeyVars, NodeDriver,
    RedisPersistence, RetentionSpec, ToolCommand, ToolOptions,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::error::ApiError;
use crate::events::{self, Event};
use crate::nodes::driver_for;
use crate::routes::catalog::{bad, conflict, core_error, not_found};
use crate::routes::databases::{load_database, DatabaseInfo};
use crate::s3::{self, Destination};
use crate::state::AppState;

use pipeline::Meter;

/// A job row with secrets decrypted. Not `Debug` on purpose.
#[derive(Clone)]
pub struct Job {
    pub id: Uuid,
    pub name: String,
    pub server_id: Option<Uuid>,
    pub service_id: Option<Uuid>,
    pub target_kind: String,
    pub target_spec: Value,
    pub destination_ids: Vec<Uuid>,
    pub path_prefix: String,
    pub filename_template: String,
    pub retention: RetentionSpec,
    pub compression: Compression,
    pub compression_level: i32,
    pub passphrase: Option<String>,
    pub tool_options: ToolOptions,
    pub consistency: Consistency,
    pub timeout_secs: i32,
    pub retries: i32,
    pub bandwidth_limit_bps: Option<i64>,
    pub verify_after_upload: bool,
    pub notify_on_success: bool,
}

/// Facts a restore needs, captured when the backup ran.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RunMeta {
    pub target_kind: String,
    pub engine: Option<String>,
    pub dump_all: bool,
    pub compression: String,
    pub encrypted: bool,
    pub source: Option<ArchiveSource>,
    pub service_id: Option<Uuid>,
    pub restore_of: Option<Uuid>,
    pub restore_target: Option<String>,
}

pub async fn load_job(state: &AppState, id: Uuid) -> Result<Job, ApiError> {
    let row = sqlx::query!(
        r#"
        SELECT id, name, server_id, service_id, target_kind, target_spec, destination_ids,
               path_prefix, filename_template, retention_spec, compression, compression_level,
               encryption_passphrase_enc, tool_options, consistency_spec, timeout_secs, retries,
               bandwidth_limit_bps, verify_after_upload, notify_on_success
        FROM backup_jobs
        WHERE id = $1
        "#,
        id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?
    .ok_or_else(|| not_found("backup job not found"))?;
    let passphrase = match row.encryption_passphrase_enc {
        Some(blob) => {
            let bytes = state.cipher.decrypt(&blob).map_err(|_| {
                ApiError::new(
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    "crypto_error",
                    "stored backup passphrase could not be decrypted",
                )
            })?;
            Some(String::from_utf8(bytes).map_err(|_| bad("invalid", "passphrase is not UTF-8"))?)
        }
        None => None,
    };
    Ok(Job {
        id: row.id,
        name: row.name,
        server_id: row.server_id,
        service_id: row.service_id,
        target_kind: row.target_kind,
        target_spec: row.target_spec,
        destination_ids: row.destination_ids,
        path_prefix: row.path_prefix,
        filename_template: row.filename_template,
        retention: serde_json::from_value(row.retention_spec).unwrap_or_default(),
        compression: Compression::parse(&row.compression).map_err(core_error)?,
        compression_level: row.compression_level,
        passphrase,
        tool_options: serde_json::from_value(row.tool_options).unwrap_or_default(),
        consistency: serde_json::from_value(row.consistency_spec).unwrap_or_default(),
        timeout_secs: row.timeout_secs,
        retries: row.retries,
        bandwidth_limit_bps: row.bandwidth_limit_bps,
        verify_after_upload: row.verify_after_upload,
        notify_on_success: row.notify_on_success,
    })
}

/// Server that executes the job.
pub async fn job_server(state: &AppState, job: &Job) -> Result<Uuid, ApiError> {
    if let Some(service_id) = job.service_id {
        return sqlx::query_scalar!(
            r#"SELECT server_id FROM services WHERE id = $1"#,
            service_id
        )
        .fetch_optional(&state.pool)
        .await
        .map_err(ApiError::database)?
        .ok_or_else(|| not_found("service not found"));
    }
    if let Some(server_id) = job.server_id {
        return Ok(server_id);
    }
    sqlx::query_scalar!(r#"SELECT id FROM servers WHERE kind = 'local' LIMIT 1"#)
        .fetch_optional(&state.pool)
        .await
        .map_err(ApiError::database)?
        .ok_or_else(|| not_found("local server not found"))
}

/// Collects run output and strips secrets before it is stored.
pub struct RunLog {
    text: String,
    secrets: Vec<String>,
    started: Instant,
}

impl RunLog {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            secrets: Vec::new(),
            started: Instant::now(),
        }
    }

    pub fn secret(&mut self, value: &str) {
        if value.len() >= 4 {
            self.secrets.push(value.to_owned());
        }
    }

    pub fn line(&mut self, message: impl AsRef<str>) {
        let refs: Vec<&str> = self.secrets.iter().map(String::as_str).collect();
        let clean = rudder_core::redact(message.as_ref(), &refs);
        let _ = writeln!(
            self.text,
            "[{:>7.1}s] {clean}",
            self.started.elapsed().as_secs_f64()
        );
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

impl Default for RunLog {
    fn default() -> Self {
        Self::new()
    }
}

/// Settings for one pass through [`upload_stream`].
pub struct UploadPlan<'a> {
    pub key: &'a str,
    pub compression: Compression,
    pub level: i32,
    pub passphrase: Option<&'a str>,
    pub bandwidth_limit_bps: Option<u64>,
    pub verify: bool,
}

#[derive(Debug, Clone)]
pub struct UploadOutcome {
    pub checksum: String,
    pub size: u64,
    pub succeeded: Vec<Uuid>,
    pub failed: Vec<(Uuid, String)>,
}

/// Compress, encrypt, hash, throttle, and upload one stream to every destination.
pub async fn upload_stream(
    source: ByteStream,
    plan: &UploadPlan<'_>,
    destinations: &[Destination],
    log: &mut RunLog,
) -> Result<UploadOutcome, Error> {
    if destinations.is_empty() {
        return Err(Error::Validation("the job has no S3 destination".into()));
    }
    let mut stream = pipeline::compress(source, plan.compression, plan.level);
    if let Some(passphrase) = plan.passphrase {
        stream = pipeline::encrypt(stream, passphrase)?;
    }
    let meter = Meter::default();
    let stream = pipeline::throttle(meter.wrap(stream), plan.bandwidth_limit_bps);
    let (feeds, source_task) = pipeline::fan_out(stream, destinations.len());
    let uploads = destinations.iter().zip(feeds).map(|(destination, feed)| {
        let store = destination.store.clone();
        let key = plan.key.to_owned();
        let id = destination.id;
        async move { (id, s3::upload(store, &key, feed).await) }
    });
    let results = futures_util::future::join_all(uploads).await;
    let source_result = source_task
        .await
        .map_err(|error| Error::Node(format!("stream task failed: {error}")))?;
    let (checksum, size) = meter.finish();
    if let Err(error) = source_result {
        for destination in destinations {
            let _ = s3::delete(destination.store.as_ref(), plan.key).await;
        }
        return Err(error);
    }
    let mut succeeded = Vec::new();
    let mut failed = Vec::new();
    for (id, result) in results {
        let name = destinations
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.name.clone())
            .unwrap_or_default();
        match result {
            Ok(bytes) => {
                log.line(format!("uploaded {bytes} bytes to {name}"));
                succeeded.push(id);
            }
            Err(error) => {
                log.line(format!("upload to {name} failed: {error}"));
                failed.push((id, error));
            }
        }
    }
    if plan.verify {
        let mut verified = Vec::new();
        for id in succeeded {
            let Some(destination) = destinations.iter().find(|d| d.id == id) else {
                continue;
            };
            let check = async {
                let stream = s3::download(destination.store.clone(), plan.key).await?;
                pipeline::sha256_of(stream).await
            }
            .await;
            match check {
                Ok((sum, len)) if sum == checksum && len == size => {
                    log.line(format!("verified {} (sha256 matches)", destination.name));
                    verified.push(id);
                }
                Ok((sum, _)) => {
                    log.line(format!(
                        "verify failed on {}: checksum {sum} differs",
                        destination.name
                    ));
                    failed.push((id, "checksum mismatch".into()));
                }
                Err(error) => {
                    log.line(format!("verify failed on {}: {error}", destination.name));
                    failed.push((id, error.to_string()));
                }
            }
        }
        succeeded = verified;
    }
    Ok(UploadOutcome {
        checksum,
        size,
        succeeded,
        failed,
    })
}

/// Download an object and undo encryption and compression.
pub async fn open_object(
    destination: &Destination,
    key: &str,
    meta: &RunMeta,
    passphrase: Option<&str>,
) -> Result<ByteStream, Error> {
    let mut stream = s3::download(destination.store.clone(), key).await?;
    if meta.encrypted {
        let passphrase = passphrase.ok_or_else(|| {
            Error::Validation("this backup is encrypted and the job has no passphrase".into())
        })?;
        stream = pipeline::decrypt(stream, passphrase);
    }
    let compression = Compression::parse(&meta.compression).unwrap_or(Compression::None);
    Ok(pipeline::decompress(stream, compression))
}

fn exec(command: ToolCommand) -> ExecSpec {
    ExecSpec {
        command: command.argv,
        env: command.env,
        user: None,
    }
}

/// Work that must happen after the stream finishes, even when it failed.
#[derive(Default)]
struct After {
    restart: Option<String>,
    post_hook: Option<(String, Vec<String>)>,
}

impl After {
    async fn run(self, driver: &dyn NodeDriver, log: &mut RunLog) -> Result<(), Error> {
        let mut failure = None;
        if let Some(container) = self.restart {
            match driver.start_container(&container).await {
                Ok(()) => log.line("service started again"),
                Err(error) => {
                    log.line(format!("could not start the service again: {error}"));
                    failure = Some(error);
                }
            }
        }
        if let Some((container, hook)) = self.post_hook {
            match driver.exec(&container, hook).await {
                Ok(output) if output.exit_code == 0 => log.line("post-hook finished"),
                Ok(output) => {
                    log.line(format!(
                        "post-hook exited {}: {}",
                        output.exit_code,
                        output.output.trim()
                    ));
                    failure.get_or_insert(Error::Node("post-hook failed".into()));
                }
                Err(error) => {
                    log.line(format!("post-hook failed: {error}"));
                    failure.get_or_insert(error);
                }
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

struct Prepared {
    stream: ByteStream,
    after: After,
    meta: RunMeta,
    extension: &'static str,
    service_name: String,
    engine: String,
}

async fn service_container(
    state: &AppState,
    service_id: Uuid,
) -> Result<(String, Option<String>, Value), ApiError> {
    let row = sqlx::query!(
        r#"SELECT name, current_container_id, spec FROM services WHERE id = $1"#,
        service_id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?
    .ok_or_else(|| not_found("service not found"))?;
    Ok((row.name, row.current_container_id, row.spec))
}

/// Value of `target_spec[key]` as a string.
fn spec_text(spec: &Value, key: &str) -> Option<String> {
    spec.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Source for volume, bind, and path jobs.
pub async fn archive_source(state: &AppState, job: &Job) -> Result<ArchiveSource, ApiError> {
    match job.target_kind.as_str() {
        "volume" => {
            let name = match spec_text(&job.target_spec, "volume") {
                Some(name) => name,
                None => {
                    let service_id = job
                        .service_id
                        .ok_or_else(|| bad("invalid", "volume jobs need target_spec.volume"))?;
                    load_database(state, service_id).await?.volume
                }
            };
            let source = ArchiveSource::Volume { name };
            source.validate().map_err(core_error)?;
            Ok(source)
        }
        "bind" => {
            let service_id = job
                .service_id
                .ok_or_else(|| bad("invalid", "bind jobs need a service"))?;
            let source = spec_text(&job.target_spec, "source")
                .ok_or_else(|| bad("invalid", "bind jobs need target_spec.source"))?;
            let (_, _, spec) = service_container(state, service_id).await?;
            let mounted = spec
                .get("volumes")
                .and_then(Value::as_array)
                .is_some_and(|volumes| {
                    volumes
                        .iter()
                        .any(|volume| volume.get("source").and_then(Value::as_str) == Some(&source))
                });
            if !mounted {
                return Err(bad(
                    "invalid",
                    "target_spec.source must be one of the service's bind mounts",
                ));
            }
            validate_host_path(&source).map_err(core_error)?;
            Ok(ArchiveSource::HostPath { path: source })
        }
        "path" => {
            let path = spec_text(&job.target_spec, "path")
                .ok_or_else(|| bad("invalid", "path jobs need target_spec.path"))?;
            validate_host_path(&path).map_err(core_error)?;
            Ok(ArchiveSource::HostPath { path })
        }
        _ => Err(bad("invalid", "not an archive job")),
    }
}

async fn prepare(
    state: &AppState,
    job: &Job,
    driver: &dyn NodeDriver,
    log: &mut RunLog,
) -> Result<Prepared, ApiError> {
    let mut meta = RunMeta {
        target_kind: job.target_kind.clone(),
        compression: job.compression.as_str().into(),
        encrypted: job.passphrase.is_some(),
        service_id: job.service_id,
        ..RunMeta::default()
    };
    if matches!(job.target_kind.as_str(), "panel_db" | "panel_settings") {
        let (stream, extension) = if job.target_kind == "panel_db" {
            let include_history = job
                .target_spec
                .get("include_history")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            log.line("dumping the panel database with pg_dump");
            let stream = crate::panel::archive(&state.pool, &state.config, include_history)
                .await
                .map_err(core_error)?;
            (stream, ".tar")
        } else {
            let export = crate::panel::settings_export(&state.pool).await?;
            let bytes = serde_json::to_vec_pretty(&export)
                .map_err(|_| bad("invalid", "could not serialize the settings export"))?;
            (
                Box::pin(futures_util::stream::once(async move {
                    Ok(bytes::Bytes::from(bytes))
                })) as ByteStream,
                ".json",
            )
        };
        return Ok(Prepared {
            stream,
            after: After::default(),
            meta,
            extension,
            service_name: String::new(),
            engine: "panel".into(),
        });
    }
    if job.target_kind == "database" {
        let service_id = job
            .service_id
            .ok_or_else(|| bad("invalid", "database jobs need a service"))?;
        let info = load_database(state, service_id).await?;
        for secret in info.secrets() {
            log.secret(secret);
        }
        let container = running_container(driver, &info).await?;
        meta.engine = Some(info.engine.as_str().into());
        meta.dump_all = job.tool_options.dump_all && info.engine == Engine::Postgres;
        let mut after = After::default();
        if !job.consistency.pre_hook.is_empty() {
            run_hook(
                driver,
                &container,
                &job.consistency.pre_hook,
                "pre-hook",
                log,
            )
            .await?;
        }
        if !job.consistency.post_hook.is_empty() {
            after.post_hook = Some((container.clone(), job.consistency.post_hook.clone()));
        }
        let databases = match info.engine {
            Engine::Mysql | Engine::Mariadb
                if !job.tool_options.include_databases.is_empty()
                    || !job.tool_options.exclude_databases.is_empty() =>
            {
                let output = pipeline::collect(
                    driver
                        .exec_read(
                            &container,
                            exec(mysql_list_databases_command(&info.credentials())),
                        )
                        .await
                        .map_err(core_error)?,
                )
                .await
                .map_err(core_error)?;
                let all: Vec<String> = String::from_utf8_lossy(&output)
                    .lines()
                    .map(|line| line.trim().to_owned())
                    .filter(|line| !line.is_empty())
                    .collect();
                let chosen = filter_databases(&all, &job.tool_options);
                log.line(format!("databases: {}", chosen.join(", ")));
                Some(chosen)
            }
            _ => None,
        };
        if info.engine == Engine::Redis {
            redis_snapshot(driver, &container, &info, log).await?;
        }
        let command = dump_command(&info.credentials(), &job.tool_options, databases.as_deref())
            .map_err(core_error)?;
        log.line(format!("running {}", command.argv.join(" ")));
        let stream = driver
            .exec_read(&container, exec(command))
            .await
            .map_err(core_error)?;
        let (service_name, _, _) = service_container(state, service_id).await?;
        return Ok(Prepared {
            stream,
            after,
            extension: dump_extension(info.engine, &job.tool_options),
            meta,
            service_name,
            engine: info.engine.as_str().into(),
        });
    }

    let source = archive_source(state, job).await?;
    meta.source = Some(source.clone());
    let mut after = After::default();
    let mut service_name = String::new();
    if let Some(service_id) = job.service_id {
        let (name, container, _) = service_container(state, service_id).await?;
        service_name = name;
        if let Some(container) = container {
            if !job.consistency.pre_hook.is_empty() {
                run_hook(
                    driver,
                    &container,
                    &job.consistency.pre_hook,
                    "pre-hook",
                    log,
                )
                .await?;
            }
            if !job.consistency.post_hook.is_empty() {
                after.post_hook = Some((container.clone(), job.consistency.post_hook.clone()));
            }
            if job.consistency.stop_service
                && driver.container_running(&container).await.unwrap_or(false)
            {
                driver
                    .stop_container(&container, 30)
                    .await
                    .map_err(core_error)?;
                log.line("service stopped for a consistent copy");
                after.restart = Some(container);
            }
        }
    }
    log.line(format!("reading {}", describe(&source)));
    let stream = match driver.archive_read(&source).await {
        Ok(stream) => stream,
        Err(error) => {
            let _ = after.run(driver, log).await;
            return Err(core_error(error));
        }
    };
    let filter = tarfilter::Filter::new(
        &job.tool_options.include_globs,
        &job.tool_options.exclude_globs,
    )
    .map_err(core_error)?;
    Ok(Prepared {
        stream: tarfilter::filter_tar(stream, filter),
        after,
        meta,
        extension: ".tar",
        service_name,
        engine: job.target_kind.clone(),
    })
}

fn describe(source: &ArchiveSource) -> String {
    match source {
        ArchiveSource::Volume { name } => format!("volume {name}"),
        ArchiveSource::HostPath { path } => format!("host path {path}"),
    }
}

async fn running_container(
    driver: &dyn NodeDriver,
    info: &DatabaseInfo,
) -> Result<String, ApiError> {
    let container = info
        .current_container_id
        .clone()
        .ok_or_else(|| conflict("not_running", "start the database before backing it up"))?;
    if !driver
        .container_running(&container)
        .await
        .map_err(core_error)?
    {
        return Err(conflict(
            "not_running",
            "the database container is not running",
        ));
    }
    Ok(container)
}

async fn run_hook(
    driver: &dyn NodeDriver,
    container: &str,
    hook: &[String],
    label: &str,
    log: &mut RunLog,
) -> Result<(), ApiError> {
    let output = driver
        .exec(container, hook.to_vec())
        .await
        .map_err(core_error)?;
    if output.exit_code != 0 {
        log.line(format!(
            "{label} exited {}: {}",
            output.exit_code,
            output.output.trim()
        ));
        return Err(bad("hook_failed", format!("{label} failed")));
    }
    log.line(format!("{label} finished"));
    Ok(())
}

async fn redis_snapshot(
    driver: &dyn NodeDriver,
    container: &str,
    info: &DatabaseInfo,
    log: &mut RunLog,
) -> Result<(), ApiError> {
    let read = |command: ToolCommand| async move {
        let bytes = pipeline::collect(driver.exec_read(container, exec(command)).await?).await?;
        Ok::<String, Error>(String::from_utf8_lossy(&bytes).trim().to_owned())
    };
    let status = || async {
        let text = read(redis_persistence_command(&info.password))
            .await
            .map_err(core_error)?;
        RedisPersistence::parse(&text).map_err(core_error)
    };
    let before = status().await?;
    let reply = read(redis_bgsave_command(&info.password))
        .await
        .map_err(core_error)?;
    log.line(format!("BGSAVE: {reply}"));
    if !reply.contains("started") && !reply.contains("in progress") && !reply.contains("scheduled")
    {
        return Err(bad("redis_error", format!("redis refused BGSAVE: {reply}")));
    }
    for _ in 0..240 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let now = status().await?;
        if now.in_progress || (reply.contains("scheduled") && now.last_save == before.last_save) {
            continue;
        }
        if !now.last_ok {
            return Err(bad("redis_error", "redis reported a failed BGSAVE"));
        }
        log.line("redis snapshot finished");
        return Ok(());
    }
    Err(bad(
        "timeout",
        "redis BGSAVE did not finish within 2 minutes",
    ))
}

async fn load_destinations(state: &AppState, ids: &[Uuid], log: &mut RunLog) -> Vec<Destination> {
    let mut out = Vec::new();
    for id in ids {
        match s3::load_destination(state, *id).await {
            Ok(destination) => out.push(destination),
            Err(error) => log.line(format!(
                "destination {id} skipped: {}",
                error.public_message()
            )),
        }
    }
    out
}

struct AttemptResult {
    key: String,
    outcome: UploadOutcome,
    meta: RunMeta,
}

async fn attempt(
    state: &AppState,
    job: &Job,
    driver: &dyn NodeDriver,
    log: &mut RunLog,
) -> Result<AttemptResult, String> {
    let destinations = load_destinations(state, &job.destination_ids, log).await;
    if destinations.is_empty() {
        return Err("no usable S3 destination".into());
    }
    let prepared = prepare(state, job, driver, log)
        .await
        .map_err(|error| error.public_message())?;
    let Prepared {
        stream,
        after,
        meta,
        extension,
        service_name,
        engine,
    } = prepared;
    let mut ext = format!("{extension}{}", job.compression.extension());
    if job.passphrase.is_some() {
        ext.push_str(".enc");
    }
    let key = match object_key(
        &job.path_prefix,
        &job.filename_template,
        &KeyVars {
            name: &job.name,
            service: if service_name.is_empty() {
                &job.name
            } else {
                &service_name
            },
            engine: &engine,
            unix: Utc::now().timestamp(),
        },
        &ext,
    ) {
        Ok(key) => key,
        Err(error) => {
            let _ = after.run(driver, log).await;
            return Err(error.to_string());
        }
    };
    log.line(format!("object key {key}"));
    let plan = UploadPlan {
        key: &key,
        compression: job.compression,
        level: job.compression_level,
        passphrase: job.passphrase.as_deref(),
        bandwidth_limit_bps: job.bandwidth_limit_bps.and_then(|v| u64::try_from(v).ok()),
        verify: job.verify_after_upload,
    };
    let result = upload_stream(stream, &plan, &destinations, log).await;
    let after_result = after.run(driver, log).await;
    let outcome = result.map_err(|error| error.to_string())?;
    if let Err(error) = after_result {
        return Err(format!("backup uploaded but cleanup failed: {error}"));
    }
    if outcome.succeeded.is_empty() {
        return Err("no destination accepted the backup".into());
    }
    Ok(AttemptResult { key, outcome, meta })
}

/// Shared limit of concurrent backup and restore runs on one server.
async fn slot(state: &AppState, server_id: Uuid) -> Arc<Semaphore> {
    let limit = sqlx::query_scalar!(
        r#"SELECT backup_concurrency FROM servers WHERE id = $1"#,
        server_id
    )
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
    .unwrap_or(2)
    .clamp(1, 16) as usize;
    let mut slots = state.backup_slots.lock().await;
    let entry = slots
        .entry(server_id)
        .or_insert_with(|| (limit, Arc::new(Semaphore::new(limit))));
    if entry.0 != limit {
        *entry = (limit, Arc::new(Semaphore::new(limit)));
    }
    entry.1.clone()
}

/// Queue a backup run and execute it in the background.
pub async fn start_run(state: &AppState, job_id: Uuid, trigger: &str) -> Result<Uuid, ApiError> {
    let job = load_job(state, job_id).await?;
    let run_id = sqlx::query_scalar!(
        r#"
        INSERT INTO backup_runs (job_id, status, kind, trigger)
        VALUES ($1, 'queued', 'backup', $2)
        RETURNING id
        "#,
        job.id,
        trigger
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let state = state.clone();
    tokio::spawn(async move {
        execute_run(&state, run_id, job).await;
    });
    Ok(run_id)
}

async fn set_running(state: &AppState, run_id: Uuid, log: &RunLog) {
    let _ = sqlx::query!(
        r#"UPDATE backup_runs SET status = 'running', started_at = now(), logs = $2 WHERE id = $1"#,
        run_id,
        log.text()
    )
    .execute(&state.pool)
    .await;
}

async fn fail_run(state: &AppState, run_id: Uuid, log: &RunLog, error: &str) {
    let _ = sqlx::query!(
        r#"
        UPDATE backup_runs
        SET status = 'failed', error = $2, logs = $3, finished_at = now()
        WHERE id = $1
        "#,
        run_id,
        error,
        log.text()
    )
    .execute(&state.pool)
    .await;
}

async fn execute_run(state: &AppState, run_id: Uuid, job: Job) {
    let mut log = RunLog::new();
    if let Some(passphrase) = &job.passphrase {
        log.secret(passphrase);
    }
    let server_id = match job_server(state, &job).await {
        Ok(id) => id,
        Err(error) => {
            fail_run(state, run_id, &log, &error.public_message()).await;
            return;
        }
    };
    let semaphore = slot(state, server_id).await;
    log.line("waiting for a backup slot");
    let Ok(_permit) = semaphore.acquire_owned().await else {
        fail_run(state, run_id, &log, "backup slots were closed").await;
        return;
    };
    set_running(state, run_id, &log).await;
    let driver = match driver_for(state, server_id).await {
        Ok(driver) => driver,
        Err(error) => {
            let message = error.public_message();
            log.line(&message);
            fail_run(state, run_id, &log, &message).await;
            notify_failure(state, &job, server_id, &message).await;
            return;
        }
    };
    let attempts = job.retries.clamp(0, 10) as u32 + 1;
    let timeout = Duration::from_secs(job.timeout_secs.clamp(10, 86_400) as u64);
    let mut last_error = String::new();
    for number in 1..=attempts {
        log.line(format!("attempt {number} of {attempts}"));
        let _ = sqlx::query!(
            r#"UPDATE backup_runs SET logs = $2 WHERE id = $1"#,
            run_id,
            log.text()
        )
        .execute(&state.pool)
        .await;
        match tokio::time::timeout(timeout, attempt(state, &job, driver.as_ref(), &mut log)).await {
            Ok(Ok(result)) => {
                finish_success(state, run_id, &job, server_id, result, &mut log).await;
                return;
            }
            Ok(Err(error)) => {
                log.line(format!("attempt failed: {error}"));
                last_error = error;
            }
            Err(_) => {
                log.line(format!("attempt timed out after {}s", timeout.as_secs()));
                last_error = format!("timed out after {}s", timeout.as_secs());
            }
        }
        if number < attempts {
            let wait = retry_backoff_secs(number);
            log.line(format!("retrying in {wait}s"));
            tokio::time::sleep(Duration::from_secs(wait)).await;
        }
    }
    let refs: Vec<String> = log.secrets.clone();
    let refs: Vec<&str> = refs.iter().map(String::as_str).collect();
    let clean = rudder_core::redact(&last_error, &refs);
    fail_run(state, run_id, &log, &clean).await;
    notify_failure(state, &job, server_id, &clean).await;
}

async fn notify_failure(state: &AppState, job: &Job, server_id: Uuid, error: &str) {
    let mut event = Event::critical("backup.failed", format!("Backup '{}' failed", job.name))
        .message(error.to_owned())
        .server(server_id)
        .payload(json!({ "job_id": job.id }));
    if let Some(service_id) = job.service_id {
        event = event.service(service_id);
    }
    events::record(&state.pool, event).await;
}

async fn finish_success(
    state: &AppState,
    run_id: Uuid,
    job: &Job,
    server_id: Uuid,
    result: AttemptResult,
    log: &mut RunLog,
) {
    let status = if result.outcome.failed.is_empty() {
        "succeeded"
    } else {
        "partial"
    };
    log.line(format!(
        "{status}: {} bytes, sha256 {}",
        result.outcome.size, result.outcome.checksum
    ));
    let meta = serde_json::to_value(&result.meta).unwrap_or_else(|_| json!({}));
    let _ = sqlx::query!(
        r#"
        UPDATE backup_runs
        SET status = $2, finished_at = now(), size_bytes = $3, checksum = $4,
            object_keys = $5, destination_ids = $6, meta = $7, logs = $8, error = $9
        WHERE id = $1
        "#,
        run_id,
        status,
        i64::try_from(result.outcome.size).unwrap_or(i64::MAX),
        result.outcome.checksum,
        &[result.key.clone()],
        &result.outcome.succeeded,
        meta,
        log.text(),
        if status == "partial" {
            Some("some destinations failed")
        } else {
            None
        }
    )
    .execute(&state.pool)
    .await;
    if let Err(error) = apply_retention(state, job, log).await {
        log.line(format!("retention failed: {error}"));
    }
    let _ = sqlx::query!(
        r#"UPDATE backup_runs SET logs = $2 WHERE id = $1"#,
        run_id,
        log.text()
    )
    .execute(&state.pool)
    .await;
    if status == "partial" {
        events::record(
            &state.pool,
            Event::warning(
                "backup.partial",
                format!("Backup '{}' reached only some destinations", job.name),
            )
            .server(server_id)
            .payload(json!({ "job_id": job.id, "run_id": run_id })),
        )
        .await;
    } else if job.notify_on_success {
        events::record(
            &state.pool,
            Event::info(
                "backup.succeeded",
                format!("Backup '{}' succeeded", job.name),
            )
            .server(server_id)
            .payload(json!({ "job_id": job.id, "run_id": run_id, "size": result.outcome.size })),
        )
        .await;
    }
}

/// Delete objects the retention policy no longer wants and mark their runs expired.
pub async fn apply_retention(
    state: &AppState,
    job: &Job,
    log: &mut RunLog,
) -> Result<usize, String> {
    let rows = sqlx::query!(
        r#"
        SELECT id, started_at, object_keys, destination_ids
        FROM backup_runs
        WHERE job_id = $1 AND kind = 'backup' AND status IN ('succeeded', 'partial')
        "#,
        job.id
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|error| error.to_string())?;
    let points: Vec<BackupPoint> = rows
        .iter()
        .map(|row| BackupPoint {
            id: row.id.to_string(),
            created_unix: row.started_at.timestamp(),
        })
        .collect();
    let expired = expired_ids(&points, &job.retention.into(), Utc::now().timestamp());
    if expired.is_empty() {
        return Ok(0);
    }
    let mut stores: HashMap<Uuid, Option<Destination>> = HashMap::new();
    let mut removed = 0;
    for row in rows
        .iter()
        .filter(|row| expired.contains(&row.id.to_string()))
    {
        let mut ok = true;
        for destination_id in &row.destination_ids {
            if !stores.contains_key(destination_id) {
                let loaded = s3::load_destination(state, *destination_id).await.ok();
                stores.insert(*destination_id, loaded);
            }
            let Some(Some(destination)) = stores.get(destination_id) else {
                continue;
            };
            for key in &row.object_keys {
                if let Err(error) = s3::delete(destination.store.as_ref(), key).await {
                    log.line(format!(
                        "could not delete {key} from {}: {error}",
                        destination.name
                    ));
                    ok = false;
                }
            }
        }
        if ok {
            let _ = sqlx::query!(
                r#"UPDATE backup_runs SET status = 'expired' WHERE id = $1"#,
                row.id
            )
            .execute(&state.pool)
            .await;
            removed += 1;
        }
    }
    log.line(format!("retention removed {removed} old backup(s)"));
    Ok(removed)
}

/// Where a restore writes.
#[derive(Debug, Clone, Default, Deserialize, utoipa::ToSchema)]
pub struct RestoreRequest {
    /// Database or service to restore into. Defaults to the job's service.
    pub target_service_id: Option<Uuid>,
    /// Volume name for volume backups. Defaults to the original volume.
    pub target_volume: Option<String>,
    /// Host path for bind and path backups. Defaults to the original path.
    pub target_path: Option<String>,
}

pub struct SourceRun {
    pub job_id: Uuid,
    pub key: String,
    pub destination_ids: Vec<Uuid>,
    pub meta: RunMeta,
}

pub async fn load_source_run(state: &AppState, run_id: Uuid) -> Result<SourceRun, ApiError> {
    let row = sqlx::query!(
        r#"
        SELECT job_id, status, kind, object_keys, destination_ids, meta
        FROM backup_runs WHERE id = $1
        "#,
        run_id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?
    .ok_or_else(|| not_found("backup run not found"))?;
    if row.kind != "backup" || !matches!(row.status.as_str(), "succeeded" | "partial") {
        return Err(bad(
            "not_restorable",
            "only a successful backup can be used",
        ));
    }
    let key = row
        .object_keys
        .first()
        .cloned()
        .ok_or_else(|| bad("not_restorable", "the run has no object"))?;
    Ok(SourceRun {
        job_id: row.job_id,
        key,
        destination_ids: row.destination_ids,
        meta: serde_json::from_value(row.meta).unwrap_or_default(),
    })
}

/// First destination of a run that can still be opened.
pub async fn first_destination(state: &AppState, ids: &[Uuid]) -> Result<Destination, ApiError> {
    for id in ids {
        if let Ok(destination) = s3::load_destination(state, *id).await {
            return Ok(destination);
        }
    }
    Err(conflict(
        "no_destination",
        "none of the run's destinations is available",
    ))
}

pub async fn start_restore(
    state: &AppState,
    source_run_id: Uuid,
    request: RestoreRequest,
) -> Result<Uuid, ApiError> {
    let source = load_source_run(state, source_run_id).await?;
    if source.meta.target_kind.starts_with("panel_") {
        return Err(conflict(
            "offline_restore",
            "panel backups are restored before the panel starts: download the backup and run `install.sh --restore <file>`",
        ));
    }
    let job = load_job(state, source.job_id).await?;
    if let Some(volume) = &request.target_volume {
        ArchiveSource::Volume {
            name: volume.clone(),
        }
        .validate()
        .map_err(core_error)?;
    }
    if let Some(path) = &request.target_path {
        validate_host_path(path).map_err(core_error)?;
    }
    let meta = RunMeta {
        restore_of: Some(source_run_id),
        restore_target: Some(
            request
                .target_service_id
                .map(|id| format!("service {id}"))
                .or_else(|| request.target_volume.clone().map(|v| format!("volume {v}")))
                .or_else(|| request.target_path.clone().map(|p| format!("path {p}")))
                .unwrap_or_else(|| "original".into()),
        ),
        ..source.meta.clone()
    };
    let run_id = sqlx::query_scalar!(
        r#"
        INSERT INTO backup_runs (job_id, status, kind, trigger, meta)
        VALUES ($1, 'queued', 'restore', 'manual', $2)
        RETURNING id
        "#,
        job.id,
        serde_json::to_value(&meta).unwrap_or_else(|_| json!({}))
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let state = state.clone();
    tokio::spawn(async move {
        execute_restore(&state, run_id, job, source, request).await;
    });
    Ok(run_id)
}

async fn execute_restore(
    state: &AppState,
    run_id: Uuid,
    job: Job,
    source: SourceRun,
    request: RestoreRequest,
) {
    let mut log = RunLog::new();
    if let Some(passphrase) = &job.passphrase {
        log.secret(passphrase);
    }
    let result = restore_inner(state, run_id, &job, &source, &request, &mut log).await;
    match result {
        Ok(()) => {
            log.line("restore finished");
            let _ = sqlx::query!(
                r#"
                UPDATE backup_runs
                SET status = 'succeeded', finished_at = now(), logs = $2
                WHERE id = $1
                "#,
                run_id,
                log.text()
            )
            .execute(&state.pool)
            .await;
            events::record(
                &state.pool,
                Event::info(
                    "backup.restored",
                    format!("Restore from '{}' finished", job.name),
                )
                .payload(json!({ "job_id": job.id, "run_id": run_id })),
            )
            .await;
        }
        Err(error) => {
            log.line(format!("restore failed: {error}"));
            fail_run(state, run_id, &log, &error).await;
            events::record(
                &state.pool,
                Event::critical(
                    "restore.failed",
                    format!("Restore from '{}' failed", job.name),
                )
                .message(error)
                .payload(json!({ "job_id": job.id, "run_id": run_id })),
            )
            .await;
        }
    }
}

async fn restore_inner(
    state: &AppState,
    run_id: Uuid,
    job: &Job,
    source: &SourceRun,
    request: &RestoreRequest,
    log: &mut RunLog,
) -> Result<(), String> {
    let text = |error: ApiError| error.public_message();
    let target_service = request.target_service_id.or(job.service_id);
    let server_id = match target_service {
        Some(service_id) => sqlx::query_scalar!(
            r#"SELECT server_id FROM services WHERE id = $1"#,
            service_id
        )
        .fetch_optional(&state.pool)
        .await
        .map_err(|error| error.to_string())?
        .ok_or("target service not found")?,
        None => job_server(state, job).await.map_err(text)?,
    };
    let semaphore = slot(state, server_id).await;
    let _permit = semaphore
        .acquire_owned()
        .await
        .map_err(|_| "backup slots were closed".to_owned())?;
    set_running(state, run_id, log).await;
    let driver = driver_for(state, server_id).await.map_err(text)?;
    let destination = first_destination(state, &source.destination_ids)
        .await
        .map_err(text)?;
    log.line(format!(
        "downloading {} from {}",
        source.key, destination.name
    ));
    let stream = open_object(
        &destination,
        &source.key,
        &source.meta,
        job.passphrase.as_deref(),
    )
    .await
    .map_err(|error| error.to_string())?;
    match source.meta.target_kind.as_str() {
        "database" => {
            let service_id = target_service.ok_or("choose a database to restore into")?;
            let info = load_database(state, service_id).await.map_err(text)?;
            for secret in info.secrets() {
                log.secret(secret);
            }
            let engine = source.meta.engine.as_deref().unwrap_or_default();
            if info.engine.as_str() != engine {
                return Err(format!(
                    "a {engine} backup cannot be restored into a {} database",
                    info.engine.as_str()
                ));
            }
            let container = running_container(driver.as_ref(), &info)
                .await
                .map_err(text)?;
            if info.engine == Engine::Redis {
                driver
                    .stop_container(&container, 30)
                    .await
                    .map_err(|error| error.to_string())?;
                log.line("redis stopped; replacing dump.rdb and appendonlydir");
                let written = driver
                    .archive_write(
                        &ArchiveSource::Volume {
                            name: info.volume.clone(),
                        },
                        stream,
                    )
                    .await;
                let started = driver.start_container(&container).await;
                written.map_err(|error| error.to_string())?;
                started.map_err(|error| error.to_string())?;
                log.line("redis started");
                return Ok(());
            }
            let command = restore_command(&info.credentials(), source.meta.dump_all)
                .map_err(|error| error.to_string())?;
            log.line(format!("running {}", command.argv.join(" ")));
            let output = driver
                .exec_write(&container, exec(command), stream)
                .await
                .map_err(|error| error.to_string())?;
            let tail: String = output
                .output
                .chars()
                .rev()
                .take(2000)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            if !tail.trim().is_empty() {
                log.line(tail.trim());
            }
            Ok(())
        }
        "volume" | "bind" | "path" => {
            let original = source
                .meta
                .source
                .clone()
                .ok_or("the backup does not record its source")?;
            let target = match (&request.target_volume, &request.target_path, original) {
                (Some(volume), _, _) => ArchiveSource::Volume {
                    name: volume.clone(),
                },
                (_, Some(path), _) => ArchiveSource::HostPath { path: path.clone() },
                (None, None, original) => original,
            };
            let mut restart = None;
            if let Some(service_id) = target_service.filter(|_| job.consistency.stop_service) {
                if let Ok((_, Some(container), _)) = service_container(state, service_id).await {
                    if driver.container_running(&container).await.unwrap_or(false) {
                        driver
                            .stop_container(&container, 30)
                            .await
                            .map_err(|error| error.to_string())?;
                        log.line("service stopped for the restore");
                        restart = Some(container);
                    }
                }
            }
            log.line(format!("extracting into {}", describe(&target)));
            let written = driver.archive_write(&target, stream).await;
            if let Some(container) = restart {
                if let Err(error) = driver.start_container(&container).await {
                    log.line(format!("service did not start again: {error}"));
                }
            }
            written.map_err(|error| error.to_string())
        }
        other => Err(format!("{other} backups cannot be restored here")),
    }
}

/// Start runs for jobs whose schedule is due. Returns how many started.
pub async fn schedule_once(state: &AppState) -> Result<usize, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT j.id, j.schedule_cron AS "schedule_cron!", j.timezone, j.created_at,
               (SELECT MAX(r.created_at) FROM backup_runs r
                WHERE r.job_id = j.id AND r.kind = 'backup') AS "last_run: DateTime<Utc>",
               EXISTS (SELECT 1 FROM backup_runs r
                       WHERE r.job_id = j.id AND r.status IN ('queued', 'running')) AS "busy!"
        FROM backup_jobs j
        WHERE j.enabled AND j.schedule_cron IS NOT NULL AND j.schedule_cron <> ''
        "#
    )
    .fetch_all(&state.pool)
    .await?;
    let now = Utc::now();
    let mut started = 0;
    for row in rows {
        if row.busy {
            continue;
        }
        let anchor = row
            .last_run
            .map_or(row.created_at, |last| last.max(row.created_at));
        match is_due(&row.schedule_cron, &row.timezone, anchor, now) {
            Ok(true) => match start_run(state, row.id, "schedule").await {
                Ok(_) => started += 1,
                Err(error) => {
                    tracing::warn!(job_id = %row.id, message = %error.public_message(), "scheduled backup did not start")
                }
            },
            Ok(false) => {}
            Err(error) => tracing::warn!(job_id = %row.id, %error, "backup schedule is invalid"),
        }
    }
    Ok(started)
}

/// Next fire time, for display.
pub fn next_run(
    cron: Option<&str>,
    timezone: &str,
    anchor: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let cron = cron.filter(|c| !c.is_empty())?;
    rudder_core::next_after(
        cron,
        timezone,
        anchor.max(Utc::now() - chrono::Duration::seconds(1)),
    )
    .ok()
}

#[cfg(test)]
mod tests;
