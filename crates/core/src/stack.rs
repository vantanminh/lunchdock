//! Rewrite a user's compose file so it runs under a Rudder project name and
//! routed services join the shared proxy network.

use serde_yaml_ng::{Mapping, Value};

use crate::{validate_name, Error, PROXY_NETWORK};

/// Largest compose file accepted, in bytes.
pub const MAX_COMPOSE_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackService {
    pub name: String,
    /// Container name Traefik routes to on the proxy network.
    pub container: String,
    pub image: Option<String>,
    /// The service has a `build:` section, so deploying builds it from source.
    pub build: bool,
    pub routed: bool,
}

#[derive(Debug, Clone)]
pub struct StackPlan {
    pub yaml: String,
    pub services: Vec<StackService>,
    pub warnings: Vec<String>,
}

fn key(name: &str) -> Value {
    Value::String(name.to_owned())
}

/// Compose service names: letters, digits, `.`, `_`, `-`, starting with a letter or digit.
pub fn validate_compose_service(name: &str) -> Result<(), Error> {
    let valid = !name.is_empty()
        && name.len() <= 63
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(Error::Validation(format!(
            "invalid compose service name '{name}'"
        )))
    }
}

/// Parse and rewrite `source`:
///
/// - top-level `name` becomes `project`;
/// - `container_name` values gain the `project-` prefix;
/// - every service gets `rudder.*` labels;
/// - services in `routed` join the external `rudder-proxy` network.
///
/// Routing itself is written by the panel to Traefik's file provider and
/// targets [`StackService::container`].
pub fn transform_compose(project: &str, source: &str, routed: &[&str]) -> Result<StackPlan, Error> {
    validate_name(project)?;
    if source.len() > MAX_COMPOSE_BYTES {
        return Err(Error::Validation(
            "compose file is larger than 256 KiB".into(),
        ));
    }
    let mut doc: Value = serde_yaml_ng::from_str(source)
        .map_err(|error| Error::Validation(format!("compose file is not valid YAML: {error}")))?;
    doc.apply_merge()
        .map_err(|error| Error::Validation(format!("compose merge keys: {error}")))?;
    let root = doc
        .as_mapping_mut()
        .ok_or_else(|| Error::Validation("compose file must be a mapping".into()))?;
    let mut warnings = Vec::new();
    if let Some(Value::String(name)) = root.get("name") {
        if name != project {
            warnings.push(format!(
                "top-level name '{name}' is replaced by '{project}'"
            ));
        }
    }
    root.remove("version");
    root.insert(key("name"), key(project));

    for name in routed {
        validate_compose_service(name)?;
    }
    let services = root
        .get_mut("services")
        .and_then(Value::as_mapping_mut)
        .filter(|services| !services.is_empty())
        .ok_or_else(|| Error::Validation("compose file has no services".into()))?;
    let mut out = Vec::new();
    for (service_key, body) in services.iter_mut() {
        let name = service_key
            .as_str()
            .ok_or_else(|| Error::Validation("service names must be strings".into()))?
            .to_owned();
        validate_compose_service(&name)?;
        let service = body
            .as_mapping_mut()
            .ok_or_else(|| Error::Validation(format!("service '{name}' must be a mapping")))?;
        let is_routed = routed.contains(&name.as_str());
        let container = match service.get("container_name") {
            Some(Value::String(custom)) => {
                validate_compose_service(custom)?;
                let prefixed = if custom.starts_with(&format!("{project}-")) {
                    custom.clone()
                } else {
                    format!("{project}-{custom}")
                };
                service.insert(key("container_name"), key(&prefixed));
                prefixed
            }
            Some(_) => {
                return Err(Error::Validation(format!(
                    "service '{name}' has a non-string container_name"
                )))
            }
            None => format!("{project}-{name}-1"),
        };
        if is_routed {
            if let Some(mode) = service.get("network_mode").and_then(Value::as_str) {
                return Err(Error::Validation(format!(
                    "service '{name}' uses network_mode '{mode}' and cannot be routed by Traefik"
                )));
            }
            attach_proxy_network(service, &name)?;
            if service.contains_key("ports") {
                warnings.push(format!(
                    "service '{name}' publishes host ports; Traefik reaches it on {PROXY_NETWORK}, so they are usually unnecessary"
                ));
            }
            let replicas = service
                .get("deploy")
                .and_then(|deploy| deploy.get("replicas"))
                .and_then(Value::as_u64)
                .unwrap_or(1);
            if replicas > 1 {
                warnings.push(format!(
                    "service '{name}' has {replicas} replicas; Traefik routes to the first one"
                ));
            }
        }
        add_labels(service, project, &name)?;
        if !service.contains_key("oom_score_adj") {
            // Under memory pressure the kernel kills stack containers before the panel.
            service.insert(
                key("oom_score_adj"),
                Value::Number(crate::WORKLOAD_OOM_SCORE_ADJ.into()),
            );
        }
        out.push(StackService {
            image: service
                .get("image")
                .and_then(Value::as_str)
                .map(str::to_owned),
            build: service.contains_key("build"),
            name,
            container,
            routed: is_routed,
        });
    }
    for name in routed {
        if !out.iter().any(|service| service.name == *name) {
            return Err(Error::Validation(format!(
                "domain targets service '{name}', which is not in the compose file"
            )));
        }
    }
    if out.iter().any(|service| service.routed) {
        let networks = root
            .entry(key("networks"))
            .or_insert_with(|| Value::Mapping(Mapping::new()));
        if networks.is_null() {
            *networks = Value::Mapping(Mapping::new());
        }
        let networks = networks
            .as_mapping_mut()
            .ok_or_else(|| Error::Validation("top-level networks must be a mapping".into()))?;
        let mut proxy = Mapping::new();
        proxy.insert(key("name"), key(PROXY_NETWORK));
        proxy.insert(key("external"), Value::Bool(true));
        networks.insert(key(PROXY_NETWORK), Value::Mapping(proxy));
    }
    let yaml = serde_yaml_ng::to_string(&doc)
        .map_err(|error| Error::Validation(format!("could not write compose file: {error}")))?;
    Ok(StackPlan {
        yaml,
        services: out,
        warnings,
    })
}

fn attach_proxy_network(service: &mut Mapping, name: &str) -> Result<(), Error> {
    let proxy = key(PROXY_NETWORK);
    match service.get_mut("networks") {
        None | Some(Value::Null) => {
            service.insert(
                key("networks"),
                Value::Sequence(vec![key("default"), proxy]),
            );
        }
        Some(Value::Sequence(list)) => {
            if !list.contains(&proxy) {
                list.push(proxy);
            }
        }
        Some(Value::Mapping(map)) => {
            if !map.contains_key(&proxy) {
                map.insert(proxy, Value::Null);
            }
        }
        Some(_) => {
            return Err(Error::Validation(format!(
                "service '{name}' has an invalid networks value"
            )))
        }
    }
    Ok(())
}

fn add_labels(service: &mut Mapping, project: &str, name: &str) -> Result<(), Error> {
    let mut labels = match service.remove("labels") {
        None | Some(Value::Null) => Mapping::new(),
        Some(Value::Mapping(map)) => map,
        Some(Value::Sequence(list)) => {
            let mut map = Mapping::new();
            for item in list {
                let text = item.as_str().ok_or_else(|| {
                    Error::Validation(format!("service '{name}' has a non-string label"))
                })?;
                let (label, value) = text.split_once('=').unwrap_or((text, ""));
                map.insert(key(label), key(value));
            }
            map
        }
        Some(_) => {
            return Err(Error::Validation(format!(
                "service '{name}' has invalid labels"
            )))
        }
    };
    labels.insert(key("rudder.managed"), key("true"));
    labels.insert(key("rudder.project"), key(project));
    labels.insert(key("rudder.service"), key(name));
    service.insert(key("labels"), Value::Mapping(labels));
    Ok(())
}

/// A variable a compose file reads with `${NAME}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariableRef {
    pub name: String,
    /// Every use has a fallback (`${NAME:-x}`), so the stack runs without a value.
    pub has_default: bool,
    /// Some use is `${NAME?error}` or `${NAME:?error}`: compose refuses to start without it.
    pub required: bool,
}

/// Variables referenced as `${NAME}`, `${NAME:-default}`, or `${NAME?err}`, in order of first use.
pub fn compose_variable_refs(source: &str) -> Vec<VariableRef> {
    let mut found: Vec<VariableRef> = Vec::new();
    let bytes = source.as_bytes();
    let mut index = 0;
    while index + 1 < bytes.len() {
        if bytes[index] == b'$' && bytes[index + 1] == b'$' {
            index += 2;
            continue;
        }
        if bytes[index] == b'$' && bytes[index + 1] == b'{' {
            let start = index + 2;
            let mut end = start;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            if end > start {
                let name = &source[start..end];
                let rest = &source[end..];
                let default = rest.starts_with(":-") || rest.starts_with('-');
                let required = rest.starts_with(":?") || rest.starts_with('?');
                match found.iter_mut().find(|existing| existing.name == name) {
                    Some(existing) => {
                        existing.has_default &= default;
                        existing.required |= required;
                    }
                    None => found.push(VariableRef {
                        name: name.to_owned(),
                        has_default: default,
                        required,
                    }),
                }
            }
            index = end;
            continue;
        }
        index += 1;
    }
    found
}

/// Names referenced as `${NAME}`, `${NAME:-default}`, or `${NAME?err}`, in order of first use.
pub fn compose_variables(source: &str) -> Vec<String> {
    compose_variable_refs(source)
        .into_iter()
        .map(|variable| variable.name)
        .collect()
}

/// Render a `.env` file. Values are single-quoted so compose does not interpolate them.
pub fn render_env_file(vars: &[(String, String)]) -> Result<String, Error> {
    let mut out = String::new();
    for (name, value) in vars {
        crate::validate_env_key(name)?;
        if value.contains('\n') || value.contains('\r') {
            return Err(Error::Validation(format!(
                "{name} contains a newline, which .env files cannot hold"
            )));
        }
        out.push_str(name);
        out.push('=');
        if value.contains('\'') {
            out.push('"');
            for ch in value.chars() {
                if matches!(ch, '"' | '\\' | '$') {
                    out.push('\\');
                }
                out.push(ch);
            }
            out.push('"');
        } else {
            out.push('\'');
            out.push_str(value);
            out.push('\'');
        }
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
version: "3.8"
name: something-else
x-common: &common
  restart: unless-stopped
services:
  web:
    <<: *common
    image: nginx:1.27
    ports: ["8080:80"]
    labels:
      - "com.example=1"
  db:
    image: postgres:16
    container_name: pg
    environment:
      POSTGRES_PASSWORD: ${DB_PASSWORD:?set it}
    volumes:
      - data:/var/lib/postgresql/data
volumes:
  data: {}
"#;

    #[test]
    fn routed_services_join_the_proxy_network_with_labels_and_prefixes() {
        let plan = transform_compose("blog-1a2b3c", SAMPLE, &["web"]).unwrap();
        let doc: Value = serde_yaml_ng::from_str(&plan.yaml).unwrap();
        assert_eq!(doc["name"], "blog-1a2b3c");
        assert!(doc.get("version").is_none());
        let web = &doc["services"]["web"];
        assert_eq!(web["restart"], "unless-stopped");
        assert_eq!(
            web["networks"],
            Value::Sequence(vec![key("default"), key("rudder-proxy")])
        );
        assert_eq!(web["labels"]["com.example"], "1");
        assert_eq!(web["oom_score_adj"], crate::WORKLOAD_OOM_SCORE_ADJ);
        assert_eq!(web["labels"]["rudder.project"], "blog-1a2b3c");
        let db = &doc["services"]["db"];
        assert_eq!(db["container_name"], "blog-1a2b3c-pg");
        assert!(
            db.get("networks").is_none(),
            "unrouted services stay private"
        );
        assert_eq!(doc["networks"]["rudder-proxy"]["external"], true);
        assert_eq!(doc["networks"]["rudder-proxy"]["name"], "rudder-proxy");
        let web_service = plan.services.iter().find(|s| s.name == "web").unwrap();
        assert_eq!(web_service.container, "blog-1a2b3c-web-1");
        assert!(web_service.routed);
        let db_service = plan.services.iter().find(|s| s.name == "db").unwrap();
        assert_eq!(db_service.container, "blog-1a2b3c-pg");
        assert!(plan
            .warnings
            .iter()
            .any(|w| w.contains("publishes host ports")));
        assert!(plan.warnings.iter().any(|w| w.contains("something-else")));
        assert!(plan.yaml.contains("${DB_PASSWORD:?set it}"));
    }

    #[test]
    fn unrouted_stacks_do_not_touch_networks_and_maps_are_extended() {
        let plan = transform_compose("app", "services:\n  a:\n    image: x\n", &[]).unwrap();
        assert!(!plan.yaml.contains("rudder-proxy"));
        let mapped =
            "services:\n  a:\n    image: x\n    networks:\n      back: {}\nnetworks:\n  back: {}\n";
        let plan = transform_compose("app", mapped, &["a"]).unwrap();
        let doc: Value = serde_yaml_ng::from_str(&plan.yaml).unwrap();
        assert!(doc["services"]["a"]["networks"]
            .as_mapping()
            .unwrap()
            .contains_key("rudder-proxy"));
        assert!(doc["networks"].as_mapping().unwrap().contains_key("back"));
    }

    #[test]
    fn rejects_invalid_input() {
        assert!(transform_compose("Bad", SAMPLE, &[]).is_err());
        assert!(transform_compose("app", "- a\n- b\n", &[]).is_err());
        assert!(transform_compose("app", "services: {}\n", &[]).is_err());
        assert!(transform_compose("app", "services:\n  a: [1]\n", &[]).is_err());
        assert!(transform_compose("app", SAMPLE, &["missing"]).is_err());
        let host = "services:\n  a:\n    image: x\n    network_mode: host\n";
        assert!(transform_compose("app", host, &["a"]).is_err());
        assert!(transform_compose("app", host, &[]).is_ok());
        assert!(transform_compose("app", "services:\n  'a b':\n    image: x\n", &[]).is_err());
        assert!(transform_compose("app", ": : :", &[]).is_err());
        let big = format!(
            "services:\n  a:\n    image: x\n# {}\n",
            "x".repeat(MAX_COMPOSE_BYTES)
        );
        assert!(transform_compose("app", &big, &[]).is_err());
    }

    #[test]
    fn keeps_an_oom_score_the_file_sets() {
        let source = "services:\n  a:\n    image: x\n    oom_score_adj: -100\n  b:\n    image: y\n";
        let plan = transform_compose("app", source, &[]).unwrap();
        let doc: Value = serde_yaml_ng::from_str(&plan.yaml).unwrap();
        assert_eq!(doc["services"]["a"]["oom_score_adj"], -100);
        assert_eq!(
            doc["services"]["b"]["oom_score_adj"],
            crate::WORKLOAD_OOM_SCORE_ADJ
        );
    }

    #[test]
    fn finds_variables_and_renders_env_files() {
        assert_eq!(
            compose_variables("a: ${A}\nb: ${B:-x} $${NOT} ${A} ${C?e}"),
            vec!["A", "B", "C"]
        );
        let refs = compose_variable_refs("a: ${A} ${A:-x}\nb: ${B:-x} ${B-y}\nc: ${C:?need}");
        assert_eq!(
            refs,
            vec![
                VariableRef {
                    name: "A".into(),
                    has_default: false,
                    required: false
                },
                VariableRef {
                    name: "B".into(),
                    has_default: true,
                    required: false
                },
                VariableRef {
                    name: "C".into(),
                    has_default: false,
                    required: true
                },
            ]
        );
        let env = render_env_file(&[
            ("A".into(), "plain $value".into()),
            ("B".into(), "it's".into()),
        ])
        .unwrap();
        assert_eq!(env, "A='plain $value'\nB=\"it's\"\n");
        assert!(render_env_file(&[("A".into(), "x\ny".into())]).is_err());
        assert!(render_env_file(&[("1A".into(), "x".into())]).is_err());
    }
}
