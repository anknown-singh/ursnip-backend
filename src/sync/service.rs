use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::AppError;
use crate::models::common::{SubscriptionStatus, Tier};

// ─── Constants ──────────────────────────────────────────────────────────────────

/// Maximum number of snippets allowed on the free tier.
const FREE_TIER_MAX_SNIPPETS: i64 = 10;

/// Maximum content length (in characters) allowed on the free tier.
const FREE_TIER_MAX_CONTENT_CHARS: usize = 2000;

/// Default limit for delta polling pagination.
const DELTA_DEFAULT_LIMIT: i64 = 500;

/// Maximum allowed limit for delta polling pagination.
const DELTA_MAX_LIMIT: i64 = 1000;

/// Delta retention window in days.
const DELTA_RETENTION_DAYS: i64 = 30;

/// Maximum number of folders allowed on the free tier.
const FREE_TIER_MAX_FOLDERS: i64 = 3;

/// Maximum number of items allowed in a single batch operation.
const MAX_BATCH_SIZE: usize = 100;

// ─── Request DTOs ───────────────────────────────────────────────────────────────

/// Payload for creating a new snippet.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateSnippetPayload {
    pub workspace_id: Uuid,
    pub trigger: String,
    pub content: String,
    pub snippet_type: String,
    pub folder_id: Option<Uuid>,
}

/// Payload for updating an existing snippet.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateSnippetPayload {
    pub trigger: Option<String>,
    pub content: Option<String>,
    pub snippet_type: Option<String>,
}

// ─── Response DTOs ──────────────────────────────────────────────────────────────

/// Response payload for snippet operations.
#[derive(Debug, Clone, Serialize)]
pub struct SnippetResponse {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub created_by: Uuid,
    pub trigger: String,
    pub content: String,
    pub snippet_type: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

/// Response payload for folder operations.
#[derive(Debug, Clone, Serialize)]
pub struct FolderResponse {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub created_by: Uuid,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

/// Full workspace snapshot response including all active folders, snippets, and current version.
#[derive(Debug, Clone, Serialize)]
pub struct SnapshotResponse {
    pub workspace_id: Uuid,
    pub version: i64,
    pub snippets: Vec<SnippetResponse>,
    pub folders: Vec<FolderResponse>,
}

/// Single delta entry in a delta polling response.
#[derive(Debug, Clone, Serialize)]
pub struct DeltaResponse {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub entity_type: String,
    pub entity_id: Uuid,
    pub operation: String,
    pub payload: serde_json::Value,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

/// Paginated delta polling response.
#[derive(Debug, Clone, Serialize)]
pub struct DeltasResponse {
    pub deltas: Vec<DeltaResponse>,
    pub has_more: bool,
    pub next_since_version: i64,
}

// ─── Folder DTOs ────────────────────────────────────────────────────────────────

/// Payload for creating a new folder.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateFolderPayload {
    pub workspace_id: Uuid,
    pub name: String,
}

/// Payload for updating an existing folder.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateFolderPayload {
    pub name: Option<String>,
}

// ─── Batch Operation DTOs ───────────────────────────────────────────────────────

/// Payload for batch snippet operations.
#[derive(Debug, Clone, Deserialize)]
pub struct BatchOperationsPayload {
    pub workspace_id: Uuid,
    pub operations: Vec<BatchOperation>,
}

/// A single operation within a batch request.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BatchOperation {
    CreateSnippet(CreateSnippetPayload),
    UpdateSnippet {
        id: Uuid,
        #[serde(flatten)]
        payload: UpdateSnippetPayload,
    },
    DeleteSnippet {
        id: Uuid,
    },
}

/// Response for batch operations.
#[derive(Debug, Clone, Serialize)]
pub struct BatchOperationsResponse {
    pub results: Vec<BatchItemResult>,
    pub workspace_version: i64,
}

/// Result of a single item within a batch.
#[derive(Debug, Clone, Serialize)]
pub struct BatchItemResult {
    pub index: usize,
    pub snippet: Option<SnippetResponse>,
    pub version: i64,
}

// ─── Internal Row Types ─────────────────────────────────────────────────────────

#[derive(Debug, sqlx::FromRow)]
struct SnippetRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub created_by: Uuid,
    pub trigger: String,
    pub content: String,
    pub snippet_type: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, sqlx::FromRow)]
struct FolderRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub created_by: Uuid,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, sqlx::FromRow)]
struct DeltaRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub entity_type: String,
    pub entity_id: Uuid,
    pub operation: String,
    pub payload: serde_json::Value,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, sqlx::FromRow)]
struct SubscriptionRow {
    pub tier: String,
    pub status: String,
}

// ─── Service ────────────────────────────────────────────────────────────────────

/// Sync service handling snippet CRUD with workspace-scoped versioning and delta recording.
pub struct SyncService {
    pool: PgPool,
}

impl SyncService {
    /// Create a new SyncService instance.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Assign the next workspace-scoped version within a transaction.
    ///
    /// Uses an advisory lock on the workspace to atomically determine the next
    /// version number. Returns max + 1.
    async fn assign_next_version(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        workspace_id: Uuid,
    ) -> Result<i64, AppError> {
        // Use advisory lock based on workspace_id to serialize version assignment
        // pg_advisory_xact_lock takes a bigint, we use the first 8 bytes of the UUID
        let lock_key = i64::from_be_bytes(workspace_id.as_bytes()[..8].try_into().unwrap());
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(lock_key)
            .execute(&mut **tx)
            .await
            .map_err(|_| AppError::InternalError)?;

        let max_version: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(version), 0) FROM sync_deltas WHERE workspace_id = $1",
        )
        .bind(workspace_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        Ok(max_version + 1)
    }

    /// Verify that a user is a member of a workspace.
    async fn verify_membership(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        workspace_id: Uuid,
        user_id: Uuid,
    ) -> Result<(), AppError> {
        // Check workspace exists
        let workspace_exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workspaces WHERE id = $1)")
                .bind(workspace_id)
                .fetch_one(&mut **tx)
                .await
                .map_err(|_| AppError::InternalError)?;

        if !workspace_exists {
            return Err(AppError::WorkspaceNotFound);
        }

        // Check membership
        let is_member: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id = $1 AND user_id = $2)",
        )
        .bind(workspace_id)
        .bind(user_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        if !is_member {
            return Err(AppError::NotAWorkspaceMember);
        }

        Ok(())
    }

    /// Load the subscription for a workspace and check tier/status constraints.
    ///
    /// Returns the parsed tier and status. If subscription is cancelled/deactivated
    /// and content exceeds free-tier limits, returns ContentSoftLocked.
    async fn load_subscription(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        workspace_id: Uuid,
    ) -> Result<(Tier, SubscriptionStatus), AppError> {
        let row = sqlx::query_as::<_, SubscriptionRow>(
            "SELECT tier, status FROM subscriptions WHERE workspace_id = $1",
        )
        .bind(workspace_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        let row = row.ok_or(AppError::SubscriptionNotFound)?;

        let tier = match row.tier.as_str() {
            "free" => Tier::Free,
            "pro" => Tier::Pro,
            "teams" => Tier::Teams,
            _ => return Err(AppError::InternalError),
        };

        let status = match row.status.as_str() {
            "active" => SubscriptionStatus::Active,
            "past_due" => SubscriptionStatus::PastDue,
            "cancelled" => SubscriptionStatus::Cancelled,
            "pending_payment" => SubscriptionStatus::PendingPayment,
            "deactivated" => SubscriptionStatus::Deactivated,
            _ => return Err(AppError::InternalError),
        };

        Ok((tier, status))
    }

    /// Enforce free-tier snippet limits.
    ///
    /// Checks:
    /// - Snippet count limit (10 for free tier)
    /// - Content length limit (2000 chars for free tier)
    /// - Soft-lock: if subscription is cancelled/deactivated and would exceed free limits
    async fn enforce_snippet_limits(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        workspace_id: Uuid,
        tier: &Tier,
        status: &SubscriptionStatus,
        content: &str,
        is_create: bool,
    ) -> Result<(), AppError> {
        // Pro and Teams tiers have no limits
        if *tier == Tier::Pro || *tier == Tier::Teams {
            return Ok(());
        }

        // Check for soft-lock: subscription cancelled/deactivated means content is read-only
        // if it would exceed free limits
        let is_soft_locked =
            *status == SubscriptionStatus::Cancelled || *status == SubscriptionStatus::Deactivated;

        // Enforce content length limit
        if content.len() > FREE_TIER_MAX_CONTENT_CHARS {
            if is_soft_locked {
                return Err(AppError::ContentSoftLocked);
            }
            return Err(AppError::SnippetContentTooLong);
        }

        // Enforce snippet count limit only on creation
        if is_create {
            let snippet_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM snippets WHERE workspace_id = $1 AND deleted_at IS NULL",
            )
            .bind(workspace_id)
            .fetch_one(&mut **tx)
            .await
            .map_err(|_| AppError::InternalError)?;

            if snippet_count >= FREE_TIER_MAX_SNIPPETS {
                if is_soft_locked {
                    return Err(AppError::ContentSoftLocked);
                }
                return Err(AppError::SnippetLimitReached);
            }
        }

        Ok(())
    }

    /// Check trigger uniqueness within the workspace (among non-deleted snippets).
    async fn check_trigger_uniqueness(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        workspace_id: Uuid,
        trigger: &str,
        exclude_snippet_id: Option<Uuid>,
    ) -> Result<(), AppError> {
        let exists: bool = match exclude_snippet_id {
            Some(exclude_id) => {
                sqlx::query_scalar(
                    r#"
                    SELECT EXISTS(
                        SELECT 1 FROM snippets
                        WHERE workspace_id = $1
                          AND trigger = $2
                          AND deleted_at IS NULL
                          AND id != $3
                    )
                    "#,
                )
                .bind(workspace_id)
                .bind(trigger)
                .bind(exclude_id)
                .fetch_one(&mut **tx)
                .await
                .map_err(|_| AppError::InternalError)?
            }
            None => {
                sqlx::query_scalar(
                    r#"
                    SELECT EXISTS(
                        SELECT 1 FROM snippets
                        WHERE workspace_id = $1
                          AND trigger = $2
                          AND deleted_at IS NULL
                    )
                    "#,
                )
                .bind(workspace_id)
                .bind(trigger)
                .fetch_one(&mut **tx)
                .await
                .map_err(|_| AppError::InternalError)?
            }
        };

        if exists {
            return Err(AppError::TriggerAlreadyExists);
        }

        Ok(())
    }

    /// Record a delta in the sync_deltas table.
    async fn record_delta(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        workspace_id: Uuid,
        entity_type: &str,
        entity_id: Uuid,
        operation: &str,
        payload: &serde_json::Value,
        version: i64,
    ) -> Result<(), AppError> {
        sqlx::query(
            r#"
            INSERT INTO sync_deltas (workspace_id, entity_type, entity_id, operation, payload, version)
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(workspace_id)
        .bind(entity_type)
        .bind(entity_id)
        .bind(operation)
        .bind(payload)
        .bind(version)
        .execute(&mut **tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        Ok(())
    }

    /// Enforce free-tier folder limits.
    ///
    /// Checks:
    /// - Folder count limit (3 for free tier)
    /// - Soft-lock: if subscription is cancelled/deactivated and would exceed free limits
    async fn enforce_folder_limits(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        workspace_id: Uuid,
        tier: &Tier,
        status: &SubscriptionStatus,
        is_create: bool,
    ) -> Result<(), AppError> {
        // Pro and Teams tiers have no limits
        if *tier == Tier::Pro || *tier == Tier::Teams {
            return Ok(());
        }

        // Check for soft-lock: subscription cancelled/deactivated means content is read-only
        let is_soft_locked =
            *status == SubscriptionStatus::Cancelled || *status == SubscriptionStatus::Deactivated;

        // Enforce folder count limit only on creation
        if is_create {
            let folder_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM folders WHERE workspace_id = $1 AND deleted_at IS NULL",
            )
            .bind(workspace_id)
            .fetch_one(&mut **tx)
            .await
            .map_err(|_| AppError::InternalError)?;

            if folder_count >= FREE_TIER_MAX_FOLDERS {
                if is_soft_locked {
                    return Err(AppError::ContentSoftLocked);
                }
                return Err(AppError::FolderLimitReached);
            }
        }

        // If soft-locked, block all write operations
        if is_soft_locked {
            return Err(AppError::ContentSoftLocked);
        }

        Ok(())
    }

    /// Create a new snippet.
    ///
    /// Validates payload, enforces tier limits (free: 10 snippets, 2000 chars),
    /// enforces trigger uniqueness per workspace, assigns version, persists, and records delta.
    ///
    /// Requirements: 2.1, 2.4, 2.5, 2.32, 2.33, 5.21
    pub async fn create_snippet(
        &self,
        user_id: Uuid,
        payload: CreateSnippetPayload,
    ) -> Result<SnippetResponse, AppError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AppError::InternalError)?;

        // Verify membership
        Self::verify_membership(&mut tx, payload.workspace_id, user_id).await?;

        // Load subscription and check tier/status
        let (tier, status) = Self::load_subscription(&mut tx, payload.workspace_id).await?;

        // Enforce tier limits
        Self::enforce_snippet_limits(
            &mut tx,
            payload.workspace_id,
            &tier,
            &status,
            &payload.content,
            true,
        )
        .await?;

        // Check trigger uniqueness
        Self::check_trigger_uniqueness(&mut tx, payload.workspace_id, &payload.trigger, None)
            .await?;

        // Assign next version
        let version = Self::assign_next_version(&mut tx, payload.workspace_id).await?;

        // Persist snippet
        let row = sqlx::query_as::<_, SnippetRow>(
            r#"
            INSERT INTO snippets (workspace_id, created_by, trigger, content, snippet_type, version)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id, workspace_id, created_by, trigger, content, snippet_type, version, created_at, updated_at, deleted_at
            "#,
        )
        .bind(payload.workspace_id)
        .bind(user_id)
        .bind(&payload.trigger)
        .bind(&payload.content)
        .bind(&payload.snippet_type)
        .bind(version)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Build delta payload
        let delta_payload = serde_json::json!({
            "id": row.id,
            "workspace_id": row.workspace_id,
            "created_by": row.created_by,
            "trigger": row.trigger,
            "content": row.content,
            "snippet_type": row.snippet_type,
            "version": row.version,
            "created_at": row.created_at,
            "updated_at": row.updated_at,
        });

        // Record delta
        Self::record_delta(
            &mut tx,
            payload.workspace_id,
            "snippet",
            row.id,
            "create",
            &delta_payload,
            version,
        )
        .await?;

        // If folder_id is provided, link snippet to folder
        if let Some(folder_id) = payload.folder_id {
            sqlx::query(
                "INSERT INTO snippet_folders (snippet_id, folder_id) VALUES ($1, $2)",
            )
            .bind(row.id)
            .bind(folder_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::InternalError)?;
        }

        tx.commit().await.map_err(|_| AppError::InternalError)?;

        Ok(SnippetResponse {
            id: row.id,
            workspace_id: row.workspace_id,
            created_by: row.created_by,
            trigger: row.trigger,
            content: row.content,
            snippet_type: row.snippet_type,
            version: row.version,
            created_at: row.created_at,
            updated_at: row.updated_at,
            deleted_at: row.deleted_at,
        })
    }

    /// Update an existing snippet.
    ///
    /// Validates payload, assigns version, updates snippet, and records delta.
    ///
    /// Requirements: 2.4, 2.6, 2.32, 2.33, 2.38, 2.39
    pub async fn update_snippet(
        &self,
        user_id: Uuid,
        snippet_id: Uuid,
        payload: UpdateSnippetPayload,
    ) -> Result<SnippetResponse, AppError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AppError::InternalError)?;

        // Fetch existing snippet
        let existing = sqlx::query_as::<_, SnippetRow>(
            r#"
            SELECT id, workspace_id, created_by, trigger, content, snippet_type, version, created_at, updated_at, deleted_at
            FROM snippets
            WHERE id = $1 AND deleted_at IS NULL
            "#,
        )
        .bind(snippet_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        let existing = existing.ok_or(AppError::WorkspaceNotFound)?;

        // Verify membership
        Self::verify_membership(&mut tx, existing.workspace_id, user_id).await?;

        // Load subscription for tier/status checks
        let (tier, status) = Self::load_subscription(&mut tx, existing.workspace_id).await?;

        // Determine new content for limit checks
        let new_content = payload.content.as_deref().unwrap_or(&existing.content);

        // Enforce tier limits (not a create, so no snippet count check)
        Self::enforce_snippet_limits(
            &mut tx,
            existing.workspace_id,
            &tier,
            &status,
            new_content,
            false,
        )
        .await?;

        // Check trigger uniqueness if trigger is being changed
        if let Some(ref new_trigger) = payload.trigger {
            Self::check_trigger_uniqueness(
                &mut tx,
                existing.workspace_id,
                new_trigger,
                Some(snippet_id),
            )
            .await?;
        }

        // Assign next version
        let version = Self::assign_next_version(&mut tx, existing.workspace_id).await?;

        // Apply updates
        let final_trigger = payload.trigger.as_deref().unwrap_or(&existing.trigger);
        let final_content = payload.content.as_deref().unwrap_or(&existing.content);
        let final_snippet_type = payload
            .snippet_type
            .as_deref()
            .unwrap_or(&existing.snippet_type);

        let row = sqlx::query_as::<_, SnippetRow>(
            r#"
            UPDATE snippets
            SET trigger = $1, content = $2, snippet_type = $3, version = $4, updated_at = now()
            WHERE id = $5
            RETURNING id, workspace_id, created_by, trigger, content, snippet_type, version, created_at, updated_at, deleted_at
            "#,
        )
        .bind(final_trigger)
        .bind(final_content)
        .bind(final_snippet_type)
        .bind(version)
        .bind(snippet_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Build delta payload
        let delta_payload = serde_json::json!({
            "id": row.id,
            "workspace_id": row.workspace_id,
            "created_by": row.created_by,
            "trigger": row.trigger,
            "content": row.content,
            "snippet_type": row.snippet_type,
            "version": row.version,
            "created_at": row.created_at,
            "updated_at": row.updated_at,
        });

        // Record delta
        Self::record_delta(
            &mut tx,
            existing.workspace_id,
            "snippet",
            snippet_id,
            "update",
            &delta_payload,
            version,
        )
        .await?;

        tx.commit().await.map_err(|_| AppError::InternalError)?;

        Ok(SnippetResponse {
            id: row.id,
            workspace_id: row.workspace_id,
            created_by: row.created_by,
            trigger: row.trigger,
            content: row.content,
            snippet_type: row.snippet_type,
            version: row.version,
            created_at: row.created_at,
            updated_at: row.updated_at,
            deleted_at: row.deleted_at,
        })
    }

    /// Soft-delete a snippet.
    ///
    /// Sets deleted_at, assigns version, and records a delete delta.
    ///
    /// Requirements: 2.4, 2.7, 2.38, 2.39, 2.40
    pub async fn delete_snippet(
        &self,
        user_id: Uuid,
        snippet_id: Uuid,
    ) -> Result<(), AppError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AppError::InternalError)?;

        // Fetch existing snippet
        let existing = sqlx::query_as::<_, SnippetRow>(
            r#"
            SELECT id, workspace_id, created_by, trigger, content, snippet_type, version, created_at, updated_at, deleted_at
            FROM snippets
            WHERE id = $1 AND deleted_at IS NULL
            "#,
        )
        .bind(snippet_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        let existing = existing.ok_or(AppError::WorkspaceNotFound)?;

        // Verify membership
        Self::verify_membership(&mut tx, existing.workspace_id, user_id).await?;

        // Assign next version
        let version = Self::assign_next_version(&mut tx, existing.workspace_id).await?;

        // Set deleted_at
        sqlx::query(
            "UPDATE snippets SET deleted_at = now(), version = $1, updated_at = now() WHERE id = $2",
        )
        .bind(version)
        .bind(snippet_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Build delta payload
        let delta_payload = serde_json::json!({
            "id": existing.id,
            "workspace_id": existing.workspace_id,
            "trigger": existing.trigger,
            "deleted_at": Utc::now(),
        });

        // Record delta
        Self::record_delta(
            &mut tx,
            existing.workspace_id,
            "snippet",
            snippet_id,
            "delete",
            &delta_payload,
            version,
        )
        .await?;

        tx.commit().await.map_err(|_| AppError::InternalError)?;

        Ok(())
    }

    // ─── Folder CRUD ────────────────────────────────────────────────────────────

    /// Create a new folder.
    ///
    /// Validates payload, enforces tier limits (free: 3 folders), assigns version,
    /// persists the folder, and records a delta.
    ///
    /// Requirements: 2.2, 2.8, 5.21
    pub async fn create_folder(
        &self,
        user_id: Uuid,
        payload: CreateFolderPayload,
    ) -> Result<FolderResponse, AppError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AppError::InternalError)?;

        // Verify membership
        Self::verify_membership(&mut tx, payload.workspace_id, user_id).await?;

        // Load subscription and check tier/status
        let (tier, status) = Self::load_subscription(&mut tx, payload.workspace_id).await?;

        // Enforce folder limits
        Self::enforce_folder_limits(&mut tx, payload.workspace_id, &tier, &status, true).await?;

        // Assign next version
        let version = Self::assign_next_version(&mut tx, payload.workspace_id).await?;

        // Persist folder
        let row = sqlx::query_as::<_, FolderRow>(
            r#"
            INSERT INTO folders (workspace_id, name, created_by, version)
            VALUES ($1, $2, $3, $4)
            RETURNING id, workspace_id, name, created_by, version, created_at, updated_at, deleted_at
            "#,
        )
        .bind(payload.workspace_id)
        .bind(&payload.name)
        .bind(user_id)
        .bind(version)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Build delta payload
        let delta_payload = serde_json::json!({
            "id": row.id,
            "workspace_id": row.workspace_id,
            "name": row.name,
            "created_by": row.created_by,
            "version": row.version,
            "created_at": row.created_at,
            "updated_at": row.updated_at,
        });

        // Record delta
        Self::record_delta(
            &mut tx,
            payload.workspace_id,
            "folder",
            row.id,
            "create",
            &delta_payload,
            version,
        )
        .await?;

        tx.commit().await.map_err(|_| AppError::InternalError)?;

        Ok(FolderResponse {
            id: row.id,
            workspace_id: row.workspace_id,
            name: row.name,
            created_by: row.created_by,
            version: row.version,
            created_at: row.created_at,
            updated_at: row.updated_at,
            deleted_at: row.deleted_at,
        })
    }

    /// Update an existing folder.
    ///
    /// Validates payload, assigns version, updates folder, and records delta.
    ///
    /// Requirements: 2.4, 2.9, 2.38, 2.39
    pub async fn update_folder(
        &self,
        user_id: Uuid,
        folder_id: Uuid,
        payload: UpdateFolderPayload,
    ) -> Result<FolderResponse, AppError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AppError::InternalError)?;

        // Fetch existing folder
        let existing = sqlx::query_as::<_, FolderRow>(
            r#"
            SELECT id, workspace_id, name, created_by, version, created_at, updated_at, deleted_at
            FROM folders
            WHERE id = $1 AND deleted_at IS NULL
            "#,
        )
        .bind(folder_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        let existing = existing.ok_or(AppError::WorkspaceNotFound)?;

        // Verify membership
        Self::verify_membership(&mut tx, existing.workspace_id, user_id).await?;

        // Load subscription for soft-lock check
        let (tier, status) = Self::load_subscription(&mut tx, existing.workspace_id).await?;

        // Enforce folder limits (not a create, so no count check)
        Self::enforce_folder_limits(&mut tx, existing.workspace_id, &tier, &status, false).await?;

        // Assign next version
        let version = Self::assign_next_version(&mut tx, existing.workspace_id).await?;

        // Apply updates
        let final_name = payload.name.as_deref().unwrap_or(&existing.name);

        let row = sqlx::query_as::<_, FolderRow>(
            r#"
            UPDATE folders
            SET name = $1, version = $2, updated_at = now()
            WHERE id = $3
            RETURNING id, workspace_id, name, created_by, version, created_at, updated_at, deleted_at
            "#,
        )
        .bind(final_name)
        .bind(version)
        .bind(folder_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Build delta payload
        let delta_payload = serde_json::json!({
            "id": row.id,
            "workspace_id": row.workspace_id,
            "name": row.name,
            "created_by": row.created_by,
            "version": row.version,
            "created_at": row.created_at,
            "updated_at": row.updated_at,
        });

        // Record delta
        Self::record_delta(
            &mut tx,
            existing.workspace_id,
            "folder",
            folder_id,
            "update",
            &delta_payload,
            version,
        )
        .await?;

        tx.commit().await.map_err(|_| AppError::InternalError)?;

        Ok(FolderResponse {
            id: row.id,
            workspace_id: row.workspace_id,
            name: row.name,
            created_by: row.created_by,
            version: row.version,
            created_at: row.created_at,
            updated_at: row.updated_at,
            deleted_at: row.deleted_at,
        })
    }

    /// Soft-delete a folder.
    ///
    /// Sets deleted_at, assigns version, and records a delete delta.
    ///
    /// Requirements: 2.4, 2.10, 2.38, 2.39, 2.43
    pub async fn delete_folder(
        &self,
        user_id: Uuid,
        folder_id: Uuid,
    ) -> Result<(), AppError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AppError::InternalError)?;

        // Fetch existing folder
        let existing = sqlx::query_as::<_, FolderRow>(
            r#"
            SELECT id, workspace_id, name, created_by, version, created_at, updated_at, deleted_at
            FROM folders
            WHERE id = $1 AND deleted_at IS NULL
            "#,
        )
        .bind(folder_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        let existing = existing.ok_or(AppError::WorkspaceNotFound)?;

        // Verify membership
        Self::verify_membership(&mut tx, existing.workspace_id, user_id).await?;

        // Assign next version
        let version = Self::assign_next_version(&mut tx, existing.workspace_id).await?;

        // Set deleted_at
        sqlx::query(
            "UPDATE folders SET deleted_at = now(), version = $1, updated_at = now() WHERE id = $2",
        )
        .bind(version)
        .bind(folder_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Build delta payload
        let delta_payload = serde_json::json!({
            "id": existing.id,
            "workspace_id": existing.workspace_id,
            "name": existing.name,
            "deleted_at": Utc::now(),
        });

        // Record delta
        Self::record_delta(
            &mut tx,
            existing.workspace_id,
            "folder",
            folder_id,
            "delete",
            &delta_payload,
            version,
        )
        .await?;

        tx.commit().await.map_err(|_| AppError::InternalError)?;

        Ok(())
    }

    // ─── Batch Operations ───────────────────────────────────────────────────────

    /// Execute batch snippet operations transactionally.
    ///
    /// Processes up to 100 snippet operations (create, update, delete) within a single
    /// transaction. Each operation is assigned a sequential workspace version.
    /// If any operation fails validation, the entire batch is rejected.
    ///
    /// Requirements: 2.34, 2.35, 2.36, 2.37
    pub async fn batch_operations(
        &self,
        user_id: Uuid,
        payload: BatchOperationsPayload,
    ) -> Result<BatchOperationsResponse, AppError> {
        // Check batch size limit
        if payload.operations.len() > MAX_BATCH_SIZE {
            return Err(AppError::BatchSizeExceeded);
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AppError::InternalError)?;

        // Verify membership once
        Self::verify_membership(&mut tx, payload.workspace_id, user_id).await?;

        // Load subscription once
        let (tier, status) = Self::load_subscription(&mut tx, payload.workspace_id).await?;

        let mut results: Vec<BatchItemResult> = Vec::with_capacity(payload.operations.len());
        let mut last_version: i64 = 0;

        for (index, operation) in payload.operations.iter().enumerate() {
            match operation {
                BatchOperation::CreateSnippet(create_payload) => {
                    // Enforce snippet limits
                    Self::enforce_snippet_limits(
                        &mut tx,
                        payload.workspace_id,
                        &tier,
                        &status,
                        &create_payload.content,
                        true,
                    )
                    .await?;

                    // Check trigger uniqueness
                    Self::check_trigger_uniqueness(
                        &mut tx,
                        payload.workspace_id,
                        &create_payload.trigger,
                        None,
                    )
                    .await?;

                    // Assign next version
                    let version =
                        Self::assign_next_version(&mut tx, payload.workspace_id).await?;

                    // Persist snippet
                    let row = sqlx::query_as::<_, SnippetRow>(
                        r#"
                        INSERT INTO snippets (workspace_id, created_by, trigger, content, snippet_type, version)
                        VALUES ($1, $2, $3, $4, $5, $6)
                        RETURNING id, workspace_id, created_by, trigger, content, snippet_type, version, created_at, updated_at, deleted_at
                        "#,
                    )
                    .bind(payload.workspace_id)
                    .bind(user_id)
                    .bind(&create_payload.trigger)
                    .bind(&create_payload.content)
                    .bind(&create_payload.snippet_type)
                    .bind(version)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|_| AppError::InternalError)?;

                    // Record delta
                    let delta_payload = serde_json::json!({
                        "id": row.id,
                        "workspace_id": row.workspace_id,
                        "created_by": row.created_by,
                        "trigger": row.trigger,
                        "content": row.content,
                        "snippet_type": row.snippet_type,
                        "version": row.version,
                        "created_at": row.created_at,
                        "updated_at": row.updated_at,
                    });

                    Self::record_delta(
                        &mut tx,
                        payload.workspace_id,
                        "snippet",
                        row.id,
                        "create",
                        &delta_payload,
                        version,
                    )
                    .await?;

                    // Link to folder if provided
                    if let Some(folder_id) = create_payload.folder_id {
                        sqlx::query(
                            "INSERT INTO snippet_folders (snippet_id, folder_id) VALUES ($1, $2)",
                        )
                        .bind(row.id)
                        .bind(folder_id)
                        .execute(&mut *tx)
                        .await
                        .map_err(|_| AppError::InternalError)?;
                    }

                    last_version = version;
                    results.push(BatchItemResult {
                        index,
                        snippet: Some(SnippetResponse {
                            id: row.id,
                            workspace_id: row.workspace_id,
                            created_by: row.created_by,
                            trigger: row.trigger,
                            content: row.content,
                            snippet_type: row.snippet_type,
                            version: row.version,
                            created_at: row.created_at,
                            updated_at: row.updated_at,
                            deleted_at: row.deleted_at,
                        }),
                        version,
                    });
                }
                BatchOperation::UpdateSnippet { id, payload: update_payload } => {
                    // Fetch existing snippet
                    let existing = sqlx::query_as::<_, SnippetRow>(
                        r#"
                        SELECT id, workspace_id, created_by, trigger, content, snippet_type, version, created_at, updated_at, deleted_at
                        FROM snippets
                        WHERE id = $1 AND deleted_at IS NULL AND workspace_id = $2
                        "#,
                    )
                    .bind(id)
                    .bind(payload.workspace_id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|_| AppError::InternalError)?;

                    let existing = existing.ok_or(AppError::WorkspaceNotFound)?;

                    // Determine new content for limit checks
                    let new_content =
                        update_payload.content.as_deref().unwrap_or(&existing.content);

                    // Enforce tier limits
                    Self::enforce_snippet_limits(
                        &mut tx,
                        payload.workspace_id,
                        &tier,
                        &status,
                        new_content,
                        false,
                    )
                    .await?;

                    // Check trigger uniqueness if trigger is being changed
                    if let Some(ref new_trigger) = update_payload.trigger {
                        Self::check_trigger_uniqueness(
                            &mut tx,
                            payload.workspace_id,
                            new_trigger,
                            Some(*id),
                        )
                        .await?;
                    }

                    // Assign next version
                    let version =
                        Self::assign_next_version(&mut tx, payload.workspace_id).await?;

                    // Apply updates
                    let final_trigger =
                        update_payload.trigger.as_deref().unwrap_or(&existing.trigger);
                    let final_content =
                        update_payload.content.as_deref().unwrap_or(&existing.content);
                    let final_snippet_type = update_payload
                        .snippet_type
                        .as_deref()
                        .unwrap_or(&existing.snippet_type);

                    let row = sqlx::query_as::<_, SnippetRow>(
                        r#"
                        UPDATE snippets
                        SET trigger = $1, content = $2, snippet_type = $3, version = $4, updated_at = now()
                        WHERE id = $5
                        RETURNING id, workspace_id, created_by, trigger, content, snippet_type, version, created_at, updated_at, deleted_at
                        "#,
                    )
                    .bind(final_trigger)
                    .bind(final_content)
                    .bind(final_snippet_type)
                    .bind(version)
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|_| AppError::InternalError)?;

                    // Record delta
                    let delta_payload = serde_json::json!({
                        "id": row.id,
                        "workspace_id": row.workspace_id,
                        "created_by": row.created_by,
                        "trigger": row.trigger,
                        "content": row.content,
                        "snippet_type": row.snippet_type,
                        "version": row.version,
                        "created_at": row.created_at,
                        "updated_at": row.updated_at,
                    });

                    Self::record_delta(
                        &mut tx,
                        payload.workspace_id,
                        "snippet",
                        *id,
                        "update",
                        &delta_payload,
                        version,
                    )
                    .await?;

                    last_version = version;
                    results.push(BatchItemResult {
                        index,
                        snippet: Some(SnippetResponse {
                            id: row.id,
                            workspace_id: row.workspace_id,
                            created_by: row.created_by,
                            trigger: row.trigger,
                            content: row.content,
                            snippet_type: row.snippet_type,
                            version: row.version,
                            created_at: row.created_at,
                            updated_at: row.updated_at,
                            deleted_at: row.deleted_at,
                        }),
                        version,
                    });
                }
                BatchOperation::DeleteSnippet { id } => {
                    // Fetch existing snippet
                    let existing = sqlx::query_as::<_, SnippetRow>(
                        r#"
                        SELECT id, workspace_id, created_by, trigger, content, snippet_type, version, created_at, updated_at, deleted_at
                        FROM snippets
                        WHERE id = $1 AND deleted_at IS NULL AND workspace_id = $2
                        "#,
                    )
                    .bind(id)
                    .bind(payload.workspace_id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|_| AppError::InternalError)?;

                    let existing = existing.ok_or(AppError::WorkspaceNotFound)?;

                    // Assign next version
                    let version =
                        Self::assign_next_version(&mut tx, payload.workspace_id).await?;

                    // Set deleted_at
                    sqlx::query(
                        "UPDATE snippets SET deleted_at = now(), version = $1, updated_at = now() WHERE id = $2",
                    )
                    .bind(version)
                    .bind(id)
                    .execute(&mut *tx)
                    .await
                    .map_err(|_| AppError::InternalError)?;

                    // Record delta
                    let delta_payload = serde_json::json!({
                        "id": existing.id,
                        "workspace_id": existing.workspace_id,
                        "trigger": existing.trigger,
                        "deleted_at": Utc::now(),
                    });

                    Self::record_delta(
                        &mut tx,
                        payload.workspace_id,
                        "snippet",
                        *id,
                        "delete",
                        &delta_payload,
                        version,
                    )
                    .await?;

                    last_version = version;
                    results.push(BatchItemResult {
                        index,
                        snippet: None,
                        version,
                    });
                }
            }
        }

        tx.commit().await.map_err(|_| AppError::InternalError)?;

        Ok(BatchOperationsResponse {
            results,
            workspace_version: last_version,
        })
    }

    // ─── Snapshot & Delta Polling ───────────────────────────────────────────────

    /// Verify workspace membership using the pool directly (for read-only operations).
    async fn verify_membership_readonly(
        &self,
        workspace_id: Uuid,
        user_id: Uuid,
    ) -> Result<(), AppError> {
        // Check workspace exists
        let workspace_exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workspaces WHERE id = $1)")
                .bind(workspace_id)
                .fetch_one(&self.pool)
                .await
                .map_err(|_| AppError::InternalError)?;

        if !workspace_exists {
            return Err(AppError::WorkspaceNotFound);
        }

        // Check membership
        let is_member: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id = $1 AND user_id = $2)",
        )
        .bind(workspace_id)
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        if !is_member {
            return Err(AppError::NotAWorkspaceMember);
        }

        Ok(())
    }

    /// Return a full snapshot of the workspace: all active folders, all active snippets,
    /// and the latest workspace version.
    ///
    /// Requirements: 2.11, 2.12
    pub async fn get_snapshot(
        &self,
        user_id: Uuid,
        workspace_id: Uuid,
    ) -> Result<SnapshotResponse, AppError> {
        // Verify membership
        self.verify_membership_readonly(workspace_id, user_id).await?;

        // Get latest workspace version
        let version: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(version), 0) FROM sync_deltas WHERE workspace_id = $1",
        )
        .bind(workspace_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Fetch all active snippets
        let snippet_rows = sqlx::query_as::<_, SnippetRow>(
            r#"
            SELECT id, workspace_id, created_by, trigger, content, snippet_type, version, created_at, updated_at, deleted_at
            FROM snippets
            WHERE workspace_id = $1 AND deleted_at IS NULL
            ORDER BY created_at ASC
            "#,
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Fetch all active folders
        let folder_rows = sqlx::query_as::<_, FolderRow>(
            r#"
            SELECT id, workspace_id, name, created_by, version, created_at, updated_at, deleted_at
            FROM folders
            WHERE workspace_id = $1 AND deleted_at IS NULL
            ORDER BY created_at ASC
            "#,
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Map to response DTOs
        let snippets: Vec<SnippetResponse> = snippet_rows
            .into_iter()
            .map(|r| SnippetResponse {
                id: r.id,
                workspace_id: r.workspace_id,
                created_by: r.created_by,
                trigger: r.trigger,
                content: r.content,
                snippet_type: r.snippet_type,
                version: r.version,
                created_at: r.created_at,
                updated_at: r.updated_at,
                deleted_at: r.deleted_at,
            })
            .collect();

        let folders: Vec<FolderResponse> = folder_rows
            .into_iter()
            .map(|r| FolderResponse {
                id: r.id,
                workspace_id: r.workspace_id,
                name: r.name,
                created_by: r.created_by,
                version: r.version,
                created_at: r.created_at,
                updated_at: r.updated_at,
                deleted_at: r.deleted_at,
            })
            .collect();

        Ok(SnapshotResponse {
            workspace_id,
            version,
            snippets,
            folders,
        })
    }

    /// Return deltas for a workspace since a given version, with pagination.
    ///
    /// - Validates `since_version` is non-negative (422 INVALID_SINCE_VERSION if not)
    /// - Checks 30-day retention: if the requested since_version is older than retained
    ///   deltas, returns 409 SNAPSHOT_REQUIRED
    /// - Returns up to `limit` deltas (default 500, max 1000), ordered by version ASC
    /// - Includes `has_more` and `next_since_version` for pagination
    ///
    /// Requirements: 2.13, 2.14, 2.15, 2.16
    pub async fn get_deltas(
        &self,
        user_id: Uuid,
        workspace_id: Uuid,
        since_version: i64,
        limit: Option<i64>,
    ) -> Result<DeltasResponse, AppError> {
        // Validate since_version is non-negative
        if since_version < 0 {
            return Err(AppError::InvalidSinceVersion);
        }

        // Verify membership
        self.verify_membership_readonly(workspace_id, user_id).await?;

        // Determine effective limit (default 500, max 1000)
        let effective_limit = limit
            .unwrap_or(DELTA_DEFAULT_LIMIT)
            .clamp(1, DELTA_MAX_LIMIT);

        // Check 30-day retention window:
        // If there are deltas for this workspace but the minimum version is greater than
        // since_version, and the oldest delta is older than 30 days, then the client needs
        // a full snapshot.
        if since_version > 0 {
            let retention_check: Option<(Option<i64>, Option<DateTime<Utc>>)> = sqlx::query_as(
                r#"
                SELECT MIN(version), MIN(created_at)
                FROM sync_deltas
                WHERE workspace_id = $1
                "#,
            )
            .bind(workspace_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| AppError::InternalError)?;

            if let Some((Some(min_version), Some(oldest_created_at))) = retention_check {
                let retention_cutoff = Utc::now() - Duration::days(DELTA_RETENTION_DAYS);
                // If the requested since_version is below what we still have,
                // and the oldest record is beyond the retention window, require snapshot
                if since_version < min_version && oldest_created_at < retention_cutoff {
                    return Err(AppError::SnapshotRequired);
                }
            }
        }

        // Fetch deltas: version > since_version, ordered ascending, limit + 1 to detect has_more
        let rows = sqlx::query_as::<_, DeltaRow>(
            r#"
            SELECT id, workspace_id, entity_type, entity_id, operation, payload, version, created_at
            FROM sync_deltas
            WHERE workspace_id = $1 AND version > $2
            ORDER BY version ASC
            LIMIT $3
            "#,
        )
        .bind(workspace_id)
        .bind(since_version)
        .bind(effective_limit + 1) // fetch one extra to detect has_more
        .fetch_all(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Determine if there are more deltas beyond our limit
        let has_more = rows.len() as i64 > effective_limit;

        // Take only up to effective_limit items
        let deltas: Vec<DeltaResponse> = rows
            .into_iter()
            .take(effective_limit as usize)
            .map(|r| DeltaResponse {
                id: r.id,
                workspace_id: r.workspace_id,
                entity_type: r.entity_type,
                entity_id: r.entity_id,
                operation: r.operation,
                payload: r.payload,
                version: r.version,
                created_at: r.created_at,
            })
            .collect();

        // Compute next_since_version: the version of the last returned delta,
        // or the original since_version if no deltas were returned
        let next_since_version = deltas
            .last()
            .map(|d| d.version)
            .unwrap_or(since_version);

        Ok(DeltasResponse {
            deltas,
            has_more,
            next_since_version,
        })
    }
}
