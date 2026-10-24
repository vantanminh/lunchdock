//! Shared rate-limit decisions.
//!
//! Traefik asks the panel before it forwards a request. The panel applies every
//! matching rule. Counts stay here so a chart can show when real visitors were
//! blocked. Keys are not written by this module.

use std::collections::HashMap;

use crate::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateKey {
    Ip,
    Header,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateRule {
    pub id: String,
    /// Empty means every app host. The panel and the Access portal are excluded
    /// by the caller.
    pub host: String,
    pub path_prefix: String,
    pub methods: Vec<String>,
    pub limit: u32,
    pub window_secs: u32,
    pub burst: u32,
    pub key: RateKey,
    pub header_name: String,
}

#[derive(Debug, Clone, Copy)]
pub struct Bucket {
    pub tokens: f64,
    pub updated_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateDecision {
    pub allowed: bool,
    /// Rule that blocked the request, when it was blocked.
    pub blocked_by: Option<String>,
    /// Rules that allowed it. Empty when it was blocked.
    pub allowed_by: Vec<String>,
    pub retry_after_secs: u32,
}

pub fn validate_rate_rule(rule: &RateRule) -> Result<(), Error> {
    if rule.id.is_empty() || rule.id.len() > 80 {
        return Err(Error::Validation("invalid rate limit id".into()));
    }
    if !(1..=100_000).contains(&rule.limit) {
        return Err(Error::Validation(
            "rate limit must be between 1 and 100000".into(),
        ));
    }
    if !(1..=86_400).contains(&rule.window_secs) {
        return Err(Error::Validation(
            "rate limit window must be between 1 second and 1 day".into(),
        ));
    }
    if rule.burst > 100_000 {
        return Err(Error::Validation("rate limit burst is too large".into()));
    }
    crate::validate_path_prefix(&rule.path_prefix)?;
    if !rule.host.is_empty() {
        crate::validate_host(&rule.host)?;
    }
    for method in &rule.methods {
        if !matches!(
            method.as_str(),
            "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS"
        ) {
            return Err(Error::Validation("unsupported rate limit method".into()));
        }
    }
    if rule.key == RateKey::Header {
        let ok = !rule.header_name.is_empty()
            && rule.header_name.len() <= 64
            && rule
                .header_name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
        if !ok {
            return Err(Error::Validation("invalid rate limit header".into()));
        }
    }
    Ok(())
}

/// Apply matching rules. A block does not spend a token on the other rules.
pub struct RateRequest<'a> {
    pub host: &'a str,
    pub path: &'a str,
    pub method: &'a str,
    pub ip: &'a str,
    pub headers: &'a HashMap<String, String>,
    pub now_ms: u64,
}

pub fn decide(
    rules: &[RateRule],
    request: &RateRequest<'_>,
    buckets: &mut HashMap<String, Bucket>,
) -> RateDecision {
    let matching: Vec<&RateRule> = rules
        .iter()
        .filter(|rule| rule_matches(rule, request.host, request.path, request.method))
        .collect();
    if let Some(rule) = matching
        .iter()
        .find(|rule| !has_token(rule, request.ip, request.headers, request.now_ms, buckets))
    {
        return RateDecision {
            allowed: false,
            blocked_by: Some(rule.id.clone()),
            allowed_by: Vec::new(),
            retry_after_secs: rule.window_secs.max(1),
        };
    }
    let mut allowed_by = Vec::new();
    for rule in matching {
        spend(rule, request.ip, request.headers, request.now_ms, buckets);
        allowed_by.push(rule.id.clone());
    }
    RateDecision {
        allowed: true,
        blocked_by: None,
        allowed_by,
        retry_after_secs: 0,
    }
}

fn rule_matches(rule: &RateRule, host: &str, path: &str, method: &str) -> bool {
    if !rule.host.is_empty() && !rule.host.eq_ignore_ascii_case(host) {
        return false;
    }
    if rule.path_prefix != "/" && !path.starts_with(&rule.path_prefix) {
        return false;
    }
    rule.methods.is_empty()
        || rule
            .methods
            .iter()
            .any(|item| item.eq_ignore_ascii_case(method))
}

fn bucket_key(rule: &RateRule, ip: &str, headers: &HashMap<String, String>) -> String {
    let raw = match rule.key {
        RateKey::Ip => ip,
        RateKey::Header => headers
            .get(&rule.header_name.to_ascii_lowercase())
            .map(String::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or(ip),
    };
    let raw = &raw[..raw.len().min(128)];
    format!("{}\n{raw}", rule.id)
}

fn has_token(
    rule: &RateRule,
    ip: &str,
    headers: &HashMap<String, String>,
    now_ms: u64,
    buckets: &HashMap<String, Bucket>,
) -> bool {
    let key = bucket_key(rule, ip, headers);
    match buckets.get(&key) {
        None => true,
        Some(bucket) => refill(rule, bucket, now_ms) >= 1.0,
    }
}

fn spend(
    rule: &RateRule,
    ip: &str,
    headers: &HashMap<String, String>,
    now_ms: u64,
    buckets: &mut HashMap<String, Bucket>,
) {
    let key = bucket_key(rule, ip, headers);
    let current = buckets.get(&key).copied();
    let tokens = match current {
        None => capacity(rule) - 1.0,
        Some(bucket) => (refill(rule, &bucket, now_ms) - 1.0).max(0.0),
    };
    buckets.insert(
        key,
        Bucket {
            tokens,
            updated_ms: now_ms,
        },
    );
}

fn capacity(rule: &RateRule) -> f64 {
    f64::from(rule.limit.saturating_add(rule.burst)).max(1.0)
}

fn refill(rule: &RateRule, bucket: &Bucket, now_ms: u64) -> f64 {
    let elapsed = now_ms.saturating_sub(bucket.updated_ms) as f64;
    let per_ms = f64::from(rule.limit) / (f64::from(rule.window_secs) * 1000.0);
    (bucket.tokens + elapsed * per_ms).min(capacity(rule))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample<'a>(
        host: &'a str,
        ip: &'a str,
        headers: &'a HashMap<String, String>,
    ) -> RateRequest<'a> {
        RateRequest {
            host,
            path: "/",
            method: "GET",
            ip,
            headers,
            now_ms: 0,
        }
    }

    fn rule(limit: u32) -> RateRule {
        RateRule {
            id: "web".into(),
            host: "app.example.com".into(),
            path_prefix: "/".into(),
            methods: vec![],
            limit,
            window_secs: 60,
            burst: 0,
            key: RateKey::Ip,
            header_name: String::new(),
        }
    }

    #[test]
    fn the_third_request_in_a_window_of_two_is_blocked() {
        let rules = vec![rule(2)];
        let mut buckets = HashMap::new();
        let headers = HashMap::new();
        let one = sample("app.example.com", "203.0.113.8", &headers);
        let two = sample("app.example.com", "203.0.113.8", &headers);
        let third = sample("app.example.com", "203.0.113.8", &headers);
        let other_ip = sample("app.example.com", "203.0.113.9", &headers);
        assert!(decide(&rules, &one, &mut buckets).allowed);
        assert!(decide(&rules, &two, &mut buckets).allowed);
        let blocked = decide(&rules, &third, &mut buckets);
        assert!(!blocked.allowed);
        assert_eq!(blocked.blocked_by.as_deref(), Some("web"));
        assert!(decide(&rules, &other_ip, &mut buckets).allowed);
    }

    #[test]
    fn a_rule_for_another_host_does_not_apply() {
        let rules = vec![rule(1)];
        let mut buckets = HashMap::new();
        let headers = HashMap::new();
        let decision = decide(
            &rules,
            &sample("other.example.com", "203.0.113.8", &headers),
            &mut buckets,
        );
        assert!(decision.allowed);
        assert!(decision.allowed_by.is_empty());
    }
}
