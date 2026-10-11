//! App templates embedded from the repository's `templates/` directory.

use std::sync::OnceLock;

use rudder_core::Template;
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "../../templates"]
#[include = "*.yml"]
struct Files;

/// Every embedded template, sorted by name. Invalid files are logged and skipped;
/// a unit test keeps the shipped set valid.
pub fn all() -> &'static [Template] {
    static TEMPLATES: OnceLock<Vec<Template>> = OnceLock::new();
    TEMPLATES.get_or_init(|| {
        let mut out: Vec<Template> = Files::iter()
            .filter_map(|path| {
                let file = Files::get(&path)?;
                let text = String::from_utf8_lossy(&file.data).into_owned();
                match Template::parse(&text) {
                    Ok(template) => Some(template),
                    Err(error) => {
                        tracing::error!(%path, %error, "invalid template skipped");
                        None
                    }
                }
            })
            .collect();
        out.sort_by_key(|a| a.name.to_lowercase());
        out
    })
}

pub fn get(slug: &str) -> Option<&'static Template> {
    all().iter().find(|template| template.slug == slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_templates_are_valid_and_named_after_their_files() {
        let mut files = 0;
        for path in Files::iter() {
            files += 1;
            let file = Files::get(&path).unwrap();
            let template = Template::parse(std::str::from_utf8(&file.data).unwrap())
                .unwrap_or_else(|error| panic!("{path}: {error}"));
            assert_eq!(
                format!("{}.yml", template.slug),
                path,
                "file name matches slug"
            );
            assert!(
                template
                    .variables
                    .iter()
                    .any(|variable| variable.kind == rudder_core::VariableKind::Domain),
                "{path} has a domain variable"
            );
        }
        assert!(files >= 10, "at least ten templates ship");
        assert_eq!(all().len(), files);
        for slug in [
            "n8n",
            "uptime-kuma",
            "gitea",
            "vaultwarden",
            "umami",
            "plausible",
            "ghost",
            "wordpress",
            "minio",
            "metabase",
        ] {
            assert!(get(slug).is_some(), "{slug} template is missing");
        }
    }
}
