//! HTTP handlers for subscription and billing webhook endpoints.
//!
//! Provides handlers for:
//! - POST /subscriptions/upgrade — initiate upgrade from free to pro
//! - POST /subscriptions/checkout — process checkout with invoice computation
//! - GET /subscriptions/current — get current subscription for a workspace
//! - POST /webhooks/billing — process billing provider webhook events

use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{Extension, Query},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::AppConfig;
use crate::errors::AppError;
use crate::middleware::auth_extractor::AccessTokenClaims;
use crate::subscription::service::{CheckoutRequest, SubscriptionService};
use crate::subscription::webhook;

// ─── Request/Response DTOs ──────────────────────────────────────────────────────

/// Request body for POST /subscriptions/upgrade.
#[derive(Debug, Deserialize)]
pub struct UpgradeRequest {
    pub workspace_id: Uuid,
}

/// Request body for POST /subscriptions/checkout.
#[derive(Debug, Deserialize)]
pub struct CheckoutRequestBody {
    pub workspace_id: Uuid,
    pub tier: String,
    pub billing_cycle_months: i32,
    pub coupon_code: Option<String>,
    pub discount_id: Option<Uuid>,
    pub country_code: Option<String>,
    pub success_url: Option<String>,
    pub cancel_url: Option<String>,
}

/// Query parameters for GET /subscriptions/current.
#[derive(Debug, Deserialize)]
pub struct CurrentSubscriptionQuery {
    pub workspace_id: Uuid,
}

// ─── Handlers ───────────────────────────────────────────────────────────────────

/// POST /subscriptions/upgrade
///
/// Initiate an upgrade from free to pro tier for the given workspace.
/// Returns 200 OK on success with a status message.
pub async fn upgrade_handler(
    Extension(service): Extension<Arc<SubscriptionService>>,
    Extension(_claims): Extension<AccessTokenClaims>,
    Json(body): Json<UpgradeRequest>,
) -> Result<impl IntoResponse, AppError> {
    service.initiate_upgrade(body.workspace_id).await?;
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({ "status": "pending_payment" })),
    ))
}

/// POST /subscriptions/checkout
///
/// Process a checkout request: validate coupon/discount, compute invoice,
/// update subscription status, and return a checkout URL with invoice breakdown.
pub async fn checkout_handler(
    Extension(service): Extension<Arc<SubscriptionService>>,
    Extension(_claims): Extension<AccessTokenClaims>,
    Json(body): Json<CheckoutRequestBody>,
) -> Result<impl IntoResponse, AppError> {
    let request = CheckoutRequest {
        workspace_id: body.workspace_id,
        tier: body.tier,
        billing_cycle_months: body.billing_cycle_months,
        coupon_code: body.coupon_code,
        discount_id: body.discount_id,
        country_code: body.country_code,
        success_url: body.success_url,
        cancel_url: body.cancel_url,
    };

    let response = service.checkout(request).await?;
    Ok((StatusCode::OK, Json(response)))
}

/// GET /subscriptions/current
///
/// Returns the current subscription for a workspace, identified by query parameter.
pub async fn current_subscription_handler(
    Extension(service): Extension<Arc<SubscriptionService>>,
    Extension(_claims): Extension<AccessTokenClaims>,
    Query(query): Query<CurrentSubscriptionQuery>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.get_current_subscription(query.workspace_id).await?;
    Ok((StatusCode::OK, Json(response)))
}

/// POST /webhooks/billing
///
/// Process a billing webhook event from the payment provider.
/// Verifies the signature from the X-Webhook-Signature header, then parses
/// and processes the webhook payload (Requirements 5.55–5.58).
pub async fn billing_webhook_handler(
    Extension(config): Extension<Arc<AppConfig>>,
    Extension(pool): Extension<PgPool>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, AppError> {
    // Extract signature from header
    let signature = headers
        .get("X-Webhook-Signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // Verify the webhook signature (Requirement 5.56)
    webhook::verify_signature(&body, signature, &config.billing_webhook_secret)?;

    // Parse the JSON payload
    let payload: webhook::WebhookPayload =
        serde_json::from_slice(&body).map_err(|_| AppError::MalformedRequestBody)?;

    // Process the webhook event (Requirements 5.55, 5.57, 5.58)
    let result = webhook::process_webhook(&pool, payload).await?;

    Ok((StatusCode::OK, Json(result)))
}
