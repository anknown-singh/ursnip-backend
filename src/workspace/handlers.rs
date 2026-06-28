//! HTTP handlers for team workspace endpoints.
//!
//! Provides handlers for creating team workspaces, managing invites,
//! joining via invite codes, removing members, and listing workspace details.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::errors::AppError;
use crate::middleware::auth_extractor::AccessTokenClaims;
use crate::workspace::service::{TeamService, WorkspaceService};

// ─── Request Body DTOs ──────────────────────────────────────────────────────────

/// Request body for creating a team workspace.
#[derive(Debug, Deserialize)]
pub struct CreateTeamRequest {
    pub name: String,
}

/// Request body for creating an invite link.
#[derive(Debug, Deserialize)]
pub struct CreateInviteRequest {
    pub max_uses: Option<i32>,
    pub expires_in_days: Option<i64>,
}

/// Request body for joining a workspace via invite code.
#[derive(Debug, Deserialize)]
pub struct JoinViaInviteRequest {
    pub invite_code: String,
}

// ─── Handlers ───────────────────────────────────────────────────────────────────

/// POST /teams
///
/// Create a new team workspace. The authenticated user becomes the owner.
pub async fn create_team_handler(
    Extension(service): Extension<Arc<WorkspaceService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Json(body): Json<CreateTeamRequest>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.create_team_workspace(claims.sub, &body.name).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// POST /teams/{workspace_id}/invites
///
/// Create an invite link for a team workspace.
pub async fn create_invite_handler(
    Extension(service): Extension<Arc<TeamService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<CreateInviteRequest>,
) -> Result<impl IntoResponse, AppError> {
    let response = service
        .create_invite(workspace_id, claims.sub, body.max_uses, body.expires_in_days)
        .await?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// POST /teams/{workspace_id}/join
///
/// Join a team workspace using an invite code.
pub async fn join_via_invite_handler(
    Extension(service): Extension<Arc<TeamService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Path(_workspace_id): Path<Uuid>,
    Json(body): Json<JoinViaInviteRequest>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.join_via_invite(body.invite_code, claims.sub).await?;
    Ok((StatusCode::OK, Json(response)))
}

/// DELETE /teams/{workspace_id}/members/{user_id}
///
/// Remove a member from a team workspace. Only the owner can do this.
pub async fn remove_member_handler(
    Extension(service): Extension<Arc<TeamService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Path((workspace_id, user_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, AppError> {
    service.remove_member(workspace_id, claims.sub, user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// GET /teams/{workspace_id}
///
/// Get team workspace details.
pub async fn get_team_handler(
    Extension(service): Extension<Arc<WorkspaceService>>,
    Path(workspace_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.get_workspace(workspace_id).await?;
    Ok((StatusCode::OK, Json(response)))
}

/// GET /teams/{workspace_id}/members
///
/// List all members of a team workspace.
pub async fn list_members_handler(
    Extension(service): Extension<Arc<TeamService>>,
    Path(workspace_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.list_members(workspace_id).await?;
    Ok((StatusCode::OK, Json(response)))
}
