//! App templates: a compose file plus the variables a user fills in.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{compose_variables, transform_compose, validate_env_key, validate_name, Error};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VariableKind {
    /// Public hostname. The first domain variable becomes the stack's domain.
    Domain,
    String,
    Email,
    /// Generated when left empty.
    Secret,
    Number,
    Boolean,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateVariable {
    pub name: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub description: String,
    pub kind: VariableKind,
    #[serde(default)]
    pub default: Option<String>,
    /// Defaults to true for domains and false otherwise.
    #[serde(default)]
    pub required: Option<bool>,
    /// Length of generated secrets. Defaults to 32.
    #[serde(default)]
    pub length: Option<usize>,
}

impl TemplateVariable {
    pub fn is_required(&self) -> bool {
        self.required
            .unwrap_or(matches!(self.kind, VariableKind::Domain))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Template {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub website: String,
    /// Compose service that receives the domain.
    pub main_service: String,
    /// Container port the domain routes to.
    pub port: u16,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub variables: Vec<TemplateVariable>,
    pub compose: String,
}

/// Values chosen for a template deploy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTemplate {
    pub env: Vec<(String, String)>,
    pub domain: Option<String>,
}

impl Template {
    pub fn parse(text: &str) -> Result<Self, Error> {
        let template: Template = serde_yaml_ng::from_str(text)
            .map_err(|error| Error::Validation(format!("template is not valid: {error}")))?;
        template.validate()?;
        Ok(template)
    }

    /// Checks a template is self-consistent: every `${VAR}` in the compose file is
    /// declared, and the main service exists and can be routed.
    pub fn validate(&self) -> Result<(), Error> {
        validate_name(&self.slug)?;
        if self.name.trim().is_empty() || self.description.trim().is_empty() {
            return Err(Error::Validation(format!(
                "template {} needs a name and description",
                self.slug
            )));
        }
        if self.port == 0 {
            return Err(Error::Validation(format!(
                "template {} has port 0",
                self.slug
            )));
        }
        for variable in &self.variables {
            validate_env_key(&variable.name)?;
            if self
                .variables
                .iter()
                .filter(|other| other.name == variable.name)
                .count()
                > 1
            {
                return Err(Error::Validation(format!(
                    "template {} declares {} twice",
                    self.slug, variable.name
                )));
            }
        }
        for used in compose_variables(&self.compose) {
            if !self.variables.iter().any(|variable| variable.name == used) {
                return Err(Error::Validation(format!(
                    "template {} uses ${{{used}}} without declaring it",
                    self.slug
                )));
            }
        }
        transform_compose(&self.slug, &self.compose, &[self.main_service.as_str()])?;
        Ok(())
    }

    /// Fill in defaults and generate secrets. `generate(length)` returns a random
    /// alphanumeric string.
    pub fn resolve(
        &self,
        values: &HashMap<String, String>,
        mut generate: impl FnMut(usize) -> String,
    ) -> Result<ResolvedTemplate, Error> {
        let mut env = Vec::new();
        let mut domain = None;
        for variable in &self.variables {
            let given = values
                .get(&variable.name)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty());
            let value = match (given, &variable.default) {
                (Some(value), _) => value,
                (None, Some(default)) => default.clone(),
                (None, None) if variable.kind == VariableKind::Secret => {
                    generate(variable.length.unwrap_or(32).clamp(16, 128))
                }
                (None, None) if variable.is_required() => {
                    return Err(Error::Validation(format!(
                        "{} is required",
                        if variable.label.is_empty() {
                            &variable.name
                        } else {
                            &variable.label
                        }
                    )))
                }
                (None, None) => String::new(),
            };
            check_value(variable, &value)?;
            if variable.kind == VariableKind::Domain && domain.is_none() && !value.is_empty() {
                domain = Some(value.clone());
            }
            env.push((variable.name.clone(), value));
        }
        for name in values.keys() {
            if !self.variables.iter().any(|variable| &variable.name == name) {
                return Err(Error::Validation(format!("unknown variable {name}")));
            }
        }
        Ok(ResolvedTemplate { env, domain })
    }
}

fn check_value(variable: &TemplateVariable, value: &str) -> Result<(), Error> {
    if value.contains('\n') || value.contains('\r') {
        return Err(Error::Validation(format!(
            "{} cannot contain newlines",
            variable.name
        )));
    }
    if value.is_empty() {
        return Ok(());
    }
    let ok = match variable.kind {
        VariableKind::Domain => crate::validate_host(value).is_ok() && !value.starts_with("*."),
        VariableKind::Email => {
            let mut parts = value.split('@');
            matches!((parts.next(), parts.next(), parts.next()), (Some(user), Some(host), None) if !user.is_empty() && host.contains('.'))
        }
        VariableKind::Number => value.parse::<i64>().is_ok(),
        VariableKind::Boolean => matches!(value, "true" | "false"),
        VariableKind::String | VariableKind::Secret => true,
    };
    if ok {
        Ok(())
    } else {
        Err(Error::Validation(format!(
            "{} is not a valid {:?}",
            variable.name, variable.kind
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLATE: &str = r#"
slug: demo
name: Demo
description: A demo app.
category: Testing
main_service: web
port: 80
variables:
  - name: DOMAIN
    label: Domain
    kind: domain
  - name: ADMIN_EMAIL
    kind: email
    required: true
  - name: SECRET
    kind: secret
    length: 48
  - name: TZ
    kind: string
    default: UTC
compose: |
  services:
    web:
      image: nginx:1.27
      environment:
        URL: https://${DOMAIN}
        EMAIL: ${ADMIN_EMAIL}
        SECRET: ${SECRET}
        TZ: ${TZ}
"#;

    #[test]
    fn resolves_defaults_secrets_and_domain() {
        let template = Template::parse(TEMPLATE).unwrap();
        let values = HashMap::from([
            ("DOMAIN".to_owned(), "demo.example.com".to_owned()),
            ("ADMIN_EMAIL".to_owned(), "me@example.com".to_owned()),
        ]);
        let resolved = template.resolve(&values, |len| "s".repeat(len)).unwrap();
        assert_eq!(resolved.domain.as_deref(), Some("demo.example.com"));
        assert_eq!(
            resolved.env,
            vec![
                ("DOMAIN".into(), "demo.example.com".into()),
                ("ADMIN_EMAIL".into(), "me@example.com".into()),
                ("SECRET".into(), "s".repeat(48)),
                ("TZ".into(), "UTC".into()),
            ]
        );
    }

    #[test]
    fn rejects_missing_invalid_and_unknown_values() {
        let template = Template::parse(TEMPLATE).unwrap();
        let generate = |len: usize| "s".repeat(len);
        assert!(template.resolve(&HashMap::new(), generate).is_err());
        let bad_email = HashMap::from([
            ("DOMAIN".to_owned(), "demo.example.com".to_owned()),
            ("ADMIN_EMAIL".to_owned(), "nope".to_owned()),
        ]);
        assert!(template.resolve(&bad_email, generate).is_err());
        let bad_domain = HashMap::from([
            ("DOMAIN".to_owned(), "not a host".to_owned()),
            ("ADMIN_EMAIL".to_owned(), "me@example.com".to_owned()),
        ]);
        assert!(template.resolve(&bad_domain, generate).is_err());
        let unknown = HashMap::from([
            ("DOMAIN".to_owned(), "demo.example.com".to_owned()),
            ("ADMIN_EMAIL".to_owned(), "me@example.com".to_owned()),
            ("EXTRA".to_owned(), "x".to_owned()),
        ]);
        assert!(template.resolve(&unknown, generate).is_err());
    }

    #[test]
    fn undeclared_variables_and_missing_services_fail_validation() {
        let undeclared = TEMPLATE.replace("TZ: ${TZ}", "TZ: ${ZONE}");
        assert!(Template::parse(&undeclared).is_err());
        let wrong_service = TEMPLATE.replace("main_service: web", "main_service: api");
        assert!(Template::parse(&wrong_service).is_err());
    }
}
