//! Facts about the machine for the session greeting.

pub fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .map(|name| name.trim().to_owned())
        .unwrap_or_default()
}

/// `PRETTY_NAME` from os-release, such as `Ubuntu 24.04 LTS`.
pub fn os_info() -> String {
    let text = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .unwrap_or_default();
    pretty_name(&text).unwrap_or_else(|| std::env::consts::OS.to_owned())
}

fn pretty_name(os_release: &str) -> Option<String> {
    os_release.lines().find_map(|line| {
        line.strip_prefix("PRETTY_NAME=")
            .map(|value| value.trim_matches('"').to_owned())
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_pretty_name() {
        let text = "NAME=\"Ubuntu\"\nPRETTY_NAME=\"Ubuntu 24.04 LTS\"\nID=ubuntu\n";
        assert_eq!(
            super::pretty_name(text).as_deref(),
            Some("Ubuntu 24.04 LTS")
        );
        assert_eq!(super::pretty_name("ID=alpine"), None);
    }
}
