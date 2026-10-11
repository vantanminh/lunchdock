//! Notification channels (Telegram, Discord, Slack, webhook, SMTP) and event delivery.
//!
//! Channel configs are encrypted at rest. Only [`ChannelConfig::summary`] is
//! ever shown; bot tokens, webhook URLs, and SMTP passwords are not.

use std::time::Duration;

use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::Sha256;
use sqlx::PgPool;
use uuid::Uuid;

use rudder_core::{Cipher, Error};

const SEND_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_ATTEMPTS: i32 = 5;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SmtpSecurity {
    Starttls,
    Tls,
    None,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ChannelConfig {
    Telegram {
        bot_token: String,
        chat_id: String,
        /// Bot API base URL, for self-hosted Bot API servers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_url: Option<String>,
    },
    Discord {
        webhook_url: String,
    },
    Slack {
        webhook_url: String,
    },
    Webhook {
        url: String,
        /// Signs the body as `X-Rudder-Signature: sha256=<hex hmac>`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        secret: Option<String>,
    },
    Smtp {
        host: String,
        port: Option<u16>,
        security: SmtpSecurity,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        username: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<String>,
        from: String,
        to: Vec<String>,
    },
}

impl std::fmt::Debug for ChannelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.summary())
    }
}

fn http_url(value: &str, https_only: bool) -> Result<url_host::Parsed, Error> {
    url_host::parse(value, https_only)
}

/// Minimal URL checks without pulling in a URL crate: scheme, host, no spaces.
mod url_host {
    use rudder_core::Error;

    pub struct Parsed {
        pub host: String,
    }

    pub fn parse(value: &str, https_only: bool) -> Result<Parsed, Error> {
        let rest = if let Some(rest) = value.strip_prefix("https://") {
            rest
        } else if let Some(rest) = value.strip_prefix("http://") {
            if https_only {
                return Err(Error::Validation("URL must use https".into()));
            }
            rest
        } else {
            return Err(Error::Validation(
                "URL must start with http:// or https://".into(),
            ));
        };
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        let host = authority.rsplit('@').next().unwrap_or("");
        if host.is_empty()
            || value.len() > 2048
            || value.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(Error::Validation("URL is not valid".into()));
        }
        Ok(Parsed {
            host: host.to_owned(),
        })
    }
}

impl ChannelConfig {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Telegram { .. } => "telegram",
            Self::Discord { .. } => "discord",
            Self::Slack { .. } => "slack",
            Self::Webhook { .. } => "webhook",
            Self::Smtp { .. } => "smtp",
        }
    }

    pub fn validate(&self) -> Result<(), Error> {
        match self {
            Self::Telegram {
                bot_token,
                chat_id,
                api_url,
            } => {
                let (id, secret) = bot_token
                    .split_once(':')
                    .ok_or_else(|| Error::Validation("bot token looks like 123456:ABC…".into()))?;
                if id.is_empty()
                    || !id.chars().all(|c| c.is_ascii_digit())
                    || secret.len() < 20
                    || !secret
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                {
                    return Err(Error::Validation("bot token looks like 123456:ABC…".into()));
                }
                let chat = chat_id.strip_prefix('-').unwrap_or(chat_id);
                let username = chat_id.strip_prefix('@').is_some_and(|name| {
                    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                });
                if !(username || (!chat.is_empty() && chat.chars().all(|c| c.is_ascii_digit()))) {
                    return Err(Error::Validation(
                        "chat id is a number or @channelname".into(),
                    ));
                }
                if let Some(url) = api_url {
                    http_url(url, false)?;
                }
                Ok(())
            }
            Self::Discord { webhook_url } | Self::Slack { webhook_url } => {
                http_url(webhook_url, true).map(|_| ())
            }
            Self::Webhook { url, secret } => {
                http_url(url, false)?;
                if secret.as_ref().is_some_and(|s| s.len() < 8) {
                    return Err(Error::Validation(
                        "webhook secret needs at least 8 characters".into(),
                    ));
                }
                Ok(())
            }
            Self::Smtp {
                host,
                from,
                to,
                username,
                password,
                ..
            } => {
                if host.is_empty()
                    || host.len() > 253
                    || !host
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
                {
                    return Err(Error::Validation("SMTP host is not valid".into()));
                }
                from.parse::<Mailbox>()
                    .map_err(|_| Error::Validation("from address is not valid".into()))?;
                if to.is_empty() || to.len() > 20 {
                    return Err(Error::Validation("give 1 to 20 recipients".into()));
                }
                for address in to {
                    address.parse::<Mailbox>().map_err(|_| {
                        Error::Validation(format!("recipient '{address}' is not valid"))
                    })?;
                }
                if username.is_some() != password.is_some() {
                    return Err(Error::Validation(
                        "SMTP username and password go together".into(),
                    ));
                }
                Ok(())
            }
        }
    }

    /// Non-secret description for list views.
    pub fn summary(&self) -> String {
        let host_of = |url: &str| {
            http_url(url, false)
                .map(|p| p.host)
                .unwrap_or_else(|_| "invalid URL".into())
        };
        match self {
            Self::Telegram { chat_id, .. } => format!("chat {chat_id}"),
            Self::Discord { webhook_url } | Self::Slack { webhook_url } => {
                format!("webhook on {}", host_of(webhook_url))
            }
            Self::Webhook { url, secret } => format!(
                "POST to {}{}",
                host_of(url),
                if secret.is_some() { ", signed" } else { "" }
            ),
            Self::Smtp { host, port, to, .. } => {
                format!("{host}:{} → {}", port.unwrap_or(587), to.join(", "))
            }
        }
    }

    /// Keep secrets from `previous` when an update leaves them empty.
    pub fn merge_secrets(mut self, previous: &ChannelConfig) -> Self {
        match (&mut self, previous) {
            (Self::Telegram { bot_token, .. }, Self::Telegram { bot_token: old, .. })
                if bot_token.is_empty() =>
            {
                *bot_token = old.clone()
            }
            (Self::Discord { webhook_url }, Self::Discord { webhook_url: old })
            | (Self::Slack { webhook_url }, Self::Slack { webhook_url: old })
                if webhook_url.is_empty() =>
            {
                *webhook_url = old.clone()
            }
            (Self::Webhook { secret, .. }, Self::Webhook { secret: old, .. })
                if secret.as_deref() == Some("") =>
            {
                *secret = old.clone()
            }
            (Self::Smtp { password, .. }, Self::Smtp { password: old, .. })
                if password.as_deref() == Some("") =>
            {
                *password = old.clone()
            }
            _ => {}
        }
        self
    }

    pub fn seal(&self, cipher: &Cipher) -> Result<Vec<u8>, Error> {
        let plain = serde_json::to_vec(self).map_err(|_| Error::Crypto)?;
        cipher.encrypt(&plain)
    }

    pub fn open(cipher: &Cipher, sealed: &[u8]) -> Result<Self, Error> {
        let plain = cipher.decrypt(sealed)?;
        serde_json::from_slice(&plain).map_err(|_| Error::Crypto)
    }
}

/// What a channel receives.
#[derive(Debug, Clone, Serialize)]
pub struct Notice {
    pub id: i64,
    pub kind: String,
    pub severity: String,
    pub title: String,
    pub message: String,
    pub server_id: Option<Uuid>,
    pub service_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

impl Notice {
    fn text(&self) -> String {
        let tag = match self.severity.as_str() {
            "critical" => "[CRITICAL]",
            "warning" => "[WARNING]",
            _ => "[INFO]",
        };
        if self.message.is_empty() {
            format!("{tag} {}", self.title)
        } else {
            format!("{tag} {}\n{}", self.title, self.message)
        }
    }
}

fn http_client() -> Result<reqwest::Client, Error> {
    reqwest::Client::builder()
        .timeout(SEND_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| Error::Node(error.to_string()))
}

fn check(status: reqwest::StatusCode) -> Result<(), Error> {
    if status.is_success() {
        Ok(())
    } else {
        Err(Error::Node(format!("endpoint answered {status}")))
    }
}

/// `sha256=<hex>` HMAC of the exact request body.
pub fn sign(secret: &str, body: &[u8]) -> Result<String, Error> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| Error::Crypto)?;
    mac.update(body);
    Ok(format!(
        "sha256={}",
        hex::encode(mac.finalize().into_bytes())
    ))
}

async fn post_json(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
) -> Result<(), Error> {
    let response = client
        .post(url)
        .json(body)
        .send()
        .await
        .map_err(|error| Error::Node(format!("request failed: {}", error.without_url())))?;
    check(response.status())
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

pub async fn send(config: &ChannelConfig, notice: &Notice) -> Result<(), Error> {
    let client = http_client()?;
    let text = notice.text();
    match config {
        ChannelConfig::Telegram {
            bot_token,
            chat_id,
            api_url,
        } => {
            let base = api_url
                .as_deref()
                .unwrap_or("https://api.telegram.org")
                .trim_end_matches('/');
            post_json(
                &client,
                &format!("{base}/bot{bot_token}/sendMessage"),
                &json!({
                    "chat_id": chat_id,
                    "text": truncate(&text, 4000),
                    "disable_web_page_preview": true
                }),
            )
            .await
        }
        ChannelConfig::Discord { webhook_url } => {
            post_json(
                &client,
                webhook_url,
                &json!({ "content": truncate(&text, 1900), "allowed_mentions": { "parse": [] } }),
            )
            .await
        }
        ChannelConfig::Slack { webhook_url } => {
            post_json(
                &client,
                webhook_url,
                &json!({ "text": truncate(&text, 3000) }),
            )
            .await
        }
        ChannelConfig::Webhook { url, secret } => {
            let bytes = serde_json::to_vec(notice).map_err(|_| Error::Crypto)?;
            let mut request = client
                .post(url)
                .header("content-type", "application/json")
                .header("x-rudder-event", &notice.kind);
            if let Some(secret) = secret {
                request = request.header("x-rudder-signature", sign(secret, &bytes)?);
            }
            let response =
                request.body(bytes).send().await.map_err(|error| {
                    Error::Node(format!("request failed: {}", error.without_url()))
                })?;
            check(response.status())
        }
        ChannelConfig::Smtp {
            host,
            port,
            security,
            username,
            password,
            from,
            to,
        } => {
            let mut message = Message::builder()
                .from(from.parse().map_err(|_| Error::Validation("from".into()))?)
                .subject(truncate(&format!("Rudder: {}", notice.title), 200));
            for address in to {
                message = message.to(address
                    .parse()
                    .map_err(|_| Error::Validation("to".into()))?);
            }
            let email = message
                .body(text)
                .map_err(|error| Error::Node(error.to_string()))?;
            let builder = match security {
                SmtpSecurity::Starttls => {
                    AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)
                }
                SmtpSecurity::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(host),
                SmtpSecurity::None => Ok(AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(
                    host.as_str(),
                )),
            }
            .map_err(|error| Error::Node(format!("SMTP: {error}")))?;
            let default_port = match security {
                SmtpSecurity::Starttls => 587,
                SmtpSecurity::Tls => 465,
                SmtpSecurity::None => 25,
            };
            let mut builder = builder
                .port(port.unwrap_or(default_port))
                .timeout(Some(SEND_TIMEOUT));
            if let (Some(user), Some(pass)) = (username, password) {
                builder = builder.credentials(Credentials::new(user.clone(), pass.clone()));
            }
            builder
                .build()
                .send(email)
                .await
                .map(|_| ())
                .map_err(|error| Error::Node(format!("SMTP: {error}")))
        }
    }
}

struct Pending {
    id: i64,
    kind: String,
    severity: String,
    title: String,
    message: String,
    server_id: Option<Uuid>,
    service_id: Option<Uuid>,
    payload: serde_json::Value,
    created_at: DateTime<Utc>,
    delivery_attempts: i32,
    delivered_channels: Vec<Uuid>,
}

/// Channels that should receive an event: the alert's own channels for
/// `alert.*` events, plus every enabled `event` rule whose patterns match.
async fn channels_for(pool: &PgPool, event: &Pending) -> Result<Vec<Uuid>, sqlx::Error> {
    let mut out: Vec<Uuid> = Vec::new();
    if let Some(alert_id) = event
        .payload
        .get("alert_id")
        .and_then(|v| v.as_str())
        .and_then(|v| Uuid::parse_str(v).ok())
    {
        if let Some(ids) = sqlx::query_scalar!(
            "SELECT channel_ids FROM alerts WHERE id = $1 AND enabled",
            alert_id
        )
        .fetch_optional(pool)
        .await?
        {
            out.extend(ids);
        }
    }
    let rules = sqlx::query!(
        "SELECT target_spec, channel_ids FROM alerts WHERE enabled AND kind = 'event'"
    )
    .fetch_all(pool)
    .await?;
    for rule in rules {
        let patterns: Vec<String> = rule
            .target_spec
            .get("events")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_else(|| vec!["*".to_owned()]);
        let min = rule
            .target_spec
            .get("min_severity")
            .and_then(|v| v.as_str())
            .unwrap_or("info");
        if rudder_core::event_matches(&patterns, &event.kind)
            && severity_rank(&event.severity) >= severity_rank(min)
        {
            out.extend(rule.channel_ids);
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn severity_rank(value: &str) -> u8 {
    match value {
        "critical" => 2,
        "warning" => 1,
        _ => 0,
    }
}

/// Deliver pending events once. Returns the number of events finished.
pub async fn deliver_once(pool: &PgPool, cipher: &Cipher) -> Result<usize, sqlx::Error> {
    let events = sqlx::query_as!(
        Pending,
        r#"
        SELECT id, kind, severity, title, message, server_id, service_id, payload, created_at,
               delivery_attempts, delivered_channels
        FROM system_events
        WHERE delivered_at IS NULL
        ORDER BY id
        LIMIT 50
        "#
    )
    .fetch_all(pool)
    .await?;
    let mut finished = 0;
    for event in events {
        let targets = channels_for(pool, &event).await?;
        let remaining: Vec<Uuid> = targets
            .into_iter()
            .filter(|id| !event.delivered_channels.contains(id))
            .collect();
        let channels = sqlx::query!(
            "SELECT id, config_enc FROM notification_channels WHERE id = ANY($1) AND enabled",
            &remaining
        )
        .fetch_all(pool)
        .await?;
        let notice = Notice {
            id: event.id,
            kind: event.kind.clone(),
            severity: event.severity.clone(),
            title: event.title.clone(),
            message: event.message.clone(),
            server_id: event.server_id,
            service_id: event.service_id,
            created_at: event.created_at,
        };
        let mut delivered = event.delivered_channels.clone();
        let mut errors = Vec::new();
        for channel in channels {
            let result = match ChannelConfig::open(cipher, &channel.config_enc) {
                Ok(config) => send(&config, &notice).await,
                Err(error) => Err(error),
            };
            match result {
                Ok(()) => delivered.push(channel.id),
                Err(error) => errors.push(format!("{}: {error}", channel.id)),
            }
        }
        let attempts = event.delivery_attempts + 1;
        let done = errors.is_empty() || attempts >= MAX_ATTEMPTS;
        sqlx::query!(
            r#"
            UPDATE system_events
            SET delivered_channels = $2, delivery_attempts = $3,
                delivery_error = $4, delivered_at = CASE WHEN $5 THEN now() ELSE NULL END
            WHERE id = $1
            "#,
            event.id,
            &delivered,
            attempts,
            (!errors.is_empty()).then(|| errors.join("; ")),
            done,
        )
        .execute(pool)
        .await?;
        if !errors.is_empty() {
            tracing::warn!(event = event.id, attempts, "notification delivery failed");
        }
        if done {
            finished += 1;
        }
    }
    Ok(finished)
}

pub async fn run(pool: PgPool, cipher: std::sync::Arc<Cipher>) {
    loop {
        if let Err(error) = deliver_once(&pool, &cipher).await {
            tracing::warn!(%error, "notification delivery query failed");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn telegram(token: &str, chat: &str) -> ChannelConfig {
        ChannelConfig::Telegram {
            bot_token: token.into(),
            chat_id: chat.into(),
            api_url: None,
        }
    }

    #[test]
    fn channel_validation() {
        assert!(
            telegram("123456:ABCDEFGHIJKLMNOPQRSTUVWXYZ_-12", "-1001234")
                .validate()
                .is_ok()
        );
        assert!(
            telegram("123456:ABCDEFGHIJKLMNOPQRSTUVWXYZ_-12", "@ops_room")
                .validate()
                .is_ok()
        );
        assert!(telegram("nope", "1").validate().is_err());
        assert!(telegram("123456:ABCDEFGHIJKLMNOPQRSTUVWXYZ", "abc")
            .validate()
            .is_err());
        assert!(ChannelConfig::Discord {
            webhook_url: "http://discord.com/api/webhooks/1/x".into()
        }
        .validate()
        .is_err());
        assert!(ChannelConfig::Webhook {
            url: "https://hooks.example.com/rudder".into(),
            secret: Some("short".into())
        }
        .validate()
        .is_err());
        let smtp = ChannelConfig::Smtp {
            host: "smtp.example.com".into(),
            port: None,
            security: SmtpSecurity::Starttls,
            username: Some("u".into()),
            password: None,
            from: "Rudder <rudder@example.com>".into(),
            to: vec!["ops@example.com".into()],
        };
        assert!(smtp.validate().is_err());
    }

    #[test]
    fn summaries_hide_secrets_and_updates_keep_them() {
        let config = telegram("123456:ABCDEFGHIJKLMNOPQRSTUVWXYZ_-12", "42");
        assert_eq!(config.summary(), "chat 42");
        assert!(!format!("{config:?}").contains("ABCDEF"));
        let slack = ChannelConfig::Slack {
            webhook_url: "https://hooks.slack.com/services/T/B/secret".into(),
        };
        assert_eq!(slack.summary(), "webhook on hooks.slack.com");
        let updated = ChannelConfig::Slack {
            webhook_url: String::new(),
        }
        .merge_secrets(&slack);
        assert_eq!(updated, slack);
    }

    #[test]
    fn severity_ordering() {
        assert!(severity_rank("critical") > severity_rank("warning"));
        assert!(severity_rank("warning") > severity_rank("info"));
    }
}
