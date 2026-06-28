//! REST handlers for the sync module.
//!
//! Provides HTTP handlers for snippet and folder CRUD operations,
//! batch operations, snapshot retrieval, delta polling, and WebSocket upgrade.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, Query},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::errors::AppError;
use crate::middleware::auth_extractor::AccessTokenClaims;
use crate::sync::service::{
    BatchOperationsPayload, CreateFolderPayload, CreateSnippetPayload, SyncService,
    UpdateFolderPayload, UpdateSnippetPayload,
};
use crate::sync::session_registry::SessionRegistry;

// ─── Query Parameter DTOs ───────────────────────────────────────────────────────

/// Query parameters for the GET /sync/snapshot endpoint.
#[derive(Debug, Deserialize)]
pub struct SnapshotQueryParams {
    pub workspace_id: Uuid,
}

/// Query parameters for the GET /sync/deltas endpoint.
#[derive(Debug, Deserialize)]
pub struct DeltasQueryParams {
    pub workspace_id: Uuid,
    pub since_version: Option<i64>,
    pub limit: Option<i64>,
}

// ─── Snippet Handlers ───────────────────────────────────────────────────────────

/// POST /sync/snippets
///
/// Create a new snippet in a workspace.
pub async fn create_snippet_handler(
    Extension(service): Extension<Arc<SyncService>>,
    Extension(registry): Extension<Arc<SessionRegistry>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Json(payload): Json<CreateSnippetPayload>,
) -> Result<impl IntoResponse, AppError> {
    let workspace_id = payload.workspace_id;
    let response = service.create_snippet(claims.sub, payload).await?;

    // Broadcast delta to workspace via WebSocket
    registry.broadcast_to_workspace(
        workspace_id,
        serde_json::json!({
            "type": "delta",
            "workspace_id": workspace_id.to_string(),
            "version": response.version,
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "payload": serde_json::to_value(&response).unwrap_or_default()
        }),
        None,
    );

    Ok((StatusCode::CREATED, Json(response)))
}

/// PATCH /sync/snippets/:id
///
/// Update an existing snippet.
pub async fn update_snippet_handler(
    Extension(service): Extension<Arc<SyncService>>,
    Extension(registry): Extension<Arc<SessionRegistry>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Path(snippet_id): Path<Uuid>,
    Json(payload): Json<UpdateSnippetPayload>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.update_snippet(claims.sub, snippet_id, payload).await?;

    // Broadcast delta to workspace via WebSocket
    registry.broadcast_to_workspace(
        response.workspace_id,
        serde_json::json!({
            "type": "delta",
            "workspace_id": response.workspace_id.to_string(),
            "version": response.version,
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "payload": serde_json::to_value(&response).unwrap_or_default()
        }),
        None,
    );

    Ok((StatusCode::OK, Json(response)))
}

/// DELETE /sync/snippets/:id
///
/// Soft-delete a snippet.
pub async fn delete_snippet_handler(
    Extension(service): Extension<Arc<SyncService>>,
    Extension(_registry): Extension<Arc<SessionRegistry>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Path(snippet_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    // We need to get the workspace_id before deletion for broadcasting.
    // The service handles the deletion; we'll broadcast after success.
    // Since delete_snippet returns () we need the workspace_id from elsewhere.
    // For now, we broadcast with the snippet_id context available from the path.
    // The service internally validates membership and workspace, so we trust it succeeded.
    service.delete_snippet(claims.sub, snippet_id).await?;

    // Note: We don't have workspace_id directly from the delete response.
    // In a production system, you'd either return it from the service or look it up.
    // For broadcasting, we'll skip since we don't have the workspace_id.
    // TODO: Consider returning workspace_id from delete_snippet for broadcast support.

    Ok(StatusCode::NO_CONTENT)
}

// ─── Batch Handler ──────────────────────────────────────────────────────────────

/// POST /sync/snippets/batch
///
/// Execute batch snippet operations (create, update, delete) atomically.
pub async fn batch_operations_handler(
    Extension(service): Extension<Arc<SyncService>>,
    Extension(registry): Extension<Arc<SessionRegistry>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Json(payload): Json<BatchOperationsPayload>,
) -> Result<impl IntoResponse, AppError> {
    let workspace_id = payload.workspace_id;
    let response = service.batch_operations(claims.sub, payload).await?;

    // Broadcast delta to workspace via WebSocket
    registry.broadcast_to_workspace(
        workspace_id,
        serde_json::json!({
            "type": "delta",
            "workspace_id": workspace_id.to_string(),
            "version": response.workspace_version,
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "payload": serde_json::to_value(&response).unwrap_or_default()
        }),
        None,
    );

    Ok((StatusCode::OK, Json(response)))
}

// ─── Folder Handlers ────────────────────────────────────────────────────────────

/// POST /sync/folders
///
/// Create a new folder in a workspace.
pub async fn create_folder_handler(
    Extension(service): Extension<Arc<SyncService>>,
    Extension(registry): Extension<Arc<SessionRegistry>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Json(payload): Json<CreateFolderPayload>,
) -> Result<impl IntoResponse, AppError> {
    let workspace_id = payload.workspace_id;
    let response = service.create_folder(claims.sub, payload).await?;

    // Broadcast delta to workspace via WebSocket
    registry.broadcast_to_workspace(
        workspace_id,
        serde_json::json!({
            "type": "delta",
            "workspace_id": workspace_id.to_string(),
            "version": response.version,
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "payload": serde_json::to_value(&response).unwrap_or_default()
        }),
        None,
    );

    Ok((StatusCode::CREATED, Json(response)))
}

/// PATCH /sync/folders/:id
///
/// Update an existing folder.
pub async fn update_folder_handler(
    Extension(service): Extension<Arc<SyncService>>,
    Extension(registry): Extension<Arc<SessionRegistry>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Path(folder_id): Path<Uuid>,
    Json(payload): Json<UpdateFolderPayload>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.update_folder(claims.sub, folder_id, payload).await?;

    // Broadcast delta to workspace via WebSocket
    registry.broadcast_to_workspace(
        response.workspace_id,
        serde_json::json!({
            "type": "delta",
            "workspace_id": response.workspace_id.to_string(),
            "version": response.version,
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "payload": serde_json::to_value(&response).unwrap_or_default()
        }),
        None,
    );

    Ok((StatusCode::OK, Json(response)))
}

/// DELETE /sync/folders/:id
///
/// Soft-delete a folder.
pub async fn delete_folder_handler(
    Extension(service): Extension<Arc<SyncService>>,
    Extension(_registry): Extension<Arc<SessionRegistry>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Path(folder_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    service.delete_folder(claims.sub, folder_id).await?;

    // Note: Similar to delete_snippet, we don't have workspace_id from the void return.
    // TODO: Consider returning workspace_id from delete_folder for broadcast support.

    Ok(StatusCode::NO_CONTENT)
}

// ─── Snapshot & Delta Handlers ──────────────────────────────────────────────────

/// GET /sync/snapshot
///
/// Retrieve a full workspace snapshot (all active snippets and folders).
pub async fn get_snapshot_handler(
    Extension(service): Extension<Arc<SyncService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Query(params): Query<SnapshotQueryParams>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.get_snapshot(claims.sub, params.workspace_id).await?;
    Ok((StatusCode::OK, Json(response)))
}

/// GET /sync/deltas
///
/// Retrieve deltas since a given version for incremental sync.
pub async fn get_deltas_handler(
    Extension(service): Extension<Arc<SyncService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Query(params): Query<DeltasQueryParams>,
) -> Result<impl IntoResponse, AppError> {
    let since_version = params.since_version.unwrap_or(0);
    let response = service
        .get_deltas(claims.sub, params.workspace_id, since_version, params.limit)
        .await?;
    Ok((StatusCode::OK, Json(response)))
}

// ─── WebSocket Handler ──────────────────────────────────────────────────────────

/// GET /sync/ws
///
/// WebSocket upgrade endpoint. Returns 501 Not Implemented as a placeholder
/// until the full WebSocket handler is wired (see sync/websocket.rs).
pub async fn ws_handler() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}
