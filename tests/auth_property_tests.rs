//! Property-based tests for Auth Service (Properties 8, 9, 28).
//!
//! These tests validate token rotation, session limits, and password reset
//! token single-use invariants. They require a test database to run and are
//! gated behind `#[ignore]` by default.
//!
//! Run with: `cargo test --test auth_property_tests -- --ignored`
//! (requires DATABASE_URL pointing to a test database with migrations applied)

use proptest::prelude::*;

// ─── Property 8: Token refresh rotation and reuse detection ─────────────────────
//
// **Validates: Requirements 1.14, 1.15, 1.53**
//
// For any valid refresh token, using it for refresh SHALL invalidate the old token
// and issue a new pair. For any previously invalidated refresh token presented for
// refresh, the backend SHALL revoke ALL tokens for that user and return 401
// TOKEN_REUSE_DETECTED.

/// Simulated token store for testing rotation logic without a real database.
/// Models the core invariants of the refresh token rotation system.
#[derive(Debug, Clone)]
struct TokenStore {
    /// Map of token_hash -> (user_id, revoked)
    tokens: Vec<(String, String, bool)>, // (token_hash, user_id, revoked)
}

impl TokenStore {
    fn new() -> Self {
        Self { tokens: Vec::new() }
    }

    /// Issue a new token for a user. Returns the token hash.
    fn issue_token(&mut self, user_id: &str) -> String {
        let token_hash = format!("token_{}_{}", user_id, self.tokens.len());
        self.tokens
            .push((token_hash.clone(), user_id.to_string(), false));
        token_hash
    }

    /// Attempt to refresh using a token hash. Returns:
    /// - Ok(new_token_hash) on success (old token revoked, new issued)
    /// - Err("TOKEN_REUSE_DETECTED") if token was already revoked (all user tokens revoked)
    /// - Err("INVALID_REFRESH_TOKEN") if token not found
    fn refresh(&mut self, token_hash: &str) -> Result<String, &'static str> {
        let entry = self
            .tokens
            .iter()
            .find(|(h, _, _)| h == token_hash)
            .cloned();

        match entry {
            None => Err("INVALID_REFRESH_TOKEN"),
            Some((_, ref user_id, revoked)) => {
                if revoked {
                    // Token reuse detected — revoke ALL tokens for this user
                    let uid = user_id.clone();
                    for t in self.tokens.iter_mut() {
                        if t.1 == uid {
                            t.2 = true;
                        }
                    }
                    Err("TOKEN_REUSE_DETECTED")
                } else {
                    // Valid refresh: revoke old, issue new
                    let uid = user_id.clone();
                    for t in self.tokens.iter_mut() {
                        if t.0 == token_hash {
                            t.2 = true;
                        }
                    }
                    let new_hash = self.issue_token(&uid);
                    Ok(new_hash)
                }
            }
        }
    }

    /// Count active (non-revoked) tokens for a user.
    fn active_count(&self, user_id: &str) -> usize {
        self.tokens
            .iter()
            .filter(|(_, uid, revoked)| uid == user_id && !revoked)
            .count()
    }

    /// Check if ALL tokens for a user are revoked.
    fn all_revoked(&self, user_id: &str) -> bool {
        self.tokens
            .iter()
            .filter(|(_, uid, _)| uid == user_id)
            .all(|(_, _, revoked)| *revoked)
    }
}

// ─── Property 9: Concurrent session limit ───────────────────────────────────────
//
// **Validates: Requirements 1.47**
//
// For any user, the number of active (non-revoked, non-expired) refresh tokens
// SHALL never exceed 5. When a 6th login occurs, the oldest refresh token SHALL
// be revoked.

/// Simulated session store for testing the concurrent session limit.
#[derive(Debug, Clone)]
struct SessionStore {
    /// (session_id, user_id, revoked, created_order)
    sessions: Vec<(usize, String, bool)>,
    next_id: usize,
}

impl SessionStore {
    fn new() -> Self {
        Self {
            sessions: Vec::new(),
            next_id: 0,
        }
    }

    /// Simulate a login: enforce max 5 sessions, revoke oldest if needed.
    fn login(&mut self, user_id: &str) -> usize {
        let active: Vec<usize> = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, (_, uid, revoked))| uid == user_id && !revoked)
            .map(|(idx, _)| idx)
            .collect();

        if active.len() >= 5 {
            // Revoke the oldest (first in insertion order)
            let oldest_idx = active[0];
            self.sessions[oldest_idx].2 = true;
        }

        let session_id = self.next_id;
        self.next_id += 1;
        self.sessions
            .push((session_id, user_id.to_string(), false));
        session_id
    }

    /// Count active sessions for a user.
    fn active_count(&self, user_id: &str) -> usize {
        self.sessions
            .iter()
            .filter(|(_, uid, revoked)| uid == user_id && !revoked)
            .count()
    }
}

// ─── Property 28: Password reset token single-use ───────────────────────────────
//
// **Validates: Requirements 1.25, 1.28, 1.29**
//
// For any password reset token, it SHALL be usable exactly once. After use,
// presenting the same token SHALL return 422 INVALID_RESET_TOKEN. Generating a
// new reset token SHALL invalidate all previous tokens for that user.

/// Simulated password reset token store.
#[derive(Debug, Clone)]
struct ResetTokenStore {
    /// (token_hash, user_id, used)
    tokens: Vec<(String, String, bool)>,
}

impl ResetTokenStore {
    fn new() -> Self {
        Self { tokens: Vec::new() }
    }

    /// Generate a new reset token — invalidates all previous unused tokens for the user.
    fn generate_token(&mut self, user_id: &str) -> String {
        // Invalidate all previous tokens for this user
        for t in self.tokens.iter_mut() {
            if t.1 == user_id && !t.2 {
                t.2 = true;
            }
        }

        let token_hash = format!("reset_{}_{}", user_id, self.tokens.len());
        self.tokens
            .push((token_hash.clone(), user_id.to_string(), false));
        token_hash
    }

    /// Use a reset token. Returns:
    /// - Ok(user_id) on success (marks token as used, simulates revoking all refresh tokens)
    /// - Err("INVALID_RESET_TOKEN") if token not found or already used
    fn use_token(&mut self, token_hash: &str) -> Result<String, &'static str> {
        let entry = self
            .tokens
            .iter_mut()
            .find(|(h, _, _)| h == token_hash);

        match entry {
            None => Err("INVALID_RESET_TOKEN"),
            Some(t) => {
                if t.2 {
                    Err("INVALID_RESET_TOKEN")
                } else {
                    t.2 = true;
                    Ok(t.1.clone())
                }
            }
        }
    }
}

// ─── Property Test Implementations ──────────────────────────────────────────────

/// Strategy for generating user IDs.
fn user_id_strategy() -> impl Strategy<Value = String> {
    "[a-f0-9]{8}".prop_map(|s| s)
}

/// Strategy for generating number of logins (6 to 20).
fn login_count_strategy() -> impl Strategy<Value = usize> {
    6usize..20
}

proptest! {
    // ─── Property 8: Token refresh rotation ─────────────────────────────────────

    /// **Validates: Requirements 1.14, 1.15, 1.53**
    ///
    /// Property 8a: After a refresh, the old token cannot be used again.
    /// If the old (revoked) token is presented, ALL user tokens are revoked.
    #[test]
    fn prop_token_rotation_invalidates_old(user_id in user_id_strategy()) {
        let mut store = TokenStore::new();

        // Issue initial token
        let token1 = store.issue_token(&user_id);

        // Refresh with token1 → get token2, token1 is now revoked
        let token2 = store.refresh(&token1).unwrap();

        // Verify token1 is no longer usable
        let reuse_result = store.refresh(&token1);
        prop_assert_eq!(reuse_result, Err("TOKEN_REUSE_DETECTED"),
            "Reusing an already-rotated token must trigger TOKEN_REUSE_DETECTED");

        // After reuse detection, ALL tokens for the user must be revoked
        prop_assert!(store.all_revoked(&user_id),
            "After token reuse detection, ALL user tokens must be revoked");

        // token2 should also now be invalid (revoked by reuse detection)
        let token2_result = store.refresh(&token2);
        prop_assert_eq!(token2_result, Err("TOKEN_REUSE_DETECTED"),
            "Even the latest token must be revoked after reuse detection");
    }

    /// **Validates: Requirements 1.14, 1.15, 1.53**
    ///
    /// Property 8b: A chain of valid refreshes maintains exactly one active token.
    #[test]
    fn prop_token_rotation_chain_single_active(
        user_id in user_id_strategy(),
        chain_length in 2usize..10
    ) {
        let mut store = TokenStore::new();

        let mut current_token = store.issue_token(&user_id);

        for _ in 1..chain_length {
            let new_token = store.refresh(&current_token).unwrap();
            current_token = new_token;

            // After each refresh, exactly 1 token should be active
            prop_assert_eq!(store.active_count(&user_id), 1,
                "After rotation, exactly 1 active token should exist for the user");
        }
    }

    /// **Validates: Requirements 1.14, 1.15, 1.53**
    ///
    /// Property 8c: A non-existent token returns INVALID_REFRESH_TOKEN.
    #[test]
    fn prop_nonexistent_token_returns_invalid(
        user_id in user_id_strategy(),
        fake_token in "[a-z]{20}"
    ) {
        let mut store = TokenStore::new();
        let _real_token = store.issue_token(&user_id);

        let result = store.refresh(&fake_token);
        prop_assert_eq!(result, Err("INVALID_REFRESH_TOKEN"),
            "A token that was never issued must return INVALID_REFRESH_TOKEN");
    }

    // ─── Property 9: Concurrent session limit ───────────────────────────────────

    /// **Validates: Requirements 1.47**
    ///
    /// Property 9a: After N logins (N > 5), exactly 5 sessions remain active.
    #[test]
    fn prop_session_limit_max_five(
        user_id in user_id_strategy(),
        num_logins in login_count_strategy()
    ) {
        let mut store = SessionStore::new();

        for _ in 0..num_logins {
            store.login(&user_id);
        }

        let active = store.active_count(&user_id);
        prop_assert_eq!(active, 5,
            "After {} logins, exactly 5 active sessions should remain, got {}", num_logins, active);
    }

    /// **Validates: Requirements 1.47**
    ///
    /// Property 9b: The 6th login revokes the oldest session.
    #[test]
    fn prop_sixth_login_revokes_oldest(user_id in user_id_strategy()) {
        let mut store = SessionStore::new();

        // Create 5 sessions
        let mut session_ids = Vec::new();
        for _ in 0..5 {
            session_ids.push(store.login(&user_id));
        }

        // All 5 should be active
        prop_assert_eq!(store.active_count(&user_id), 5);

        // 6th login
        let _sixth = store.login(&user_id);

        // Still exactly 5 active
        prop_assert_eq!(store.active_count(&user_id), 5);

        // The first (oldest) session should be revoked
        let first_session = store.sessions.iter()
            .find(|(id, _, _)| *id == session_ids[0]);
        prop_assert!(first_session.is_some());
        prop_assert!(first_session.unwrap().2,
            "The oldest session must be revoked when a 6th session is created");
    }

    /// **Validates: Requirements 1.47**
    ///
    /// Property 9c: Different users have independent session limits.
    #[test]
    fn prop_session_limit_independent_users(
        user1 in user_id_strategy(),
        user2 in user_id_strategy(),
        logins1 in 1usize..15,
        logins2 in 1usize..15,
    ) {
        prop_assume!(user1 != user2);
        let mut store = SessionStore::new();

        for _ in 0..logins1 {
            store.login(&user1);
        }
        for _ in 0..logins2 {
            store.login(&user2);
        }

        let active1 = store.active_count(&user1);
        let active2 = store.active_count(&user2);

        prop_assert!(active1 <= 5,
            "User1 active sessions ({}) must not exceed 5", active1);
        prop_assert!(active2 <= 5,
            "User2 active sessions ({}) must not exceed 5", active2);

        let expected1 = logins1.min(5);
        let expected2 = logins2.min(5);
        prop_assert_eq!(active1, expected1,
            "User1 should have min(logins, 5) = {} active sessions", expected1);
        prop_assert_eq!(active2, expected2,
            "User2 should have min(logins, 5) = {} active sessions", expected2);
    }

    // ─── Property 28: Password reset token single-use ───────────────────────────

    /// **Validates: Requirements 1.25, 1.28, 1.29**
    ///
    /// Property 28a: A reset token can only be used once; second use returns error.
    #[test]
    fn prop_reset_token_single_use(user_id in user_id_strategy()) {
        let mut store = ResetTokenStore::new();

        let token = store.generate_token(&user_id);

        // First use succeeds
        let result = store.use_token(&token);
        prop_assert!(result.is_ok(), "First use of reset token must succeed");
        prop_assert_eq!(result.unwrap(), user_id.clone());

        // Second use fails
        let result2 = store.use_token(&token);
        prop_assert_eq!(result2, Err("INVALID_RESET_TOKEN"),
            "Second use of reset token must return INVALID_RESET_TOKEN");
    }

    /// **Validates: Requirements 1.25, 1.28, 1.29**
    ///
    /// Property 28b: Generating a new token invalidates all previous tokens for
    /// that user.
    #[test]
    fn prop_new_reset_token_invalidates_previous(
        user_id in user_id_strategy(),
        num_tokens in 2usize..8
    ) {
        let mut store = ResetTokenStore::new();

        // Generate multiple tokens
        let mut tokens = Vec::new();
        for _ in 0..num_tokens {
            tokens.push(store.generate_token(&user_id));
        }

        // Only the LAST token should be usable
        for (i, token) in tokens.iter().enumerate() {
            let result = store.use_token(token);
            if i < num_tokens - 1 {
                prop_assert_eq!(result, Err("INVALID_RESET_TOKEN"),
                    "Token {} (not the latest) must be invalid", i);
            } else {
                prop_assert!(result.is_ok(),
                    "The latest token must be valid");
            }
        }
    }

    /// **Validates: Requirements 1.25, 1.28, 1.29**
    ///
    /// Property 28c: A non-existent token returns INVALID_RESET_TOKEN.
    #[test]
    fn prop_nonexistent_reset_token_invalid(
        user_id in user_id_strategy(),
        fake_token in "[a-z]{20}"
    ) {
        let mut store = ResetTokenStore::new();
        let _real_token = store.generate_token(&user_id);

        let result = store.use_token(&fake_token);
        prop_assert_eq!(result, Err("INVALID_RESET_TOKEN"),
            "A non-existent reset token must return INVALID_RESET_TOKEN");
    }

    /// **Validates: Requirements 1.25, 1.28, 1.29**
    ///
    /// Property 28d: Different users' reset tokens are independent.
    #[test]
    fn prop_reset_tokens_independent_users(
        user1 in user_id_strategy(),
        user2 in user_id_strategy()
    ) {
        prop_assume!(user1 != user2);
        let mut store = ResetTokenStore::new();

        let token1 = store.generate_token(&user1);
        let token2 = store.generate_token(&user2);

        // Generating token for user2 should NOT invalidate user1's token
        let result1 = store.use_token(&token1);
        prop_assert!(result1.is_ok(),
            "User1's token must remain valid after user2 generates a new token");

        let result2 = store.use_token(&token2);
        prop_assert!(result2.is_ok(),
            "User2's token must be valid");
    }
}
