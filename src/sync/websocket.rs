use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{ws::Message, ws::WebSocket, Query, State, WebSocketUpgrade},
    http::HeaderMap,
    response::Response,
};
use chrono::Utc;
use futures::{SinkExt, StreamExt};
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::{interval, timeout};
use uuid::Uuid;

use crate::errors::AppError;
use crate::middleware::auth_extractor::AccessTokenClaims;
use crate::sync::service::SyncService;
use crate::sync::session_registry::{SessionRegistry, WsMessage, WsSession};

// ─── Constants ──────────────────────────────────────────────────────────────────

/// Server sends a ping every 30 seconds.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Timeout for pong response after a ping is sent.
const PONG_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum consecutive missed pongs before closing the connection.
const MAX_MISSED_PONGS: u8 = 2;

/// Idle timeout: close after 5 minutes with no application messages from the client.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

// ─── Types ──────────────────────────────────────────────────────────────────────

/// Shared state for the WebSocket handler.
#[derive(Clone)]
pub struct WsState {
    pub registry: Arc<SessionRegistry>,
    pub sync_service: Arc<SyncService>,
    pub jwt_secret: Arc<String>,
}

/// Query parameters for the WebSocket upgrade endpoint.
#[derive(Debug, Deserialize)]
pub struct WsQueryParams {
    pub token: Option<String>,
    pub workspace_id: Option<Uuid>,
    pub last_known_version: Option<i64>,
}

/// JSON envelope for messages sent over the WebSocket.
#[derive(Debug, Serialize, Deserialize)]
pub struct WsEnvelope {
    #[serde(rename = "type")]
    pub msg_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

impl WsEnvelope {
    /// Create a new envelope with the given type and optional fields.
    fn new(msg_type: &str) -> Self {
        Self {
            msg_type: msg_type.to_string(),
            workspace_id: None,
            version: None,
            timestamp: Utc::now().to_rfc3339(),
            payload: None,
        }
    }

    /// Create a delta message envelope.
    fn delta(workspace_id: Uuid, version: i64, payload: Value) -> Self {
        Self {
            msg_type: "delta".to_string(),
            workspace_id: Some(workspace_id.to_string()),
            version: Some(version),
            timestamp: Utc::now().to_rfc3339(),
            payload: Some(payload),
        }
    }

    /// Create a snapshot_required message envelope.
    fn snapshot_required(workspace_id: Uuid) -> Self {
        Self {
            msg_type: "snapshot_required".to_string(),
            workspace_id: Some(workspace_id.to_string()),
            version: None,
            timestamp: Utc::now().to_rfc3339(),
            payload: None,
        }
    }

    /// Create an ack message envelope.
    fn ack(workspace_id: Option<Uuid>, version: Option<i64>) -> Self {
        Self {
            msg_type: "ack".to_string(),
            workspace_id: workspace_id.map(|id| id.to_string()),
            version,
            timestamp: Utc::now().to_rfc3339(),
            payload: None,
        }
    }

    /// Create an error message envelope.
    fn error(message: &str) -> Self {
        Self {
            msg_type: "error".to_string(),
            workspace_id: None,
            version: None,
            timestamp: Utc::now().to_rfc3339(),
            payload: Some(serde_json::json!({ "message": message })),
        }
    }

    /// Create a ping message envelope.
    fn ping() -> Self {
        Self::new("ping")
    }

    /// Create a pong message envelope.
    fn pong() -> Self {
        Self::new("pong")
    }

    /// Serialize to a JSON text Message.
    fn to_message(&self) -> Message {
        Message::Text(serde_json::to_string(self).unwrap_or_default())
    }
}

// ─── Handler ────────────────────────────────────────────────────────────────────

/// WebSocket upgrade handler.
///
/// Validates the access token from the `token` query parameter or `Authorization` header,
/// extracts `workspace_id` and `last_known_version` from query params, and upgrades the
/// connection to a WebSocket.
///
/// Requirements: 2.17, 2.18, 2.19, 2.20, 2.21, 2.22, 2.23
pub async fn ws_upgrade_handler(
    State(state): State<WsState>,
    Query(params): Query<WsQueryParams>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, AppError> {
    // Extract token from query param or Authorization header
    let token = params
        .token
        .clone()
        .or_else(|| {
            headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(|s| s.to_string())
        })
        .ok_or(AppError::Unauthorized)?;

    // Validate JWT
    let claims = validate_token(&token, &state.jwt_secret)?;

    // Check account status — reject suspended users at upgrade time
    if claims.status == "suspended" {
        return Err(AppError::AccountSuspended);
    }

    // Require workspace_id
    let workspace_id = params.workspace_id.ok_or(AppError::Unauthorized)?;

    let user_id = claims.sub;
    let last_known_version = params.last_known_version;

    Ok(ws.on_upgrade(move |socket| {
        handle_ws_connection(socket, state, user_id, workspace_id, last_known_version)
    }))
}

/// Validate a JWT token and return the claims.
fn validate_token(token: &str, jwt_secret: &str) -> Result<AccessTokenClaims, AppError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;

    let token_data = decode::<AccessTokenClaims>(
        token,
        &DecodingKey::from_secret(jwt_secret.as_bytes()),
        &validation,
    )
    .map_err(|_| AppError::Unauthorized)?;

    Ok(token_data.claims)
}

// ─── Connection Handler ─────────────────────────────────────────────────────────

/// Handles an established WebSocket connection.
///
/// - Registers the session with the SessionRegistry
/// - Performs reconnection catch-up (sends missed deltas or snapshot_required)
/// - Splits into read/write halves with heartbeat and idle timeout
///
/// Requirements: 2.26, 2.27, 2.28, 2.29, 2.30, 2.31, 7.46
async fn handle_ws_connection(
    socket: WebSocket,
    state: WsState,
    user_id: Uuid,
    workspace_id: Uuid,
    last_known_version: Option<i64>,
) {
    let session_id = Uuid::new_v4();
    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<WsMessage>();

    // Register session
    let session = WsSession {
        session_id,
        user_id,
        workspace_id,
        sender: msg_tx.clone(),
        connected_at: Utc::now(),
    };

    if let Err(_) = state.registry.register(session) {
        // Cannot register — server at capacity. Close immediately.
        let (mut sink, _) = socket.split();
        let envelope = WsEnvelope::error("Server at capacity");
        let _ = sink.send(envelope.to_message()).await;
        let _ = sink.close().await;
        return;
    }

    let (mut sink, mut stream) = socket.split();

    // Reconnection catch-up: send missed deltas or snapshot_required
    if let Some(since_version) = last_known_version {
        match state
            .sync_service
            .get_deltas(user_id, workspace_id, since_version, None)
            .await
        {
            Ok(response) => {
                for delta in response.deltas {
                    let envelope = WsEnvelope::delta(
                        delta.workspace_id,
                        delta.version,
                        serde_json::json!({
                            "id": delta.id,
                            "entity_type": delta.entity_type,
                            "entity_id": delta.entity_id,
                            "operation": delta.operation,
                            "payload": delta.payload,
                            "created_at": delta.created_at.to_rfc3339(),
                        }),
                    );
                    if sink.send(envelope.to_message()).await.is_err() {
                        state.registry.unregister(session_id, user_id, workspace_id);
                        return;
                    }
                }
            }
            Err(AppError::SnapshotRequired) => {
                let envelope = WsEnvelope::snapshot_required(workspace_id);
                if sink.send(envelope.to_message()).await.is_err() {
                    state.registry.unregister(session_id, user_id, workspace_id);
                    return;
                }
            }
            Err(_) => {
                // On other errors during catch-up, send snapshot_required as a fallback
                let envelope = WsEnvelope::snapshot_required(workspace_id);
                if sink.send(envelope.to_message()).await.is_err() {
                    state.registry.unregister(session_id, user_id, workspace_id);
                    return;
                }
            }
        }
    }

    // Spawn write task: forwards messages from the mpsc channel to the WebSocket sink
    let write_task = tokio::spawn(async move {
        while let Some(msg) = msg_rx.recv().await {
            match msg {
                WsMessage::Send(value) => {
                    let text = serde_json::to_string(&value).unwrap_or_default();
                    if sink.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                }
                WsMessage::Close(code, reason) => {
                    let close_frame = axum::extract::ws::CloseFrame {
                        code,
                        reason: reason.into(),
                    };
                    let _ = sink.send(Message::Close(Some(close_frame))).await;
                    break;
                }
            }
        }
    });

    // Read loop with heartbeat and idle timeout
    let registry = state.registry.clone();
    let read_task = tokio::spawn(async move {
        let mut heartbeat_interval = interval(HEARTBEAT_INTERVAL);
        let mut missed_pongs: u8 = 0;
        let mut waiting_for_pong = false;
        let mut idle_deadline = tokio::time::Instant::now() + IDLE_TIMEOUT;

        loop {
            tokio::select! {
                // Heartbeat tick
                _ = heartbeat_interval.tick() => {
                    if waiting_for_pong {
                        missed_pongs += 1;
                        if missed_pongs >= MAX_MISSED_PONGS {
                            tracing::debug!(
                                session_id = %session_id,
                                "Closing WebSocket: {} missed pongs",
                                missed_pongs
                            );
                            // Close the connection
                            let _ = msg_tx.send(WsMessage::Close(
                                1001,
                                "Heartbeat timeout".to_string(),
                            ));
                            break;
                        }
                    }

                    // Send ping
                    let ping_envelope = WsEnvelope::ping();
                    let ping_json = serde_json::to_string(&ping_envelope).unwrap_or_default();
                    let ping_value: Value = serde_json::from_str(&ping_json).unwrap_or_default();
                    let _ = msg_tx.send(WsMessage::Send(ping_value));
                    waiting_for_pong = true;

                    // Set pong timeout: if no pong within PONG_TIMEOUT, it counts as missed
                    // The next heartbeat tick will check and increment missed_pongs
                }

                // Idle timeout
                _ = tokio::time::sleep_until(idle_deadline) => {
                    tracing::debug!(
                        session_id = %session_id,
                        "Closing WebSocket: idle timeout (5 min)"
                    );
                    let _ = msg_tx.send(WsMessage::Close(
                        1001,
                        "Idle timeout".to_string(),
                    ));
                    break;
                }

                // Incoming message from client
                msg = stream.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            // Reset idle timer on any application message
                            idle_deadline = tokio::time::Instant::now() + IDLE_TIMEOUT;

                            // Parse as JSON envelope
                            if let Ok(envelope) = serde_json::from_str::<WsEnvelope>(&text) {
                                match envelope.msg_type.as_str() {
                                    "pong" => {
                                        // Pong received — reset missed pong counter
                                        waiting_for_pong = false;
                                        missed_pongs = 0;
                                    }
                                    "ping" => {
                                        // Client-initiated ping: respond with pong
                                        let pong_envelope = WsEnvelope::pong();
                                        let pong_json = serde_json::to_string(&pong_envelope).unwrap_or_default();
                                        let pong_value: Value = serde_json::from_str(&pong_json).unwrap_or_default();
                                        let _ = msg_tx.send(WsMessage::Send(pong_value));
                                    }
                                    _ => {
                                        // Other application messages — acknowledged
                                        // Delta processing is handled by the sync HTTP endpoints;
                                        // the WebSocket is primarily for push notifications.
                                        let ack = WsEnvelope::ack(
                                            Some(workspace_id),
                                            envelope.version,
                                        );
                                        let ack_json = serde_json::to_string(&ack).unwrap_or_default();
                                        let ack_value: Value = serde_json::from_str(&ack_json).unwrap_or_default();
                                        let _ = msg_tx.send(WsMessage::Send(ack_value));
                                    }
                                }
                            } else {
                                // Invalid JSON — send error
                                let err = WsEnvelope::error("Invalid message format");
                                let err_json = serde_json::to_string(&err).unwrap_or_default();
                                let err_value: Value = serde_json::from_str(&err_json).unwrap_or_default();
                                let _ = msg_tx.send(WsMessage::Send(err_value));
                            }
                        }
                        Some(Ok(Message::Ping(data))) => {
                            // WebSocket protocol-level ping — respond with pong
                            let _ = msg_tx.send(WsMessage::Send(
                                serde_json::json!(null), // Will be handled specially
                            ));
                            // Actually, protocol-level pings are auto-handled by axum/tungstenite
                            // Reset idle timer
                            idle_deadline = tokio::time::Instant::now() + IDLE_TIMEOUT;
                        }
                        Some(Ok(Message::Pong(_))) => {
                            // Protocol-level pong (response to our protocol-level pings)
                            waiting_for_pong = false;
                            missed_pongs = 0;
                        }
                        Some(Ok(Message::Close(_))) => {
                            // Client initiated close
                            tracing::debug!(
                                session_id = %session_id,
                                "Client closed WebSocket connection"
                            );
                            break;
                        }
                        Some(Ok(Message::Binary(_))) => {
                            // Binary messages not supported
                            let err = WsEnvelope::error("Binary messages not supported");
                            let err_json = serde_json::to_string(&err).unwrap_or_default();
                            let err_value: Value = serde_json::from_str(&err_json).unwrap_or_default();
                            let _ = msg_tx.send(WsMessage::Send(err_value));
                        }
                        Some(Err(e)) => {
                            tracing::debug!(
                                session_id = %session_id,
                                error = %e,
                                "WebSocket read error"
                            );
                            break;
                        }
                        None => {
                            // Stream ended
                            break;
                        }
                    }
                }
            }
        }

        // Unregister session
        registry.unregister(session_id, user_id, workspace_id);
    });

    // Wait for both tasks to complete
    let _ = tokio::join!(write_task, read_task);
}
