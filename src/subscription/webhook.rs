use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::AppError;

// ─── Types ──────────────────────────────────────────────────────────────────────

/// Incoming webhook payload from the billing provider.
#[derive(Debug, Clone, Deserialize)]
pub struct WebhookPayload {
    pub event_id: String,
    pub event_type: String,
    pub workspace_id: Option<Uuid>,
    pub external_subscription_id: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub data: serde_json::Value,
}

/// Webhook processing result.
#[derive(Debug, Serialize)]
pub struct WebhookResult {
    pub status: String,
    pub event_id: String,
}

// ─── Public Functions ───────────────────────────────────────────────────────────

/// Verify the webhook signature using SHA-256.
///
/// Computes `SHA256(secret || body)` and compares the hex-encoded result to the
/// provided signature. Returns `Err(AppError::InvalidWebhookSignature)` on mismatch
/// or if the signature is empty (Requirement 5.56).
pub fn verify_signature(body: &[u8], signature: &str, secret: &str) -> Result<(), AppError> {
    if signature.is_empty() {
        return Err(AppError::InvalidWebhookSignature);
    }

    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hasher.update(body);
    let computed = hex::encode(hasher.finalize());

    // Constant-length comparison via equality on fixed-length hex strings.
    // Both are 64-char hex strings if the provided signature is a valid SHA-256 digest.
    if computed.len() != signature.len() || computed != signature {
        return Err(AppError::InvalidWebhookSignature);
    }

    Ok(())
}

/// Process a billing webhook event (Requirements 5.55, 5.57, 5.58).
///
/// 1. Check idempotency via `billing_events` table — if `external_event_id` already
///    exists, return HTTP 200 without re-processing.
/// 2. Insert the event into `billing_events`.
/// 3. Apply the subscription state transition based on `event_type`.
/// 4. Mark the event as processed.
///
/// For the 5-second response constraint (Requirement 5.58): the actual DB operations
/// here are lightweight and complete well within 5 seconds. For heavier future
/// processing (e.g., sending emails, syncing to analytics), use `tokio::spawn` to
/// defer work after returning the response from the handler.
pub async fn process_webhook(
    pool: &PgPool,
    payload: WebhookPayload,
) -> Result<WebhookResult, AppError> {
    // Step 1: Idempotency check (Requirement 5.57)
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM billing_events WHERE external_event_id = $1",
    )
    .bind(&payload.event_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| AppError::InternalError)?;

    if existing.is_some() {
        return Ok(WebhookResult {
            status: "already_processed".to_string(),
            event_id: payload.event_id,
        });
    }

    // Step 2: Record the event in billing_events
    let event_id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO billing_events (id, external_event_id, event_type, workspace_id, payload, created_at)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
    )
    .bind(event_id)
    .bind(&payload.event_id)
    .bind(&payload.event_type)
    .bind(payload.workspace_id)
    .bind(&payload.data)
    .bind(Utc::now())
    .execute(pool)
    .await
    .map_err(|_| AppError::InternalError)?;

    // Step 3: Apply subscription state transition (Requirement 5.55)
    if let Some(workspace_id) = payload.workspace_id {
        apply_subscription_transition(pool, &payload.event_type, workspace_id, &payload).await?;
    }

    // Step 4: Mark event as processed
    sqlx::query(
        "UPDATE billing_events SET processed_at = $1 WHERE id = $2",
    )
    .bind(Utc::now())
    .bind(event_id)
    .execute(pool)
    .await
    .map_err(|_| AppError::InternalError)?;

    Ok(WebhookResult {
        status: "processed".to_string(),
        event_id: payload.event_id,
    })
}

// ─── Private Helpers ────────────────────────────────────────────────────────────

/// Apply the subscription state transition based on event type (Requirement 5.55).
async fn apply_subscription_transition(
    pool: &PgPool,
    event_type: &str,
    workspace_id: Uuid,
    payload: &WebhookPayload,
) -> Result<(), AppError> {
    match event_type {
        "subscription.activated" => {
            // Set status=active, period_start=now, period_end=now+12 months,
            // and external_subscription_id if provided.
            let now = Utc::now();
            let period_end = now + Duration::days(365);

            sqlx::query(
                r#"
                UPDATE subscriptions
                SET status = 'active',
                    period_start = $1,
                    period_end = $2,
                    external_subscription_id = COALESCE($3, external_subscription_id),
                    updated_at = now()
                WHERE workspace_id = $4
                "#,
            )
            .bind(now)
            .bind(period_end)
            .bind(payload.external_subscription_id.as_deref())
            .bind(workspace_id)
            .execute(pool)
            .await
            .map_err(|_| AppError::InternalError)?;
        }
        "subscription.renewed" => {
            // Extend period_end by 12 months, ensure status=active.
            sqlx::query(
                r#"
                UPDATE subscriptions
                SET period_end = period_end + INTERVAL '12 months',
                    status = 'active',
                    updated_at = now()
                WHERE workspace_id = $1
                "#,
            )
            .bind(workspace_id)
            .execute(pool)
            .await
            .map_err(|_| AppError::InternalError)?;
        }
        "subscription.past_due" => {
            // Set status=past_due, grace_period_end = event timestamp + 7 days.
            let grace_period_end = payload.timestamp + Duration::days(7);

            sqlx::query(
                r#"
                UPDATE subscriptions
                SET status = 'past_due',
                    grace_period_end = $1,
                    updated_at = now()
                WHERE workspace_id = $2
                "#,
            )
            .bind(grace_period_end)
            .bind(workspace_id)
            .execute(pool)
            .await
            .map_err(|_| AppError::InternalError)?;
        }
        "subscription.cancelled" => {
            // Set status=cancelled, cancelled_at=now.
            sqlx::query(
                r#"
                UPDATE subscriptions
                SET status = 'cancelled',
                    cancelled_at = now(),
                    updated_at = now()
                WHERE workspace_id = $1
                "#,
            )
            .bind(workspace_id)
            .execute(pool)
            .await
            .map_err(|_| AppError::InternalError)?;
        }
        "subscription.reactivated" => {
            // Set status=active, clear cancelled_at and grace_period_end.
            sqlx::query(
                r#"
                UPDATE subscriptions
                SET status = 'active',
                    cancelled_at = NULL,
                    grace_period_end = NULL,
                    updated_at = now()
                WHERE workspace_id = $1
                "#,
            )
            .bind(workspace_id)
            .execute(pool)
            .await
            .map_err(|_| AppError::InternalError)?;
        }
        _ => {
            // Unknown event type — log and skip. Not an error per spec.
            tracing::warn!(event_type = %event_type, "Unknown billing webhook event type, skipping");
        }
    }

    Ok(())
}

// ─── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_signature_valid() {
        let secret = "my_secret";
        let body = b"hello world";

        // Compute expected signature: SHA256(secret || body)
        let mut hasher = Sha256::new();
        hasher.update(secret.as_bytes());
        hasher.update(body);
        let expected = hex::encode(hasher.finalize());

        assert!(verify_signature(body, &expected, secret).is_ok());
    }

    #[test]
    fn test_verify_signature_invalid() {
        let secret = "my_secret";
        let body = b"hello world";
        let bad_sig = "0000000000000000000000000000000000000000000000000000000000000000";

        assert!(verify_signature(body, bad_sig, secret).is_err());
    }

    #[test]
    fn test_verify_signature_empty_signature() {
        let secret = "my_secret";
        let body = b"hello world";

        let result = verify_signature(body, "", secret);
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_signature_wrong_secret() {
        let body = b"payload data";

        // Compute signature with one secret
        let mut hasher = Sha256::new();
        hasher.update(b"correct_secret");
        hasher.update(body);
        let sig = hex::encode(hasher.finalize());

        // Verify with different secret should fail
        let result = verify_signature(body, &sig, "wrong_secret");
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_signature_different_body() {
        let secret = "my_secret";
        let body = b"original body";

        // Compute signature for original body
        let mut hasher = Sha256::new();
        hasher.update(secret.as_bytes());
        hasher.update(body);
        let sig = hex::encode(hasher.finalize());

        // Verify with different body should fail
        let result = verify_signature(b"tampered body", &sig, secret);
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_signature_length_mismatch() {
        let secret = "my_secret";
        let body = b"hello";

        // Short signature (not 64 hex chars)
        let result = verify_signature(body, "abcdef", secret);
        assert!(result.is_err());
    }
}
