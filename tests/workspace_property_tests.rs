//! Property-based tests for Workspace/Teams Service (Property 18).
//!
//! These tests validate the team workspace seat limit invariant using a
//! simulated in-memory store (no real database needed).
//!
//! Run with: `cargo test --test workspace_property_tests`

use proptest::prelude::*;

// ─── Property 18: Team workspace seat limit ─────────────────────────────────────
//
// **Validates: Requirements 5.17**
//
// For any team workspace, the total number of members (including the owner)
// SHALL never exceed 3. Any join attempt when the workspace is at capacity
// SHALL return 422 SEAT_LIMIT_REACHED.

/// Maximum number of members allowed in a team workspace (including owner).
const MAX_TEAM_SEATS: usize = 3;

/// Simulated error type for workspace operations.
#[derive(Debug, Clone, PartialEq)]
enum WorkspaceError {
    SeatLimitReached,
    AlreadyAMember,
    CannotRemoveOwner,
    MemberNotFound,
}

/// Simulated team workspace store for testing seat limit logic without a real database.
/// Models the core invariants of the team workspace membership system.
#[derive(Debug, Clone)]
struct TeamWorkspaceStore {
    /// Each workspace: (workspace_id, owner_id, members: Vec<user_id>)
    /// The owner is always included in the members list.
    workspaces: Vec<(String, String, Vec<String>)>,
}

impl TeamWorkspaceStore {
    fn new() -> Self {
        Self {
            workspaces: Vec::new(),
        }
    }

    /// Create a new team workspace. The owner automatically occupies 1 seat.
    fn create_workspace(&mut self, workspace_id: &str, owner_id: &str) {
        self.workspaces.push((
            workspace_id.to_string(),
            owner_id.to_string(),
            vec![owner_id.to_string()],
        ));
    }

    /// Attempt to join a workspace. Enforces:
    /// - Seat limit (max 3 members including owner)
    /// - No duplicate membership
    fn join_workspace(
        &mut self,
        workspace_id: &str,
        user_id: &str,
    ) -> Result<(), WorkspaceError> {
        let workspace = self
            .workspaces
            .iter_mut()
            .find(|(wid, _, _)| wid == workspace_id);

        match workspace {
            None => panic!("Workspace not found in test — this is a test setup error"),
            Some((_, _, members)) => {
                // Check if already a member
                if members.iter().any(|m| m == user_id) {
                    return Err(WorkspaceError::AlreadyAMember);
                }

                // Check seat limit
                if members.len() >= MAX_TEAM_SEATS {
                    return Err(WorkspaceError::SeatLimitReached);
                }

                members.push(user_id.to_string());
                Ok(())
            }
        }
    }

    /// Remove a member from a workspace. Cannot remove the owner.
    fn remove_member(
        &mut self,
        workspace_id: &str,
        target_user_id: &str,
    ) -> Result<(), WorkspaceError> {
        let workspace = self
            .workspaces
            .iter_mut()
            .find(|(wid, _, _)| wid == workspace_id);

        match workspace {
            None => panic!("Workspace not found in test — this is a test setup error"),
            Some((_, owner_id, members)) => {
                // Cannot remove owner
                if target_user_id == owner_id.as_str() {
                    return Err(WorkspaceError::CannotRemoveOwner);
                }

                let idx = members.iter().position(|m| m == target_user_id);
                match idx {
                    None => Err(WorkspaceError::MemberNotFound),
                    Some(i) => {
                        members.remove(i);
                        Ok(())
                    }
                }
            }
        }
    }

    /// Get the member count for a workspace.
    fn member_count(&self, workspace_id: &str) -> usize {
        self.workspaces
            .iter()
            .find(|(wid, _, _)| wid == workspace_id)
            .map(|(_, _, members)| members.len())
            .unwrap_or(0)
    }
}

// ─── Strategies ─────────────────────────────────────────────────────────────────

/// Strategy for generating workspace IDs.
fn workspace_id_strategy() -> impl Strategy<Value = String> {
    "[a-f0-9]{8}".prop_map(|s| format!("ws_{}", s))
}

/// Strategy for generating user IDs.
fn user_id_strategy() -> impl Strategy<Value = String> {
    "[a-f0-9]{8}".prop_map(|s| format!("user_{}", s))
}

/// Strategy for generating a set of distinct user IDs (for join attempts).
fn distinct_user_ids(count: usize) -> impl Strategy<Value = Vec<String>> {
    proptest::collection::hash_set("[a-f0-9]{8}".prop_map(|s| format!("user_{}", s)), count)
        .prop_map(|set| set.into_iter().collect())
}

// ─── Property Test Implementations ──────────────────────────────────────────────

proptest! {
    // ─── Property 18a: Team workspace can never exceed 3 total members ──────────

    /// **Validates: Requirements 5.17**
    ///
    /// Property 18a: A team workspace can never exceed 3 total members
    /// (including owner), regardless of how many join attempts are made.
    #[test]
    fn prop_workspace_never_exceeds_max_seats(
        workspace_id in workspace_id_strategy(),
        owner_id in user_id_strategy(),
        joiners in distinct_user_ids(10),
    ) {
        let mut store = TeamWorkspaceStore::new();
        store.create_workspace(&workspace_id, &owner_id);

        // Try to join many users — seat limit must hold
        for joiner in &joiners {
            if joiner == &owner_id {
                continue; // skip if same as owner
            }
            let _ = store.join_workspace(&workspace_id, joiner);
        }

        let count = store.member_count(&workspace_id);
        prop_assert!(count <= MAX_TEAM_SEATS,
            "Workspace member count ({}) must never exceed MAX_TEAM_SEATS ({})",
            count, MAX_TEAM_SEATS);
    }

    // ─── Property 18b: At capacity, join returns SeatLimitReached ────────────────

    /// **Validates: Requirements 5.17**
    ///
    /// Property 18b: When at 3 members, any join attempt returns SeatLimitReached.
    #[test]
    fn prop_join_at_capacity_returns_seat_limit_reached(
        workspace_id in workspace_id_strategy(),
        users in distinct_user_ids(5),
    ) {
        prop_assume!(users.len() >= 5);

        let mut store = TeamWorkspaceStore::new();
        let owner_id = &users[0];
        store.create_workspace(&workspace_id, owner_id);

        // Fill to capacity (owner + 2 members = 3)
        let member1 = &users[1];
        let member2 = &users[2];
        let result1 = store.join_workspace(&workspace_id, member1);
        let result2 = store.join_workspace(&workspace_id, member2);
        prop_assert!(result1.is_ok(), "First member join must succeed");
        prop_assert!(result2.is_ok(), "Second member join must succeed");

        // Now at capacity — subsequent joins must fail
        let extra_user = &users[3];
        let result = store.join_workspace(&workspace_id, extra_user);
        prop_assert_eq!(result, Err(WorkspaceError::SeatLimitReached),
            "Join attempt when at capacity must return SeatLimitReached");
    }

    // ─── Property 18c: After removal, a new member can join ──────────────────────

    /// **Validates: Requirements 5.17**
    ///
    /// Property 18c: After a member is removed from a full workspace, a new
    /// member can join (back to 3).
    #[test]
    fn prop_join_succeeds_after_member_removal(
        workspace_id in workspace_id_strategy(),
        users in distinct_user_ids(5),
    ) {
        prop_assume!(users.len() >= 5);

        let mut store = TeamWorkspaceStore::new();
        let owner_id = &users[0];
        store.create_workspace(&workspace_id, owner_id);

        // Fill to capacity
        let member1 = &users[1];
        let member2 = &users[2];
        store.join_workspace(&workspace_id, member1).unwrap();
        store.join_workspace(&workspace_id, member2).unwrap();

        // Verify at capacity
        prop_assert_eq!(store.member_count(&workspace_id), 3);

        // Remove one member
        store.remove_member(&workspace_id, member1).unwrap();
        prop_assert_eq!(store.member_count(&workspace_id), 2);

        // Now a new member should be able to join
        let new_member = &users[3];
        let result = store.join_workspace(&workspace_id, new_member);
        prop_assert!(result.is_ok(),
            "After removing a member, a new member should be able to join");
        prop_assert_eq!(store.member_count(&workspace_id), 3);
    }

    // ─── Property 18d: Multiple join attempts at capacity all fail ───────────────

    /// **Validates: Requirements 5.17**
    ///
    /// Property 18d: Multiple join attempts when at capacity all fail with
    /// SeatLimitReached.
    #[test]
    fn prop_multiple_joins_at_capacity_all_fail(
        workspace_id in workspace_id_strategy(),
        users in distinct_user_ids(8),
    ) {
        prop_assume!(users.len() >= 8);

        let mut store = TeamWorkspaceStore::new();
        let owner_id = &users[0];
        store.create_workspace(&workspace_id, owner_id);

        // Fill to capacity
        store.join_workspace(&workspace_id, &users[1]).unwrap();
        store.join_workspace(&workspace_id, &users[2]).unwrap();
        prop_assert_eq!(store.member_count(&workspace_id), 3);

        // All subsequent join attempts must fail
        for i in 3..users.len() {
            if users[i] == *owner_id {
                continue;
            }
            let result = store.join_workspace(&workspace_id, &users[i]);
            prop_assert_eq!(result, Err(WorkspaceError::SeatLimitReached),
                "Join attempt #{} at capacity must return SeatLimitReached", i);
        }

        // Count must still be exactly 3
        prop_assert_eq!(store.member_count(&workspace_id), 3,
            "Member count must remain at 3 after all rejected join attempts");
    }

    // ─── Property 18e: Different workspaces have independent seat limits ─────────

    /// **Validates: Requirements 5.17**
    ///
    /// Property 18e: Different workspaces have independent seat limits.
    /// Filling one workspace does not affect another.
    #[test]
    fn prop_workspaces_have_independent_seat_limits(
        ws1_id in workspace_id_strategy(),
        ws2_id in workspace_id_strategy(),
        users in distinct_user_ids(7),
    ) {
        prop_assume!(users.len() >= 7);
        prop_assume!(ws1_id != ws2_id);

        let mut store = TeamWorkspaceStore::new();

        // Create two workspaces with different owners
        let owner1 = &users[0];
        let owner2 = &users[1];
        store.create_workspace(&ws1_id, owner1);
        store.create_workspace(&ws2_id, owner2);

        // Fill workspace 1 to capacity
        store.join_workspace(&ws1_id, &users[2]).unwrap();
        store.join_workspace(&ws1_id, &users[3]).unwrap();
        prop_assert_eq!(store.member_count(&ws1_id), 3);

        // Workspace 2 should still accept members independently
        let result = store.join_workspace(&ws2_id, &users[4]);
        prop_assert!(result.is_ok(),
            "Workspace 2 must accept members independently of workspace 1");

        let result2 = store.join_workspace(&ws2_id, &users[5]);
        prop_assert!(result2.is_ok(),
            "Workspace 2 must accept a second member independently");

        prop_assert_eq!(store.member_count(&ws2_id), 3);

        // ws1 still rejects
        let ws1_reject = store.join_workspace(&ws1_id, &users[6]);
        prop_assert_eq!(ws1_reject, Err(WorkspaceError::SeatLimitReached),
            "Workspace 1 must still reject joins when at capacity");
    }

    // ─── Property 18f: The owner always occupies one seat ────────────────────────

    /// **Validates: Requirements 5.17**
    ///
    /// Property 18f: The owner always occupies one seat, so only 2 additional
    /// members can ever join.
    #[test]
    fn prop_owner_always_occupies_one_seat(
        workspace_id in workspace_id_strategy(),
        owner_id in user_id_strategy(),
        joiners in distinct_user_ids(5),
    ) {
        let mut store = TeamWorkspaceStore::new();
        store.create_workspace(&workspace_id, &owner_id);

        // Owner already occupies 1 seat
        prop_assert_eq!(store.member_count(&workspace_id), 1,
            "After creation, owner must occupy exactly 1 seat");

        // Count successful joins
        let mut successful_joins = 0;
        for joiner in &joiners {
            if joiner == &owner_id {
                continue;
            }
            if store.join_workspace(&workspace_id, joiner).is_ok() {
                successful_joins += 1;
            }
        }

        // At most 2 additional members can join (since owner takes 1 of 3 seats)
        prop_assert!(successful_joins <= 2,
            "At most 2 members can join (owner occupies 1 of {} seats), but {} joined",
            MAX_TEAM_SEATS, successful_joins);

        // Total never exceeds MAX_TEAM_SEATS
        prop_assert!(store.member_count(&workspace_id) <= MAX_TEAM_SEATS,
            "Total members must not exceed {}", MAX_TEAM_SEATS);
    }
}
