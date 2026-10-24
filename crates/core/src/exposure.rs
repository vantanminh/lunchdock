//! Classify a response from a check against one of our own domains.
//!
//! The check looks for files that are often left public by a generated app.
//! The body is used only to decide. Callers must not store it.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    pub path: &'static str,
    pub family: &'static str,
    /// Severity when the path answers and is not the app's normal HTML page.
    pub severity: &'static str,
}

/// Paths scanners try. Kept short on purpose: this is a warning, not an attack.
pub const PROBES: &[Probe] = &[
    Probe {
        path: "/.env",
        family: "env",
        severity: "high",
    },
    Probe {
        path: "/.env.local",
        family: "env",
        severity: "high",
    },
    Probe {
        path: "/.env.production",
        family: "env",
        severity: "high",
    },
    Probe {
        path: "/.env.development",
        family: "env",
        severity: "high",
    },
    Probe {
        path: "/.npmrc",
        family: "env",
        severity: "high",
    },
    Probe {
        path: "/wp-admin",
        family: "wordpress",
        severity: "medium",
    },
    Probe {
        path: "/wp-login.php",
        family: "wordpress",
        severity: "medium",
    },
    Probe {
        path: "/wp-config.php",
        family: "wordpress",
        severity: "critical",
    },
    Probe {
        path: "/.git/HEAD",
        family: "git",
        severity: "critical",
    },
    Probe {
        path: "/.git/config",
        family: "git",
        severity: "critical",
    },
    Probe {
        path: "/firebase.json",
        family: "firebase",
        severity: "medium",
    },
    Probe {
        path: "/.firebase/hosting.json",
        family: "firebase",
        severity: "medium",
    },
    Probe {
        path: "/firebase-debug.log",
        family: "firebase",
        severity: "high",
    },
    Probe {
        path: "/serviceAccountKey.json",
        family: "firebase",
        severity: "critical",
    },
    Probe {
        path: "/service-account.json",
        family: "firebase",
        severity: "critical",
    },
    Probe {
        path: "/__/firebase/init.json",
        family: "firebase",
        severity: "low",
    },
    Probe {
        path: "/.aws/credentials",
        family: "cloud",
        severity: "critical",
    },
    Probe {
        path: "/id_rsa",
        family: "keys",
        severity: "critical",
    },
    Probe {
        path: "/.ssh/id_rsa",
        family: "keys",
        severity: "critical",
    },
    Probe {
        path: "/phpinfo.php",
        family: "debug",
        severity: "high",
    },
    Probe {
        path: "/actuator/env",
        family: "debug",
        severity: "critical",
    },
    Probe {
        path: "/debug/vars",
        family: "debug",
        severity: "high",
    },
    Probe {
        path: "/backup.sql",
        family: "dump",
        severity: "high",
    },
    Probe {
        path: "/dump.sql",
        family: "dump",
        severity: "high",
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exposure {
    pub severity: &'static str,
    pub summary: String,
}

/// `None` when the response is not worth a warning.
pub fn classify_probe(
    probe: &Probe,
    status: u16,
    content_type: &str,
    body: &str,
) -> Option<Exposure> {
    if !matches!(status, 200 | 206) {
        return None;
    }
    let sample = &body[..body.len().min(8192)];
    if looks_like_secret(sample) {
        return Some(Exposure {
            severity: "critical",
            summary: format!(
                "{} answered and the response looks like a secret. The contents were not saved.",
                probe.path
            ),
        });
    }
    let html = content_type.to_ascii_lowercase().contains("text/html")
        || sample.to_ascii_lowercase().contains("<html");
    let wordpress = probe.family == "wordpress" && probe.path != "/wp-config.php";
    if html && !wordpress {
        return None;
    }
    if sample.trim().is_empty() {
        return None;
    }
    Some(Exposure {
        severity: probe.severity,
        summary: format!(
            "{} is reachable ({}, {}). Confirm it should be public.",
            probe.path, probe.family, status
        ),
    })
}

fn looks_like_secret(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    if lower.contains("private key")
        || lower.contains("private_key")
        || lower.contains("begin rsa")
        || lower.contains("begin openssh")
        || lower.contains("begin certificate")
        || body.contains("AKIA")
        || lower.contains("\"type\": \"service_account\"")
        || lower.contains("\"type\":\"service_account\"")
    {
        return true;
    }
    let assignments = body
        .lines()
        .filter(|line| {
            let line = line.trim();
            let Some((key, value)) = line.split_once('=') else {
                return false;
            };
            !value.is_empty()
                && key.len() >= 2
                && key
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        })
        .count();
    assignments >= 3
}

/// `true` when a host is an IP address rather than a name.
pub fn host_is_ip(host: &str) -> bool {
    let host = host.trim().trim_matches(['[', ']']);
    if host.parse::<std::net::Ipv4Addr>().is_ok() {
        return true;
    }
    host.parse::<std::net::Ipv6Addr>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_fallback_is_not_an_exposed_env_file() {
        let probe = PROBES.iter().find(|probe| probe.path == "/.env").unwrap();
        assert!(classify_probe(probe, 200, "text/html", "<html>app</html>").is_none());
        assert!(classify_probe(probe, 404, "text/plain", "missing").is_none());
    }

    #[test]
    fn env_assignments_are_critical_and_not_echoed() {
        let probe = PROBES.iter().find(|probe| probe.path == "/.env").unwrap();
        let body = "DATABASE_URL=postgres://secret\nAPI_KEY=abc\nFIREBASE_KEY=xyz\n";
        let finding = classify_probe(probe, 200, "text/plain", body).unwrap();
        assert_eq!(finding.severity, "critical");
        assert!(!finding.summary.contains("postgres://secret"));
        assert!(!finding.summary.contains("API_KEY"));
    }

    #[test]
    fn wordpress_admin_html_is_still_reported() {
        let probe = PROBES
            .iter()
            .find(|probe| probe.path == "/wp-admin")
            .unwrap();
        let finding = classify_probe(probe, 200, "text/html", "<html>login</html>").unwrap();
        assert_eq!(finding.severity, "medium");
    }

    #[test]
    fn ip_hosts_are_recognized() {
        assert!(host_is_ip("203.0.113.8"));
        assert!(host_is_ip("[2001:db8::1]"));
        assert!(!host_is_ip("app.example.com"));
    }
}
