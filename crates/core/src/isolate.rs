//! Per-app network isolation.
//!
//! When isolation is on, an app keeps the port it listens on inside its own
//! network namespace and is not published on the host. Two apps can both listen
//! on port 80. Traefik reaches each one by its name on [`crate::PROXY_NETWORK`],
//! which has its own address.

use serde_json::Value;

use crate::driver::PortMapping;

/// Missing `network_isolation` means on. An explicit `false` publishes host ports.
pub fn network_isolation_enabled(spec: &Value) -> bool {
    spec.get("network_isolation")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

/// Drop host port bindings. The container port stays so the proxy can reach it.
pub fn isolated_ports(ports: Vec<PortMapping>, isolate: bool) -> Vec<PortMapping> {
    if !isolate {
        return ports;
    }
    ports
        .into_iter()
        .map(|mut port| {
            port.host = None;
            port
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn isolation_defaults_on_and_can_be_turned_off() {
        assert!(network_isolation_enabled(&json!({})));
        assert!(network_isolation_enabled(
            &json!({ "network_isolation": true })
        ));
        assert!(!network_isolation_enabled(
            &json!({ "network_isolation": false })
        ));
    }

    #[test]
    fn isolated_ports_keep_the_container_port_only() {
        let ports = vec![PortMapping {
            container: 80,
            host: Some(80),
            protocol: "tcp".into(),
        }];
        let isolated = isolated_ports(ports.clone(), true);
        assert_eq!(isolated[0].container, 80);
        assert_eq!(isolated[0].host, None);
        assert_eq!(isolated_ports(ports, false)[0].host, Some(80));
    }
}
