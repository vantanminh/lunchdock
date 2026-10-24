//! Outbound mail for Rudder Access codes.
//!
//! Resend, SMTP, and Cloudflare Email Sending. Secrets stay in the encrypted
//! provider config and are not logged.

use std::time::Duration;

use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::ApiError;
use crate::monitor::notify::SmtpSecurity;

const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum MailConfig {
    Resend {
        api_key: String,
    },
    Smtp {
        host: String,
        port: Option<u16>,
        security: SmtpSecurity,
        username: Option<String>,
        password: Option<String>,
    },
    Cloudflare {
        account_id: String,
        api_token: String,
    },
}

impl std::fmt::Debug for MailConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resend { .. } => formatter.write_str("resend"),
            Self::Smtp {
                host,
                port,
                security,
                ..
            } => formatter
                .debug_struct("smtp")
                .field("host", host)
                .field("port", port)
                .field("security", security)
                .finish(),
            Self::Cloudflare { account_id, .. } => formatter
                .debug_struct("cloudflare")
                .field("account_id", account_id)
                .finish(),
        }
    }
}

pub async fn send_login_code(
    client: &reqwest::Client,
    config: &MailConfig,
    from: &str,
    to: &str,
    code: &str,
) -> Result<(), ApiError> {
    let text = format!(
        "Your sign-in code is {code}. It expires in 10 minutes. If you did not ask for it, ignore this message."
    );
    send_text(client, config, from, to, "Your Rudder Access code", &text).await
}

pub async fn send_text(
    client: &reqwest::Client,
    config: &MailConfig,
    from: &str,
    to: &str,
    subject: &str,
    text: &str,
) -> Result<(), ApiError> {
    match config {
        MailConfig::Resend { api_key } => {
            post_json(
                client,
                "https://api.resend.com/emails",
                Some(api_key),
                json!({ "from": from, "to": [to], "subject": subject, "text": text }),
            )
            .await
        }
        MailConfig::Cloudflare {
            account_id,
            api_token,
        } => {
            let (name, address) = split_from(from)?;
            let mut from_value = serde_json::Map::new();
            from_value.insert("address".into(), json!(address));
            if let Some(name) = name {
                from_value.insert("name".into(), json!(name));
            }
            post_json(
                client,
                &format!(
                    "https://api.cloudflare.com/client/v4/accounts/{account_id}/email/sending/send"
                ),
                Some(api_token),
                json!({
                    "to": to,
                    "from": from_value,
                    "subject": subject,
                    "text": text
                }),
            )
            .await
        }
        MailConfig::Smtp {
            host,
            port,
            security,
            username,
            password,
        } => {
            send_smtp(
                host,
                *port,
                security,
                username.as_deref(),
                password.as_deref(),
                from,
                to,
                subject,
                text,
            )
            .await
        }
    }
}

fn split_from(from: &str) -> Result<(Option<String>, String), ApiError> {
    let trimmed = from.trim();
    if let Some(start) = trimmed.rfind('<') {
        if let Some(address) = trimmed[start + 1..].strip_suffix('>') {
            let name = trimmed[..start].trim().trim_matches('"');
            let name = if name.is_empty() {
                None
            } else {
                Some(name.to_owned())
            };
            return Ok((name, address.trim().to_owned()));
        }
    }
    if trimmed.contains('@') && !trimmed.contains(' ') {
        return Ok((None, trimmed.to_owned()));
    }
    Err(ApiError::new(
        axum::http::StatusCode::BAD_REQUEST,
        "invalid_from",
        "the From address must be an email or Name <email>",
    ))
}

async fn post_json(
    client: &reqwest::Client,
    url: &str,
    bearer: Option<&str>,
    body: serde_json::Value,
) -> Result<(), ApiError> {
    let mut request = client
        .post(url)
        .timeout(TIMEOUT)
        .header("content-type", "application/json");
    if let Some(bearer) = bearer {
        request = request.bearer_auth(bearer);
    }
    let response = request
        .body(body.to_string())
        .send()
        .await
        .map_err(|error| {
            tracing::warn!(error = %error.without_url(), "access email request failed");
            ApiError::new(
                axum::http::StatusCode::BAD_GATEWAY,
                "email_failed",
                "the email provider did not accept the message",
            )
        })?;
    if response.status().is_success() {
        Ok(())
    } else {
        tracing::warn!(status = %response.status(), "access email provider rejected the message");
        Err(ApiError::new(
            axum::http::StatusCode::BAD_GATEWAY,
            "email_failed",
            "the email provider did not accept the message",
        ))
    }
}

#[allow(clippy::too_many_arguments)]
async fn send_smtp(
    host: &str,
    port: Option<u16>,
    security: &SmtpSecurity,
    username: Option<&str>,
    password: Option<&str>,
    from: &str,
    to: &str,
    subject: &str,
    text: &str,
) -> Result<(), ApiError> {
    let email = Message::builder()
        .from(parse_mailbox(from)?)
        .to(parse_mailbox(to)?)
        .subject(subject)
        .body(text.to_owned())
        .map_err(|_| {
            ApiError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "invalid_from",
                "the email address could not be used",
            )
        })?;
    let builder = match security {
        SmtpSecurity::Starttls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host),
        SmtpSecurity::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(host),
        SmtpSecurity::None => Ok(AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(
            host,
        )),
    }
    .map_err(|_| {
        ApiError::new(
            axum::http::StatusCode::BAD_GATEWAY,
            "email_failed",
            "could not reach the SMTP server",
        )
    })?;
    let default_port = match security {
        SmtpSecurity::Starttls => 587,
        SmtpSecurity::Tls => 465,
        SmtpSecurity::None => 25,
    };
    let mut builder = builder
        .port(port.unwrap_or(default_port))
        .timeout(Some(TIMEOUT));
    if let (Some(user), Some(pass)) = (username, password) {
        if !user.is_empty() {
            builder = builder.credentials(Credentials::new(user.to_owned(), pass.to_owned()));
        }
    }
    builder
        .build()
        .send(email)
        .await
        .map(|_| ())
        .map_err(|error| {
            tracing::warn!(error = %error, "access SMTP send failed");
            ApiError::new(
                axum::http::StatusCode::BAD_GATEWAY,
                "email_failed",
                "the SMTP server did not accept the message",
            )
        })
}

fn parse_mailbox(value: &str) -> Result<Mailbox, ApiError> {
    value.parse().map_err(|_| {
        ApiError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "invalid_from",
            "the email address could not be used",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_address_keeps_the_display_name() {
        let (name, address) = split_from("Rudder Access <login@example.com>").unwrap();
        assert_eq!(name.as_deref(), Some("Rudder Access"));
        assert_eq!(address, "login@example.com");
        assert!(split_from("not an email").is_err());
    }
}
