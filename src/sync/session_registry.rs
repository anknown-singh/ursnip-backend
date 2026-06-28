use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::errors::AppError;

// ─── Constants ──────────────────────────────────────────────────────────────────

/// Maximum WebSocket connections per user.
const MAX_CONNECTIONS_PER_USER: usize = 5;

// ─── Types ──────────────────────────────────────────────────────────────────────

/// Message that can be sent to a WebSocket connection.
#[derive(Debug, Clone)]
pub enum WsMessage {
    /// Send a JSON message to the client.
    Send(Value),
    /// Close the connection with the given code and reason.
    Close(u16, String),
}

/// Represents a single WebSocket session connected to a specific workspace.
#[derive(Debug, Clone)]
pub struct WsSession {
    pub session_id: Uuid,
    pub user_id: Uuid,
    pub workspace_id: Uuid,
    pub sender: mpsc::UnboundedSender<WsMessage>,
    pub connected_at: DateTime<Utc>,
}

/// Tracks a user's WebSocket connection (for per-user limit enforcement).
#[derive(Debug, Clone)]
pub struct UserConnection {
    pub session_id: Uuid,
    pub workspace_id: Uuid,
    pub sender: mpsc::UnboundedSender<WsMessage>,
    pub connected_at: DateTime<Utc>,
}

// ─── Registry ───────────────────────────────────────────────────────────────────

/// Registry managing all active WebSocket sessions.
///
/// Provides workspace-scoped broadcasting and user-scoped session management.
/// Thread-safe and designed for concurrent access from multiple async tasks.
pub struct SessionRegistry {
    /// workspace_id → list of active sessions for that workspace
    workspace_sessions: DashMap<Uuid, Vec<WsSession>>,
    /// user_id → list of active connections for that user
    user_connections: DashMap<Uuid, Vec<UserConnection>>,
    /// Total number of active connections server-wide
    total_connections: AtomicUsize,
    /// Maximum connections allowed server-wide
    max_connections: usize,
}

impl SessionRegistry {
    /// Create a new `SessionRegistry` with the given server-wide connection limit.
    pub fn new(max_connections: usize) -> Self {
        Self {
            workspace_sessions: DashMap::new(),
            user_connections: DashMap::new(),
            total_connections: AtomicUsize::new(0),
            max_connections,
        }
    }

    /// Register a new WebSocket session.
    ///
    /// Enforces:
    /// - Server-wide connection limit (`max_connections`). Returns `ServiceUnavailable` (503) if exceeded.
    /// - Per-user connection limit (5). Closes the oldest connection if a 6th connects.
    ///
    /// Requirements: 2.23, 2.24, 7.42, 7.43
    pub fn register(&self, session: WsSession) -> Result<(), AppError> {
        // Check server-wide limit. Use compare-exchange loop to atomically increment
        // only if we're below the limit.
        loop {
            let current = self.total_connections.load(Ordering::Acquire);
            if current >= self.max_connections {
                return Err(AppError::ServiceUnavailable);
            }
            match self.total_connections.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(_) => continue, // Retry on contention
            }
        }

        let user_id = session.user_id;
        let workspace_id = session.workspace_id;

        // Enforce per-user connection limit: close oldest if exceeding MAX_CONNECTIONS_PER_USER
        {
            let mut user_conns = self.user_connections.entry(user_id).or_default();
            if user_conns.len() >= MAX_CONNECTIONS_PER_USER {
                // Sort by connected_at ascending so the oldest is first
                user_conns.sort_by_key(|c| c.connected_at);

                // Close the oldest connection
                let oldest = user_conns.remove(0);
                let _ = oldest.sender.send(WsMessage::Close(
                    1008,
                    "Connection limit exceeded".to_string(),
                ));

                // Remove the oldest from workspace_sessions as well
                self.remove_from_workspace(oldest.workspace_id, oldest.session_id);

                // Decrement total since we're evicting one
                self.total_connections.fetch_sub(1, Ordering::AcqRel);
            }

            // Add the new connection to user map
            user_conns.push(UserConnection {
                session_id: session.session_id,
                workspace_id: session.workspace_id,
                sender: session.sender.clone(),
                connected_at: session.connected_at,
            });
        }

        // Add session to workspace map
        {
            let mut workspace = self.workspace_sessions.entry(workspace_id).or_default();
            workspace.push(session);
        }

        Ok(())
    }

    /// Unregister a WebSocket session by its identifiers.
    ///
    /// Removes the session from both workspace and user maps and decrements the total counter.
    ///
    /// Requirements: 2.25
    pub fn unregister(&self, session_id: Uuid, user_id: Uuid, workspace_id: Uuid) {
        // Remove from workspace map
        self.remove_from_workspace(workspace_id, session_id);

        // Remove from user map
        if let Some(mut user_conns) = self.user_connections.get_mut(&user_id) {
            user_conns.retain(|c| c.session_id != session_id);
            if user_conns.is_empty() {
                drop(user_conns);
                self.user_connections.remove(&user_id);
            }
        }

        // Decrement total
        self.total_connections.fetch_sub(1, Ordering::AcqRel);
    }

    /// Broadcast a JSON message to all sessions in a workspace, optionally excluding the originator.
    ///
    /// Requirements: 7.44
    pub fn broadcast_to_workspace(
        &self,
        workspace_id: Uuid,
        message: Value,
        exclude_session_id: Option<Uuid>,
    ) {
        if let Some(sessions) = self.workspace_sessions.get(&workspace_id) {
            for session in sessions.iter() {
                if Some(session.session_id) == exclude_session_id {
                    continue;
                }
                // Best-effort send; ignore closed channels
                let _ = session.sender.send(WsMessage::Send(message.clone()));
            }
        }
    }

    /// Close all WebSocket sessions for a given user.
    ///
    /// Sends a Close message with the provided code and reason to every connection the user has,
    /// then removes them from all maps.
    ///
    /// Called during logout, account suspension, password reset, or lockout.
    ///
    /// Requirements: 7.42, 7.45
    pub fn close_user_sessions(&self, user_id: Uuid, code: u16, reason: String) {
        if let Some((_, connections)) = self.user_connections.remove(&user_id) {
            for conn in &connections {
                let _ = conn
                    .sender
                    .send(WsMessage::Close(code, reason.clone()));

                // Remove from workspace map
                self.remove_from_workspace(conn.workspace_id, conn.session_id);
            }

            // Decrement total by number of removed connections
            self.total_connections
                .fetch_sub(connections.len(), Ordering::AcqRel);
        }
    }

    /// Close all WebSocket sessions for a given workspace.
    ///
    /// Sends a Close message with the provided code and reason to every session in the workspace,
    /// then removes them from the workspace map and the user maps.
    ///
    /// Called when a workspace is deactivated.
    ///
    /// Requirements: 7.43, 7.45
    pub fn close_workspace_sessions(&self, workspace_id: Uuid, code: u16, reason: String) {
        if let Some((_, sessions)) = self.workspace_sessions.remove(&workspace_id) {
            for session in &sessions {
                let _ = session
                    .sender
                    .send(WsMessage::Close(code, reason.clone()));

                // Remove from user map
                if let Some(mut user_conns) = self.user_connections.get_mut(&session.user_id) {
                    user_conns.retain(|c| c.session_id != session.session_id);
                    if user_conns.is_empty() {
                        drop(user_conns);
                        self.user_connections.remove(&session.user_id);
                    }
                }
            }

            // Decrement total by number of removed sessions
            self.total_connections
                .fetch_sub(sessions.len(), Ordering::AcqRel);
        }
    }

    /// Returns the current total number of active connections.
    pub fn total_connections(&self) -> usize {
        self.total_connections.load(Ordering::Acquire)
    }

    /// Close all active WebSocket sessions with the given code and reason.
    ///
    /// Used during graceful shutdown to notify all connected clients.
    ///
    /// Requirements: 7.68
    pub fn close_all_sessions(&self, code: u16, reason: String) {
        // Drain all workspace sessions
        let workspace_ids: Vec<Uuid> = self
            .workspace_sessions
            .iter()
            .map(|entry| *entry.key())
            .collect();

        for workspace_id in workspace_ids {
            if let Some((_, sessions)) = self.workspace_sessions.remove(&workspace_id) {
                for session in &sessions {
                    let _ = session.sender.send(WsMessage::Close(code, reason.clone()));
                }
            }
        }

        // Clear user connections map
        self.user_connections.clear();

        // Reset total to 0
        self.total_connections.store(0, Ordering::Release);
    }

    // ─── Internal Helpers ───────────────────────────────────────────────────────

    /// Remove a session from the workspace map by session_id.
    fn remove_from_workspace(&self, workspace_id: Uuid, session_id: Uuid) {
        if let Some(mut sessions) = self.workspace_sessions.get_mut(&workspace_id) {
            sessions.retain(|s| s.session_id != session_id);
            if sessions.is_empty() {
                drop(sessions);
                self.workspace_sessions.remove(&workspace_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a WsSession with a sender, returning the receiver for assertions.
    fn make_session(
        user_id: Uuid,
        workspace_id: Uuid,
    ) -> (WsSession, mpsc::UnboundedReceiver<WsMessage>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let session = WsSession {
            session_id: Uuid::new_v4(),
            user_id,
            workspace_id,
            sender: tx,
            connected_at: Utc::now(),
        };
        (session, rx)
    }

    #[test]
    fn test_register_and_unregister() {
        let registry = SessionRegistry::new(100);
        let user_id = Uuid::new_v4();
        let workspace_id = Uuid::new_v4();

        let (session, _rx) = make_session(user_id, workspace_id);
        let session_id = session.session_id;

        registry.register(session).unwrap();
        assert_eq!(registry.total_connections(), 1);

        registry.unregister(session_id, user_id, workspace_id);
        assert_eq!(registry.total_connections(), 0);
    }

    #[test]
    fn test_server_wide_limit_rejects_with_service_unavailable() {
        let registry = SessionRegistry::new(2);
        let user_id = Uuid::new_v4();
        let workspace_id = Uuid::new_v4();

        let (s1, _r1) = make_session(user_id, workspace_id);
        let (s2, _r2) = make_session(user_id, workspace_id);

        // Use different users to avoid per-user limit
        let user_id2 = Uuid::new_v4();
        let (s3, _r3) = make_session(user_id2, workspace_id);

        registry.register(s1).unwrap();
        registry.register(s2).unwrap();

        // Third registration should fail with ServiceUnavailable
        let result = registry.register(s3);
        assert!(result.is_err());
    }

    #[test]
    fn test_per_user_limit_closes_oldest() {
        let registry = SessionRegistry::new(100);
        let user_id = Uuid::new_v4();
        let workspace_id = Uuid::new_v4();

        let mut receivers = Vec::new();
        let mut session_ids = Vec::new();

        // Register MAX_CONNECTIONS_PER_USER sessions
        for _ in 0..MAX_CONNECTIONS_PER_USER {
            let (session, rx) = make_session(user_id, workspace_id);
            session_ids.push(session.session_id);
            registry.register(session).unwrap();
            receivers.push(rx);
        }

        assert_eq!(registry.total_connections(), MAX_CONNECTIONS_PER_USER);

        // Register one more — should close the oldest
        let (extra_session, _extra_rx) = make_session(user_id, workspace_id);
        registry.register(extra_session).unwrap();

        // Total should still be MAX_CONNECTIONS_PER_USER (evicted one, added one)
        assert_eq!(registry.total_connections(), MAX_CONNECTIONS_PER_USER);

        // The oldest receiver should have received a Close message
        let msg = receivers[0].try_recv().unwrap();
        match msg {
            WsMessage::Close(code, _) => assert_eq!(code, 1008),
            _ => panic!("Expected Close message"),
        }
    }

    #[tokio::test]
    async fn test_broadcast_to_workspace_excludes_originator() {
        let registry = SessionRegistry::new(100);
        let user_id = Uuid::new_v4();
        let workspace_id = Uuid::new_v4();

        let (s1, mut r1) = make_session(user_id, workspace_id);
        let s1_id = s1.session_id;
        let (s2, mut r2) = make_session(user_id, workspace_id);

        registry.register(s1).unwrap();
        registry.register(s2).unwrap();

        let msg = serde_json::json!({"type": "snippet_updated"});
        registry.broadcast_to_workspace(workspace_id, msg, Some(s1_id));

        // s1 (originator) should NOT receive the message
        assert!(r1.try_recv().is_err());

        // s2 should receive the message
        let received = r2.try_recv().unwrap();
        match received {
            WsMessage::Send(v) => assert_eq!(v["type"], "snippet_updated"),
            _ => panic!("Expected Send message"),
        }
    }

    #[test]
    fn test_close_user_sessions() {
        let registry = SessionRegistry::new(100);
        let user_id = Uuid::new_v4();
        let workspace_id = Uuid::new_v4();

        let (s1, mut r1) = make_session(user_id, workspace_id);
        let (s2, mut r2) = make_session(user_id, workspace_id);

        registry.register(s1).unwrap();
        registry.register(s2).unwrap();

        assert_eq!(registry.total_connections(), 2);

        registry.close_user_sessions(user_id, 1001, "Account suspended".to_string());

        assert_eq!(registry.total_connections(), 0);

        // Both receivers should have Close messages
        match r1.try_recv().unwrap() {
            WsMessage::Close(code, reason) => {
                assert_eq!(code, 1001);
                assert_eq!(reason, "Account suspended");
            }
            _ => panic!("Expected Close message"),
        }
        match r2.try_recv().unwrap() {
            WsMessage::Close(code, reason) => {
                assert_eq!(code, 1001);
                assert_eq!(reason, "Account suspended");
            }
            _ => panic!("Expected Close message"),
        }
    }

    #[test]
    fn test_close_workspace_sessions() {
        let registry = SessionRegistry::new(100);
        let user_id1 = Uuid::new_v4();
        let user_id2 = Uuid::new_v4();
        let workspace_id = Uuid::new_v4();

        let (s1, mut r1) = make_session(user_id1, workspace_id);
        let (s2, mut r2) = make_session(user_id2, workspace_id);

        registry.register(s1).unwrap();
        registry.register(s2).unwrap();

        assert_eq!(registry.total_connections(), 2);

        registry.close_workspace_sessions(workspace_id, 1001, "Workspace deactivated".to_string());

        assert_eq!(registry.total_connections(), 0);

        match r1.try_recv().unwrap() {
            WsMessage::Close(code, reason) => {
                assert_eq!(code, 1001);
                assert_eq!(reason, "Workspace deactivated");
            }
            _ => panic!("Expected Close message"),
        }
        match r2.try_recv().unwrap() {
            WsMessage::Close(code, reason) => {
                assert_eq!(code, 1001);
                assert_eq!(reason, "Workspace deactivated");
            }
            _ => panic!("Expected Close message"),
        }
    }
}
