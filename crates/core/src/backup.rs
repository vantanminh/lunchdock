//! Backup job rules that do not touch Docker, S3, or the database.
//!
//! Dump commands run inside the database container with `docker exec`, so the
//! tool version always matches the server. Every command is an argv vector;
//! passwords travel in the exec environment, never on the command line.

use serde::{Deserialize, Serialize};

use crate::{Engine, Error, RetentionPolicy};

/// Compression applied before upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    None,
    Gzip,
    Zstd,
}

impl Compression {
    pub fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "none" => Ok(Self::None),
            "gzip" => Ok(Self::Gzip),
            "zstd" => Ok(Self::Zstd),
            _ => Err(Error::Validation(
                "compression must be none, gzip, or zstd".into(),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Gzip => "gzip",
            Self::Zstd => "zstd",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Gzip => ".gz",
            Self::Zstd => ".zst",
        }
    }

    /// Clamp a user level into the range the codec accepts.
    pub fn validate_level(self, level: i32) -> Result<i32, Error> {
        let (min, max) = match self {
            Self::None => return Ok(0),
            Self::Gzip => (1, 9),
            Self::Zstd => (1, 19),
        };
        if (min..=max).contains(&level) {
            Ok(level)
        } else {
            Err(Error::Validation(format!(
                "{} level must be {min} to {max}",
                self.as_str()
            )))
        }
    }
}

/// Engine-specific dump options stored in `backup_jobs.tool_options`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolOptions {
    /// Postgres: use `pg_dumpall` (plain SQL of every database) instead of `pg_dump -Fc`.
    pub dump_all: bool,
    pub include_databases: Vec<String>,
    pub exclude_databases: Vec<String>,
    pub include_tables: Vec<String>,
    pub exclude_tables: Vec<String>,
    /// Extra argv tokens appended to the dump command. See [`validate_extra_args`].
    pub extra_args: Vec<String>,
    /// Glob filters for volume, bind, and path backups, relative to the root.
    pub include_globs: Vec<String>,
    pub exclude_globs: Vec<String>,
}

/// Stop the service and run hooks around a backup. Stored in `consistency_spec`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Consistency {
    pub stop_service: bool,
    pub pre_hook: Vec<String>,
    pub post_hook: Vec<String>,
}

impl Consistency {
    pub fn validate(&self) -> Result<(), Error> {
        for hook in [&self.pre_hook, &self.post_hook] {
            if hook.len() > 64
                || hook
                    .iter()
                    .any(|arg| arg.contains('\0') || arg.len() > 4096)
            {
                return Err(Error::Validation("invalid hook command".into()));
            }
        }
        Ok(())
    }
}

/// Database credentials needed by the dump tools. Never logged.
#[derive(Clone)]
pub struct DumpCredentials {
    pub engine: Engine,
    pub database: String,
    pub username: String,
    pub password: String,
    pub root_password: Option<String>,
}

impl std::fmt::Debug for DumpCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DumpCredentials")
            .field("engine", &self.engine)
            .field("database", &self.database)
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

/// A command plus the environment it needs inside the container.
#[derive(Clone, PartialEq, Eq)]
pub struct ToolCommand {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl std::fmt::Debug for ToolCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let keys: Vec<&str> = self.env.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("ToolCommand")
            .field("argv", &self.argv)
            .field("env_keys", &keys)
            .finish()
    }
}

/// File extension of the raw dump, before compression and encryption.
pub fn dump_extension(engine: Engine, options: &ToolOptions) -> &'static str {
    match engine {
        Engine::Postgres if options.dump_all => ".sql",
        Engine::Postgres => ".dump",
        Engine::Mysql | Engine::Mariadb => ".sql",
        Engine::Mongo => ".archive",
        Engine::Redis => ".tar",
    }
}

/// Flags that would redirect output, read secrets, or change the transport.
const DENIED_FLAGS: &[&str] = &[
    "-f",
    "--file",
    "-o",
    "--out",
    "--output",
    "-r",
    "--result-file",
    "--archive",
    "-p",
    "--password",
    "-W",
    "--host",
    "-h",
    "--uri",
    "--port",
    "--socket",
    "-S",
    "--defaults-file",
    "--defaults-extra-file",
    "--config",
    "--tab",
    "-T",
    "--plugin-dir",
];

/// Extra argv tokens must look like flags, carry safe values, and avoid
/// [`DENIED_FLAGS`]. They are appended as separate argv elements.
pub fn validate_extra_args(args: &[String]) -> Result<(), Error> {
    if args.len() > 32 {
        return Err(Error::Validation("at most 32 extra arguments".into()));
    }
    for arg in args {
        let (flag, value) = match arg.split_once('=') {
            Some((flag, value)) => (flag, Some(value)),
            None => (arg.as_str(), None),
        };
        let body = flag
            .strip_prefix("--")
            .or_else(|| flag.strip_prefix('-').filter(|rest| rest.len() == 1));
        let flag_ok = body.is_some_and(|body| {
            body.bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphanumeric())
                && body
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        });
        let value_ok = value.is_none_or(|value| {
            value.len() <= 256
                && value.bytes().all(|b| {
                    b.is_ascii_alphanumeric()
                        || matches!(
                            b,
                            b'_' | b'-' | b'.' | b',' | b':' | b'@' | b'%' | b'+' | b'*'
                        )
                })
        });
        if !flag_ok || !value_ok {
            return Err(Error::Validation(format!("invalid extra argument '{arg}'")));
        }
        if DENIED_FLAGS.contains(&flag) {
            return Err(Error::Validation(format!(
                "extra argument '{flag}' is not allowed"
            )));
        }
    }
    Ok(())
}

/// Database and table names in filters: letters, digits, `_`, `-`, `.`, `*`.
fn validate_filter(values: &[String], label: &str) -> Result<(), Error> {
    for value in values {
        let ok = !value.is_empty()
            && value.len() <= 128
            && value.bytes().all(|b| {
                b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'*' | b'$')
            })
            && !value.starts_with('-');
        if !ok {
            return Err(Error::Validation(format!("invalid {label} '{value}'")));
        }
    }
    Ok(())
}

pub fn validate_tool_options(options: &ToolOptions) -> Result<(), Error> {
    validate_extra_args(&options.extra_args)?;
    validate_filter(&options.include_databases, "database")?;
    validate_filter(&options.exclude_databases, "database")?;
    validate_filter(&options.include_tables, "table")?;
    validate_filter(&options.exclude_tables, "table")?;
    for glob in options.include_globs.iter().chain(&options.exclude_globs) {
        if glob.is_empty() || glob.len() > 512 || glob.contains('\0') || glob.starts_with('/') {
            return Err(Error::Validation(format!(
                "glob '{glob}' must be relative and non-empty"
            )));
        }
    }
    Ok(())
}

/// Dump command for one database service. `databases` is the resolved list of
/// MySQL/MariaDB databases when include or exclude filters are set.
pub fn dump_command(
    creds: &DumpCredentials,
    options: &ToolOptions,
    databases: Option<&[String]>,
) -> Result<ToolCommand, Error> {
    validate_tool_options(options)?;
    let mut argv: Vec<String> = Vec::new();
    let mut env = Vec::new();
    match creds.engine {
        Engine::Postgres => {
            env.push(("PGPASSWORD".to_owned(), creds.password.clone()));
            if options.dump_all {
                argv.extend(["pg_dumpall".into(), "-U".into(), creds.username.clone()]);
                for db in &options.exclude_databases {
                    argv.push(format!("--exclude-database={db}"));
                }
            } else {
                let database = options
                    .include_databases
                    .first()
                    .cloned()
                    .unwrap_or_else(|| creds.database.clone());
                argv.extend([
                    "pg_dump".into(),
                    "-U".into(),
                    creds.username.clone(),
                    "-d".into(),
                    database,
                    "-Fc".into(),
                ]);
                for table in &options.include_tables {
                    argv.push(format!("--table={table}"));
                }
                for table in &options.exclude_tables {
                    argv.push(format!("--exclude-table={table}"));
                }
            }
        }
        Engine::Mysql | Engine::Mariadb => {
            let tool = if creds.engine == Engine::Mariadb {
                "mariadb-dump"
            } else {
                "mysqldump"
            };
            env.push((
                "MYSQL_PWD".to_owned(),
                creds
                    .root_password
                    .clone()
                    .unwrap_or_else(|| creds.password.clone()),
            ));
            let user = if creds.root_password.is_some() {
                "root".to_owned()
            } else {
                creds.username.clone()
            };
            argv.extend([
                tool.into(),
                "-u".into(),
                user,
                "--single-transaction".into(),
                "--routines".into(),
                "--triggers".into(),
            ]);
            for table in &options.exclude_tables {
                if !table.contains('.') {
                    return Err(Error::Validation(
                        "MySQL table excludes must be written as database.table".into(),
                    ));
                }
                argv.push(format!("--ignore-table={table}"));
            }
            match databases {
                Some(list) if !list.is_empty() => {
                    argv.push("--databases".into());
                    argv.extend(list.iter().cloned());
                }
                Some(_) => return Err(Error::Validation("no databases left to back up".into())),
                None => argv.push("--all-databases".into()),
            }
        }
        Engine::Mongo => {
            env.push(("MONGO_PASSWORD".to_owned(), creds.password.clone()));
            argv.extend([
                "mongodump".into(),
                "--archive".into(),
                "--authenticationDatabase=admin".into(),
                format!("--username={}", creds.username),
                "--quiet".into(),
            ]);
            // mongodump reads the password from stdin-less flags only, so the
            // wrapper below passes it via the environment.
            if let Some(db) = options.include_databases.first() {
                argv.push(format!("--db={db}"));
                if let Some(collection) = options.include_tables.first() {
                    argv.push(format!("--collection={collection}"));
                }
                for collection in &options.exclude_tables {
                    argv.push(format!("--excludeCollection={collection}"));
                }
            }
            argv = mongo_password_wrapper(argv);
        }
        Engine::Redis => {
            argv.extend([
                "tar".into(),
                "-C".into(),
                "/data".into(),
                "-cf".into(),
                "-".into(),
                "--ignore-failed-read".into(),
                "dump.rdb".into(),
                "appendonlydir".into(),
            ]);
            return Ok(ToolCommand { argv, env });
        }
    }
    argv.extend(options.extra_args.iter().cloned());
    Ok(ToolCommand { argv, env })
}

/// mongodump has no password environment variable. `sh -c` with a constant
/// script expands `$MONGO_PASSWORD` from the exec environment; user values
/// stay positional arguments (`"$@"`), so nothing is interpolated into the script.
fn mongo_password_wrapper(argv: Vec<String>) -> Vec<String> {
    let mut wrapped = vec![
        "sh".to_owned(),
        "-c".to_owned(),
        "exec \"$@\" --password=\"$MONGO_PASSWORD\"".to_owned(),
        "rudder".to_owned(),
    ];
    wrapped.extend(argv);
    wrapped
}

/// Redis: trigger a background save, then poll [`redis_persistence_command`].
pub fn redis_bgsave_command(password: &str) -> ToolCommand {
    ToolCommand {
        argv: vec!["redis-cli".into(), "BGSAVE".into()],
        env: vec![("REDISCLI_AUTH".into(), password.to_owned())],
    }
}

/// `INFO persistence`, parsed by [`RedisPersistence::parse`].
pub fn redis_persistence_command(password: &str) -> ToolCommand {
    ToolCommand {
        argv: vec!["redis-cli".into(), "INFO".into(), "persistence".into()],
        env: vec![("REDISCLI_AUTH".into(), password.to_owned())],
    }
}

/// The fields of `INFO persistence` that decide whether a BGSAVE finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedisPersistence {
    pub in_progress: bool,
    pub last_ok: bool,
    /// `rdb_last_save_time`, which only has one-second resolution.
    pub last_save: i64,
}

impl RedisPersistence {
    pub fn parse(info: &str) -> Result<Self, Error> {
        let mut in_progress = None;
        let mut last_ok = None;
        let mut last_save = None;
        for line in info.lines() {
            let Some((key, value)) = line.trim().split_once(':') else {
                continue;
            };
            match key {
                "rdb_bgsave_in_progress" => in_progress = Some(value == "1"),
                "rdb_last_bgsave_status" => last_ok = Some(value == "ok"),
                "rdb_last_save_time" => last_save = value.parse().ok(),
                _ => {}
            }
        }
        match (in_progress, last_ok, last_save) {
            (Some(in_progress), Some(last_ok), Some(last_save)) => Ok(Self {
                in_progress,
                last_ok,
                last_save,
            }),
            _ => Err(Error::Node(format!(
                "unexpected redis INFO reply: {}",
                info.lines().next().unwrap_or_default()
            ))),
        }
    }
}

/// MySQL database list, used to apply include/exclude filters.
pub fn mysql_list_databases_command(creds: &DumpCredentials) -> ToolCommand {
    let client = if creds.engine == Engine::Mariadb {
        "mariadb"
    } else {
        "mysql"
    };
    ToolCommand {
        argv: vec![
            client.into(),
            "-u".into(),
            if creds.root_password.is_some() {
                "root".into()
            } else {
                creds.username.clone()
            },
            "-N".into(),
            "-B".into(),
            "-e".into(),
            "SHOW DATABASES".into(),
        ],
        env: vec![(
            "MYSQL_PWD".into(),
            creds
                .root_password
                .clone()
                .unwrap_or_else(|| creds.password.clone()),
        )],
    }
}

/// Apply include and exclude filters to the server's database list. System
/// schemas are skipped unless explicitly included.
pub fn filter_databases(all: &[String], options: &ToolOptions) -> Vec<String> {
    const SYSTEM: &[&str] = &["information_schema", "performance_schema", "sys", "mysql"];
    all.iter()
        .filter(|db| {
            if !options.include_databases.is_empty() {
                options.include_databases.iter().any(|item| item == *db)
            } else {
                !SYSTEM.contains(&db.as_str())
            }
        })
        .filter(|db| !options.exclude_databases.iter().any(|item| item == *db))
        .cloned()
        .collect()
}

/// Restore command fed the decompressed dump on stdin.
pub fn restore_command(creds: &DumpCredentials, dump_all: bool) -> Result<ToolCommand, Error> {
    let command = match creds.engine {
        Engine::Postgres if dump_all => ToolCommand {
            argv: vec![
                "psql".into(),
                "-U".into(),
                creds.username.clone(),
                "-d".into(),
                "postgres".into(),
                "-v".into(),
                "ON_ERROR_STOP=0".into(),
            ],
            env: vec![("PGPASSWORD".into(), creds.password.clone())],
        },
        Engine::Postgres => ToolCommand {
            argv: vec![
                "pg_restore".into(),
                "-U".into(),
                creds.username.clone(),
                "-d".into(),
                creds.database.clone(),
                "--clean".into(),
                "--if-exists".into(),
                "--no-owner".into(),
            ],
            env: vec![("PGPASSWORD".into(), creds.password.clone())],
        },
        Engine::Mysql | Engine::Mariadb => ToolCommand {
            argv: vec![
                if creds.engine == Engine::Mariadb {
                    "mariadb".into()
                } else {
                    "mysql".into()
                },
                "-u".into(),
                if creds.root_password.is_some() {
                    "root".into()
                } else {
                    creds.username.clone()
                },
            ],
            env: vec![(
                "MYSQL_PWD".into(),
                creds
                    .root_password
                    .clone()
                    .unwrap_or_else(|| creds.password.clone()),
            )],
        },
        Engine::Mongo => ToolCommand {
            argv: mongo_password_wrapper(vec![
                "mongorestore".into(),
                "--archive".into(),
                "--drop".into(),
                "--authenticationDatabase=admin".into(),
                format!("--username={}", creds.username),
                "--quiet".into(),
            ]),
            env: vec![("MONGO_PASSWORD".into(), creds.password.clone())],
        },
        Engine::Redis => {
            return Err(Error::Validation(
                "redis restores replace the data volume; use the volume restore path".into(),
            ))
        }
    };
    Ok(command)
}

/// Variables a filename template may use.
pub struct KeyVars<'a> {
    pub name: &'a str,
    pub service: &'a str,
    pub engine: &'a str,
    pub unix: i64,
}

/// Build the object key: `prefix/template + extension`. Templates may use
/// `{name}`, `{service}`, `{engine}`, `{timestamp}` (`YYYYMMDDTHHMMSSZ`),
/// `{date}` (`YYYY-MM-DD`), and `{unix}`.
pub fn object_key(
    prefix: &str,
    template: &str,
    vars: &KeyVars<'_>,
    extension: &str,
) -> Result<String, Error> {
    validate_prefix(prefix)?;
    let time = chrono::DateTime::from_timestamp(vars.unix, 0)
        .ok_or_else(|| Error::Validation("invalid timestamp".into()))?;
    let mut name = template.to_owned();
    for (token, value) in [
        ("{name}", vars.name.to_owned()),
        ("{service}", vars.service.to_owned()),
        ("{engine}", vars.engine.to_owned()),
        ("{timestamp}", time.format("%Y%m%dT%H%M%SZ").to_string()),
        ("{date}", time.format("%Y-%m-%d").to_string()),
        ("{unix}", vars.unix.to_string()),
    ] {
        name = name.replace(token, &value);
    }
    if name.contains('{') || name.contains('}') {
        return Err(Error::Validation(
            "filename template has an unknown {placeholder}".into(),
        ));
    }
    if name.is_empty()
        || name.len() > 200
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/'))
        || name.split('/').any(|part| part.is_empty() || part == "..")
    {
        return Err(Error::Validation(
            "filename template must produce letters, digits, '-', '_', '.', and '/'".into(),
        ));
    }
    let prefix = prefix.trim_matches('/');
    Ok(if prefix.is_empty() {
        format!("{name}{extension}")
    } else {
        format!("{prefix}/{name}{extension}")
    })
}

pub fn validate_prefix(prefix: &str) -> Result<(), Error> {
    let ok = prefix.len() <= 200
        && prefix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/'))
        && !prefix.split('/').any(|part| part == "..");
    if ok {
        Ok(())
    } else {
        Err(Error::Validation("invalid path prefix".into()))
    }
}

/// `backup_jobs.retention_spec` as stored JSON.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RetentionSpec {
    pub keep_last: Option<u32>,
    pub keep_days: Option<u32>,
    pub daily: Option<u32>,
    pub weekly: Option<u32>,
    pub monthly: Option<u32>,
}

impl From<RetentionSpec> for RetentionPolicy {
    fn from(spec: RetentionSpec) -> Self {
        RetentionPolicy {
            keep_last: spec.keep_last,
            keep_days: spec.keep_days,
            daily: spec.daily,
            weekly: spec.weekly,
            monthly: spec.monthly,
        }
    }
}

/// Exponential backoff for retry `attempt` (1-based): 5s, 10s, 20s... capped at 5 min.
pub fn retry_backoff_secs(attempt: u32) -> u64 {
    let exp = attempt.saturating_sub(1).min(16);
    (5u64 << exp).min(300)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds(engine: Engine) -> DumpCredentials {
        DumpCredentials {
            engine,
            database: "app".into(),
            username: "app".into(),
            password: "pw-secret-value-1234".into(),
            root_password: Some("root-secret-value-1234".into()),
        }
    }

    fn no_secret(cmd: &ToolCommand) {
        for arg in &cmd.argv {
            assert!(!arg.contains("secret-value"), "secret on argv: {arg}");
        }
        assert!(!format!("{cmd:?}").contains("secret-value"));
    }

    #[test]
    fn postgres_dump_is_custom_format_and_password_stays_in_env() {
        let mut options = ToolOptions {
            include_tables: vec!["public.users".into()],
            exclude_tables: vec!["audit_*".into()],
            extra_args: vec!["--no-owner".into(), "--compress=0".into()],
            ..ToolOptions::default()
        };
        let cmd = dump_command(&creds(Engine::Postgres), &options, None).unwrap();
        assert_eq!(
            cmd.argv,
            [
                "pg_dump",
                "-U",
                "app",
                "-d",
                "app",
                "-Fc",
                "--table=public.users",
                "--exclude-table=audit_*",
                "--no-owner",
                "--compress=0"
            ]
        );
        assert_eq!(cmd.env[0].0, "PGPASSWORD");
        no_secret(&cmd);
        options.dump_all = true;
        options.exclude_databases = vec!["template0".into()];
        let all = dump_command(&creds(Engine::Postgres), &options, None).unwrap();
        assert_eq!(all.argv[0], "pg_dumpall");
        assert!(all.argv.contains(&"--exclude-database=template0".into()));
        assert_eq!(dump_extension(Engine::Postgres, &options), ".sql");
    }

    #[test]
    fn mysql_and_mariadb_use_single_transaction_and_database_lists() {
        let options = ToolOptions {
            exclude_tables: vec!["app.sessions".into()],
            ..ToolOptions::default()
        };
        let all = vec![
            "app".to_owned(),
            "mysql".to_owned(),
            "reports".to_owned(),
            "sys".to_owned(),
        ];
        let filtered = filter_databases(
            &all,
            &ToolOptions {
                exclude_databases: vec!["reports".into()],
                ..ToolOptions::default()
            },
        );
        assert_eq!(filtered, ["app"]);
        let cmd = dump_command(&creds(Engine::Mysql), &options, Some(&filtered)).unwrap();
        assert_eq!(cmd.argv[0], "mysqldump");
        assert!(cmd.argv.contains(&"--single-transaction".into()));
        assert!(cmd.argv.contains(&"--ignore-table=app.sessions".into()));
        assert_eq!(cmd.argv[cmd.argv.len() - 2..], ["--databases", "app"]);
        no_secret(&cmd);
        let maria = dump_command(&creds(Engine::Mariadb), &ToolOptions::default(), None).unwrap();
        assert_eq!(maria.argv[0], "mariadb-dump");
        assert!(maria.argv.contains(&"--all-databases".into()));
        assert!(dump_command(
            &creds(Engine::Mysql),
            &ToolOptions {
                exclude_tables: vec!["sessions".into()],
                ..ToolOptions::default()
            },
            None
        )
        .is_err());
    }

    #[test]
    fn mongo_and_redis_commands() {
        let cmd = dump_command(
            &creds(Engine::Mongo),
            &ToolOptions {
                include_databases: vec!["app".into()],
                exclude_tables: vec!["logs".into()],
                ..ToolOptions::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(
            cmd.argv[..3],
            ["sh", "-c", "exec \"$@\" --password=\"$MONGO_PASSWORD\""]
        );
        assert!(cmd.argv.contains(&"mongodump".into()));
        assert!(cmd.argv.contains(&"--archive".into()));
        assert!(cmd.argv.contains(&"--excludeCollection=logs".into()));
        no_secret(&cmd);
        let redis = dump_command(&creds(Engine::Redis), &ToolOptions::default(), None).unwrap();
        assert_eq!(redis.argv[0], "tar");
        assert!(redis.argv.contains(&"dump.rdb".into()));
        let save = redis_bgsave_command("pw-secret-value-1234");
        assert_eq!(save.argv, ["redis-cli", "BGSAVE"]);
        no_secret(&save);
        let restore = restore_command(&creds(Engine::Postgres), false).unwrap();
        assert_eq!(restore.argv[0], "pg_restore");
        no_secret(&restore);
        assert!(restore_command(&creds(Engine::Redis), false).is_err());
    }

    #[test]
    fn extra_args_are_validated() {
        for ok in ["--no-owner", "--jobs=4", "-x", "--exclude-schema=audit_*"] {
            assert!(validate_extra_args(&[ok.into()]).is_ok(), "{ok}");
        }
        for bad in [
            "--file=/etc/passwd",
            "-f",
            "--password=x",
            "--host=evil",
            "; rm -rf /",
            "--x=$(id)",
            "plain",
            "---a",
            "--defaults-file=/tmp/x",
        ] {
            assert!(validate_extra_args(&[bad.into()]).is_err(), "{bad}");
        }
        assert!(validate_tool_options(&ToolOptions {
            include_tables: vec!["-x".into()],
            ..ToolOptions::default()
        })
        .is_err());
        assert!(validate_tool_options(&ToolOptions {
            include_globs: vec!["/abs/**".into()],
            ..ToolOptions::default()
        })
        .is_err());
    }

    #[test]
    fn object_keys_follow_the_template() {
        let vars = KeyVars {
            name: "nightly",
            service: "appdb",
            engine: "postgres",
            unix: 1_700_000_000,
        };
        assert_eq!(
            object_key("rudder/", "{name}-{timestamp}", &vars, ".dump.zst").unwrap(),
            "rudder/nightly-20231114T221320Z.dump.zst"
        );
        assert_eq!(
            object_key("", "{service}/{date}/{unix}", &vars, ".tar").unwrap(),
            "appdb/2023-11-14/1700000000.tar"
        );
        assert!(object_key("x", "{nope}", &vars, "").is_err());
        assert!(object_key("x", "../{name}", &vars, "").is_err());
        assert!(object_key("../x", "{name}", &vars, "").is_err());
        assert!(object_key("x", "a b", &vars, "").is_err());
    }

    #[test]
    fn compression_levels_and_backoff() {
        assert_eq!(Compression::parse("zstd").unwrap().extension(), ".zst");
        assert!(Compression::Zstd.validate_level(19).is_ok());
        assert!(Compression::Zstd.validate_level(22).is_err());
        assert!(Compression::Gzip.validate_level(0).is_err());
        assert_eq!(Compression::None.validate_level(99).unwrap(), 0);
        assert_eq!(retry_backoff_secs(1), 5);
        assert_eq!(retry_backoff_secs(3), 20);
        assert_eq!(retry_backoff_secs(30), 300);
        let spec: RetentionSpec = serde_json::from_str(r#"{"keep_last":3,"daily":7}"#).unwrap();
        let policy: RetentionPolicy = spec.into();
        assert_eq!(policy.keep_last, Some(3));
        assert_eq!(policy.daily, Some(7));
    }

    #[test]
    fn redis_persistence_parses_info() {
        let info = "# Persistence\r\nloading:0\r\nrdb_bgsave_in_progress:1\r\n\
                    rdb_last_save_time:1791692869\r\nrdb_last_bgsave_status:ok\r\n";
        let parsed = RedisPersistence::parse(info).unwrap();
        assert!(parsed.in_progress);
        assert!(parsed.last_ok);
        assert_eq!(parsed.last_save, 1_791_692_869);
        assert!(RedisPersistence::parse("NOAUTH Authentication required.").is_err());
    }
}
