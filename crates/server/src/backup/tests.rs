use chrono::{Duration, TimeZone, Utc};

use super::next_run;

#[test]
fn next_run_uses_timezone_and_anchor() {
    let day = (Utc::now() + Duration::days(30)).date_naive();
    let anchor = Utc.from_utc_datetime(&day.and_hms_opt(0, 0, 0).unwrap());
    let next = next_run(Some("@daily"), "UTC", anchor).unwrap();
    assert_eq!(next, anchor + Duration::hours(3));
    let tokyo = next_run(Some("@daily"), "Asia/Tokyo", anchor).unwrap();
    assert_eq!(tokyo, anchor + Duration::hours(18));
    assert!(next_run(None, "UTC", anchor).is_none());
    assert!(next_run(Some(""), "UTC", anchor).is_none());
}

#[test]
fn next_run_never_points_into_the_past() {
    let anchor = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
    let next = next_run(Some("@hourly"), "UTC", anchor).unwrap();
    assert!(next > Utc::now());
}
