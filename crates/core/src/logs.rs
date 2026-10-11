//! Log line parsing and retention math shared by the panel and the agent.

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Map, Value};

use crate::Error;

/// Where a log line came from. Retention is configured per source.
pub const LOG_SOURCES: &[&str] = &["app", "panel", "proxy", "cache", "agent", "system"];
pub const LOG_LEVELS: &[&str] = &["trace", "debug", "info", "warn", "error"];
/// Longer messages are cut at this many bytes.
pub const MAX_LOG_MESSAGE: usize = 16 * 1024;
/// Structured fields larger than this are dropped from a line.
const MAX_FIELDS_BYTES: usize = 8 * 1024;
/// Daily partitions of `log_entries` are named `log_entries_YYYYMMDD`.
pub const LOG_PARTITION_PREFIX: &str = "log_entries_";

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedLog {
    pub level: &'static str,
    pub message: String,
    pub http_status: Option<i32>,
    pub fields: Map<String, Value>,
}

pub fn validate_source(source: &str) -> Result<(), Error> {
    if LOG_SOURCES.contains(&source) {
        Ok(())
    } else {
        Err(Error::Validation(format!(
            "log source must be one of {}",
            LOG_SOURCES.join(", ")
        )))
    }
}

/// Map common level spellings to `trace`, `debug`, `info`, `warn`, or `error`.
pub fn normalize_level(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "trace" | "trc" => Some("trace"),
        "debug" | "dbg" | "verbose" => Some("debug"),
        "info" | "inf" | "information" | "notice" => Some("info"),
        "warn" | "warning" | "wrn" => Some("warn"),
        "error" | "err" | "fatal" | "critical" | "crit" | "panic" | "emerg" | "alert" => {
            Some("error")
        }
        _ => None,
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn as_status(value: &Value) -> Option<i32> {
    let number = match value {
        Value::Number(n) => n.as_i64()?,
        Value::String(s) => s.parse().ok()?,
        _ => return None,
    };
    (100..600).contains(&number).then_some(number as i32)
}

fn level_for_status(status: i32) -> &'static str {
    match status {
        500.. => "error",
        400..=499 => "warn",
        _ => "info",
    }
}

/// Parse one line of output. JSON objects (including Traefik access logs and `tracing` JSON)
/// keep their fields; plain text gets a level from its first words or an HTTP status.
pub fn parse_line(text: &str) -> ParsedLog {
    let text = text.trim_end_matches(['\r', '\n']);
    if text.starts_with('{') {
        if let Ok(Value::Object(object)) = serde_json::from_str::<Value>(text) {
            return parse_json(object, text);
        }
    }
    let status = access_log_status(text);
    let level = word_level(text)
        .or_else(|| status.map(level_for_status))
        .unwrap_or("info");
    ParsedLog {
        level,
        message: truncate(text, MAX_LOG_MESSAGE),
        http_status: status,
        fields: Map::new(),
    }
}

fn parse_json(mut object: Map<String, Value>, raw: &str) -> ParsedLog {
    let mut take_str = |keys: &[&str]| {
        keys.iter().find_map(|key| match object.remove(*key) {
            Some(Value::String(value)) => Some(value),
            Some(other) => {
                object.insert((*key).to_owned(), other);
                None
            }
            None => None,
        })
    };
    let level_text = take_str(&["level", "lvl", "severity", "log.level"]);
    let mut message = take_str(&["msg", "message"]);
    if message.is_none() {
        if let Some(Value::Object(fields)) = object.get_mut("fields") {
            if let Some(Value::String(value)) = fields.remove("message") {
                message = Some(value);
            }
        }
    }
    let status = ["DownstreamStatus", "status", "status_code", "statusCode"]
        .iter()
        .find_map(|key| object.get(*key).and_then(as_status));
    let message = match message {
        Some(message) => message,
        None if object.contains_key("RequestMethod") => format!(
            "{} {}{} {}",
            object["RequestMethod"].as_str().unwrap_or("-"),
            object
                .get("RequestHost")
                .and_then(Value::as_str)
                .unwrap_or(""),
            object
                .get("RequestPath")
                .and_then(Value::as_str)
                .unwrap_or("/"),
            status.map_or_else(|| "-".to_owned(), |s| s.to_string()),
        ),
        None => raw.to_owned(),
    };
    let level = level_text
        .as_deref()
        .and_then(normalize_level)
        .or_else(|| status.map(level_for_status))
        .unwrap_or("info");
    let fields = if serde_json::to_string(&object).map_or(0, |s| s.len()) > MAX_FIELDS_BYTES {
        Map::new()
    } else {
        object
    };
    ParsedLog {
        level,
        message: truncate(&message, MAX_LOG_MESSAGE),
        http_status: status,
        fields,
    }
}

fn word_level(text: &str) -> Option<&'static str> {
    let head: String = text.chars().take(96).collect();
    head.split(|c: char| !c.is_ascii_alphabetic())
        .filter(|word| word.len() >= 3 && word.chars().all(|c| c.is_ascii_uppercase()))
        .find_map(normalize_level)
        .or_else(|| {
            let lower = head.to_ascii_lowercase();
            ["level=error", "level=warn", "level=info", "level=debug"]
                .iter()
                .find(|needle| lower.contains(*needle))
                .and_then(|needle| normalize_level(&needle[6..]))
        })
}

/// Status code from a combined/common access log line: `"GET / HTTP/1.1" 200 ...`.
fn access_log_status(text: &str) -> Option<i32> {
    let after = text.split_once(" HTTP/")?.1;
    let after = after.split_once("\" ")?.1;
    let code = after.split_whitespace().next()?;
    code.parse::<i32>()
        .ok()
        .filter(|status| (100..600).contains(status))
}

pub fn parse_docker_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

/// Escape `%`, `_`, and `\` for a `LIKE ... ESCAPE '\'` pattern.
pub fn escape_like(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Day of a daily partition named `<prefix>YYYYMMDD`.
pub fn partition_day(prefix: &str, name: &str) -> Option<NaiveDate> {
    let digits = name.strip_prefix(prefix)?;
    if digits.len() != 8 {
        return None;
    }
    NaiveDate::parse_from_str(digits, "%Y%m%d").ok()
}

/// Daily partitions that hold only rows older than `keep_days` before `today`.
pub fn partitions_to_drop(
    prefix: &str,
    names: &[String],
    today: NaiveDate,
    keep_days: u32,
) -> Vec<String> {
    let cutoff = today - chrono::Duration::days(i64::from(keep_days));
    let mut out: Vec<String> = names
        .iter()
        .filter(|name| partition_day(prefix, name).is_some_and(|day| day < cutoff))
        .cloned()
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_lines_keep_fields_and_levels() {
        let traefik = parse_line(
            r#"{"ClientHost":"1.2.3.4","DownstreamStatus":502,"RequestHost":"a.test","RequestMethod":"GET","RequestPath":"/x","Duration":1200}"#,
        );
        assert_eq!(traefik.level, "error");
        assert_eq!(traefik.http_status, Some(502));
        assert_eq!(traefik.message, "GET a.test/x 502");
        assert_eq!(traefik.fields["ClientHost"], "1.2.3.4");

        let tracing = parse_line(
            r#"{"timestamp":"2026-10-11T00:00:00Z","level":"WARN","fields":{"message":"slow query","ms":900},"target":"app"}"#,
        );
        assert_eq!(tracing.level, "warn");
        assert_eq!(tracing.message, "slow query");
        assert_eq!(tracing.fields["fields"]["ms"], 900);

        let pino = parse_line(r#"{"level":"fatal","msg":"boom","pid":1}"#);
        assert_eq!((pino.level, pino.message.as_str()), ("error", "boom"));
        let numeric_level = parse_line(r#"{"level":30,"msg":"hi"}"#);
        assert_eq!(numeric_level.level, "info");
        assert_eq!(numeric_level.fields["level"], 30);
    }

    #[test]
    fn plain_lines_detect_levels_and_statuses() {
        assert_eq!(parse_line("2026/10/11 [ERROR] disk full").level, "error");
        assert_eq!(parse_line("WARNING: low memory\n").level, "warn");
        assert_eq!(parse_line("time=x level=debug msg=y").level, "debug");
        assert_eq!(parse_line("just text with error inside").level, "info");
        let nginx = parse_line(
            r#"10.0.0.1 - - [11/Oct/2026:10:00:00 +0000] "GET /a HTTP/1.1" 404 12 "-" "curl""#,
        );
        assert_eq!((nginx.level, nginx.http_status), ("warn", Some(404)));
        let long = "x".repeat(MAX_LOG_MESSAGE + 10);
        assert!(parse_line(&long).message.len() <= MAX_LOG_MESSAGE + 3);
    }

    #[test]
    fn helpers() {
        assert_eq!(escape_like("50%_a\\b"), "50\\%\\_a\\\\b");
        assert!(validate_source("proxy").is_ok());
        assert!(validate_source("nginx").is_err());
        assert_eq!(normalize_level("Warning"), Some("warn"));
        assert!(parse_docker_timestamp("2026-10-11T05:37:21.658956006Z").is_some());
        let names: Vec<String> = [
            "log_entries_20261001",
            "log_entries_20261005",
            "log_entries_20261010",
            "log_entries_default",
            "log_entries_2026",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let today = NaiveDate::from_ymd_opt(2026, 10, 11).unwrap();
        assert_eq!(
            partitions_to_drop(LOG_PARTITION_PREFIX, &names, today, 6),
            vec!["log_entries_20261001".to_string()]
        );
        assert_eq!(
            partitions_to_drop(LOG_PARTITION_PREFIX, &names, today, 0).len(),
            3
        );
    }
}
