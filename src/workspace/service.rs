use chrono::{DateTime, Duration, Utc};
use rand::Rng;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::AppError;

// ─── Response DTOs ──────────────────────────────────────────────────────────────

/// Response payload for workspace operations.
#[derive(Debug, Serialize)]
pub struct WorkspaceResponse {
    pub id: Uuid,
    #[serde(rename = "type")]
    pub workspace_type: String,
    pub owner_id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

/// Response payload for workspace member listing.
#[derive(Debug, Serialize)]
pub struct WorkspaceMemberResponse {
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub role: String,
    pub joined_at: DateTime<Utc>,
}

// ─── Internal Row Types ─────────────────────────────────────────────────────────

#[derive(Debug, sqlx::FromRow)]
struct WorkspaceRow {
    pub id: Uuid,
    #[sqlx(rename = "type")]
    pub workspace_type: String,
    pub owner_id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, sqlx::FromRow)]
struct WorkspaceMemberRow {
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub role: String,
    pub joined_at: DateTime<Utc>,
}

// ─── Service ────────────────────────────────────────────────────────────────────

/// Workspace service handling workspace creation and membership operations.
pub struct WorkspaceService {
    pool: PgPool,
}

impl WorkspaceService {
    /// Create a new WorkspaceService instance.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create an individual workspace for a user.
    ///
    /// Creates a workspace with type=individual, adds the user as owner member,
    /// and creates a free subscription with status=active.
    ///
    /// This is called during user registration (Requirement 5.1, 5.2, 5.3).
    pub async fn create_individual_workspace(
        &self,
        owner_id: Uuid,
        name: &str,
    ) -> Result<WorkspaceResponse, AppError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AppError::InternalError)?;

        // Create workspace with type=individual
        let row = sqlx::query_as::<_, WorkspaceRow>(
            r#"
            INSERT INTO workspaces (type, owner_id, name)
            VALUES ('individual', $1, $2)
            RETURNING id, type, owner_id, name, created_at
            "#,
        )
        .bind(owner_id)
        .bind(name)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Add owner membership
        sqlx::query(
            r#"
            INSERT INTO workspace_members (workspace_id, user_id, role)
            VALUES ($1, $2, 'owner')
            "#,
        )
        .bind(row.id)
        .bind(owner_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Create free subscription with status=active
        sqlx::query(
            r#"
            INSERT INTO subscriptions (workspace_id, tier, status)
            VALUES ($1, 'free', 'active')
            "#,
        )
        .bind(row.id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        tx.commit().await.map_err(|_| AppError::InternalError)?;

        Ok(WorkspaceResponse {
            id: row.id,
            workspace_type: row.workspace_type,
            owner_id: row.owner_id,
            name: row.name,
            created_at: row.created_at,
        })
    }

    /// Create a team workspace.
    ///
    /// Creates a workspace with type=team, adds the user as owner member,
    /// and creates a teams subscription with status=pending_payment and
    /// a 7-day payment_deadline.
    ///
    /// (Requirements 5.4, 5.9, 5.10)
    pub async fn create_team_workspace(
        &self,
        owner_id: Uuid,
        name: &str,
    ) -> Result<WorkspaceResponse, AppError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AppError::InternalError)?;

        // Create workspace with type=team
        let row = sqlx::query_as::<_, WorkspaceRow>(
            r#"
            INSERT INTO workspaces (type, owner_id, name)
            VALUES ('team', $1, $2)
            RETURNING id, type, owner_id, name, created_at
            "#,
        )
        .bind(owner_id)
        .bind(name)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Add owner membership
        sqlx::query(
            r#"
            INSERT INTO workspace_members (workspace_id, user_id, role)
            VALUES ($1, $2, 'owner')
            "#,
        )
        .bind(row.id)
        .bind(owner_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Create teams subscription with status=pending_payment and 7-day payment_deadline
        let payment_deadline = Utc::now() + Duration::days(7);

        sqlx::query(
            r#"
            INSERT INTO subscriptions (workspace_id, tier, status, payment_deadline)
            VALUES ($1, 'teams', 'pending_payment', $2)
            "#,
        )
        .bind(row.id)
        .bind(payment_deadline)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        tx.commit().await.map_err(|_| AppError::InternalError)?;

        Ok(WorkspaceResponse {
            id: row.id,
            workspace_type: row.workspace_type,
            owner_id: row.owner_id,
            name: row.name,
            created_at: row.created_at,
        })
    }

    /// Get a workspace by ID.
    ///
    /// Returns WorkspaceNotFound if the workspace does not exist.
    pub async fn get_workspace(
        &self,
        workspace_id: Uuid,
    ) -> Result<WorkspaceResponse, AppError> {
        let row = sqlx::query_as::<_, WorkspaceRow>(
            r#"
            SELECT id, type, owner_id, name, created_at
            FROM workspaces
            WHERE id = $1
            "#,
        )
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        let row = row.ok_or(AppError::WorkspaceNotFound)?;

        Ok(WorkspaceResponse {
            id: row.id,
            workspace_type: row.workspace_type,
            owner_id: row.owner_id,
            name: row.name,
            created_at: row.created_at,
        })
    }

    /// List all workspaces a user is a member of.
    ///
    /// Returns workspaces where the user has a membership entry (owner or member).
    pub async fn list_user_workspaces(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<WorkspaceResponse>, AppError> {
        let rows = sqlx::query_as::<_, WorkspaceRow>(
            r#"
            SELECT w.id, w.type, w.owner_id, w.name, w.created_at
            FROM workspaces w
            INNER JOIN workspace_members wm ON wm.workspace_id = w.id
            WHERE wm.user_id = $1
            ORDER BY w.created_at ASC
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        let workspaces = rows
            .into_iter()
            .map(|row| WorkspaceResponse {
                id: row.id,
                workspace_type: row.workspace_type,
                owner_id: row.owner_id,
                name: row.name,
                created_at: row.created_at,
            })
            .collect();

        Ok(workspaces)
    }

    /// Verify that a user is a member of a workspace.
    ///
    /// Returns Ok(()) if the user is a member, or NotAWorkspaceMember (403) if not.
    /// Also returns WorkspaceNotFound if the workspace does not exist.
    ///
    /// (Requirement 5.12)
    pub async fn verify_membership(
        &self,
        workspace_id: Uuid,
        user_id: Uuid,
    ) -> Result<(), AppError> {
        // First check workspace exists
        let workspace_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM workspaces WHERE id = $1)",
        )
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
}

// ─── Team Invite Response DTO ───────────────────────────────────────────────────

/// Response payload for team invite operations.
#[derive(Debug, Serialize)]
pub struct TeamInviteResponse {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub invite_code: String,
    pub max_uses: i32,
    pub times_used: i32,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

// ─── Internal Row Type for Team Invites ─────────────────────────────────────────

#[derive(Debug, sqlx::FromRow)]
struct TeamInviteRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub invite_code: String,
    pub max_uses: i32,
    pub times_used: i32,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

// ─── Team Service ───────────────────────────────────────────────────────────────

/// Maximum number of members allowed in a team workspace (including owner).
const MAX_TEAM_SEATS: i64 = 3;

/// Team service handling invite creation, join-via-invite, member removal, and listing.
pub struct TeamService {
    pool: PgPool,
}

impl TeamService {
    /// Create a new TeamService instance.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Generate a random URL-safe alphanumeric invite code (8 characters).
    fn generate_invite_code() -> String {
        const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        let mut rng = rand::thread_rng();
        (0..8)
            .map(|_| {
                let idx = rng.gen_range(0..CHARSET.len());
                CHARSET[idx] as char
            })
            .collect()
    }

    /// Create a team invite with a unique invite_code.
    ///
    /// Generates a unique code, stores it in team_invites with the specified
    /// max_uses (default 1) and expires_at (default 7 days from now).
    ///
    /// (Requirements 5.13, 5.14)
    pub async fn create_invite(
        &self,
        workspace_id: Uuid,
        created_by: Uuid,
        max_uses: Option<i32>,
        expires_in_days: Option<i64>,
    ) -> Result<TeamInviteResponse, AppError> {
        // Verify workspace exists
        let workspace_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM workspaces WHERE id = $1)",
        )
        .bind(workspace_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        if !workspace_exists {
            return Err(AppError::WorkspaceNotFound);
        }

        let invite_code = Self::generate_invite_code();
        let effective_max_uses = max_uses.unwrap_or(1);
        let effective_expires_in_days = expires_in_days.unwrap_or(7);
        let expires_at = Utc::now() + Duration::days(effective_expires_in_days);

        let row = sqlx::query_as::<_, TeamInviteRow>(
            r#"
            INSERT INTO team_invites (workspace_id, invite_code, max_uses, expires_at, created_by)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING id, workspace_id, invite_code, max_uses, times_used, expires_at, created_at
            "#,
        )
        .bind(workspace_id)
        .bind(&invite_code)
        .bind(effective_max_uses)
        .bind(expires_at)
        .bind(created_by)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        Ok(TeamInviteResponse {
            id: row.id,
            workspace_id: row.workspace_id,
            invite_code: row.invite_code,
            max_uses: row.max_uses,
            times_used: row.times_used,
            expires_at: row.expires_at,
            created_at: row.created_at,
        })
    }

    /// Join a workspace via an invite code.
    ///
    /// Validates that:
    /// - The invite code exists (WorkspaceNotFound if not)
    /// - times_used < max_uses (InviteUsageLimitReached)
    /// - The invite has not expired (InviteExpired)
    /// - The workspace has fewer than 3 members (SeatLimitReached)
    /// - The user is not already a member (AlreadyAMember)
    ///
    /// On success, adds the user as a member and increments times_used.
    ///
    /// (Requirements 5.15, 5.16, 5.17, 5.18)
    pub async fn join_via_invite(
        &self,
        invite_code: String,
        user_id: Uuid,
    ) -> Result<WorkspaceMemberResponse, AppError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AppError::InternalError)?;

        // Fetch the invite by code
        let invite = sqlx::query_as::<_, TeamInviteRow>(
            r#"
            SELECT id, workspace_id, invite_code, max_uses, times_used, expires_at, created_at
            FROM team_invites
            WHERE invite_code = $1
            "#,
        )
        .bind(&invite_code)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        let invite = invite.ok_or(AppError::WorkspaceNotFound)?;

        // Check usage limit
        if invite.times_used >= invite.max_uses {
            return Err(AppError::InviteUsageLimitReached);
        }

        // Check expiration
        if Utc::now() > invite.expires_at {
            return Err(AppError::InviteExpired);
        }

        // Check seat limit (max 3 members including owner)
        let member_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM workspace_members WHERE workspace_id = $1",
        )
        .bind(invite.workspace_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        if member_count >= MAX_TEAM_SEATS {
            return Err(AppError::SeatLimitReached);
        }

        // Check if user is already a member
        let already_member: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id = $1 AND user_id = $2)",
        )
        .bind(invite.workspace_id)
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        if already_member {
            return Err(AppError::AlreadyAMember);
        }

        // Add membership
        let member_row = sqlx::query_as::<_, WorkspaceMemberRow>(
            r#"
            INSERT INTO workspace_members (workspace_id, user_id, role)
            VALUES ($1, $2, 'member')
            RETURNING workspace_id, user_id, role, joined_at
            "#,
        )
        .bind(invite.workspace_id)
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Increment times_used
        sqlx::query(
            "UPDATE team_invites SET times_used = times_used + 1 WHERE id = $1",
        )
        .bind(invite.id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        tx.commit().await.map_err(|_| AppError::InternalError)?;

        Ok(WorkspaceMemberResponse {
            workspace_id: member_row.workspace_id,
            user_id: member_row.user_id,
            role: member_row.role,
            joined_at: member_row.joined_at,
        })
    }

    /// Remove a member from a workspace.
    ///
    /// The requester must be the workspace owner (Forbidden otherwise).
    /// The target user must not be the owner (CannotRemoveOwner).
    ///
    /// (Requirements 5.19, 5.20)
    pub async fn remove_member(
        &self,
        workspace_id: Uuid,
        requester_id: Uuid,
        target_user_id: Uuid,
    ) -> Result<(), AppError> {
        // Verify workspace exists and get owner_id
        let owner_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT owner_id FROM workspaces WHERE id = $1",
        )
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        let owner_id = owner_id.ok_or(AppError::WorkspaceNotFound)?;

        // Requester must be the owner
        if requester_id != owner_id {
            return Err(AppError::Forbidden);
        }

        // Cannot remove the owner
        if target_user_id == owner_id {
            return Err(AppError::CannotRemoveOwner);
        }

        // Remove membership
        let result = sqlx::query(
            "DELETE FROM workspace_members WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(workspace_id)
        .bind(target_user_id)
        .execute(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        if result.rows_affected() == 0 {
            return Err(AppError::WorkspaceNotFound);
        }

        Ok(())
    }

    /// List all members of a workspace.
    pub async fn list_members(
        &self,
        workspace_id: Uuid,
    ) -> Result<Vec<WorkspaceMemberResponse>, AppError> {
        // Verify workspace exists
        let workspace_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM workspaces WHERE id = $1)",
        )
        .bind(workspace_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        if !workspace_exists {
            return Err(AppError::WorkspaceNotFound);
        }

        let rows = sqlx::query_as::<_, WorkspaceMemberRow>(
            r#"
            SELECT workspace_id, user_id, role, joined_at
            FROM workspace_members
            WHERE workspace_id = $1
            ORDER BY joined_at ASC
            "#,
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        let members = rows
            .into_iter()
            .map(|row| WorkspaceMemberResponse {
                workspace_id: row.workspace_id,
                user_id: row.user_id,
                role: row.role,
                joined_at: row.joined_at,
            })
            .collect();

        Ok(members)
    }
}
