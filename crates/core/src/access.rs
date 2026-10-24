//! Who may open an internal app, and whether a Turnstile result is acceptable.
//!
//! The control plane stores people, departments, and rules. This module only
//! decides. It does not talk to Google, Cloudflare, or the database.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Membership {
    pub department: String,
    pub rank: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessRule {
    Email(String),
    /// The department and every department under it.
    Department(String),
    Rank(String),
}

/// `edges` are `(department slug, parent slug)`.
///
/// The set for a department contains itself and every descendant. A cycle is
/// ignored once a department is already on the path being walked.
pub fn department_closure(
    edges: &[(String, Option<String>)],
) -> BTreeMap<String, BTreeSet<String>> {
    let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut slugs = BTreeSet::new();
    for (slug, parent) in edges {
        slugs.insert(slug.clone());
        if let Some(parent) = parent {
            children
                .entry(parent.clone())
                .or_default()
                .push(slug.clone());
        }
    }
    let mut out = BTreeMap::new();
    for slug in slugs {
        let mut seen = BTreeSet::new();
        let mut stack = vec![slug.clone()];
        while let Some(current) = stack.pop() {
            if !seen.insert(current.clone()) {
                continue;
            }
            if let Some(kids) = children.get(&current) {
                stack.extend(kids.iter().cloned());
            }
        }
        out.insert(slug, seen);
    }
    out
}

/// `true` when any rule matches this person.
pub fn access_allowed(
    email: &str,
    memberships: &[Membership],
    rules: &[AccessRule],
    closure: &BTreeMap<String, BTreeSet<String>>,
) -> bool {
    let email = email.trim().to_ascii_lowercase();
    rules.iter().any(|rule| match rule {
        AccessRule::Email(allowed) => allowed == &email,
        AccessRule::Rank(rank) => memberships
            .iter()
            .any(|member| member.rank.as_deref() == Some(rank.as_str())),
        AccessRule::Department(department) => {
            let covered = closure
                .get(department)
                .map(|set| set.iter().map(String::as_str).collect::<Vec<_>>())
                .unwrap_or_else(|| vec![department.as_str()]);
            memberships
                .iter()
                .any(|member| covered.iter().any(|slug| *slug == member.department))
        }
    })
}

/// A path we will send someone back to after sign-in. It stays on one host.
pub fn safe_return_path(path: &str) -> Option<String> {
    if path == "/" {
        return Some(path.to_owned());
    }
    let ok = path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains('\\')
        && !path.contains('\0')
        && path.len() <= 2048
        && !path.split('/').any(|part| part == "..")
        && path
            .bytes()
            .all(|byte| byte.is_ascii() && !byte.is_ascii_control() && byte != b' ');
    ok.then(|| path.to_owned())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnstileResult {
    pub success: bool,
    pub action: Option<String>,
    pub hostname: Option<String>,
    pub error_codes: Vec<String>,
}

/// Read the fields the gate cares about. Extra fields are ignored.
pub fn parse_turnstile(body: &Value) -> TurnstileResult {
    TurnstileResult {
        success: body
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        action: body
            .get("action")
            .and_then(Value::as_str)
            .map(str::to_owned),
        hostname: body
            .get("hostname")
            .and_then(Value::as_str)
            .map(str::to_owned),
        error_codes: body
            .get("error-codes")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// Accept only a successful token for the expected action and an allowed hostname.
pub fn accept_turnstile(
    result: &TurnstileResult,
    expected_action: &str,
    hostnames: &[String],
) -> Result<(), &'static str> {
    if hostnames.is_empty() {
        return Err("turnstile hostnames are not configured");
    }
    if !result.success {
        return Err("turnstile rejected the request");
    }
    if result.action.as_deref() != Some(expected_action) {
        return Err("turnstile action did not match");
    }
    let Some(hostname) = result.hostname.as_deref() else {
        return Err("turnstile did not return a hostname");
    };
    if !hostnames
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(hostname))
    {
        return Err("turnstile hostname is not allowed");
    }
    Ok(())
}

/// Action name sent with the Access sign-in widget.
pub const ACCESS_LOGIN_ACTION: &str = "access_login";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn closure() -> BTreeMap<String, BTreeSet<String>> {
        department_closure(&[
            ("company".into(), None),
            ("finance".into(), Some("company".into())),
            ("accounting".into(), Some("finance".into())),
        ])
    }

    #[test]
    fn department_grant_covers_child_departments() {
        let closure = closure();
        let rules = vec![AccessRule::Department("finance".into())];
        let member = [Membership {
            department: "accounting".into(),
            rank: Some("staff".into()),
        }];
        assert!(access_allowed("a@example.com", &member, &rules, &closure));
        assert!(!access_allowed(
            "a@example.com",
            &[Membership {
                department: "company".into(),
                rank: None,
            }],
            &rules,
            &closure
        ));
    }

    #[test]
    fn email_and_rank_are_exact() {
        let closure = closure();
        let member = [Membership {
            department: "accounting".into(),
            rank: Some("lead".into()),
        }];
        assert!(access_allowed(
            "A@Example.com",
            &member,
            &[AccessRule::Email("a@example.com".into())],
            &closure
        ));
        assert!(access_allowed(
            "a@example.com",
            &member,
            &[AccessRule::Rank("lead".into())],
            &closure
        ));
        assert!(!access_allowed(
            "other@example.com",
            &member,
            &[AccessRule::Email("a@example.com".into())],
            &closure
        ));
    }

    #[test]
    fn turnstile_requires_success_action_and_hostname() {
        let ok = parse_turnstile(&json!({
            "success": true,
            "action": "access_login",
            "hostname": "Access.Example.com",
            "error-codes": []
        }));
        assert!(accept_turnstile(&ok, ACCESS_LOGIN_ACTION, &["access.example.com".into()]).is_ok());
        assert!(accept_turnstile(&ok, ACCESS_LOGIN_ACTION, &[]).is_err());
        assert!(accept_turnstile(&ok, "other", &["access.example.com".into()]).is_err());
        let bad = parse_turnstile(
            &json!({ "success": false, "error-codes": ["invalid-input-response"] }),
        );
        assert!(
            accept_turnstile(&bad, ACCESS_LOGIN_ACTION, &["access.example.com".into()]).is_err()
        );
    }

    #[test]
    fn return_paths_stay_on_the_host() {
        assert_eq!(safe_return_path("/invoices").as_deref(), Some("/invoices"));
        assert!(safe_return_path("//evil.example").is_none());
        assert!(safe_return_path("/../etc").is_none());
        assert!(safe_return_path("https://app.example/x").is_none());
    }
}
