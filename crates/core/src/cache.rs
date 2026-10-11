//! HTTP cache policies shared by the panel and the cache server.
//!
//! The panel renders a [`CacheDocument`] per server. The cache server loads it,
//! finds the [`CacheSite`] for each request, and asks the site's
//! [`CachePolicy`] whether to look up the cache and for how long to keep a
//! response. Everything here is pure so both sides agree on the result.

use serde::{Deserialize, Serialize};

use crate::{validate_host, validate_path_prefix, Error};

/// Path of the rules file inside the node data directory.
pub const CACHE_RULES_FILE: &str = "cache/rules.json";
/// Admin bearer token file inside the node data directory. The cache server creates it.
pub const CACHE_TOKEN_FILE: &str = "cache/admin.token";
/// Container name and port Traefik forwards cached domains to.
pub const CACHE_CONTAINER: &str = "rudder-cache";
pub const CACHE_PORT: u16 = 8080;
pub const DEFAULT_MEMORY_BYTES: u64 = 256 * 1024 * 1024;
pub const DEFAULT_MAX_OBJECT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheDocument {
    #[serde(default)]
    pub version: u64,
    #[serde(default)]
    pub memory_max_bytes: Option<u64>,
    #[serde(default)]
    pub sites: Vec<CacheSite>,
    #[serde(default)]
    pub policies: Vec<CachePolicy>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheSite {
    pub host: String,
    #[serde(default = "root")]
    pub path_prefix: String,
    /// `host:port` of the origin container.
    pub upstream: String,
    pub policy: String,
}

fn root() -> String {
    "/".into()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachePolicy {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub max_object_bytes: Option<u64>,
    #[serde(default)]
    pub rules: Vec<CacheRule>,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheRule {
    #[serde(default)]
    pub name: String,
    #[serde(rename = "match", default)]
    pub matcher: RuleMatch,
    pub action: RuleAction,
}

/// Every set field must match. An empty matcher matches every request.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleMatch {
    /// Path must start with one of these.
    pub path_prefixes: Vec<String>,
    /// File extensions without the dot, compared case-insensitively.
    pub extensions: Vec<String>,
    /// `*` matches any run of characters; anchored at both ends.
    pub path_glob: Option<String>,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleMode {
    #[default]
    Cache,
    Bypass,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleAction {
    pub mode: RuleMode,
    /// Freshness when the origin gives none, or always when `respect_origin` is false.
    pub ttl_secs: u32,
    /// Use `s-maxage`/`max-age` from the origin when present, and honor `no-store`/`private`.
    pub respect_origin: bool,
    pub stale_while_revalidate_secs: u32,
    pub stale_if_error_secs: u32,
    /// Drop the query string from the cache key.
    pub ignore_query: bool,
    /// When non-empty, only these query parameters are part of the key (sorted).
    pub query_keys: Vec<String>,
    /// Requests carrying a cookie whose name starts with one of these skip the cache.
    pub bypass_cookies: Vec<String>,
    /// Response statuses that may be stored.
    pub statuses: Vec<u16>,
}

impl Default for RuleAction {
    fn default() -> Self {
        Self {
            mode: RuleMode::Cache,
            ttl_secs: 300,
            respect_origin: true,
            stale_while_revalidate_secs: 0,
            stale_if_error_secs: 0,
            ignore_query: false,
            query_keys: Vec::new(),
            bypass_cookies: Vec::new(),
            statuses: vec![200, 203, 204, 300, 301, 308, 404, 410],
        }
    }
}

/// What a request carries that affects the decision.
#[derive(Debug, Clone, Copy)]
pub struct CacheRequest<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub query: Option<&'a str>,
    pub cookie: Option<&'a str>,
    pub authorization: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestDecision {
    Bypass(&'static str),
    /// Look up the cache. `key` is the normalized path and query.
    Lookup {
        rule: usize,
        key: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Freshness {
    pub ttl_secs: u32,
    pub stale_while_revalidate_secs: u32,
    pub stale_if_error_secs: u32,
}

impl CacheDocument {
    pub fn parse(text: &str) -> Result<Self, Error> {
        let document: Self = serde_json::from_str(text)
            .map_err(|error| Error::Validation(format!("cache rules are not valid: {error}")))?;
        document.validate()?;
        Ok(document)
    }

    pub fn validate(&self) -> Result<(), Error> {
        for policy in &self.policies {
            policy.validate()?;
        }
        for site in &self.sites {
            validate_host(&site.host)?;
            validate_path_prefix(&site.path_prefix)?;
            validate_upstream(&site.upstream)?;
            if !self.policies.iter().any(|policy| policy.id == site.policy) {
                return Err(Error::Validation(format!(
                    "site {} uses unknown policy {}",
                    site.host, site.policy
                )));
            }
        }
        Ok(())
    }

    /// The site with the longest matching path prefix for `host`.
    pub fn site(&self, host: &str, path: &str) -> Option<&CacheSite> {
        let host = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
        self.sites
            .iter()
            .filter(|site| site.host == host && prefix_matches(&site.path_prefix, path))
            .max_by_key(|site| site.path_prefix.len())
    }

    pub fn policy(&self, id: &str) -> Option<&CachePolicy> {
        self.policies.iter().find(|policy| policy.id == id)
    }
}

fn prefix_matches(prefix: &str, path: &str) -> bool {
    prefix == "/"
        || path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/') || prefix.ends_with('/'))
}

pub fn validate_upstream(upstream: &str) -> Result<(), Error> {
    let (host, port) = upstream
        .rsplit_once(':')
        .ok_or_else(|| Error::Validation("upstream must be host:port".into()))?;
    let host_ok = !host.is_empty()
        && host.len() <= 253
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'));
    if !host_ok || port.parse::<u16>().map_or(true, |port| port == 0) {
        return Err(Error::Validation(format!("invalid upstream {upstream}")));
    }
    Ok(())
}

impl CachePolicy {
    pub fn validate(&self) -> Result<(), Error> {
        if self.id.is_empty() || self.name.trim().is_empty() {
            return Err(Error::Validation(
                "cache policy needs an id and a name".into(),
            ));
        }
        if self.rules.len() > 64 {
            return Err(Error::Validation("a policy has at most 64 rules".into()));
        }
        for rule in &self.rules {
            rule.validate()?;
        }
        Ok(())
    }

    pub fn max_object_bytes(&self) -> u64 {
        self.max_object_bytes.unwrap_or(DEFAULT_MAX_OBJECT_BYTES)
    }

    /// First matching rule wins. With no match the request bypasses the cache.
    pub fn decide(&self, request: &CacheRequest<'_>) -> RequestDecision {
        if request.method != "GET" {
            return RequestDecision::Bypass("method");
        }
        if request.authorization {
            return RequestDecision::Bypass("authorization");
        }
        let Some((index, rule)) = self
            .rules
            .iter()
            .enumerate()
            .find(|(_, rule)| rule.matcher.matches(request.path))
        else {
            return RequestDecision::Bypass("no_rule");
        };
        if rule.action.mode == RuleMode::Bypass {
            return RequestDecision::Bypass("rule");
        }
        if let Some(cookie) = request.cookie {
            if has_bypass_cookie(cookie, &rule.action.bypass_cookies) {
                return RequestDecision::Bypass("cookie");
            }
        }
        RequestDecision::Lookup {
            rule: index,
            key: cache_path(request.path, request.query, &rule.action),
        }
    }
}

impl CacheRule {
    pub fn validate(&self) -> Result<(), Error> {
        for prefix in &self.matcher.path_prefixes {
            if !prefix.starts_with('/')
                || prefix.len() > 200
                || prefix.contains(char::is_whitespace)
            {
                return Err(Error::Validation(format!("invalid path prefix {prefix}")));
            }
        }
        for extension in &self.matcher.extensions {
            if extension.is_empty()
                || extension.len() > 16
                || !extension.bytes().all(|b| b.is_ascii_alphanumeric())
            {
                return Err(Error::Validation(format!("invalid extension {extension}")));
            }
        }
        if let Some(glob) = &self.matcher.path_glob {
            if !glob.starts_with('/') && !glob.starts_with('*') || glob.len() > 200 {
                return Err(Error::Validation(format!("invalid path glob {glob}")));
            }
        }
        let action = &self.action;
        if action.ttl_secs > 365 * 86_400
            || action.stale_while_revalidate_secs > 30 * 86_400
            || action.stale_if_error_secs > 30 * 86_400
        {
            return Err(Error::Validation(
                "ttl is at most a year and stale windows at most 30 days".into(),
            ));
        }
        if action
            .statuses
            .iter()
            .any(|status| !(200..600).contains(status))
        {
            return Err(Error::Validation(
                "cacheable statuses must be 200-599".into(),
            ));
        }
        for name in action.query_keys.iter().chain(&action.bypass_cookies) {
            if name.is_empty() || name.len() > 128 || name.contains([';', '=', ' ', '&']) {
                return Err(Error::Validation(format!("invalid name {name}")));
            }
        }
        Ok(())
    }

    /// How long to keep a response, or `None` when it must not be stored.
    pub fn freshness(
        &self,
        status: u16,
        cache_control: Option<&str>,
        set_cookie: bool,
        vary: Option<&str>,
    ) -> Option<Freshness> {
        let action = &self.action;
        if action.mode == RuleMode::Bypass || set_cookie || !action.statuses.contains(&status) {
            return None;
        }
        if let Some(vary) = vary {
            let unsupported = vary
                .split(',')
                .map(str::trim)
                .any(|name| !name.eq_ignore_ascii_case("accept-encoding") && !name.is_empty());
            if unsupported {
                return None;
            }
        }
        let directives = cache_control.map(CacheControl::parse).unwrap_or_default();
        if action.respect_origin {
            if directives.no_store || directives.private || directives.no_cache {
                return None;
            }
            let ttl = directives
                .s_maxage
                .or(directives.max_age)
                .unwrap_or(action.ttl_secs);
            if ttl == 0 {
                return None;
            }
            return Some(Freshness {
                ttl_secs: ttl,
                stale_while_revalidate_secs: directives
                    .stale_while_revalidate
                    .unwrap_or(action.stale_while_revalidate_secs),
                stale_if_error_secs: directives
                    .stale_if_error
                    .unwrap_or(action.stale_if_error_secs),
            });
        }
        if action.ttl_secs == 0 {
            return None;
        }
        Some(Freshness {
            ttl_secs: action.ttl_secs,
            stale_while_revalidate_secs: action.stale_while_revalidate_secs,
            stale_if_error_secs: action.stale_if_error_secs,
        })
    }
}

impl RuleMatch {
    pub fn matches(&self, path: &str) -> bool {
        if !self.path_prefixes.is_empty()
            && !self
                .path_prefixes
                .iter()
                .any(|prefix| path.starts_with(prefix))
        {
            return false;
        }
        if !self.extensions.is_empty() {
            let file = path.rsplit('/').next().unwrap_or("");
            let Some((_, extension)) = file.rsplit_once('.') else {
                return false;
            };
            if !self
                .extensions
                .iter()
                .any(|wanted| wanted.eq_ignore_ascii_case(extension))
            {
                return false;
            }
        }
        if let Some(glob) = &self.path_glob {
            if !glob_matches(glob, path) {
                return false;
            }
        }
        true
    }
}

fn glob_matches(glob: &str, text: &str) -> bool {
    let parts: Vec<&str> = glob.split('*').collect();
    if parts.len() == 1 {
        return glob == text;
    }
    let mut rest = text;
    let first = parts[0];
    if !rest.starts_with(first) {
        return false;
    }
    rest = &rest[first.len()..];
    let last = parts[parts.len() - 1];
    for middle in &parts[1..parts.len() - 1] {
        match rest.find(middle) {
            Some(at) => rest = &rest[at + middle.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

fn has_bypass_cookie(header: &str, prefixes: &[String]) -> bool {
    header.split(';').any(|pair| {
        let name = pair.split('=').next().unwrap_or("").trim();
        !name.is_empty()
            && prefixes
                .iter()
                .any(|prefix| name.starts_with(prefix.as_str()))
    })
}

/// The path and query that identify a cached object for a rule.
pub fn cache_path(path: &str, query: Option<&str>, action: &RuleAction) -> String {
    let query = query.filter(|q| !q.is_empty() && !action.ignore_query);
    let Some(query) = query else {
        return path.to_owned();
    };
    let mut pairs: Vec<&str> = query.split('&').filter(|pair| !pair.is_empty()).collect();
    if !action.query_keys.is_empty() {
        pairs.retain(|pair| {
            let name = pair.split('=').next().unwrap_or("");
            action.query_keys.iter().any(|key| key == name)
        });
    }
    pairs.sort_unstable();
    if pairs.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{}", pairs.join("&"))
    }
}

/// `br`, `gzip`, or `identity`: the encoding a cached object is stored under.
pub fn encoding_class(accept_encoding: Option<&str>) -> &'static str {
    let Some(value) = accept_encoding else {
        return "identity";
    };
    let accepts = |name: &str| {
        value.split(',').any(|item| {
            let mut parts = item.split(';');
            let coding = parts.next().unwrap_or("").trim();
            let refused = parts.any(|param| {
                let param = param.trim();
                param == "q=0" || param == "q=0.0" || param == "q=0.00" || param == "q=0.000"
            });
            coding.eq_ignore_ascii_case(name) && !refused
        })
    };
    if accepts("br") {
        "br"
    } else if accepts("gzip") {
        "gzip"
    } else {
        "identity"
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheControl {
    pub no_store: bool,
    pub no_cache: bool,
    pub private: bool,
    pub max_age: Option<u32>,
    pub s_maxage: Option<u32>,
    pub stale_while_revalidate: Option<u32>,
    pub stale_if_error: Option<u32>,
}

impl CacheControl {
    pub fn parse(header: &str) -> Self {
        let mut out = Self::default();
        for directive in header.split(',') {
            let (name, value) = match directive.split_once('=') {
                Some((name, value)) => (name.trim(), Some(value.trim().trim_matches('"'))),
                None => (directive.trim(), None),
            };
            let number = value
                .and_then(|v| v.parse::<u64>().ok())
                .map(|v| v.min(u64::from(u32::MAX)) as u32);
            match name.to_ascii_lowercase().as_str() {
                "no-store" => out.no_store = true,
                // `no-cache="field"` only restricts named headers; treat the bare form as uncacheable.
                "no-cache" if value.is_none() => out.no_cache = true,
                "private" if value.is_none() => out.private = true,
                "max-age" => out.max_age = number,
                "s-maxage" => out.s_maxage = number,
                "stale-while-revalidate" => out.stale_while_revalidate = number,
                "stale-if-error" => out.stale_if_error = number,
                _ => {}
            }
        }
        out
    }
}

/// Purge request sent to a cache server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachePurge {
    pub host: String,
    /// Exact paths (with query) to remove.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Remove every object whose path starts with this.
    #[serde(default)]
    pub prefix: Option<String>,
    /// Remove every object for the host.
    #[serde(default)]
    pub all: bool,
}

impl CachePurge {
    pub fn validate(&self) -> Result<(), Error> {
        validate_host(&self.host)?;
        let selected = usize::from(!self.paths.is_empty())
            + usize::from(self.prefix.is_some())
            + usize::from(self.all);
        if selected != 1 {
            return Err(Error::Validation(
                "choose exactly one of paths, prefix, or all".into(),
            ));
        }
        if self.paths.len() > 1000 {
            return Err(Error::Validation("purge at most 1000 paths at once".into()));
        }
        for path in self.paths.iter().chain(self.prefix.iter()) {
            if !path.starts_with('/') || path.len() > 2048 || path.contains(char::is_whitespace) {
                return Err(Error::Validation(format!("invalid purge path {path}")));
            }
        }
        Ok(())
    }
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachePurgeResult {
    pub removed: u64,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheHostStats {
    pub host: String,
    pub hits: u64,
    pub misses: u64,
    pub stale: u64,
    pub bypass: u64,
    pub bytes_served: u64,
    pub objects: u64,
    pub stored_bytes: u64,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheStats {
    pub version: String,
    pub rules_version: u64,
    pub objects: u64,
    pub stored_bytes: u64,
    pub memory_max_bytes: u64,
    pub evictions: u64,
    pub reloads: u64,
    pub hosts: Vec<CacheHostStats>,
}

/// Starting points for new policies.
pub struct CachePreset {
    pub slug: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub rules: fn() -> Vec<CacheRule>,
}

const STATIC_EXTENSIONS: &[&str] = &[
    "css", "js", "mjs", "map", "png", "jpg", "jpeg", "gif", "svg", "webp", "avif", "ico", "woff",
    "woff2", "ttf", "otf", "eot", "mp4", "webm", "mp3", "pdf", "txt", "xml",
];

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

fn static_rule(ttl_secs: u32) -> CacheRule {
    CacheRule {
        name: "Static assets".into(),
        matcher: RuleMatch {
            extensions: strings(STATIC_EXTENSIONS),
            ..RuleMatch::default()
        },
        action: RuleAction {
            ttl_secs,
            stale_if_error_secs: 86_400,
            ..RuleAction::default()
        },
    }
}

fn bypass(name: &str, prefixes: &[&str]) -> CacheRule {
    CacheRule {
        name: name.into(),
        matcher: RuleMatch {
            path_prefixes: strings(prefixes),
            ..RuleMatch::default()
        },
        action: RuleAction {
            mode: RuleMode::Bypass,
            ..RuleAction::default()
        },
    }
}

fn catch_all(name: &str, action: RuleAction) -> CacheRule {
    CacheRule {
        name: name.into(),
        matcher: RuleMatch::default(),
        action,
    }
}

pub const CACHE_PRESETS: &[CachePreset] = &[
    CachePreset {
        slug: "static-assets",
        name: "Static assets",
        description: "Cache CSS, JavaScript, images, and fonts for 7 days; everything else goes to the origin.",
        rules: || vec![static_rule(7 * 86_400)],
    },
    CachePreset {
        slug: "honor-origin",
        name: "Honor origin headers",
        description: "Cache only what the origin marks cacheable with Cache-Control max-age or s-maxage.",
        rules: || {
            vec![catch_all(
                "Origin decides",
                RuleAction {
                    ttl_secs: 0,
                    ..RuleAction::default()
                },
            )]
        },
    },
    CachePreset {
        slug: "wordpress",
        name: "WordPress",
        description: "Skip admin, login, REST, and logged-in or cart cookies; static files 30 days, pages 5 minutes.",
        rules: || {
            let cookies = strings(&[
                "wordpress_logged_in_",
                "wp-postpass_",
                "comment_author_",
                "woocommerce_",
                "wp_woocommerce_session_",
            ]);
            vec![
                bypass(
                    "Admin and dynamic endpoints",
                    &["/wp-admin", "/wp-login.php", "/wp-json", "/xmlrpc.php", "/wp-cron.php", "/cart", "/checkout", "/my-account"],
                ),
                static_rule(30 * 86_400),
                catch_all(
                    "Pages",
                    RuleAction {
                        ttl_secs: 300,
                        respect_origin: false,
                        stale_while_revalidate_secs: 60,
                        stale_if_error_secs: 86_400,
                        bypass_cookies: cookies,
                        statuses: vec![200, 301, 404],
                        ..RuleAction::default()
                    },
                ),
            ]
        },
    },
    CachePreset {
        slug: "ghost",
        name: "Ghost",
        description: "Skip the admin and member areas and signed-in visitors; static files 7 days, pages 10 minutes.",
        rules: || {
            vec![
                bypass("Admin and members", &["/ghost", "/members", "/p/"]),
                static_rule(7 * 86_400),
                catch_all(
                    "Pages",
                    RuleAction {
                        ttl_secs: 600,
                        respect_origin: false,
                        stale_while_revalidate_secs: 60,
                        stale_if_error_secs: 86_400,
                        bypass_cookies: strings(&["ghost-admin-api-session", "ghost-members-ssr"]),
                        statuses: vec![200, 301, 404],
                        ..RuleAction::default()
                    },
                ),
            ]
        },
    },
    CachePreset {
        slug: "nextjs",
        name: "Next.js",
        description: "Immutable build assets for a year, optimized images for a day, pages as the origin says.",
        rules: || {
            vec![
                CacheRule {
                    name: "Build assets".into(),
                    matcher: RuleMatch {
                        path_prefixes: strings(&["/_next/static/"]),
                        ..RuleMatch::default()
                    },
                    action: RuleAction {
                        ttl_secs: 365 * 86_400,
                        respect_origin: false,
                        ignore_query: true,
                        ..RuleAction::default()
                    },
                },
                CacheRule {
                    name: "Optimized images".into(),
                    matcher: RuleMatch {
                        path_prefixes: strings(&["/_next/image"]),
                        ..RuleMatch::default()
                    },
                    action: RuleAction {
                        ttl_secs: 86_400,
                        query_keys: strings(&["url", "w", "q"]),
                        ..RuleAction::default()
                    },
                },
                catch_all(
                    "Pages",
                    RuleAction {
                        ttl_secs: 0,
                        ..RuleAction::default()
                    },
                ),
            ]
        },
    },
    CachePreset {
        slug: "api-microcache",
        name: "API micro-cache",
        description: "Cache GET responses for 5 seconds to absorb bursts; requests with a session cookie skip it.",
        rules: || {
            vec![catch_all(
                "Micro-cache",
                RuleAction {
                    ttl_secs: 5,
                    respect_origin: false,
                    stale_while_revalidate_secs: 5,
                    bypass_cookies: strings(&["session", "sid", "token"]),
                    statuses: vec![200],
                    ..RuleAction::default()
                },
            )]
        },
    },
];

pub fn cache_preset(slug: &str) -> Option<&'static CachePreset> {
    CACHE_PRESETS.iter().find(|preset| preset.slug == slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request<'a>(
        path: &'a str,
        query: Option<&'a str>,
        cookie: Option<&'a str>,
    ) -> CacheRequest<'a> {
        CacheRequest {
            method: "GET",
            path,
            query,
            cookie,
            authorization: false,
        }
    }

    fn policy(slug: &str) -> CachePolicy {
        CachePolicy {
            id: slug.into(),
            name: slug.into(),
            max_object_bytes: None,
            rules: (cache_preset(slug).unwrap().rules)(),
        }
    }

    #[test]
    fn presets_are_valid_and_wordpress_skips_admin_and_logged_in_users() {
        for preset in CACHE_PRESETS {
            policy(preset.slug).validate().unwrap();
        }
        let wp = policy("wordpress");
        assert_eq!(
            wp.decide(&request("/wp-admin/edit.php", None, None)),
            RequestDecision::Bypass("rule")
        );
        assert_eq!(
            wp.decide(&request(
                "/hello/",
                None,
                Some("a=1; wordpress_logged_in_abc=x")
            )),
            RequestDecision::Bypass("cookie")
        );
        assert_eq!(
            wp.decide(&request("/hello/", Some("b=2&a=1"), Some("theme=dark"))),
            RequestDecision::Lookup {
                rule: 2,
                key: "/hello/?a=1&b=2".into()
            }
        );
        assert!(matches!(
            wp.decide(&request("/app.CSS", None, None)),
            RequestDecision::Lookup { rule: 1, .. }
        ));
        let post = CacheRequest {
            method: "POST",
            ..request("/", None, None)
        };
        assert_eq!(wp.decide(&post), RequestDecision::Bypass("method"));
        let auth = CacheRequest {
            authorization: true,
            ..request("/", None, None)
        };
        assert_eq!(wp.decide(&auth), RequestDecision::Bypass("authorization"));
        assert_eq!(
            policy("static-assets").decide(&request("/", None, None)),
            RequestDecision::Bypass("no_rule")
        );
    }

    #[test]
    fn query_normalization() {
        let mut action = RuleAction::default();
        assert_eq!(cache_path("/a", Some("z=1&a=2&&"), &action), "/a?a=2&z=1");
        action.query_keys = vec!["w".into()];
        assert_eq!(
            cache_path("/img", Some("url=x&w=10&utm=1"), &action),
            "/img?w=10"
        );
        assert_eq!(cache_path("/img", Some("utm=1"), &action), "/img");
        action.ignore_query = true;
        assert_eq!(cache_path("/a", Some("x=1"), &action), "/a");
    }

    #[test]
    fn freshness_respects_origin_and_overrides() {
        let honor = &policy("honor-origin").rules[0];
        assert_eq!(
            honor.freshness(200, None, false, None),
            None,
            "no origin ttl, preset ttl 0"
        );
        let fresh = honor
            .freshness(
                200,
                Some("public, max-age=60, s-maxage=120, stale-while-revalidate=30"),
                false,
                None,
            )
            .unwrap();
        assert_eq!(fresh.ttl_secs, 120);
        assert_eq!(fresh.stale_while_revalidate_secs, 30);
        assert_eq!(
            honor.freshness(200, Some("private, max-age=60"), false, None),
            None
        );
        assert_eq!(honor.freshness(200, Some("no-store"), false, None), None);
        assert_eq!(
            honor.freshness(200, Some("max-age=60"), true, None),
            None,
            "set-cookie never cached"
        );
        assert_eq!(honor.freshness(500, Some("max-age=60"), false, None), None);
        assert_eq!(
            honor.freshness(200, Some("max-age=60"), false, Some("Cookie")),
            None
        );
        assert!(honor
            .freshness(200, Some("max-age=60"), false, Some("Accept-Encoding"))
            .is_some());
        let pages = &policy("wordpress").rules[2];
        let forced = pages
            .freshness(200, Some("no-cache, max-age=0"), false, None)
            .unwrap();
        assert_eq!(
            forced.ttl_secs, 300,
            "respect_origin false ignores origin directives"
        );
        assert_eq!(pages.freshness(302, None, false, None), None);
    }

    #[test]
    fn matching_and_sites() {
        let matcher = RuleMatch {
            path_glob: Some("/blog/*/amp".into()),
            ..RuleMatch::default()
        };
        assert!(matcher.matches("/blog/post-1/amp"));
        assert!(!matcher.matches("/blog/post-1/amp/x"));
        assert!(glob_matches("*.php", "/index.php"));
        assert!(!glob_matches("/a*b*c", "/abx"));
        let doc = CacheDocument {
            version: 1,
            memory_max_bytes: None,
            policies: vec![policy("static-assets")],
            sites: vec![
                CacheSite {
                    host: "a.example.com".into(),
                    path_prefix: "/".into(),
                    upstream: "web-1:80".into(),
                    policy: "static-assets".into(),
                },
                CacheSite {
                    host: "a.example.com".into(),
                    path_prefix: "/api".into(),
                    upstream: "api-1:8080".into(),
                    policy: "static-assets".into(),
                },
            ],
        };
        doc.validate().unwrap();
        assert_eq!(
            doc.site("A.example.com:443", "/api/x").unwrap().upstream,
            "api-1:8080"
        );
        assert_eq!(
            doc.site("a.example.com", "/apix").unwrap().upstream,
            "web-1:80"
        );
        assert!(doc.site("b.example.com", "/").is_none());
        let text = serde_json::to_string(&doc).unwrap();
        assert_eq!(CacheDocument::parse(&text).unwrap(), doc);
        let mut bad = doc.clone();
        bad.sites[0].policy = "missing".into();
        assert!(bad.validate().is_err());
        bad.sites[0].policy = "static-assets".into();
        bad.sites[0].upstream = "web-1".into();
        assert!(bad.validate().is_err());
    }

    #[test]
    fn encodings_and_purge_validation() {
        assert_eq!(encoding_class(Some("gzip, deflate, br")), "br");
        assert_eq!(encoding_class(Some("gzip;q=1.0, br;q=0")), "gzip");
        assert_eq!(encoding_class(Some("deflate")), "identity");
        assert_eq!(encoding_class(None), "identity");
        let mut purge = CachePurge {
            host: "a.example.com".into(),
            all: true,
            ..CachePurge::default()
        };
        purge.validate().unwrap();
        purge.prefix = Some("/x".into());
        assert!(purge.validate().is_err());
        purge.all = false;
        purge.validate().unwrap();
        purge.prefix = Some("x".into());
        assert!(purge.validate().is_err());
    }
}
