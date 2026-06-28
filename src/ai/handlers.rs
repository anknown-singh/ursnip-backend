//! HTTP handlers for the AI expansion endpoint.
//!
//! Provides:
//! - POST /ai/expand — expand a trigger via the AI provider

use std::sync::Arc;

use axum::{extract::Extension, Json};

use crate::ai::service::{AiExpandRequest, AiExpandResponse, AiService};
use crate::errors::AppError;
use crate::middleware::auth_extractor::AccessTokenClaims;

// ─── Handlers ───────────────────────────────────────────────────────────────────

/// POST /ai/expand
///
/// Accepts a JSON body with `trigger` and `system_prompt`, forwards the request
/// to the AI provider via AiService, and returns `{ "expanded_text": "..." }`.
///
/// Requirements:
/// - Authenticated user (AccessTokenClaims injected by auth middleware)
/// - client_type == "native" (enforced by client_type_guard middleware on /ai/*)
///
/// Returns HTTP 200 with `AiExpandResponse` on success.
pub async fn expand_handler(
    Extension(service): Extension<Arc<AiService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Json(body): Json<AiExpandRequest>,
) -> Result<Json<AiExpandResponse>, AppError> {
    let user_id = claims.sub;
    let response = service.expand(user_id, body).await?;
    Ok(Json(response))
}
