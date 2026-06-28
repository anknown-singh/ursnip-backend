//! Property-based tests for AI Service (Property 27).
//!
//! These tests validate AI tier-based quota enforcement including free/paid limits,
//! grace period handling, cancelled/expired downgrades, rolling window logic, and
//! per-user quota independence.
//! They use simulated in-memory stores (no real database needed).
//!
//! Run with: `cargo test --test ai_property_tests`

use proptest::prelude::*;
use std::collections::{HashMap, VecDeque};

// ─── Constants ──────────────────────────────────────────────────────────────────

/// Free tier: 50 AI expansion requests per 24-hour rolling window.
const FREE_TIER_QUOTA: usize = 50;

/// Paid tier (pro/teams): 1000 AI expansion requests per 24-hour rolling window.
const PAID_TIER_QUOTA: usize = 1000;

/// Rolling window duration in seconds (24 hours).
const WINDOW_SECONDS: i64 = 24 * 60 * 60;

// ─── Simulated Types ────────────────────────────────────────────────────────────

/// Simulated subscription tiers.
#[derive(Debug, Clone, PartialEq)]
enum SimTier {
    Free,
    Pro,
    Teams,
}

/// Simulated subscription statuses.
#[derive(Debug, Clone, PartialEq)]
enum SimStatus {
    Active,
    PastDue,
    Cancelled,
    Expired,
}

/// Simulated error types for AI quota operations.
#[derive(Debug, Clone, PartialEq)]
enum AiError {
    AiQuotaExceeded,
}

// ─── Simulated Quota Tracker ────────────────────────────────────────────────────

/// Simulated in-memory AI quota tracker per user.
/// Tracks request timestamps as seconds-since-epoch.
#[derive(Debug, Clone)]
struct AiQuotaTracker {
    /// user_id → sorted timestamps (seconds since epoch) of recent requests.
    usage: HashMap<String, VecDeque<i64>>,
}

impl AiQuotaTracker {
    fn new() -> Self {
        Self {
            usage: HashMap::new(),
        }
    }

    /// Count requests within the 24h rolling window ending at `now_seconds`.
    fn count_usage_in_window(&self, user_id: &str, now_seconds: i64) -> usize {
        let window_start = now_seconds - WINDOW_SECONDS;
        match self.usage.get(user_id) {
            Some(timestamps) => timestamps.iter().filter(|ts| **ts >= window_start).count(),
            None => 0,
        }
    }

    /// Attempt an AI expansion request. Returns Ok(()) if allowed, Err if quota exceeded.
    fn attempt_request(
        &mut self,
        user_id: &str,
        now_seconds: i64,
        quota_limit: usize,
    ) -> Result<(), AiError> {
        let current_usage = self.count_usage_in_window(user_id, now_seconds);

        if current_usage >= quota_limit {
            return Err(AiError::AiQuotaExceeded);
        }

        // Record the request
        let entry = self.usage.entry(user_id.to_string()).or_insert_with(VecDeque::new);

        // Prune expired entries from the front
        let window_start = now_seconds - WINDOW_SECONDS;
        while let Some(front) = entry.front() {
            if *front < window_start {
                entry.pop_front();
            } else {
                break;
            }
        }

        entry.push_back(now_seconds);
        Ok(())
    }
}

// ─── Quota Limit Determination ──────────────────────────────────────────────────

/// Determine the quota limit for a user based on tier and status.
///
/// Rules (from Requirements 3.4–3.7):
/// - free tier → 50 requests / 24h
/// - pro/teams (active) → 1000 requests / 24h
/// - pro/teams (past_due / within grace period) → 1000 requests / 24h
/// - cancelled/expired → 50 requests / 24h (regardless of tier field)
fn get_quota_limit(tier: &SimTier, status: &SimStatus) -> usize {
    match (tier, status) {
        // Active paid tier → paid limit
        (SimTier::Pro | SimTier::Teams, SimStatus::Active) => PAID_TIER_QUOTA,
        // Past due (within grace period) → still gets paid limit
        (SimTier::Pro | SimTier::Teams, SimStatus::PastDue) => PAID_TIER_QUOTA,
        // Cancelled or expired → free limit regardless of tier
        (_, SimStatus::Cancelled | SimStatus::Expired) => FREE_TIER_QUOTA,
        // Free tier (any status) → free limit
        _ => FREE_TIER_QUOTA,
    }
}

// ─── Strategies ─────────────────────────────────────────────────────────────────

/// Strategy for generating user IDs.
fn user_id_strategy() -> impl Strategy<Value = String> {
    "[a-f0-9]{8}".prop_map(|s| format!("user_{}", s))
}

/// Strategy for generating a SimTier.
fn tier_strategy() -> impl Strategy<Value = SimTier> {
    prop_oneof![
        Just(SimTier::Free),
        Just(SimTier::Pro),
        Just(SimTier::Teams),
    ]
}

/// Strategy for generating paid tiers only.
fn paid_tier_strategy() -> impl Strategy<Value = SimTier> {
    prop_oneof![Just(SimTier::Pro), Just(SimTier::Teams)]
}

/// Strategy for generating cancelled/expired statuses.
fn cancelled_status_strategy() -> impl Strategy<Value = SimStatus> {
    prop_oneof![Just(SimStatus::Cancelled), Just(SimStatus::Expired)]
}

/// Strategy for a base timestamp (seconds since epoch, arbitrary large value).
fn base_timestamp_strategy() -> impl Strategy<Value = i64> {
    // Simulate timestamps around a realistic epoch range
    1_700_000_000i64..1_800_000_000i64
}

/// Strategy for generating a time offset within the 24h window (0 to 86399 seconds).
fn in_window_offset_strategy() -> impl Strategy<Value = i64> {
    0i64..(WINDOW_SECONDS - 1)
}

/// Strategy for generating a time offset beyond the 24h window.
/// Must be far enough past the window that even with FREE_TIER_QUOTA sequential
/// requests (offset 0..49), all timestamps fall outside the window boundary.
fn beyond_window_offset_strategy() -> impl Strategy<Value = i64> {
    (WINDOW_SECONDS + FREE_TIER_QUOTA as i64)..(WINDOW_SECONDS * 2)
}

// ─── Property Test Implementations ──────────────────────────────────────────────

proptest! {
    // ─── Property 27a: Free tier users get exactly 50 requests ──────────────────
    //
    // **Validates: Requirements 3.4**
    //
    // Free tier users get exactly 50 requests; the 51st is rejected with AiQuotaExceeded.

    #[test]
    fn prop_free_tier_enforces_50_request_limit(
        user_id in user_id_strategy(),
        now in base_timestamp_strategy(),
    ) {
        let mut tracker = AiQuotaTracker::new();
        let limit = get_quota_limit(&SimTier::Free, &SimStatus::Active);
        prop_assert_eq!(limit, FREE_TIER_QUOTA);

        // Make exactly 50 requests (should all succeed)
        for i in 0..FREE_TIER_QUOTA {
            let result = tracker.attempt_request(&user_id, now + i as i64, limit);
            prop_assert!(result.is_ok(),
                "Request {} within free tier limit must succeed", i + 1);
        }

        // The 51st must fail
        let result = tracker.attempt_request(&user_id, now + FREE_TIER_QUOTA as i64, limit);
        prop_assert_eq!(result, Err(AiError::AiQuotaExceeded),
            "Request 51 must fail with AiQuotaExceeded for free tier");
    }

    // ─── Property 27b: Pro/Teams (active) users get up to 1000 requests ────────
    //
    // **Validates: Requirements 3.5**
    //
    // Pro/Teams (active) users get up to 1000 requests; the 1001st is rejected.

    #[test]
    fn prop_paid_tier_enforces_1000_request_limit(
        user_id in user_id_strategy(),
        tier in paid_tier_strategy(),
        now in base_timestamp_strategy(),
    ) {
        let mut tracker = AiQuotaTracker::new();
        let limit = get_quota_limit(&tier, &SimStatus::Active);
        prop_assert_eq!(limit, PAID_TIER_QUOTA);

        // Make exactly 1000 requests (should all succeed)
        for i in 0..PAID_TIER_QUOTA {
            let result = tracker.attempt_request(&user_id, now + i as i64, limit);
            prop_assert!(result.is_ok(),
                "Request {} within paid tier limit must succeed", i + 1);
        }

        // The 1001st must fail
        let result = tracker.attempt_request(&user_id, now + PAID_TIER_QUOTA as i64, limit);
        prop_assert_eq!(result, Err(AiError::AiQuotaExceeded),
            "Request 1001 must fail with AiQuotaExceeded for paid tier");
    }

    // ─── Property 27c: Past_due users with paid tier still get paid limits ──────
    //
    // **Validates: Requirements 3.6**
    //
    // Past_due users (with paid tier) still get paid limits (1000).

    #[test]
    fn prop_past_due_users_get_paid_limits(
        user_id in user_id_strategy(),
        tier in paid_tier_strategy(),
        now in base_timestamp_strategy(),
    ) {
        let mut tracker = AiQuotaTracker::new();
        let limit = get_quota_limit(&tier, &SimStatus::PastDue);
        prop_assert_eq!(limit, PAID_TIER_QUOTA,
            "Past_due users with paid tier must get 1000 request limit");

        // Verify they can make more than 50 requests (proving paid limit applies)
        for i in 0..=FREE_TIER_QUOTA {
            let result = tracker.attempt_request(&user_id, now + i as i64, limit);
            prop_assert!(result.is_ok(),
                "Past_due paid-tier user request {} must succeed (beyond free limit)", i + 1);
        }
    }

    // ─── Property 27d: Cancelled/expired users get free limits ──────────────────
    //
    // **Validates: Requirements 3.7**
    //
    // Cancelled/expired users get free limits (50) regardless of their tier field.

    #[test]
    fn prop_cancelled_expired_users_get_free_limits(
        user_id in user_id_strategy(),
        tier in tier_strategy(),
        status in cancelled_status_strategy(),
        now in base_timestamp_strategy(),
    ) {
        let mut tracker = AiQuotaTracker::new();
        let limit = get_quota_limit(&tier, &status);
        prop_assert_eq!(limit, FREE_TIER_QUOTA,
            "Cancelled/expired users must get free tier limit (50) regardless of tier {:?}", tier);

        // Make exactly 50 requests (should all succeed)
        for i in 0..FREE_TIER_QUOTA {
            let result = tracker.attempt_request(&user_id, now + i as i64, limit);
            prop_assert!(result.is_ok(),
                "Request {} within free limit for cancelled user must succeed", i + 1);
        }

        // The 51st must fail
        let result = tracker.attempt_request(&user_id, now + FREE_TIER_QUOTA as i64, limit);
        prop_assert_eq!(result, Err(AiError::AiQuotaExceeded),
            "Cancelled/expired user request 51 must fail with AiQuotaExceeded");
    }

    // ─── Property 27e: Rolling window — old requests don't count ────────────────
    //
    // **Validates: Requirements 3.4, 3.5**
    //
    // Requests older than 24h do NOT count against the limit.

    #[test]
    fn prop_rolling_window_expires_old_requests(
        user_id in user_id_strategy(),
        now in base_timestamp_strategy(),
        old_offset in beyond_window_offset_strategy(),
    ) {
        let mut tracker = AiQuotaTracker::new();
        let limit = get_quota_limit(&SimTier::Free, &SimStatus::Active);

        // Make requests in the past (beyond the 24h window)
        let old_time = now - old_offset;
        for i in 0..FREE_TIER_QUOTA {
            let result = tracker.attempt_request(&user_id, old_time + i as i64, limit);
            prop_assert!(result.is_ok(),
                "Old request {} must succeed", i + 1);
        }

        // Now at current time, old requests are expired — user should have full quota again
        let current_usage = tracker.count_usage_in_window(&user_id, now);
        prop_assert_eq!(current_usage, 0,
            "Requests older than 24h must not count against quota");

        // Should be able to make fresh requests at current time
        let result = tracker.attempt_request(&user_id, now, limit);
        prop_assert!(result.is_ok(),
            "After old requests expire, new requests must succeed");
    }

    // ─── Property 27f: Different users have independent quotas ──────────────────
    //
    // **Validates: Requirements 3.4, 3.5**
    //
    // User A's usage doesn't affect user B.

    #[test]
    fn prop_independent_user_quotas(
        user_a in user_id_strategy(),
        user_b in user_id_strategy(),
        now in base_timestamp_strategy(),
    ) {
        // Ensure user IDs are different
        prop_assume!(user_a != user_b);

        let mut tracker = AiQuotaTracker::new();
        let limit = get_quota_limit(&SimTier::Free, &SimStatus::Active);

        // Exhaust user A's quota
        for i in 0..FREE_TIER_QUOTA {
            let result = tracker.attempt_request(&user_a, now + i as i64, limit);
            prop_assert!(result.is_ok(),
                "User A request {} must succeed", i + 1);
        }

        // User A is now at limit
        let result_a = tracker.attempt_request(&user_a, now + FREE_TIER_QUOTA as i64, limit);
        prop_assert_eq!(result_a, Err(AiError::AiQuotaExceeded),
            "User A must be quota-exceeded");

        // User B should still have full quota (unaffected by A)
        for i in 0..FREE_TIER_QUOTA {
            let result = tracker.attempt_request(&user_b, now + i as i64, limit);
            prop_assert!(result.is_ok(),
                "User B request {} must succeed despite User A being at limit", i + 1);
        }
    }
}
