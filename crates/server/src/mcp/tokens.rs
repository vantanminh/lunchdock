//! Stored grants, authorization codes, and rotating refresh tokens.
//!
//! Only hashes are stored. A refresh token that is presented again after it
//! was rotated revokes the whole grant.

use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::{self, AuthUser};
use crate::error::ApiError;
use crate::state::AppState;

const CODE_SECS: i64 = 120;
const ACCESS_SECS: i64 = 900;
const REFRESH_DAYS: i64 = 30;
const GRANT_DAYS: i64 = 90;

pub struct Issued {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
    pub scope: String,
}

struct GrantRow {
    id: Uuid,
    client_id: String,
    scopes: Vec<String>,
    created_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
}

pub async fn load_access(state: &AppState, token: &str) -> Result<AuthUser, ApiError> {
    let hash = auth::sha256_hex(token);
    let row = sqlx::query!(
        r#"
        SELECT a.expires_at, a.revoked_at, g.id AS grant_id, g.scopes, g.revoked_at AS grant_revoked,
               u.id AS user_id, u.email, u.display_name, u.role, u.totp_enabled, u.is_owner
        FROM mcp_access_tokens a
        JOIN mcp_grants g ON g.id = a.grant_id
        JOIN users u ON u.id = g.user_id
        WHERE a.token_hash = $1
        "#,
        hash,
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let Some(row) = row else {
        return Err(unauthorized("invalid agent token"));
    };
    if row.revoked_at.is_some() || row.grant_revoked.is_some() || row.expires_at <= Utc::now() {
        return Err(unauthorized("agent token expired"));
    }
    let _ = sqlx::query!(
        "UPDATE mcp_grants SET last_used_at = now() WHERE id = $1",
        row.grant_id
    )
    .execute(&state.pool)
    .await;
    Ok(AuthUser {
        id: row.user_id,
        email: row.email,
        display_name: row.display_name,
        role: row.role,
        totp_enabled: row.totp_enabled,
        is_owner: row.is_owner,
        csrf_token: None,
        scopes: row.scopes,
        via_token: true,
        via_mcp: true,
    })
}

pub async fn describe(state: &AppState, token: &str) -> Result<Value, ApiError> {
    let hash = auth::sha256_hex(token);
    let row = sqlx::query!(
        r#"
        SELECT a.expires_at, g.template, g.scopes, g.name, g.created_at, u.email, u.display_name, u.role
        FROM mcp_access_tokens a
        JOIN mcp_grants g ON g.id = a.grant_id
        JOIN users u ON u.id = g.user_id
        WHERE a.token_hash = $1 AND a.revoked_at IS NULL AND g.revoked_at IS NULL
        "#,
        hash,
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let Some(row) = row else {
        return Err(unauthorized("invalid agent token"));
    };
    Ok(json!({
        "email": row.email,
        "display_name": row.display_name,
        "role": row.role,
        "approval": row.name,
        "template": row.template,
        "scopes": row.scopes,
        "approved_at": row.created_at,
        "access_expires_at": row.expires_at,
    }))
}

#[allow(clippy::too_many_arguments)]
pub async fn store_code(
    state: &AppState,
    user_id: Uuid,
    client_id: &str,
    name: &str,
    template: &str,
    scopes: &[String],
    redirect_uri: &str,
    code_challenge: &str,
) -> Result<String, ApiError> {
    let mut tx = state.pool.begin().await.map_err(ApiError::database)?;
    let grant_id = sqlx::query_scalar!(
        r#"
        INSERT INTO mcp_grants (user_id, client_id, name, template, scopes)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id
        "#,
        user_id,
        client_id,
        name,
        template,
        scopes,
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(ApiError::database)?;
    let code = auth::random_token("rdr_mcc");
    let hash = auth::sha256_hex(&code);
    let expires = Utc::now() + Duration::seconds(CODE_SECS);
    sqlx::query!(
        r#"
        INSERT INTO oauth_codes (code_hash, grant_id, client_id, redirect_uri, code_challenge, expires_at)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
        hash,
        grant_id,
        client_id,
        redirect_uri,
        code_challenge,
        expires,
    )
    .execute(&mut *tx)
    .await
    .map_err(ApiError::database)?;
    tx.commit().await.map_err(ApiError::database)?;
    Ok(code)
}

pub async fn exchange_code(
    state: &AppState,
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<Issued, ExchangeError> {
    let hash = auth::sha256_hex(code);
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| ExchangeError::Server)?;
    let row = sqlx::query!(
        r#"
        SELECT grant_id, client_id, redirect_uri, code_challenge, expires_at, used_at
        FROM oauth_codes
        WHERE code_hash = $1
        FOR UPDATE
        "#,
        hash,
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| ExchangeError::Server)?;
    let Some(row) = row else {
        return Err(ExchangeError::Invalid("unknown authorization code"));
    };
    if row.used_at.is_some() {
        revoke_grant(&mut tx, row.grant_id).await.ok();
        let _ = tx.commit().await;
        return Err(ExchangeError::Invalid("authorization code already used"));
    }
    if row.expires_at <= Utc::now()
        || row.client_id != client_id
        || row.redirect_uri != redirect_uri
        || !pkce_ok(&row.code_challenge, verifier)
    {
        return Err(ExchangeError::Invalid(
            "authorization code, redirect, or code verifier does not match",
        ));
    }
    sqlx::query!(
        "UPDATE oauth_codes SET used_at = now() WHERE code_hash = $1",
        hash
    )
    .execute(&mut *tx)
    .await
    .map_err(|_| ExchangeError::Server)?;
    let issued = issue(&mut tx, row.grant_id, None)
        .await
        .map_err(|_| ExchangeError::Server)?;
    tx.commit().await.map_err(|_| ExchangeError::Server)?;
    Ok(issued)
}

pub async fn exchange_refresh(
    state: &AppState,
    client_id: &str,
    refresh: &str,
) -> Result<Issued, ExchangeError> {
    let hash = auth::sha256_hex(refresh);
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| ExchangeError::Server)?;
    let row = sqlx::query!(
        r#"
        SELECT grant_id, family_id, expires_at, rotated_at, revoked_at
        FROM mcp_refresh_tokens
        WHERE token_hash = $1
        FOR UPDATE
        "#,
        hash,
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| ExchangeError::Server)?;
    let Some(row) = row else {
        return Err(ExchangeError::Invalid("unknown refresh token"));
    };
    if row.rotated_at.is_some() || row.revoked_at.is_some() {
        revoke_grant(&mut tx, row.grant_id).await.ok();
        let _ = tx.commit().await;
        tracing::warn!(grant = %row.grant_id, "refresh token reuse; approval revoked");
        return Err(ExchangeError::Invalid(
            "refresh token was already used; approve the agent again",
        ));
    }
    if row.expires_at <= Utc::now() {
        return Err(ExchangeError::Invalid("refresh token expired"));
    }
    let grant = load_grant(&mut tx, row.grant_id).await?;
    if grant.revoked_at.is_some() || grant.client_id != client_id {
        return Err(ExchangeError::Invalid("approval was revoked"));
    }
    if grant.created_at + Duration::days(GRANT_DAYS) <= Utc::now() {
        revoke_grant(&mut tx, grant.id).await.ok();
        let _ = tx.commit().await;
        return Err(ExchangeError::Invalid(
            "approval expired after 90 days; approve the agent again",
        ));
    }
    sqlx::query!(
        "UPDATE mcp_refresh_tokens SET rotated_at = now() WHERE token_hash = $1",
        hash
    )
    .execute(&mut *tx)
    .await
    .map_err(|_| ExchangeError::Server)?;
    let issued = issue(&mut tx, grant.id, Some(row.family_id))
        .await
        .map_err(|_| ExchangeError::Server)?;
    tx.commit().await.map_err(|_| ExchangeError::Server)?;
    Ok(issued)
}

pub async fn revoke(state: &AppState, token: &str, client_id: &str) -> Result<(), ApiError> {
    let hash = auth::sha256_hex(token);
    let mut tx = state.pool.begin().await.map_err(ApiError::database)?;
    if token.starts_with(super::ACCESS_PREFIX) {
        let grant = sqlx::query!(
            r#"
            SELECT a.grant_id, g.client_id
            FROM mcp_access_tokens a
            JOIN mcp_grants g ON g.id = a.grant_id
            WHERE a.token_hash = $1
            "#,
            hash,
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(ApiError::database)?;
        if let Some(grant) = grant {
            if client_id.is_empty() || grant.client_id == client_id {
                sqlx::query!(
                    "UPDATE mcp_access_tokens SET revoked_at = now() WHERE token_hash = $1",
                    hash
                )
                .execute(&mut *tx)
                .await
                .map_err(ApiError::database)?;
            }
        }
    } else {
        let grant = sqlx::query!(
            r#"
            SELECT r.grant_id, g.client_id
            FROM mcp_refresh_tokens r
            JOIN mcp_grants g ON g.id = r.grant_id
            WHERE r.token_hash = $1
            "#,
            hash,
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(ApiError::database)?;
        if let Some(grant) = grant {
            if client_id.is_empty() || grant.client_id == client_id {
                revoke_grant(&mut tx, grant.grant_id)
                    .await
                    .map_err(|_| ApiError::database(sqlx::Error::RowNotFound))?;
            }
        }
    }
    tx.commit().await.map_err(ApiError::database)?;
    Ok(())
}

pub async fn revoke_grant_id(state: &AppState, grant_id: Uuid) -> Result<bool, ApiError> {
    let mut tx = state.pool.begin().await.map_err(ApiError::database)?;
    let updated = sqlx::query!(
        r#"
        UPDATE mcp_grants
        SET revoked_at = now()
        WHERE id = $1 AND revoked_at IS NULL
        "#,
        grant_id,
    )
    .execute(&mut *tx)
    .await
    .map_err(ApiError::database)?;
    if updated.rows_affected() == 0 {
        return Ok(false);
    }
    revoke_tokens(&mut tx, grant_id)
        .await
        .map_err(|_| ApiError::database(sqlx::Error::RowNotFound))?;
    tx.commit().await.map_err(ApiError::database)?;
    Ok(true)
}

pub async fn maintain(state: &AppState) -> Result<(), ApiError> {
    sqlx::query!(
        r#"
        DELETE FROM oauth_codes
        WHERE expires_at < now() - interval '1 day' OR used_at < now() - interval '1 day'
        "#
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    sqlx::query!(
        r#"
        DELETE FROM mcp_access_tokens
        WHERE expires_at < now() - interval '1 day' OR revoked_at < now() - interval '1 day'
        "#
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    sqlx::query!(
        r#"
        DELETE FROM mcp_refresh_tokens
        WHERE expires_at < now() - interval '1 day'
        "#
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    sqlx::query!(
        r#"
        DELETE FROM oauth_clients c
        WHERE c.created_at < now() - interval '7 days'
          AND NOT EXISTS (SELECT 1 FROM mcp_grants g WHERE g.client_id = c.client_id)
        "#
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(())
}

pub fn pkce_ok(challenge: &str, verifier: &str) -> bool {
    if !(43..=128).contains(&verifier.len()) {
        return false;
    }
    if !verifier
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
    {
        return false;
    }
    auth::tokens_eq(&pkce_s256(verifier), challenge)
}

pub fn pkce_s256(verifier: &str) -> String {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

async fn issue(
    tx: &mut sqlx::PgConnection,
    grant_id: Uuid,
    family: Option<Uuid>,
) -> Result<Issued, sqlx::Error> {
    let grant = load_grant(tx, grant_id)
        .await
        .map_err(|_| sqlx::Error::RowNotFound)?;
    if grant.revoked_at.is_some() {
        return Err(sqlx::Error::RowNotFound);
    }
    let family = family.unwrap_or_else(Uuid::new_v4);
    let now = Utc::now();
    let access_exp = now + Duration::seconds(ACCESS_SECS);
    let refresh_cap = grant.created_at + Duration::days(GRANT_DAYS);
    let refresh_exp = (now + Duration::days(REFRESH_DAYS)).min(refresh_cap);
    if refresh_exp <= now {
        return Err(sqlx::Error::RowNotFound);
    }
    sqlx::query!(
        r#"
        UPDATE mcp_access_tokens
        SET revoked_at = now()
        WHERE grant_id = $1 AND revoked_at IS NULL
        "#,
        grant_id,
    )
    .execute(&mut *tx)
    .await?;
    let access = auth::random_token("rdr_mca");
    let refresh = auth::random_token("rdr_mcr");
    sqlx::query!(
        r#"
        INSERT INTO mcp_access_tokens (token_hash, grant_id, expires_at)
        VALUES ($1, $2, $3)
        "#,
        auth::sha256_hex(&access),
        grant_id,
        access_exp,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        r#"
        INSERT INTO mcp_refresh_tokens (token_hash, grant_id, family_id, expires_at)
        VALUES ($1, $2, $3, $4)
        "#,
        auth::sha256_hex(&refresh),
        grant_id,
        family,
        refresh_exp,
    )
    .execute(&mut *tx)
    .await?;
    Ok(Issued {
        access_token: access,
        refresh_token: refresh,
        expires_in: ACCESS_SECS,
        scope: grant.scopes.join(" "),
    })
}

async fn load_grant(
    tx: &mut sqlx::PgConnection,
    grant_id: Uuid,
) -> Result<GrantRow, ExchangeError> {
    let row = sqlx::query!(
        r#"
        SELECT id, client_id, scopes, created_at, revoked_at
        FROM mcp_grants
        WHERE id = $1
        FOR UPDATE
        "#,
        grant_id,
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| ExchangeError::Server)?;
    row.map(|row| GrantRow {
        id: row.id,
        client_id: row.client_id,
        scopes: row.scopes,
        created_at: row.created_at,
        revoked_at: row.revoked_at,
    })
    .ok_or(ExchangeError::Invalid("approval was revoked"))
}

async fn revoke_grant(tx: &mut sqlx::PgConnection, grant_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE mcp_grants SET revoked_at = COALESCE(revoked_at, now()) WHERE id = $1",
        grant_id,
    )
    .execute(&mut *tx)
    .await?;
    revoke_tokens(tx, grant_id).await
}

async fn revoke_tokens(tx: &mut sqlx::PgConnection, grant_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        UPDATE mcp_access_tokens
        SET revoked_at = COALESCE(revoked_at, now())
        WHERE grant_id = $1
        "#,
        grant_id,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        r#"
        UPDATE mcp_refresh_tokens
        SET revoked_at = COALESCE(revoked_at, now())
        WHERE grant_id = $1
        "#,
        grant_id,
    )
    .execute(&mut *tx)
    .await?;
    Ok(())
}

fn unauthorized(message: &str) -> ApiError {
    ApiError::new(
        axum::http::StatusCode::UNAUTHORIZED,
        "unauthorized",
        message,
    )
}

pub enum ExchangeError {
    Invalid(&'static str),
    Server,
}
