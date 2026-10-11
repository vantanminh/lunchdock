//! One-click database images.
//!
//! Versions are an allow-list so the image tag is a single Docker argument.
//! Passwords are fields on the plan and are not written by this module.

use crate::{validate_image_ref, validate_name, Error};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Postgres,
    Mysql,
    Mariadb,
    Mongo,
    Redis,
}

impl Engine {
    pub fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "postgres" => Ok(Self::Postgres),
            "mysql" => Ok(Self::Mysql),
            "mariadb" => Ok(Self::Mariadb),
            "mongo" => Ok(Self::Mongo),
            "redis" => Ok(Self::Redis),
            _ => Err(Error::Validation(
                "engine must be postgres, mysql, mariadb, mongo, or redis".into(),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Mysql => "mysql",
            Self::Mariadb => "mariadb",
            Self::Mongo => "mongo",
            Self::Redis => "redis",
        }
    }

    pub fn port(self) -> u16 {
        match self {
            Self::Postgres => 5432,
            Self::Mysql | Self::Mariadb => 3306,
            Self::Mongo => 27017,
            Self::Redis => 6379,
        }
    }

    fn versions(self) -> &'static [&'static str] {
        match self {
            Self::Postgres => &["16", "17"],
            Self::Mysql => &["8.4", "8.0"],
            Self::Mariadb => &["11.4", "10.11"],
            Self::Mongo => &["8", "7"],
            Self::Redis => &["7.4", "7.2"],
        }
    }
}

pub struct DatabaseRequest {
    pub engine: Engine,
    pub version: String,
    pub service: String,
    pub database: String,
    pub username: String,
    pub password: String,
    pub root_password: String,
}

pub struct DatabaseRuntime {
    pub image: String,
    pub env: Vec<(String, String)>,
    pub command: Vec<String>,
    pub port: u16,
    pub volume_name: String,
    pub volume_target: String,
}

pub fn database_runtime(request: &DatabaseRequest) -> Result<DatabaseRuntime, Error> {
    validate_name(&request.service)?;
    if !request
        .engine
        .versions()
        .contains(&request.version.as_str())
    {
        return Err(Error::Validation("unsupported database version".into()));
    }
    validate_ident(&request.database, "database")?;
    if request.engine != Engine::Redis {
        validate_ident(&request.username, "username")?;
    }
    validate_secret(&request.password)?;
    let image = format!("{}:{}", request.engine.as_str(), request.version);
    validate_image_ref(&image)?;
    let volume_name = format!("db-{}", request.service);
    let (env, command, volume_target) = match request.engine {
        Engine::Postgres => (
            vec![
                ("POSTGRES_USER".into(), request.username.clone()),
                ("POSTGRES_PASSWORD".into(), request.password.clone()),
                ("POSTGRES_DB".into(), request.database.clone()),
            ],
            Vec::new(),
            "/var/lib/postgresql/data".to_owned(),
        ),
        Engine::Mysql => (
            vec![
                ("MYSQL_USER".into(), request.username.clone()),
                ("MYSQL_PASSWORD".into(), request.password.clone()),
                ("MYSQL_DATABASE".into(), request.database.clone()),
                ("MYSQL_ROOT_PASSWORD".into(), request.root_password.clone()),
            ],
            Vec::new(),
            "/var/lib/mysql".to_owned(),
        ),
        Engine::Mariadb => (
            vec![
                ("MARIADB_USER".into(), request.username.clone()),
                ("MARIADB_PASSWORD".into(), request.password.clone()),
                ("MARIADB_DATABASE".into(), request.database.clone()),
                (
                    "MARIADB_ROOT_PASSWORD".into(),
                    request.root_password.clone(),
                ),
            ],
            Vec::new(),
            "/var/lib/mysql".to_owned(),
        ),
        Engine::Mongo => (
            vec![
                (
                    "MONGO_INITDB_ROOT_USERNAME".into(),
                    request.username.clone(),
                ),
                (
                    "MONGO_INITDB_ROOT_PASSWORD".into(),
                    request.password.clone(),
                ),
                ("MONGO_INITDB_DATABASE".into(), request.database.clone()),
            ],
            Vec::new(),
            "/data/db".to_owned(),
        ),
        Engine::Redis => (
            Vec::new(),
            vec![
                "redis-server".into(),
                "--requirepass".into(),
                request.password.clone(),
                "--appendonly".into(),
                "yes".into(),
            ],
            "/data".to_owned(),
        ),
    };
    Ok(DatabaseRuntime {
        image,
        env,
        command,
        port: request.engine.port(),
        volume_name,
        volume_target,
    })
}

pub fn connection_string(
    engine: Engine,
    host: &str,
    port: u16,
    database: &str,
    username: &str,
    password: &str,
) -> Result<String, Error> {
    if host.is_empty() || host.contains([' ', '\n', '/']) {
        return Err(Error::Validation("invalid database host".into()));
    }
    let password = url_encode(password);
    let username = url_encode(username);
    let database = url_encode(database);
    let value = match engine {
        Engine::Postgres => format!("postgres://{username}:{password}@{host}:{port}/{database}"),
        Engine::Mysql | Engine::Mariadb => {
            format!("mysql://{username}:{password}@{host}:{port}/{database}")
        }
        Engine::Mongo => {
            format!("mongodb://{username}:{password}@{host}:{port}/{database}?authSource=admin")
        }
        Engine::Redis => format!("redis://:{password}@{host}:{port}/0"),
    };
    Ok(value)
}

fn validate_ident(value: &str, label: &str) -> Result<(), Error> {
    let ok = !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && value.as_bytes()[0].is_ascii_alphabetic();
    if ok {
        Ok(())
    } else {
        Err(Error::Validation(format!("invalid {label}")))
    }
}

fn validate_secret(value: &str) -> Result<(), Error> {
    if value.len() >= 16
        && value.len() <= 128
        && !value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b' ')
    {
        Ok(())
    } else {
        Err(Error::Validation("database password is invalid".into()))
    }
}

fn url_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(engine: Engine) -> DatabaseRequest {
        DatabaseRequest {
            engine,
            version: "16".into(),
            service: "appdb".into(),
            database: "app".into(),
            username: "app".into(),
            password: "password-value-ok".into(),
            root_password: "root-password-value".into(),
        }
    }

    #[test]
    fn postgres_and_redis_images_are_pinned() {
        let postgres = database_runtime(&request(Engine::Postgres)).unwrap();
        assert_eq!(postgres.image, "postgres:16");
        assert_eq!(postgres.port, 5432);
        assert_eq!(postgres.volume_target, "/var/lib/postgresql/data");
        assert!(postgres
            .env
            .iter()
            .any(|(key, _)| key == "POSTGRES_PASSWORD"));
        let mut redis = request(Engine::Redis);
        redis.version = "7.4".into();
        let redis = database_runtime(&redis).unwrap();
        assert_eq!(redis.image, "redis:7.4");
        assert_eq!(
            redis.command,
            [
                "redis-server",
                "--requirepass",
                "password-value-ok",
                "--appendonly",
                "yes"
            ]
        );
        assert!(database_runtime(&{
            let mut bad = request(Engine::Postgres);
            bad.version = "9.9".into();
            bad
        })
        .is_err());
    }

    #[test]
    fn connection_string_encodes_secrets() {
        let encoded =
            connection_string(Engine::Postgres, "db", 5432, "app", "app", "p@ss/word").unwrap();
        assert_eq!(encoded, "postgres://app:p%40ss%2Fword@db:5432/app");
        let redis = connection_string(
            Engine::Redis,
            "db",
            6379,
            "ignored",
            "ignored",
            "secret-value-ok!!",
        )
        .unwrap();
        assert!(redis.starts_with("redis://:secret-value-ok%21%21@db:6379/0"));
    }
}
