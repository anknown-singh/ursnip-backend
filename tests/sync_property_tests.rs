//! Property-based tests for Sync Service (Properties 2, 3, 4, 5, 26).
//!
//! These tests validate workspace-scoped version monotonicity, trigger uniqueness,
//! batch operation atomicity, delta retention window enforcement, and trigger reuse
//! after soft-deletion. They use simulated in-memory stores (no real database needed).
//!
//! Run with: `cargo test --test sync_property_tests`

use proptest::prelude::*;

// ─── Constants ──────────────────────────────────────────────────────────────────

/// Maximum number of items allowed in a single batch operation.
const MAX_BATCH_SIZE: usize = 100;

/// Delta retention window in days.
const DELTA_RETENTION_DAYS: i64 = 30;

/// Maximum snippets on free tier.
#[allow(dead_code)]
const FREE_TIER_MAX_SNIPPETS: usize = 10;

/// Maximum content chars on free tier.
#[allow(dead_code)]
const FREE_TIER_MAX_CONTENT_CHARS: usize = 2000;

// ─── Property 2: Workspace-scoped version monotonicity ──────────────────────────
//
// **Validates: Requirements 2.4**
//
// For any sequence of mutations within a workspace, the version numbers SHALL be
// strictly monotonically increasing (each > previous). Different workspaces have
// independent version sequences.

/// Simulated version store for testing workspace-scoped version monotonicity.
/// Tracks the current version for each workspace independently.
#[derive(Debug, Clone)]
struct VersionStore {
    /// Map of workspace_id -> current version
    versions: Vec<(String, i64)>,
}

impl VersionStore {
    fn new() -> Self {
        Self {
            versions: Vec::new(),
        }
    }

    /// Get the current version for a workspace, or 0 if not yet initialized.
    fn current_version(&self, workspace_id: &str) -> i64 {
        self.versions
            .iter()
            .find(|(wid, _)| wid == workspace_id)
            .map(|(_, v)| *v)
            .unwrap_or(0)
    }

    /// Apply a mutation to a workspace — returns the assigned version.
    /// Each mutation increments the workspace version by exactly 1.
    fn apply_mutation(&mut self, workspace_id: &str) -> i64 {
        let entry = self.versions.iter_mut().find(|(wid, _)| wid == workspace_id);
        match entry {
            Some((_, v)) => {
                *v += 1;
                *v
            }
            None => {
                self.versions.push((workspace_id.to_string(), 1));
                1
            }
        }
    }
}

// ─── Property 3: Trigger uniqueness per workspace ───────────────────────────────
//
// **Validates: Requirements 2.32**
//
// Within a workspace, no two active (non-deleted) snippets SHALL have the same
// trigger. Creating a snippet with a duplicate trigger in the same workspace must
// fail. Different workspaces CAN have the same trigger.

/// Simulated error type for snippet operations.
#[derive(Debug, Clone, PartialEq)]
enum SyncError {
    TriggerAlreadyExists,
    BatchSizeExceeded,
    BatchValidationFailed(usize), // index of failing item
    SnapshotRequired,
    SnippetNotFound,
}

/// Simulated snippet record.
#[derive(Debug, Clone)]
struct SimSnippet {
    id: usize,
    workspace_id: String,
    trigger: String,
    deleted_at: Option<i64>, // simulated timestamp (days from epoch)
}

/// Simulated trigger uniqueness store for testing snippet creation logic.
#[derive(Debug, Clone)]
struct TriggerStore {
    snippets: Vec<SimSnippet>,
    next_id: usize,
}

impl TriggerStore {
    fn new() -> Self {
        Self {
            snippets: Vec::new(),
            next_id: 0,
        }
    }

    /// Create a snippet. Enforces trigger uniqueness among active snippets
    /// within the same workspace.
    fn create_snippet(
        &mut self,
        workspace_id: &str,
        trigger: &str,
    ) -> Result<usize, SyncError> {
        // Check for duplicate trigger among active snippets in same workspace
        let has_duplicate = self.snippets.iter().any(|s| {
            s.workspace_id == workspace_id
                && s.trigger == trigger
                && s.deleted_at.is_none()
        });

        if has_duplicate {
            return Err(SyncError::TriggerAlreadyExists);
        }

        let id = self.next_id;
        self.next_id += 1;
        self.snippets.push(SimSnippet {
            id,
            workspace_id: workspace_id.to_string(),
            trigger: trigger.to_string(),
            deleted_at: None,
        });
        Ok(id)
    }

    /// Soft-delete a snippet by setting deleted_at.
    fn soft_delete(&mut self, snippet_id: usize, timestamp: i64) -> Result<(), SyncError> {
        let snippet = self.snippets.iter_mut().find(|s| s.id == snippet_id);
        match snippet {
            Some(s) => {
                s.deleted_at = Some(timestamp);
                Ok(())
            }
            None => Err(SyncError::SnippetNotFound),
        }
    }

    /// Count active (non-deleted) snippets in a workspace.
    fn active_count(&self, workspace_id: &str) -> usize {
        self.snippets
            .iter()
            .filter(|s| s.workspace_id == workspace_id && s.deleted_at.is_none())
            .count()
    }

    /// Check if a trigger is active in a workspace.
    fn trigger_exists_active(&self, workspace_id: &str, trigger: &str) -> bool {
        self.snippets.iter().any(|s| {
            s.workspace_id == workspace_id
                && s.trigger == trigger
                && s.deleted_at.is_none()
        })
    }
}

// ─── Property 4: Batch operation atomicity ──────────────────────────────────────
//
// **Validates: Requirements 2.34, 2.37**
//
// A batch of up to 100 operations SHALL either all succeed or all fail.
// If any operation in the batch fails validation, no changes are persisted.
// Each operation in a successful batch gets a sequential version.

/// A simulated batch operation item.
#[derive(Debug, Clone)]
struct BatchItem {
    trigger: String,
    valid: bool, // false simulates a validation failure
}

/// Simulated batch store for testing atomicity.
#[derive(Debug, Clone)]
struct BatchStore {
    /// (workspace_id, trigger, version)
    committed_snippets: Vec<(String, String, i64)>,
    /// Current version per workspace
    versions: Vec<(String, i64)>,
}

impl BatchStore {
    fn new() -> Self {
        Self {
            committed_snippets: Vec::new(),
            versions: Vec::new(),
        }
    }

    fn current_version(&self, workspace_id: &str) -> i64 {
        self.versions
            .iter()
            .find(|(wid, _)| wid == workspace_id)
            .map(|(_, v)| *v)
            .unwrap_or(0)
    }

    /// Execute a batch of operations atomically.
    /// Returns Ok(versions_assigned) if all succeed, Err if any fail.
    fn execute_batch(
        &mut self,
        workspace_id: &str,
        items: &[BatchItem],
    ) -> Result<Vec<i64>, SyncError> {
        // Validate batch size
        if items.len() > MAX_BATCH_SIZE {
            return Err(SyncError::BatchSizeExceeded);
        }

        // Check all items for validity first (atomic: all-or-nothing)
        for (idx, item) in items.iter().enumerate() {
            if !item.valid {
                return Err(SyncError::BatchValidationFailed(idx));
            }
        }

        // Check for duplicate triggers within the batch itself
        let mut seen_triggers: Vec<&str> = Vec::new();
        for (idx, item) in items.iter().enumerate() {
            if seen_triggers.contains(&item.trigger.as_str()) {
                return Err(SyncError::BatchValidationFailed(idx));
            }
            // Also check against already committed active snippets
            let already_exists = self.committed_snippets.iter().any(|(wid, t, _)| {
                wid == workspace_id && t == &item.trigger
            });
            if already_exists {
                return Err(SyncError::BatchValidationFailed(idx));
            }
            seen_triggers.push(&item.trigger);
        }

        // All validated — commit all operations with sequential versions
        let mut assigned_versions = Vec::new();
        let base_version = self.current_version(workspace_id);

        for (i, item) in items.iter().enumerate() {
            let version = base_version + (i as i64) + 1;
            self.committed_snippets.push((
                workspace_id.to_string(),
                item.trigger.clone(),
                version,
            ));
            assigned_versions.push(version);
        }

        // Update the workspace version
        let final_version = base_version + items.len() as i64;
        let entry = self.versions.iter_mut().find(|(wid, _)| wid == workspace_id);
        match entry {
            Some((_, v)) => *v = final_version,
            None => self.versions.push((workspace_id.to_string(), final_version)),
        }

        Ok(assigned_versions)
    }

    /// Count committed snippets for a workspace.
    fn snippet_count(&self, workspace_id: &str) -> usize {
        self.committed_snippets
            .iter()
            .filter(|(wid, _, _)| wid == workspace_id)
            .count()
    }
}

// ─── Property 5: Delta retention window enforcement ─────────────────────────────
//
// **Validates: Requirements 2.16, 2.45**
//
// If the oldest delta in a workspace is older than 30 days AND the requested
// since_version is below the minimum available version, return SNAPSHOT_REQUIRED (409).
// If the oldest delta is within 30 days, deltas are available normally.

/// A simulated delta record.
#[derive(Debug, Clone, PartialEq)]
struct SimDelta {
    workspace_id: String,
    version: i64,
    created_days_ago: i64, // how many days ago this delta was created
}

/// Simulated delta store for testing retention window logic.
#[derive(Debug, Clone)]
struct DeltaStore {
    deltas: Vec<SimDelta>,
}

impl DeltaStore {
    fn new() -> Self {
        Self { deltas: Vec::new() }
    }

    /// Add a delta with a specific age (days ago).
    fn add_delta(&mut self, workspace_id: &str, version: i64, days_ago: i64) {
        self.deltas.push(SimDelta {
            workspace_id: workspace_id.to_string(),
            version,
            created_days_ago: days_ago,
        });
    }

    /// Query deltas since a given version. Returns:
    /// - Ok(deltas) if the since_version is within the retention window
    /// - Err(SnapshotRequired) if the since_version refers to expired deltas
    fn get_deltas_since(
        &self,
        workspace_id: &str,
        since_version: i64,
    ) -> Result<Vec<&SimDelta>, SyncError> {
        let workspace_deltas: Vec<&SimDelta> = self
            .deltas
            .iter()
            .filter(|d| d.workspace_id == workspace_id)
            .collect();

        if workspace_deltas.is_empty() {
            // No deltas at all — return empty (valid, nothing to sync)
            return Ok(Vec::new());
        }

        // Find the minimum available version (after retention purge)
        let available_deltas: Vec<&SimDelta> = workspace_deltas
            .iter()
            .filter(|d| d.created_days_ago <= DELTA_RETENTION_DAYS)
            .copied()
            .collect();

        if available_deltas.is_empty() {
            // All deltas are expired
            return Err(SyncError::SnapshotRequired);
        }

        let min_available_version = available_deltas
            .iter()
            .map(|d| d.version)
            .min()
            .unwrap();

        // If since_version is below the minimum available, snapshot required
        if since_version < min_available_version - 1 {
            return Err(SyncError::SnapshotRequired);
        }

        // Return deltas with version > since_version that are within retention
        let result: Vec<&SimDelta> = available_deltas
            .into_iter()
            .filter(|d| d.version > since_version)
            .collect();

        Ok(result)
    }
}

// ─── Strategies ─────────────────────────────────────────────────────────────────

/// Strategy for generating workspace IDs.
fn workspace_id_strategy() -> impl Strategy<Value = String> {
    "[a-f0-9]{8}".prop_map(|s| format!("ws_{}", s))
}

/// Strategy for generating trigger strings.
fn trigger_strategy() -> impl Strategy<Value = String> {
    "[a-z]{2,8}".prop_map(|s| s)
}

/// Strategy for generating a set of distinct triggers.
fn distinct_triggers(count: usize) -> impl Strategy<Value = Vec<String>> {
    proptest::collection::hash_set("[a-z]{3,8}".prop_map(|s| s), count)
        .prop_map(|set| set.into_iter().collect())
}

/// Strategy for generating mutation counts.
fn mutation_count_strategy() -> impl Strategy<Value = usize> {
    1usize..50
}

/// Strategy for generating batch sizes (valid, within MAX_BATCH_SIZE).
fn valid_batch_size_strategy() -> impl Strategy<Value = usize> {
    1usize..=MAX_BATCH_SIZE
}

/// Strategy for days ago (within retention).
fn days_within_retention() -> impl Strategy<Value = i64> {
    0i64..DELTA_RETENTION_DAYS
}

/// Strategy for days ago (beyond retention).
fn days_beyond_retention() -> impl Strategy<Value = i64> {
    (DELTA_RETENTION_DAYS + 1)..=(DELTA_RETENTION_DAYS * 3)
}

// ─── Property Test Implementations ──────────────────────────────────────────────

proptest! {
    // ─── Property 2: Workspace-scoped version monotonicity ──────────────────────

    /// **Validates: Requirements 2.4**
    ///
    /// Property 2a: Each mutation within a workspace produces a strictly increasing
    /// version number (each > previous, no gaps).
    #[test]
    fn prop_version_strictly_monotonic(
        workspace_id in workspace_id_strategy(),
        num_mutations in mutation_count_strategy()
    ) {
        let mut store = VersionStore::new();
        let mut previous_version = 0i64;

        for _ in 0..num_mutations {
            let version = store.apply_mutation(&workspace_id);
            prop_assert!(version > previous_version,
                "Version {} must be strictly greater than previous {}",
                version, previous_version);
            prop_assert_eq!(version, previous_version + 1,
                "Version must increment by exactly 1 (no gaps)");
            previous_version = version;
        }
    }

    /// **Validates: Requirements 2.4**
    ///
    /// Property 2b: Different workspaces have independent version sequences.
    /// Mutations in one workspace do not affect another's version counter.
    #[test]
    fn prop_workspace_versions_independent(
        ws1 in workspace_id_strategy(),
        ws2 in workspace_id_strategy(),
        mutations1 in 1usize..20,
        mutations2 in 1usize..20,
    ) {
        prop_assume!(ws1 != ws2);
        let mut store = VersionStore::new();

        // Apply mutations to workspace 1
        for _ in 0..mutations1 {
            store.apply_mutation(&ws1);
        }

        // Apply mutations to workspace 2
        for _ in 0..mutations2 {
            store.apply_mutation(&ws2);
        }

        // Each workspace should have its own version, independent of the other
        prop_assert_eq!(store.current_version(&ws1), mutations1 as i64,
            "Workspace 1 version should equal its own mutation count");
        prop_assert_eq!(store.current_version(&ws2), mutations2 as i64,
            "Workspace 2 version should equal its own mutation count");
    }

    /// **Validates: Requirements 2.4**
    ///
    /// Property 2c: Interleaved mutations across workspaces maintain monotonicity
    /// independently in each workspace.
    #[test]
    fn prop_interleaved_mutations_maintain_monotonicity(
        ws1 in workspace_id_strategy(),
        ws2 in workspace_id_strategy(),
        sequence in proptest::collection::vec(prop_oneof![Just(true), Just(false)], 5..30),
    ) {
        prop_assume!(ws1 != ws2);
        let mut store = VersionStore::new();
        let mut last_ws1: i64 = 0;
        let mut last_ws2: i64 = 0;

        for apply_to_ws1 in &sequence {
            if *apply_to_ws1 {
                let v = store.apply_mutation(&ws1);
                prop_assert!(v > last_ws1,
                    "WS1 version {} must exceed previous {}", v, last_ws1);
                last_ws1 = v;
            } else {
                let v = store.apply_mutation(&ws2);
                prop_assert!(v > last_ws2,
                    "WS2 version {} must exceed previous {}", v, last_ws2);
                last_ws2 = v;
            }
        }
    }

    // ─── Property 3: Trigger uniqueness per workspace ─────────────────────────────

    /// **Validates: Requirements 2.32**
    ///
    /// Property 3a: Creating two snippets with the same trigger in the same
    /// workspace SHALL fail with TriggerAlreadyExists.
    #[test]
    fn prop_duplicate_trigger_same_workspace_fails(
        workspace_id in workspace_id_strategy(),
        trigger in trigger_strategy()
    ) {
        let mut store = TriggerStore::new();

        // First creation succeeds
        let result1 = store.create_snippet(&workspace_id, &trigger);
        prop_assert!(result1.is_ok(), "First snippet creation must succeed");

        // Second creation with same trigger must fail
        let result2 = store.create_snippet(&workspace_id, &trigger);
        prop_assert_eq!(result2, Err(SyncError::TriggerAlreadyExists),
            "Duplicate trigger in same workspace must return TriggerAlreadyExists");
    }

    /// **Validates: Requirements 2.32**
    ///
    /// Property 3b: Different workspaces CAN use identical trigger values
    /// independently.
    #[test]
    fn prop_same_trigger_different_workspaces_allowed(
        ws1 in workspace_id_strategy(),
        ws2 in workspace_id_strategy(),
        trigger in trigger_strategy()
    ) {
        prop_assume!(ws1 != ws2);
        let mut store = TriggerStore::new();

        let result1 = store.create_snippet(&ws1, &trigger);
        let result2 = store.create_snippet(&ws2, &trigger);

        prop_assert!(result1.is_ok(),
            "Creating trigger in workspace 1 must succeed");
        prop_assert!(result2.is_ok(),
            "Same trigger in a different workspace must also succeed");
    }

    /// **Validates: Requirements 2.32**
    ///
    /// Property 3c: After creating multiple distinct triggers, all are active and
    /// no uniqueness violation occurs.
    #[test]
    fn prop_distinct_triggers_all_succeed(
        workspace_id in workspace_id_strategy(),
        triggers in distinct_triggers(5),
    ) {
        let mut store = TriggerStore::new();

        for trigger in &triggers {
            let result = store.create_snippet(&workspace_id, trigger);
            prop_assert!(result.is_ok(),
                "Creating distinct trigger '{}' must succeed", trigger);
        }

        prop_assert_eq!(store.active_count(&workspace_id), triggers.len(),
            "All distinct triggers should be active");
    }

    // ─── Property 4: Batch operation atomicity ──────────────────────────────────

    /// **Validates: Requirements 2.34, 2.37**
    ///
    /// Property 4a: A valid batch of N operations all succeed and get sequential
    /// versions starting from current_version + 1.
    #[test]
    fn prop_valid_batch_assigns_sequential_versions(
        workspace_id in workspace_id_strategy(),
        batch_size in valid_batch_size_strategy(),
        triggers in distinct_triggers(100),
    ) {
        prop_assume!(triggers.len() >= batch_size);
        let mut store = BatchStore::new();

        let items: Vec<BatchItem> = triggers[..batch_size]
            .iter()
            .map(|t| BatchItem { trigger: t.clone(), valid: true })
            .collect();

        let result = store.execute_batch(&workspace_id, &items);
        prop_assert!(result.is_ok(), "Valid batch must succeed");

        let versions = result.unwrap();
        prop_assert_eq!(versions.len(), batch_size,
            "Must assign one version per item");

        // Versions must be sequential starting from 1
        for (i, v) in versions.iter().enumerate() {
            prop_assert_eq!(*v, (i as i64) + 1,
                "Version at index {} must be {}", i, i + 1);
        }

        // Workspace version should be at the last assigned
        prop_assert_eq!(store.current_version(&workspace_id), batch_size as i64);
    }

    /// **Validates: Requirements 2.34, 2.37**
    ///
    /// Property 4b: If any item in the batch fails validation, the entire batch
    /// is rejected and no snippets are persisted.
    #[test]
    fn prop_batch_with_invalid_item_rolls_back(
        workspace_id in workspace_id_strategy(),
        valid_count in 1usize..10,
        fail_index in 0usize..10,
        triggers in distinct_triggers(15),
    ) {
        prop_assume!(triggers.len() >= valid_count + 1);
        let fail_pos = fail_index.min(valid_count); // ensure fail_index is within bounds

        let mut store = BatchStore::new();
        let version_before = store.current_version(&workspace_id);
        let count_before = store.snippet_count(&workspace_id);

        let mut items: Vec<BatchItem> = triggers[..=valid_count]
            .iter()
            .map(|t| BatchItem { trigger: t.clone(), valid: true })
            .collect();

        // Make one item invalid
        items[fail_pos].valid = false;

        let result = store.execute_batch(&workspace_id, &items);
        prop_assert!(result.is_err(),
            "Batch with an invalid item must fail");

        // No changes should be persisted
        prop_assert_eq!(store.current_version(&workspace_id), version_before,
            "Version must not change after failed batch");
        prop_assert_eq!(store.snippet_count(&workspace_id), count_before,
            "No snippets should be committed after failed batch");
    }

    /// **Validates: Requirements 2.34, 2.37**
    ///
    /// Property 4c: A batch exceeding MAX_BATCH_SIZE (100) is rejected.
    #[test]
    fn prop_batch_exceeding_max_size_rejected(
        workspace_id in workspace_id_strategy(),
        extra in 1usize..50,
    ) {
        let mut store = BatchStore::new();
        let size = MAX_BATCH_SIZE + extra;

        let items: Vec<BatchItem> = (0..size)
            .map(|i| BatchItem { trigger: format!("trig_{}", i), valid: true })
            .collect();

        let result = store.execute_batch(&workspace_id, &items);
        prop_assert_eq!(result, Err(SyncError::BatchSizeExceeded),
            "Batch of size {} must be rejected (max {})", size, MAX_BATCH_SIZE);
    }

    /// **Validates: Requirements 2.34, 2.37**
    ///
    /// Property 4d: Multiple successful batches produce a continuous version
    /// sequence with no gaps between batches.
    #[test]
    fn prop_multiple_batches_continuous_versions(
        workspace_id in workspace_id_strategy(),
        batch1_size in 1usize..20,
        batch2_size in 1usize..20,
    ) {
        let mut store = BatchStore::new();

        let items1: Vec<BatchItem> = (0..batch1_size)
            .map(|i| BatchItem { trigger: format!("a_{}", i), valid: true })
            .collect();
        let items2: Vec<BatchItem> = (0..batch2_size)
            .map(|i| BatchItem { trigger: format!("b_{}", i), valid: true })
            .collect();

        let v1 = store.execute_batch(&workspace_id, &items1).unwrap();
        let v2 = store.execute_batch(&workspace_id, &items2).unwrap();

        // Last version of batch 1 + 1 == first version of batch 2
        let last_v1 = *v1.last().unwrap();
        let first_v2 = *v2.first().unwrap();
        prop_assert_eq!(first_v2, last_v1 + 1,
            "First version of batch 2 must be contiguous with last of batch 1");

        // Total version should match sum of batch sizes
        prop_assert_eq!(store.current_version(&workspace_id),
            (batch1_size + batch2_size) as i64);
    }

    // ─── Property 5: Delta retention window enforcement ────────────────────────────

    /// **Validates: Requirements 2.16, 2.45**
    ///
    /// Property 5a: Requesting deltas within the retention window succeeds and
    /// returns the expected deltas.
    #[test]
    fn prop_deltas_within_retention_available(
        workspace_id in workspace_id_strategy(),
        num_deltas in 1usize..20,
        days_ago in days_within_retention(),
    ) {
        let mut store = DeltaStore::new();

        for i in 1..=(num_deltas as i64) {
            store.add_delta(&workspace_id, i, days_ago);
        }

        // Request from version 0 (all deltas)
        let result = store.get_deltas_since(&workspace_id, 0);
        prop_assert!(result.is_ok(),
            "Deltas within retention window must be available");

        let deltas = result.unwrap();
        prop_assert_eq!(deltas.len(), num_deltas,
            "All {} deltas should be returned", num_deltas);
    }

    /// **Validates: Requirements 2.16, 2.45**
    ///
    /// Property 5b: Requesting deltas older than the retention window returns
    /// SNAPSHOT_REQUIRED.
    #[test]
    fn prop_deltas_beyond_retention_returns_snapshot_required(
        workspace_id in workspace_id_strategy(),
        num_deltas in 1usize..10,
        days_ago in days_beyond_retention(),
    ) {
        let mut store = DeltaStore::new();

        for i in 1..=(num_deltas as i64) {
            store.add_delta(&workspace_id, i, days_ago);
        }

        // Request from version 0 — all deltas are expired
        let result = store.get_deltas_since(&workspace_id, 0);
        prop_assert_eq!(result, Err(SyncError::SnapshotRequired),
            "All deltas expired → must return SnapshotRequired");
    }

    /// **Validates: Requirements 2.16, 2.45**
    ///
    /// Property 5c: When some deltas are within retention and some are beyond,
    /// requesting a version that is within the available range succeeds.
    #[test]
    fn prop_mixed_retention_recent_since_version_succeeds(
        workspace_id in workspace_id_strategy(),
        recent_count in 1usize..10,
        expired_count in 1usize..5,
        recent_days in days_within_retention(),
        expired_days in days_beyond_retention(),
    ) {
        let mut store = DeltaStore::new();

        // Add expired deltas (older versions)
        for i in 1..=(expired_count as i64) {
            store.add_delta(&workspace_id, i, expired_days);
        }

        // Add recent deltas (newer versions, starting after expired ones)
        let base = expired_count as i64;
        for i in 1..=(recent_count as i64) {
            store.add_delta(&workspace_id, base + i, recent_days);
        }

        // Request from a version just before the recent deltas — should succeed
        let since_version = base; // asking for everything after the expired ones
        let result = store.get_deltas_since(&workspace_id, since_version);
        prop_assert!(result.is_ok(),
            "Since version at the boundary of available deltas should succeed");

        let deltas = result.unwrap();
        prop_assert_eq!(deltas.len(), recent_count,
            "Should return only the {} recent deltas", recent_count);
    }

    /// **Validates: Requirements 2.16, 2.45**
    ///
    /// Property 5d: When some deltas are beyond retention, requesting a version
    /// older than the minimum available returns SNAPSHOT_REQUIRED.
    #[test]
    fn prop_mixed_retention_old_since_version_fails(
        workspace_id in workspace_id_strategy(),
        recent_count in 1usize..5,
        expired_count in 2usize..5,
        recent_days in days_within_retention(),
        expired_days in days_beyond_retention(),
    ) {
        let mut store = DeltaStore::new();

        // Add expired deltas
        for i in 1..=(expired_count as i64) {
            store.add_delta(&workspace_id, i, expired_days);
        }

        // Add recent deltas
        let base = expired_count as i64;
        for i in 1..=(recent_count as i64) {
            store.add_delta(&workspace_id, base + i, recent_days);
        }

        // Request from version 0 — which is before the min available (base + 1)
        let result = store.get_deltas_since(&workspace_id, 0);
        prop_assert_eq!(result, Err(SyncError::SnapshotRequired),
            "Since version below minimum available must return SnapshotRequired");
    }

    // ─── Property 26: Partial unique index allows trigger reuse after deletion ───

    /// **Validates: Requirements 2.45, 6.13**
    ///
    /// Property 26a: After soft-deleting a snippet, its trigger can be reused in
    /// the same workspace by a new snippet.
    #[test]
    fn prop_trigger_reuse_after_soft_delete(
        workspace_id in workspace_id_strategy(),
        trigger in trigger_strategy()
    ) {
        let mut store = TriggerStore::new();

        // Create snippet with trigger
        let id = store.create_snippet(&workspace_id, &trigger).unwrap();

        // Soft-delete it
        store.soft_delete(id, 1).unwrap();

        // Now creating a new snippet with the same trigger must succeed
        let result = store.create_snippet(&workspace_id, &trigger);
        prop_assert!(result.is_ok(),
            "After soft-delete, the trigger must be reusable in the same workspace");
    }

    /// **Validates: Requirements 2.45, 6.13**
    ///
    /// Property 26b: A soft-deleted snippet's trigger does NOT block new
    /// creation, but an active snippet's trigger DOES block.
    #[test]
    fn prop_active_snippet_blocks_but_deleted_does_not(
        workspace_id in workspace_id_strategy(),
        trigger in trigger_strategy()
    ) {
        let mut store = TriggerStore::new();

        // Create and soft-delete
        let id1 = store.create_snippet(&workspace_id, &trigger).unwrap();
        store.soft_delete(id1, 1).unwrap();

        // Create again (reuse) — succeeds
        let id2 = store.create_snippet(&workspace_id, &trigger).unwrap();
        prop_assert!(id2 != id1, "New snippet must have a different ID");

        // Now trying to create AGAIN while id2 is active must fail
        let result = store.create_snippet(&workspace_id, &trigger);
        prop_assert_eq!(result, Err(SyncError::TriggerAlreadyExists),
            "Active snippet must block duplicate trigger creation");
    }

    /// **Validates: Requirements 2.45, 6.13**
    ///
    /// Property 26c: Multiple soft-deletes and re-creations of the same trigger
    /// all succeed (the partial unique index always allows reuse after deletion).
    #[test]
    fn prop_repeated_delete_and_reuse_cycle(
        workspace_id in workspace_id_strategy(),
        trigger in trigger_strategy(),
        cycles in 2usize..8
    ) {
        let mut store = TriggerStore::new();

        for cycle in 0..cycles {
            // Create
            let id = store.create_snippet(&workspace_id, &trigger);
            prop_assert!(id.is_ok(),
                "Cycle {}: creation must succeed after previous deletion", cycle);

            // Soft-delete
            store.soft_delete(id.unwrap(), cycle as i64 + 1).unwrap();
        }

        // After all deletions, one more creation must succeed
        let final_result = store.create_snippet(&workspace_id, &trigger);
        prop_assert!(final_result.is_ok(),
            "Final creation after all cycles must succeed");

        // Only one active snippet should exist
        prop_assert!(store.trigger_exists_active(&workspace_id, &trigger),
            "The trigger should be active after final creation");
    }

    /// **Validates: Requirements 2.45, 6.13**
    ///
    /// Property 26d: The uniqueness constraint only applies to active (non-deleted)
    /// snippets; multiple deleted snippets with the same trigger can coexist.
    #[test]
    fn prop_multiple_deleted_same_trigger_coexist(
        workspace_id in workspace_id_strategy(),
        trigger in trigger_strategy(),
        count in 2usize..6
    ) {
        let mut store = TriggerStore::new();

        // Create and delete multiple snippets with the same trigger
        for i in 0..count {
            let id = store.create_snippet(&workspace_id, &trigger).unwrap();
            store.soft_delete(id, i as i64 + 1).unwrap();
        }

        // All deleted snippets coexist — count should be in the store
        let total_snippets = store.snippets.len();
        prop_assert_eq!(total_snippets, count,
            "All {} deleted snippets should exist in the store", count);

        // No active snippets with this trigger
        prop_assert!(!store.trigger_exists_active(&workspace_id, &trigger),
            "No active snippet should exist after all deletions");

        // Creating a new one should succeed
        let result = store.create_snippet(&workspace_id, &trigger);
        prop_assert!(result.is_ok(),
            "Creating after all deletions must succeed");
    }
}
