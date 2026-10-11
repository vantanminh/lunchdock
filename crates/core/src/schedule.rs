//! Cron schedules with presets and IANA time zones.
//!
//! Users write five-field cron (`min hour dom mon dow`) or a preset. The
//! `cron` crate wants a seconds field, so one is prepended.

use std::str::FromStr;

use chrono::{DateTime, Utc};

use crate::Error;

/// Preset names shown in the UI and their five-field expansion.
pub const PRESETS: &[(&str, &str)] = &[
    ("@hourly", "0 * * * *"),
    ("@daily", "0 3 * * *"),
    ("@weekly", "0 3 * * 0"),
    ("@monthly", "0 3 1 * *"),
    ("@every6h", "0 */6 * * *"),
];

fn expand(expr: &str) -> Result<String, Error> {
    let trimmed = expr.trim();
    if let Some((_, value)) = PRESETS.iter().find(|(name, _)| *name == trimmed) {
        return Ok(format!("0 {value}"));
    }
    let fields: Vec<&str> = trimmed.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(Error::Validation(
            "cron must have five fields (minute hour day month weekday) or be a preset".into(),
        ));
    }
    if trimmed.len() > 128 {
        return Err(Error::Validation("cron expression is too long".into()));
    }
    Ok(format!("0 {}", fields.join(" ")))
}

pub fn parse_timezone(name: &str) -> Result<chrono_tz::Tz, Error> {
    chrono_tz::Tz::from_str(name)
        .map_err(|_| Error::Validation(format!("unknown time zone '{name}'")))
}

/// Validate an expression and time zone together.
pub fn validate_schedule(expr: &str, timezone: &str) -> Result<(), Error> {
    next_after(expr, timezone, Utc::now()).map(|_| ())
}

/// First fire time strictly after `after`, evaluated in `timezone`.
pub fn next_after(
    expr: &str,
    timezone: &str,
    after: DateTime<Utc>,
) -> Result<DateTime<Utc>, Error> {
    let tz = parse_timezone(timezone)?;
    let schedule = cron::Schedule::from_str(&expand(expr)?)
        .map_err(|error| Error::Validation(format!("invalid cron: {error}")))?;
    schedule
        .after(&after.with_timezone(&tz))
        .next()
        .map(|time| time.with_timezone(&Utc))
        .ok_or_else(|| Error::Validation("cron never fires".into()))
}

/// True when a job last anchored at `anchor` is due at `now`.
pub fn is_due(
    expr: &str,
    timezone: &str,
    anchor: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<bool, Error> {
    Ok(next_after(expr, timezone, anchor)? <= now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn presets_and_five_field_crons() {
        let after = Utc.with_ymd_and_hms(2026, 1, 1, 10, 30, 0).unwrap();
        assert_eq!(
            next_after("@hourly", "UTC", after).unwrap(),
            Utc.with_ymd_and_hms(2026, 1, 1, 11, 0, 0).unwrap()
        );
        assert_eq!(
            next_after("@daily", "UTC", after).unwrap(),
            Utc.with_ymd_and_hms(2026, 1, 2, 3, 0, 0).unwrap()
        );
        assert_eq!(
            next_after("*/15 * * * *", "UTC", after).unwrap(),
            Utc.with_ymd_and_hms(2026, 1, 1, 10, 45, 0).unwrap()
        );
        assert!(next_after("* * *", "UTC", after).is_err());
        assert!(next_after("0 0 * * * *", "UTC", after).is_err());
        assert!(next_after("61 * * * *", "UTC", after).is_err());
    }

    #[test]
    fn time_zones_shift_the_fire_time() {
        let after = Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap();
        // 03:00 in Ho Chi Minh City (UTC+7) is 20:00 UTC the previous day.
        assert_eq!(
            next_after("0 3 * * *", "Asia/Ho_Chi_Minh", after).unwrap(),
            Utc.with_ymd_and_hms(2026, 7, 1, 20, 0, 0).unwrap()
        );
        // New York observes daylight saving in July (UTC-4).
        assert_eq!(
            next_after("0 3 * * *", "America/New_York", after).unwrap(),
            Utc.with_ymd_and_hms(2026, 7, 1, 7, 0, 0).unwrap()
        );
        assert!(validate_schedule("@daily", "Mars/Olympus").is_err());
        let anchor = Utc.with_ymd_and_hms(2026, 7, 1, 2, 0, 0).unwrap();
        assert!(!is_due("@daily", "UTC", anchor, anchor).unwrap());
        assert!(is_due(
            "@daily",
            "UTC",
            anchor,
            Utc.with_ymd_and_hms(2026, 7, 1, 3, 0, 1).unwrap()
        )
        .unwrap());
    }
}
