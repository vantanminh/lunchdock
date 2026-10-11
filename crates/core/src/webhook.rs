//! Webhook authentication and branch selection.
//!
//! GitHub signs the raw body with HMAC-SHA256. GitLab sends the shared token
//! in a header. Both comparisons are constant-time.

use hmac::Mac;
use serde_json::Value;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::Error;

type HmacSha256 = hmac::Hmac<Sha256>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookKind {
    Github,
    Gitlab,
}

pub struct HookInput<'a> {
    pub kind: HookKind,
    pub event: &'a str,
    pub body: &'a [u8],
    pub github_signature: Option<&'a str>,
    pub gitlab_token: Option<&'a str>,
    pub secret: &'a [u8],
    pub branch: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOutcome {
    Unauthorized,
    Ignored(&'static str),
    Rejected(&'static str),
    Deploy { git_sha: Option<String> },
}

pub fn evaluate_hook(input: &HookInput<'_>) -> HookOutcome {
    let authorized = match input.kind {
        HookKind::Github => input
            .github_signature
            .is_some_and(|header| verify_github(input.secret, input.body, header)),
        HookKind::Gitlab => input
            .gitlab_token
            .is_some_and(|token| token.as_bytes().ct_eq(input.secret).unwrap_u8() == 1),
    };
    if !authorized {
        return HookOutcome::Unauthorized;
    }
    let event = input.event.trim();
    if event.eq_ignore_ascii_case("ping") {
        return HookOutcome::Ignored("ping");
    }
    let push = event.eq_ignore_ascii_case("push") || event.eq_ignore_ascii_case("push hook");
    if !push {
        return HookOutcome::Ignored("event ignored");
    }
    let Ok(payload) = serde_json::from_slice::<Value>(input.body) else {
        return HookOutcome::Rejected("payload is not json");
    };
    let git_ref = payload.get("ref").and_then(Value::as_str).unwrap_or("");
    let Ok(expected) = push_ref(input.branch) else {
        return HookOutcome::Rejected("branch filter is invalid");
    };
    if git_ref != expected {
        return HookOutcome::Ignored("branch filtered");
    }
    let git_sha = payload
        .get("after")
        .and_then(Value::as_str)
        .or_else(|| payload.get("checkout_sha").and_then(Value::as_str))
        .filter(|sha| is_git_sha(sha))
        .map(str::to_owned);
    HookOutcome::Deploy { git_sha }
}

pub fn validate_branch(branch: &str) -> Result<(), Error> {
    let ok = !branch.is_empty()
        && branch.len() <= 128
        && !branch.starts_with('-')
        && !branch.contains("..")
        && !branch.contains("//")
        && branch
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'/' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(Error::Validation("invalid git branch".into()))
    }
}

fn push_ref(branch: &str) -> Result<String, Error> {
    validate_branch(branch)?;
    Ok(format!("refs/heads/{branch}"))
}

fn is_git_sha(sha: &str) -> bool {
    (7..=64).contains(&sha.len()) && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn verify_github(secret: &[u8], body: &[u8], header: &str) -> bool {
    let Some(encoded) = header.trim().strip_prefix("sha256=") else {
        return false;
    };
    let Ok(provided) = hex::decode(encoded) else {
        return false;
    };
    let Ok(mut mac) = HmacSha256::new_from_slice(secret) else {
        return false;
    };
    mac.update(body);
    let expected = mac.finalize().into_bytes();
    expected.as_slice().ct_eq(provided.as_slice()).unwrap_u8() == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = r#"{"ref":"refs/heads/main","after":"abcdef1"}"#;
    const SIG: &str = "sha256=1a6711ea22a018924121d41bef751097200a1c54b4bfa5503efd21289f787674";

    fn github<'a>(body: &'a str, signature: Option<&'a str>, branch: &'a str) -> HookInput<'a> {
        HookInput {
            kind: HookKind::Github,
            event: "push",
            body: body.as_bytes(),
            github_signature: signature,
            gitlab_token: None,
            secret: b"secret",
            branch,
        }
    }

    #[test]
    fn github_hmac_accepts_the_known_vector() {
        let outcome = evaluate_hook(&github(BODY, Some(SIG), "main"));
        assert_eq!(
            outcome,
            HookOutcome::Deploy {
                git_sha: Some("abcdef1".into())
            }
        );
    }

    #[test]
    fn github_hmac_rejects_a_tampered_body() {
        let outcome = evaluate_hook(&github(
            r#"{"ref":"refs/heads/main","after":"abcdef2"}"#,
            Some(SIG),
            "main",
        ));
        assert_eq!(outcome, HookOutcome::Unauthorized);
    }

    #[test]
    fn gitlab_token_and_branch_filter() {
        let body = r#"{"ref":"refs/heads/dev","checkout_sha":"abc1234"}"#;
        let ignored = evaluate_hook(&HookInput {
            kind: HookKind::Gitlab,
            event: "Push Hook",
            body: body.as_bytes(),
            github_signature: None,
            gitlab_token: Some("secret"),
            secret: b"secret",
            branch: "main",
        });
        assert_eq!(ignored, HookOutcome::Ignored("branch filtered"));
        let ping = evaluate_hook(&HookInput {
            kind: HookKind::Github,
            event: "ping",
            body: b"{}",
            github_signature: Some(
                "sha256=5d1111e1e1111111111111111111111111111111111111111111111111111111",
            ),
            gitlab_token: None,
            secret: b"secret",
            branch: "main",
        });
        // A ping still has to carry a valid signature. The vector above is not
        // for `{}`, so it is unauthorized. The signed empty object is checked next.
        assert_eq!(ping, HookOutcome::Unauthorized);
        let mut mac = HmacSha256::new_from_slice(b"secret").unwrap();
        mac.update(b"{}");
        let digest = hex::encode(mac.finalize().into_bytes());
        let header = format!("sha256={digest}");
        let accepted = evaluate_hook(&HookInput {
            kind: HookKind::Github,
            event: "ping",
            body: b"{}",
            github_signature: Some(&header),
            gitlab_token: None,
            secret: b"secret",
            branch: "main",
        });
        assert_eq!(accepted, HookOutcome::Ignored("ping"));
        let bad_token = evaluate_hook(&HookInput {
            kind: HookKind::Gitlab,
            event: "Push Hook",
            body: body.as_bytes(),
            github_signature: None,
            gitlab_token: Some("nope"),
            secret: b"secret",
            branch: "dev",
        });
        assert_eq!(bad_token, HookOutcome::Unauthorized);
    }

    #[test]
    fn branch_rejects_traversal_and_flags() {
        assert!(validate_branch("main").is_ok());
        assert!(validate_branch("feature/web").is_ok());
        assert!(validate_branch("../main").is_err());
        assert!(validate_branch("-rf").is_err());
        assert!(validate_branch("main;rm").is_err());
    }
}
