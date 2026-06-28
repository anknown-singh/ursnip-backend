//! Property-based tests for Subscription Service (Properties 13, 14, 15, 16, 17, 19, 20).
//!
//! These tests validate per-tier feature limits, soft-lock on downgrade, grace period
//! transitions, referral reward idempotency, billing webhook idempotency, invoice
//! calculation correctness, and coupon validation completeness.
//! They use simulated in-memory stores (no real database needed).
//!
//! Run with: `cargo test --test subscription_property_tests`

use proptest::prelude::*;
use rust_decimal::Decimal;
use rust_decimal::prelude::Zero;
use rust_decimal::RoundingStrategy;
use std::str::FromStr;

// ─── Constants ──────────────────────────────────────────────────────────────────

/// Maximum snippets on free tier (Requirement 5.21).
const FREE_MAX_SNIPPETS: usize = 10;

/// Maximum folders on free tier (Requirement 5.21).
const FREE_MAX_FOLDERS: usize = 3;

/// Maximum content characters on free tier (Requirement 5.21).
const FREE_MAX_CONTENT_CHARS: usize = 2000;

/// Grace period duration in days (Requirement 5.30).
const GRACE_PERIOD_DAYS: i64 = 7;

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

/// Simulated error types for subscription operations.
#[derive(Debug, Clone, PartialEq)]
enum SubError {
    SnippetLimitReached,
    FolderLimitReached,
    SnippetContentTooLong,
    ContentSoftLocked,
    AlreadyProcessed,
    // Coupon errors
    CouponNotFound,
    CouponInactive,
    CouponNotYetValid,
    CouponExpired,
    CouponUsageLimitReached,
}

// ─── Property 13: Per-tier feature limits enforcement ───────────────────────────
//
// **Validates: Requirements 5.21, 5.23**
//
// For any write operation on a workspace with `free` tier subscription, the backend
// SHALL enforce: max 10 snippets, max 3 folders, max 2000 chars per snippet content.
// For any workspace with `pro` or `teams` tier, these limits SHALL NOT be enforced.

/// Simulated tier limit store tracking snippets/folders per workspace.
#[derive(Debug, Clone)]
struct TierLimitStore {
    /// (workspace_id, tier, snippet_count, folder_count)
    workspaces: Vec<(String, SimTier, usize, usize)>,
}

impl TierLimitStore {
    fn new() -> Self {
        Self {
            workspaces: Vec::new(),
        }
    }

    fn add_workspace(&mut self, workspace_id: &str, tier: SimTier) {
        self.workspaces
            .push((workspace_id.to_string(), tier, 0, 0));
    }

    /// Attempt to create a snippet. Enforces free tier limits.
    fn create_snippet(
        &mut self,
        workspace_id: &str,
        content_length: usize,
    ) -> Result<(), SubError> {
        let ws = self
            .workspaces
            .iter_mut()
            .find(|(wid, _, _, _)| wid == workspace_id)
            .unwrap();

        // Pro/Teams have no limits
        if ws.1 != SimTier::Free {
            ws.2 += 1;
            return Ok(());
        }

        // Check content length
        if content_length > FREE_MAX_CONTENT_CHARS {
            return Err(SubError::SnippetContentTooLong);
        }

        // Check snippet count
        if ws.2 >= FREE_MAX_SNIPPETS {
            return Err(SubError::SnippetLimitReached);
        }

        ws.2 += 1;
        Ok(())
    }

    /// Attempt to create a folder. Enforces free tier limits.
    fn create_folder(&mut self, workspace_id: &str) -> Result<(), SubError> {
        let ws = self
            .workspaces
            .iter_mut()
            .find(|(wid, _, _, _)| wid == workspace_id)
            .unwrap();

        // Pro/Teams have no limits
        if ws.1 != SimTier::Free {
            ws.3 += 1;
            return Ok(());
        }

        // Check folder count
        if ws.3 >= FREE_MAX_FOLDERS {
            return Err(SubError::FolderLimitReached);
        }

        ws.3 += 1;
        Ok(())
    }
}

// ─── Property 14: Soft-lock on downgrade ────────────────────────────────────────
//
// **Validates: Requirements 5.24, 5.25**
//
// For any user whose subscription expires or is cancelled and whose content exceeds
// free tier limits, write operations SHALL return 422 CONTENT_SOFT_LOCKED.
// If content <= free limits, writes are still allowed after downgrade.

/// Simulated soft-lock store for testing downgrade behavior.
#[derive(Debug, Clone)]
struct SoftLockStore {
    /// (workspace_id, status, snippet_count, folder_count)
    workspaces: Vec<(String, SimStatus, usize, usize)>,
}

impl SoftLockStore {
    fn new() -> Self {
        Self {
            workspaces: Vec::new(),
        }
    }

    fn add_workspace(
        &mut self,
        workspace_id: &str,
        status: SimStatus,
        snippets: usize,
        folders: usize,
    ) {
        self.workspaces
            .push((workspace_id.to_string(), status, snippets, folders));
    }

    /// Attempt a write operation. Returns ContentSoftLocked if the workspace
    /// is cancelled/expired AND content exceeds free limits.
    fn attempt_write(&self, workspace_id: &str) -> Result<(), SubError> {
        let ws = self
            .workspaces
            .iter()
            .find(|(wid, _, _, _)| wid == workspace_id)
            .unwrap();

        let is_cancelled_or_expired =
            ws.1 == SimStatus::Cancelled || ws.1 == SimStatus::Expired;

        if !is_cancelled_or_expired {
            return Ok(());
        }

        // Check if content exceeds free tier limits
        if ws.2 > FREE_MAX_SNIPPETS || ws.3 > FREE_MAX_FOLDERS {
            return Err(SubError::ContentSoftLocked);
        }

        Ok(())
    }
}

// ─── Property 15: Grace period transitions ──────────────────────────────────────
//
// **Validates: Requirements 5.30, 5.31**
//
// For any subscription in `past_due` status, the backend SHALL continue granting
// paid-tier access for exactly 7 calendar days. After the grace period expires
// without renewal, the status SHALL transition to `cancelled`.

/// Simulated grace period store.
#[derive(Debug, Clone)]
struct GracePeriodStore {
    /// (workspace_id, status, grace_period_end_day, current_day)
    subscriptions: Vec<(String, SimStatus, i64, i64)>,
}

impl GracePeriodStore {
    fn new() -> Self {
        Self {
            subscriptions: Vec::new(),
        }
    }

    /// Add a past_due subscription with a grace period end.
    fn add_past_due(
        &mut self,
        workspace_id: &str,
        grace_period_end_day: i64,
    ) {
        self.subscriptions.push((
            workspace_id.to_string(),
            SimStatus::PastDue,
            grace_period_end_day,
            0, // current_day set later
        ));
    }

    /// Check grace periods and transition expired ones to cancelled.
    /// Returns the number of transitions performed.
    fn check_grace_periods(&mut self, current_day: i64) -> usize {
        let mut transitioned = 0;
        for sub in self.subscriptions.iter_mut() {
            if sub.1 == SimStatus::PastDue && current_day > sub.2 {
                sub.1 = SimStatus::Cancelled;
                transitioned += 1;
            }
        }
        transitioned
    }

    /// Get current status of a subscription.
    fn get_status(&self, workspace_id: &str) -> &SimStatus {
        &self
            .subscriptions
            .iter()
            .find(|(wid, _, _, _)| wid == workspace_id)
            .unwrap()
            .1
    }
}

// ─── Property 16: Referral reward idempotency ───────────────────────────────────
//
// **Validates: Requirements 5.48**
//
// For any (referrer_id, referred_user_id) pair, the referral reward (1 month
// extension) SHALL be applied at most once. Duplicate conversion attempts SHALL
// be idempotent — no error returned, no additional reward applied.

/// Simulated referral store for testing idempotency.
#[derive(Debug, Clone)]
struct ReferralStore {
    /// (referrer_id, referred_user_id, status, reward_months_applied)
    referrals: Vec<(String, String, String, u32)>,
}

impl ReferralStore {
    fn new() -> Self {
        Self {
            referrals: Vec::new(),
        }
    }

    /// Record a referral (pending state).
    fn record_referral(
        &mut self,
        referrer_id: &str,
        referred_user_id: &str,
    ) -> Result<(), SubError> {
        // Unique constraint on (referrer_id, referred_user_id)
        let exists = self.referrals.iter().any(|(r, u, _, _)| {
            r == referrer_id && u == referred_user_id
        });
        if exists {
            // ON CONFLICT DO NOTHING — idempotent, no error
            return Ok(());
        }
        self.referrals.push((
            referrer_id.to_string(),
            referred_user_id.to_string(),
            "pending".to_string(),
            0,
        ));
        Ok(())
    }

    /// Apply the referral reward. Idempotent: only applies once per pair.
    /// Returns the total reward months for the referrer after this call.
    fn apply_reward(
        &mut self,
        referrer_id: &str,
        referred_user_id: &str,
    ) -> u32 {
        let referral = self.referrals.iter_mut().find(|(r, u, _, _)| {
            r == referrer_id && u == referred_user_id
        });

        match referral {
            Some(r) => {
                if r.2 == "pending" {
                    r.2 = "converted".to_string();
                    r.3 = 1;
                }
                // If already converted, no additional reward (idempotent)
                r.3
            }
            None => 0,
        }
    }

    /// Get total reward months applied for a referrer.
    fn total_reward_months(&self, referrer_id: &str) -> u32 {
        self.referrals
            .iter()
            .filter(|(r, _, _, _)| r == referrer_id)
            .map(|(_, _, _, months)| months)
            .sum()
    }
}

// ─── Property 17: Billing webhook idempotency ───────────────────────────────────
//
// **Validates: Requirements 5.57**
//
// For any billing webhook event with a given `external_event_id`, processing it
// multiple times SHALL produce the same database state as processing it once.
// Duplicate events SHALL return "already_processed" without re-processing.

/// Simulated billing event store for testing webhook idempotency.
#[derive(Debug, Clone)]
struct WebhookStore {
    /// Set of processed event IDs.
    processed_events: Vec<String>,
    /// Subscription status per workspace (workspace_id -> status).
    workspace_statuses: Vec<(String, SimStatus)>,
}

impl WebhookStore {
    fn new() -> Self {
        Self {
            processed_events: Vec::new(),
            workspace_statuses: Vec::new(),
        }
    }

    fn add_workspace(&mut self, workspace_id: &str, status: SimStatus) {
        self.workspace_statuses
            .push((workspace_id.to_string(), status));
    }

    /// Process a webhook event. Returns "processed" or "already_processed".
    fn process_event(
        &mut self,
        event_id: &str,
        workspace_id: &str,
        new_status: SimStatus,
    ) -> String {
        // Idempotency check
        if self.processed_events.contains(&event_id.to_string()) {
            return "already_processed".to_string();
        }

        // Record event
        self.processed_events.push(event_id.to_string());

        // Apply state transition
        if let Some(ws) = self
            .workspace_statuses
            .iter_mut()
            .find(|(wid, _)| wid == workspace_id)
        {
            ws.1 = new_status;
        }

        "processed".to_string()
    }

    /// Get workspace status.
    fn get_status(&self, workspace_id: &str) -> Option<&SimStatus> {
        self.workspace_statuses
            .iter()
            .find(|(wid, _)| wid == workspace_id)
            .map(|(_, s)| s)
    }
}

// ─── Property 19: Invoice calculation correctness ───────────────────────────────
//
// **Validates: Requirements 5.33, 5.51, 5.53**
//
// For any base price, discount (percentage or flat), and tax rate, the invoice total
// SHALL equal `(base_price - discount_amount) × (1 + tax_rate)` where
// `discount_amount` is clamped to `[0, base_price]` and the final total is rounded
// to 2 decimal places.

/// Round a Decimal to 2 decimal places using banker's rounding.
fn round2(value: Decimal) -> Decimal {
    value.round_dp_with_strategy(2, RoundingStrategy::MidpointNearestEven)
}

/// Clamp a discount so it stays within [0, base_price].
fn clamp_discount(discount: Decimal, base_price: Decimal) -> Decimal {
    if discount < Decimal::ZERO {
        Decimal::ZERO
    } else if discount > base_price {
        base_price
    } else {
        discount
    }
}

/// Compute a simulated invoice.
/// Returns (discount_amount, subtotal, tax_amount, total).
fn compute_invoice_sim(
    base_price: Decimal,
    discount_type: Option<&str>,
    discount_value: Option<Decimal>,
    tax_rate: Decimal,
) -> (Decimal, Decimal, Decimal, Decimal) {
    let discount_amount = match (discount_type, discount_value) {
        (Some("percentage"), Some(rate)) => {
            let raw = base_price * rate;
            clamp_discount(raw, base_price)
        }
        (Some("flat"), Some(flat)) => clamp_discount(flat, base_price),
        _ => Decimal::ZERO,
    };

    let subtotal = round2(base_price - discount_amount);
    let tax_amount = round2(subtotal * tax_rate);
    let total = subtotal + tax_amount;

    (round2(discount_amount), subtotal, tax_amount, total)
}

// ─── Property 20: Coupon validation completeness ────────────────────────────────
//
// **Validates: Requirements 5.37, 5.38**
//
// For any coupon code submitted at checkout, the backend SHALL validate all
// conditions (exists, active, valid_from ≤ now, valid_until is null or > now,
// times_used < max_uses) and reject with the specific error code corresponding
// to the first failing condition.

/// Simulated coupon for validation testing.
#[derive(Debug, Clone)]
struct SimCoupon {
    code: String,
    active: bool,
    valid_from_day: i64,   // relative day (0 = today)
    valid_until_day: Option<i64>, // None = no expiry
    max_uses: Option<u32>,
    times_used: u32,
}

/// Validate a coupon given the current day (0 = today).
fn validate_coupon(
    coupons: &[SimCoupon],
    code: &str,
    current_day: i64,
) -> Result<(), SubError> {
    // Step 1: Find coupon (case-insensitive)
    let coupon = coupons
        .iter()
        .find(|c| c.code.to_lowercase() == code.to_lowercase());

    let coupon = match coupon {
        Some(c) => c,
        None => return Err(SubError::CouponNotFound),
    };

    // Step 2: Check active
    if !coupon.active {
        return Err(SubError::CouponInactive);
    }

    // Step 3: Check valid_from <= current_day
    if coupon.valid_from_day > current_day {
        return Err(SubError::CouponNotYetValid);
    }

    // Step 4: Check valid_until is None or > current_day
    if let Some(until) = coupon.valid_until_day {
        if until <= current_day {
            return Err(SubError::CouponExpired);
        }
    }

    // Step 5: Check max_uses
    if let Some(max) = coupon.max_uses {
        if coupon.times_used >= max {
            return Err(SubError::CouponUsageLimitReached);
        }
    }

    Ok(())
}

// ─── Strategies ─────────────────────────────────────────────────────────────────

/// Strategy for generating workspace IDs.
fn workspace_id_strategy() -> impl Strategy<Value = String> {
    "[a-f0-9]{8}".prop_map(|s| format!("ws_{}", s))
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

/// Strategy for generating content lengths within free limit.
fn valid_content_length() -> impl Strategy<Value = usize> {
    0..=FREE_MAX_CONTENT_CHARS
}

/// Strategy for generating content lengths exceeding free limit.
fn over_limit_content_length() -> impl Strategy<Value = usize> {
    (FREE_MAX_CONTENT_CHARS + 1)..=(FREE_MAX_CONTENT_CHARS * 3)
}

/// Strategy for generating a positive Decimal base price (1.00 to 999.99).
fn base_price_strategy() -> impl Strategy<Value = Decimal> {
    (100u32..99999u32).prop_map(|cents| {
        Decimal::new(cents as i64, 2)
    })
}

/// Strategy for generating a percentage rate (0.01 to 2.00, allowing over 100%).
fn percentage_rate_strategy() -> impl Strategy<Value = Decimal> {
    (1u32..200u32).prop_map(|v| Decimal::new(v as i64, 2))
}

/// Strategy for generating a flat discount (0.01 to 1500.00).
fn flat_discount_strategy() -> impl Strategy<Value = Decimal> {
    (1u32..150000u32).prop_map(|cents| Decimal::new(cents as i64, 2))
}

/// Strategy for generating a tax rate (0.00 to 0.30).
fn tax_rate_strategy() -> impl Strategy<Value = Decimal> {
    (0u32..=30u32).prop_map(|v| Decimal::new(v as i64, 2))
}

/// Strategy for generating referrer/user IDs.
fn user_id_strategy() -> impl Strategy<Value = String> {
    "[a-f0-9]{8}".prop_map(|s| format!("user_{}", s))
}

/// Strategy for generating event IDs.
fn event_id_strategy() -> impl Strategy<Value = String> {
    "[a-z0-9]{12}".prop_map(|s| format!("evt_{}", s))
}

// ─── Property Test Implementations ──────────────────────────────────────────────

proptest! {
    // ─── Property 13: Per-tier feature limits enforcement ───────────────────────

    /// **Validates: Requirements 5.21, 5.23**
    ///
    /// Property 13a: Free tier enforces max 10 snippets — creating the 11th fails.
    #[test]
    fn prop_free_tier_enforces_snippet_limit(
        workspace_id in workspace_id_strategy(),
        content_len in valid_content_length(),
    ) {
        let mut store = TierLimitStore::new();
        store.add_workspace(&workspace_id, SimTier::Free);

        // Create exactly FREE_MAX_SNIPPETS snippets (should all succeed)
        for _ in 0..FREE_MAX_SNIPPETS {
            let result = store.create_snippet(&workspace_id, content_len);
            prop_assert!(result.is_ok(),
                "Creating snippets within limit must succeed");
        }

        // The next one must fail
        let result = store.create_snippet(&workspace_id, content_len);
        prop_assert_eq!(result, Err(SubError::SnippetLimitReached),
            "Creating snippet beyond limit must fail with SnippetLimitReached");
    }

    /// **Validates: Requirements 5.21, 5.23**
    ///
    /// Property 13b: Free tier enforces max 3 folders — creating the 4th fails.
    #[test]
    fn prop_free_tier_enforces_folder_limit(
        workspace_id in workspace_id_strategy(),
    ) {
        let mut store = TierLimitStore::new();
        store.add_workspace(&workspace_id, SimTier::Free);

        // Create exactly FREE_MAX_FOLDERS folders
        for _ in 0..FREE_MAX_FOLDERS {
            let result = store.create_folder(&workspace_id);
            prop_assert!(result.is_ok(),
                "Creating folders within limit must succeed");
        }

        // The next one must fail
        let result = store.create_folder(&workspace_id);
        prop_assert_eq!(result, Err(SubError::FolderLimitReached),
            "Creating folder beyond limit must fail with FolderLimitReached");
    }

    /// **Validates: Requirements 5.21, 5.23**
    ///
    /// Property 13c: Free tier enforces max 2000 chars content length.
    #[test]
    fn prop_free_tier_enforces_content_limit(
        workspace_id in workspace_id_strategy(),
        content_len in over_limit_content_length(),
    ) {
        let mut store = TierLimitStore::new();
        store.add_workspace(&workspace_id, SimTier::Free);

        let result = store.create_snippet(&workspace_id, content_len);
        prop_assert_eq!(result, Err(SubError::SnippetContentTooLong),
            "Content exceeding 2000 chars must fail with SnippetContentTooLong");
    }

    /// **Validates: Requirements 5.21, 5.23**
    ///
    /// Property 13d: Pro/Teams tier allows any number of snippets/folders without error.
    #[test]
    fn prop_paid_tier_no_limits(
        workspace_id in workspace_id_strategy(),
        tier in paid_tier_strategy(),
        snippet_count in 1usize..50,
        folder_count in 1usize..50,
        content_len in over_limit_content_length(),
    ) {
        let mut store = TierLimitStore::new();
        store.add_workspace(&workspace_id, tier);

        // Create many snippets with large content — all must succeed
        for _ in 0..snippet_count {
            let result = store.create_snippet(&workspace_id, content_len);
            prop_assert!(result.is_ok(),
                "Paid tier must allow any snippet count and content length");
        }

        // Create many folders — all must succeed
        for _ in 0..folder_count {
            let result = store.create_folder(&workspace_id);
            prop_assert!(result.is_ok(),
                "Paid tier must allow any folder count");
        }
    }

    // ─── Property 14: Soft-lock on downgrade ────────────────────────────────────

    /// **Validates: Requirements 5.24, 5.25**
    ///
    /// Property 14a: If content exceeds free limits after downgrade, writes are soft-locked.
    #[test]
    fn prop_softlock_when_content_exceeds_free_limits(
        workspace_id in workspace_id_strategy(),
        status in cancelled_status_strategy(),
        snippets in (FREE_MAX_SNIPPETS + 1)..50usize,
        folders in 0..=FREE_MAX_FOLDERS,
    ) {
        let mut store = SoftLockStore::new();
        store.add_workspace(&workspace_id, status, snippets, folders);

        let result = store.attempt_write(&workspace_id);
        prop_assert_eq!(result, Err(SubError::ContentSoftLocked),
            "Write must be soft-locked when snippets exceed free limit after downgrade");
    }

    /// **Validates: Requirements 5.24, 5.25**
    ///
    /// Property 14b: If content exceeds free folder limits after downgrade, writes
    /// are soft-locked.
    #[test]
    fn prop_softlock_when_folders_exceed_free_limits(
        workspace_id in workspace_id_strategy(),
        status in cancelled_status_strategy(),
        snippets in 0..=FREE_MAX_SNIPPETS,
        folders in (FREE_MAX_FOLDERS + 1)..20usize,
    ) {
        let mut store = SoftLockStore::new();
        store.add_workspace(&workspace_id, status, snippets, folders);

        let result = store.attempt_write(&workspace_id);
        prop_assert_eq!(result, Err(SubError::ContentSoftLocked),
            "Write must be soft-locked when folders exceed free limit after downgrade");
    }

    /// **Validates: Requirements 5.24, 5.25**
    ///
    /// Property 14c: If content is within free limits after downgrade, writes are
    /// still allowed.
    #[test]
    fn prop_writes_allowed_when_within_free_limits_after_downgrade(
        workspace_id in workspace_id_strategy(),
        status in cancelled_status_strategy(),
        snippets in 0..=FREE_MAX_SNIPPETS,
        folders in 0..=FREE_MAX_FOLDERS,
    ) {
        let mut store = SoftLockStore::new();
        store.add_workspace(&workspace_id, status, snippets, folders);

        let result = store.attempt_write(&workspace_id);
        prop_assert!(result.is_ok(),
            "Write must succeed when content is within free limits after downgrade");
    }

    // ─── Property 15: Grace period transitions ──────────────────────────────────

    /// **Validates: Requirements 5.30, 5.31**
    ///
    /// Property 15a: While within the grace period, no transition occurs.
    #[test]
    fn prop_no_transition_within_grace_period(
        workspace_id in workspace_id_strategy(),
        days_remaining in 1i64..=GRACE_PERIOD_DAYS,
    ) {
        let mut store = GracePeriodStore::new();
        // Grace period ends at day 7, current day is before that
        let grace_end = GRACE_PERIOD_DAYS;
        let current_day = grace_end - days_remaining;
        store.add_past_due(&workspace_id, grace_end);

        let transitioned = store.check_grace_periods(current_day);
        prop_assert_eq!(transitioned, 0,
            "No transition should occur while within grace period");
        prop_assert_eq!(store.get_status(&workspace_id), &SimStatus::PastDue,
            "Status must remain PastDue during grace period");
    }

    /// **Validates: Requirements 5.30, 5.31**
    ///
    /// Property 15b: When grace_period_end passes, status transitions to cancelled.
    #[test]
    fn prop_transition_after_grace_period_expires(
        workspace_id in workspace_id_strategy(),
        days_past in 1i64..30,
    ) {
        let mut store = GracePeriodStore::new();
        let grace_end = GRACE_PERIOD_DAYS;
        let current_day = grace_end + days_past;
        store.add_past_due(&workspace_id, grace_end);

        let transitioned = store.check_grace_periods(current_day);
        prop_assert_eq!(transitioned, 1,
            "Exactly one transition should occur");
        prop_assert_eq!(store.get_status(&workspace_id), &SimStatus::Cancelled,
            "Status must transition to Cancelled after grace period expires");
    }

    /// **Validates: Requirements 5.30, 5.31**
    ///
    /// Property 15c: At exactly the grace_period_end boundary, no transition occurs
    /// (must be strictly past).
    #[test]
    fn prop_no_transition_at_exact_boundary(
        workspace_id in workspace_id_strategy(),
    ) {
        let mut store = GracePeriodStore::new();
        let grace_end = GRACE_PERIOD_DAYS;
        store.add_past_due(&workspace_id, grace_end);

        // Current day == grace_period_end: not past yet
        let transitioned = store.check_grace_periods(grace_end);
        prop_assert_eq!(transitioned, 0,
            "No transition at exact boundary (must be strictly past)");
        prop_assert_eq!(store.get_status(&workspace_id), &SimStatus::PastDue,
            "Status must remain PastDue at exact boundary");
    }

    // ─── Property 16: Referral reward idempotency ───────────────────────────────

    /// **Validates: Requirements 5.48**
    ///
    /// Property 16a: Applying reward multiple times produces same result.
    #[test]
    fn prop_referral_reward_idempotent(
        referrer_id in user_id_strategy(),
        referred_id in user_id_strategy(),
        repeat_count in 2usize..10,
    ) {
        prop_assume!(referrer_id != referred_id);
        let mut store = ReferralStore::new();
        store.record_referral(&referrer_id, &referred_id).unwrap();

        // Apply reward multiple times
        let first_result = store.apply_reward(&referrer_id, &referred_id);
        prop_assert_eq!(first_result, 1,
            "First reward application must give 1 month");

        for _ in 1..repeat_count {
            let result = store.apply_reward(&referrer_id, &referred_id);
            prop_assert_eq!(result, 1,
                "Subsequent applications must still show 1 month (idempotent)");
        }

        // Total reward months for this referrer should be exactly 1
        prop_assert_eq!(store.total_reward_months(&referrer_id), 1,
            "Total reward must be exactly 1 month regardless of repeat calls");
    }

    /// **Validates: Requirements 5.48**
    ///
    /// Property 16b: Unique constraint on (referrer_id, referred_user_id) prevents
    /// duplicate referral records.
    #[test]
    fn prop_referral_unique_constraint(
        referrer_id in user_id_strategy(),
        referred_id in user_id_strategy(),
        attempts in 2usize..8,
    ) {
        prop_assume!(referrer_id != referred_id);
        let mut store = ReferralStore::new();

        // Record the same referral multiple times — all should succeed (idempotent)
        for _ in 0..attempts {
            let result = store.record_referral(&referrer_id, &referred_id);
            prop_assert!(result.is_ok(),
                "Duplicate referral recording must not error (ON CONFLICT DO NOTHING)");
        }

        // Only one referral record should exist
        let count = store.referrals.len();
        prop_assert_eq!(count, 1,
            "Only one referral record must exist for a (referrer, referred) pair");
    }

    // ─── Property 17: Billing webhook idempotency ───────────────────────────────

    /// **Validates: Requirements 5.57**
    ///
    /// Property 17a: Processing the same event_id again produces no state change.
    #[test]
    fn prop_webhook_duplicate_event_no_change(
        workspace_id in workspace_id_strategy(),
        event_id in event_id_strategy(),
        repeat_count in 2usize..10,
    ) {
        let mut store = WebhookStore::new();
        store.add_workspace(&workspace_id, SimStatus::Active);

        // Process first time — transitions to PastDue
        let result1 = store.process_event(&event_id, &workspace_id, SimStatus::PastDue);
        prop_assert_eq!(result1, "processed");
        prop_assert_eq!(store.get_status(&workspace_id), Some(&SimStatus::PastDue));

        // Process same event_id again — must be "already_processed"
        for _ in 1..repeat_count {
            let result = store.process_event(&event_id, &workspace_id, SimStatus::Cancelled);
            prop_assert_eq!(result, "already_processed",
                "Duplicate event must return already_processed");
            // Status should NOT change to Cancelled
            prop_assert_eq!(store.get_status(&workspace_id), Some(&SimStatus::PastDue),
                "Status must not change on duplicate event");
        }
    }

    /// **Validates: Requirements 5.57**
    ///
    /// Property 17b: Different event_ids each produce their expected transition.
    #[test]
    fn prop_webhook_different_events_each_transition(
        workspace_id in workspace_id_strategy(),
        event1 in event_id_strategy(),
        event2 in event_id_strategy(),
    ) {
        prop_assume!(event1 != event2);
        let mut store = WebhookStore::new();
        store.add_workspace(&workspace_id, SimStatus::Active);

        // First event transitions to PastDue
        let result1 = store.process_event(&event1, &workspace_id, SimStatus::PastDue);
        prop_assert_eq!(result1, "processed");
        prop_assert_eq!(store.get_status(&workspace_id), Some(&SimStatus::PastDue));

        // Second (different) event transitions to Cancelled
        let result2 = store.process_event(&event2, &workspace_id, SimStatus::Cancelled);
        prop_assert_eq!(result2, "processed");
        prop_assert_eq!(store.get_status(&workspace_id), Some(&SimStatus::Cancelled));
    }

    // ─── Property 19: Invoice calculation correctness ───────────────────────────

    /// **Validates: Requirements 5.33, 5.51, 5.53**
    ///
    /// Property 19a: Discount never makes subtotal negative (clamped to 0).
    #[test]
    fn prop_invoice_subtotal_never_negative(
        base_price in base_price_strategy(),
        discount_value in flat_discount_strategy(),
        tax_rate in tax_rate_strategy(),
    ) {
        let (_, subtotal, _, _) = compute_invoice_sim(
            base_price, Some("flat"), Some(discount_value), tax_rate,
        );
        prop_assert!(subtotal >= Decimal::ZERO,
            "Subtotal must never be negative, got {}", subtotal);
    }

    /// **Validates: Requirements 5.33, 5.51, 5.53**
    ///
    /// Property 19b: Percentage discount = base_price * rate (clamped).
    #[test]
    fn prop_invoice_percentage_discount_correct(
        base_price in base_price_strategy(),
        rate in percentage_rate_strategy(),
        tax_rate in tax_rate_strategy(),
    ) {
        let (discount_amount, _, _, _) = compute_invoice_sim(
            base_price, Some("percentage"), Some(rate), tax_rate,
        );
        let expected_raw = base_price * rate;
        let expected = round2(clamp_discount(expected_raw, base_price));
        prop_assert_eq!(discount_amount, expected,
            "Percentage discount must equal base_price * rate (clamped)");
    }

    /// **Validates: Requirements 5.33, 5.51, 5.53**
    ///
    /// Property 19c: Flat discount = min(flat_amount, base_price).
    #[test]
    fn prop_invoice_flat_discount_correct(
        base_price in base_price_strategy(),
        flat_amount in flat_discount_strategy(),
        tax_rate in tax_rate_strategy(),
    ) {
        let (discount_amount, _, _, _) = compute_invoice_sim(
            base_price, Some("flat"), Some(flat_amount), tax_rate,
        );
        let expected = round2(clamp_discount(flat_amount, base_price));
        prop_assert_eq!(discount_amount, expected,
            "Flat discount must equal min(flat_amount, base_price)");
    }

    /// **Validates: Requirements 5.33, 5.51, 5.53**
    ///
    /// Property 19d: Tax = subtotal * tax_rate, total = subtotal + tax.
    #[test]
    fn prop_invoice_tax_and_total_correct(
        base_price in base_price_strategy(),
        discount_value in flat_discount_strategy(),
        tax_rate in tax_rate_strategy(),
    ) {
        let (_, subtotal, tax_amount, total) = compute_invoice_sim(
            base_price, Some("flat"), Some(discount_value), tax_rate,
        );
        let expected_tax = round2(subtotal * tax_rate);
        let expected_total = subtotal + expected_tax;

        prop_assert_eq!(tax_amount, expected_tax,
            "Tax must equal round2(subtotal * tax_rate)");
        prop_assert_eq!(total, expected_total,
            "Total must equal subtotal + tax (both already rounded)");
    }

    /// **Validates: Requirements 5.33, 5.51, 5.53**
    ///
    /// Property 19e: All invoice amounts are rounded to 2 decimal places.
    #[test]
    fn prop_invoice_amounts_rounded_to_2dp(
        base_price in base_price_strategy(),
        rate in percentage_rate_strategy(),
        tax_rate in tax_rate_strategy(),
    ) {
        let (discount_amount, subtotal, tax_amount, total) = compute_invoice_sim(
            base_price, Some("percentage"), Some(rate), tax_rate,
        );

        // All amounts must have at most 2 decimal places
        let two = Decimal::from_str("0.01").unwrap();
        prop_assert_eq!(discount_amount, discount_amount.round_dp(2),
            "Discount amount must be rounded to 2dp");
        prop_assert_eq!(subtotal, subtotal.round_dp(2),
            "Subtotal must be rounded to 2dp");
        prop_assert_eq!(tax_amount, tax_amount.round_dp(2),
            "Tax amount must be rounded to 2dp");
        prop_assert_eq!(total, total.round_dp(2),
            "Total must be rounded to 2dp");
        let _ = two; // suppress unused warning
    }

    // ─── Property 20: Coupon validation completeness ────────────────────────────

    /// **Validates: Requirements 5.37, 5.38**
    ///
    /// Property 20a: A coupon that doesn't exist returns CouponNotFound.
    #[test]
    fn prop_coupon_not_found(
        code in "[A-Z]{6,10}",
    ) {
        let coupons: Vec<SimCoupon> = vec![];
        let result = validate_coupon(&coupons, &code, 0);
        prop_assert_eq!(result, Err(SubError::CouponNotFound),
            "Non-existent coupon must return CouponNotFound");
    }

    /// **Validates: Requirements 5.37, 5.38**
    ///
    /// Property 20b: An inactive coupon returns CouponInactive.
    #[test]
    fn prop_coupon_inactive(
        code in "[A-Z]{6,10}",
    ) {
        let coupons = vec![SimCoupon {
            code: code.clone(),
            active: false,
            valid_from_day: -10,
            valid_until_day: Some(100),
            max_uses: None,
            times_used: 0,
        }];
        let result = validate_coupon(&coupons, &code, 0);
        prop_assert_eq!(result, Err(SubError::CouponInactive),
            "Inactive coupon must return CouponInactive");
    }

    /// **Validates: Requirements 5.37, 5.38**
    ///
    /// Property 20c: A coupon with valid_from in the future returns CouponNotYetValid.
    #[test]
    fn prop_coupon_not_yet_valid(
        code in "[A-Z]{6,10}",
        future_days in 1i64..100,
    ) {
        let coupons = vec![SimCoupon {
            code: code.clone(),
            active: true,
            valid_from_day: future_days,
            valid_until_day: None,
            max_uses: None,
            times_used: 0,
        }];
        let result = validate_coupon(&coupons, &code, 0);
        prop_assert_eq!(result, Err(SubError::CouponNotYetValid),
            "Coupon with future valid_from must return CouponNotYetValid");
    }

    /// **Validates: Requirements 5.37, 5.38**
    ///
    /// Property 20d: An expired coupon returns CouponExpired.
    #[test]
    fn prop_coupon_expired(
        code in "[A-Z]{6,10}",
        expired_days_ago in 1i64..100,
    ) {
        let coupons = vec![SimCoupon {
            code: code.clone(),
            active: true,
            valid_from_day: -100,
            valid_until_day: Some(-expired_days_ago),
            max_uses: None,
            times_used: 0,
        }];
        let result = validate_coupon(&coupons, &code, 0);
        prop_assert_eq!(result, Err(SubError::CouponExpired),
            "Expired coupon must return CouponExpired");
    }

    /// **Validates: Requirements 5.37, 5.38**
    ///
    /// Property 20e: A coupon that has reached max_uses returns CouponUsageLimitReached.
    #[test]
    fn prop_coupon_usage_limit_reached(
        code in "[A-Z]{6,10}",
        max_uses in 1u32..100,
    ) {
        let coupons = vec![SimCoupon {
            code: code.clone(),
            active: true,
            valid_from_day: -10,
            valid_until_day: None,
            max_uses: Some(max_uses),
            times_used: max_uses, // exactly at limit
        }];
        let result = validate_coupon(&coupons, &code, 0);
        prop_assert_eq!(result, Err(SubError::CouponUsageLimitReached),
            "Coupon at max_uses must return CouponUsageLimitReached");
    }

    /// **Validates: Requirements 5.37, 5.38**
    ///
    /// Property 20f: A valid coupon (active, valid_from <= now, not expired,
    /// uses < max) passes all checks.
    #[test]
    fn prop_valid_coupon_passes_all_checks(
        code in "[A-Z]{6,10}",
        uses in 0u32..50,
        max_uses in 50u32..100,
    ) {
        let coupons = vec![SimCoupon {
            code: code.clone(),
            active: true,
            valid_from_day: -10,
            valid_until_day: Some(100),
            max_uses: Some(max_uses),
            times_used: uses,
        }];
        let result = validate_coupon(&coupons, &code, 0);
        prop_assert!(result.is_ok(),
            "Valid coupon must pass all validation checks");
    }
}
