//! Append-only audit rows for mutating actions.
//!
//! Metadata must not contain secrets, passwords, tokens, or ciphertext.

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn record(
    pool: &PgPool,
    actor_user_id: Option<Uuid>,
    action: &str,
    resource_type: &str,
    resource_id: Option<&str>,
    metadata: Value,
    ip: Option<&str>,
) {
    let result = sqlx::query!(
        r#"
        INSERT INTO audit_log (actor_user_id, action, resource_type, resource_id, metadata, ip)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
        actor_user_id,
        action,
        resource_type,
        resource_id,
        metadata,
        ip,
    )
    .execute(pool)
    .await;
    if let Err(error) = result {
        tracing::error!(%error, %action, "failed to write audit log");
    }
}
