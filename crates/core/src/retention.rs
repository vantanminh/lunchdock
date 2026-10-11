//! Backup retention.
//!
//! Keep-last, max age, and grandfather-father-son counts are combined with a
//! union: a backup survives if any configured rule still wants it. When no
//! rule is set, nothing is expired.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub keep_last: Option<u32>,
    pub keep_days: Option<u32>,
    pub daily: Option<u32>,
    pub weekly: Option<u32>,
    pub monthly: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupPoint {
    pub id: String,
    pub created_unix: i64,
}

pub fn expired_ids(points: &[BackupPoint], policy: &RetentionPolicy, now_unix: i64) -> Vec<String> {
    if policy.keep_last.is_none()
        && policy.keep_days.is_none()
        && policy.daily.is_none()
        && policy.weekly.is_none()
        && policy.monthly.is_none()
    {
        return Vec::new();
    }
    let mut ordered = points.to_vec();
    ordered.sort_by_key(|point| std::cmp::Reverse(point.created_unix));
    let mut keep = std::collections::BTreeSet::new();
    if let Some(limit) = policy.keep_last {
        for point in ordered.iter().take(limit as usize) {
            keep.insert(point.id.clone());
        }
    }
    if let Some(days) = policy.keep_days {
        let horizon = now_unix.saturating_sub(i64::from(days) * 86_400);
        for point in &ordered {
            if point.created_unix >= horizon {
                keep.insert(point.id.clone());
            }
        }
    }
    keep_buckets(&ordered, policy.daily, 86_400, &mut keep);
    keep_buckets(&ordered, policy.weekly, 7 * 86_400, &mut keep);
    keep_buckets(&ordered, policy.monthly, 30 * 86_400, &mut keep);
    ordered
        .into_iter()
        .filter(|point| !keep.contains(&point.id))
        .map(|point| point.id)
        .collect()
}

fn keep_buckets(
    ordered: &[BackupPoint],
    count: Option<u32>,
    width: i64,
    keep: &mut std::collections::BTreeSet<String>,
) {
    let Some(count) = count else {
        return;
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut kept = 0u32;
    for point in ordered {
        let bucket = point.created_unix.div_euclid(width);
        if !seen.insert(bucket) {
            continue;
        }
        if kept >= count {
            break;
        }
        keep.insert(point.id.clone());
        kept += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn points(ids: &[&str]) -> Vec<BackupPoint> {
        ids.iter()
            .enumerate()
            .map(|(index, id)| BackupPoint {
                id: (*id).into(),
                created_unix: 1_700_000_000 + (index as i64) * 86_400,
            })
            .collect()
    }

    #[test]
    fn keep_last_and_age_are_a_union() {
        let points = points(&["a", "b", "c", "d"]);
        let now = 1_700_000_000 + 3 * 86_400;
        let expired = expired_ids(
            &points,
            &RetentionPolicy {
                keep_last: Some(1),
                keep_days: Some(2),
                daily: None,
                weekly: None,
                monthly: None,
            },
            now,
        );
        assert!(expired.contains(&"a".into()));
        assert!(!expired.contains(&"d".into()));
        assert!(!expired.contains(&"c".into()));
    }

    #[test]
    fn gfs_keeps_one_point_per_bucket() {
        let points = points(&["d0", "d1", "d2", "d3", "d4"]);
        let expired = expired_ids(
            &points,
            &RetentionPolicy {
                keep_last: None,
                keep_days: None,
                daily: Some(2),
                weekly: None,
                monthly: None,
            },
            1_700_000_000 + 10 * 86_400,
        );
        assert_eq!(
            expired,
            vec![String::from("d2"), String::from("d1"), String::from("d0")]
        );
    }

    #[test]
    fn empty_policy_keeps_everything() {
        let points = points(&["a", "b"]);
        assert!(expired_ids(
            &points,
            &RetentionPolicy {
                keep_last: None,
                keep_days: None,
                daily: None,
                weekly: None,
                monthly: None,
            },
            1_800_000_000,
        )
        .is_empty());
    }
}
