use std::sync::Arc;

use chrono::{DateTime, Months, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::AppError;
use crate::models::common::{PaginatedResponse, Pagination};
use crate::sync::session_registry::SessionRegistry;

// ─── Filter / Request Types ─────────────────────────────────────────────────────

/// Filters for the `list_users` endpoint.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UserFilters {
    /// Search string matching email, first_name, or last_name (case-insensitive).
    pub search: Option<String>,
    /// Filter by role.
    pub role: Option<String>,
    /// Filter by subscription tier.
    pub subscription_tier: Option<String>,
    /// Filter by user status (e.g. "active", "suspended").
    pub status: Option<String>,
}

/// Filters for the `list_workspaces` endpoint.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WorkspaceFilters {
    /// Filter by workspace type: "individual" or "team".
    pub workspace_type: Option<String>,
    /// Filter by subscription status: "active", "past_due", "cancelled", etc.
    pub subscription_status: Option<String>,
}

// ─── Response DTOs ──────────────────────────────────────────────────────────────

/// Summary view of a user for admin listing.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct AdminUserSummary {
    pub id: Uuid,
    pub email: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub role: String,
    pub status: String,
    pub subscription_tier: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Detailed user view for admin single-user endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct AdminUserDetail {
    pub id: Uuid,
    pub email: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub profile_picture_url: Option<String>,
    pub timezone: Option<String>,
    pub language: Option<String>,
    pub country_code: Option<String>,
    pub phone: Option<String>,
    pub role: String,
    pub status: String,
    pub referral_code: Option<String>,
    pub must_reset_password: bool,
    pub deleted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub subscriptions: Vec<UserSubscriptionInfo>,
    pub workspaces: Vec<UserWorkspaceInfo>,
    pub referrals: Vec<UserReferralInfo>,
}

/// Internal DB row for user detail query.
#[derive(Debug, sqlx::FromRow)]
struct UserDetailRow {
    pub id: Uuid,
    pub email: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub profile_picture_url: Option<String>,
    pub timezone: Option<String>,
    pub language: Option<String>,
    pub country_code: Option<String>,
    pub phone: Option<String>,
    pub role: String,
    pub status: String,
    pub referral_code: Option<String>,
    pub must_reset_password: bool,
    pub deleted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Subscription info attached to a user detail response.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct UserSubscriptionInfo {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub tier: String,
    pub status: String,
    pub period_start: Option<DateTime<Utc>>,
    pub period_end: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Workspace info attached to a user detail response.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct UserWorkspaceInfo {
    pub workspace_id: Uuid,
    pub workspace_name: String,
    pub workspace_type: String,
    pub role: String,
    pub joined_at: DateTime<Utc>,
}

/// Referral info attached to a user detail response.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct UserReferralInfo {
    pub id: Uuid,
    pub referred_user_id: Uuid,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

/// Summary view of a workspace for admin listing.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct AdminWorkspaceSummary {
    pub id: Uuid,
    pub workspace_type: String,
    pub name: String,
    pub owner_id: Uuid,
    pub member_count: i64,
    pub subscription_tier: Option<String>,
    pub subscription_status: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Detailed workspace view for admin single-workspace endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct AdminWorkspaceDetail {
    pub id: Uuid,
    pub workspace_type: String,
    pub name: String,
    pub owner_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub members: Vec<WorkspaceMemberInfo>,
    pub snippet_count: i64,
    pub folder_count: i64,
    pub subscription: Option<WorkspaceSubscriptionInfo>,
}

/// Workspace member info for admin workspace detail response.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct WorkspaceMemberInfo {
    pub user_id: Uuid,
    pub email: String,
    pub role: String,
    pub joined_at: DateTime<Utc>,
}

/// Subscription info attached to a workspace detail response.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct WorkspaceSubscriptionInfo {
    pub id: Uuid,
    pub tier: String,
    pub status: String,
    pub period_start: Option<DateTime<Utc>>,
    pub period_end: Option<DateTime<Utc>>,
    pub grace_period_end: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Internal DB row for workspace detail base query.
#[derive(Debug, sqlx::FromRow)]
struct WorkspaceDetailRow {
    pub id: Uuid,
    pub workspace_type: String,
    pub name: String,
    pub owner_id: Uuid,
    pub created_at: DateTime<Utc>,
}

// ─── Discount / Coupon / Referral Types ─────────────────────────────────────────

/// Discount response.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct DiscountResponse {
    pub id: Uuid,
    #[sqlx(rename = "type")]
    pub discount_type: String,
    pub value: Decimal,
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

/// Request to create a discount.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateDiscountRequest {
    pub discount_type: String,
    pub value: Decimal,
}

/// Request to update a discount.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateDiscountRequest {
    pub discount_type: Option<String>,
    pub value: Option<Decimal>,
    pub active: Option<bool>,
}

/// Coupon filters for listing.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CouponFilters {
    pub coupon_type: Option<String>,
    pub active: Option<bool>,
}

/// Coupon response.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct CouponResponse {
    pub id: Uuid,
    pub code: String,
    #[sqlx(rename = "type")]
    pub coupon_type: String,
    pub discount_id: Uuid,
    pub owner_id: Option<Uuid>,
    pub max_uses: Option<i32>,
    pub times_used: i32,
    pub valid_from: DateTime<Utc>,
    pub valid_until: Option<DateTime<Utc>>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

/// Request to create a platform coupon.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateCouponRequest {
    pub code: String,
    pub discount_id: Uuid,
    pub max_uses: Option<i32>,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_until: Option<DateTime<Utc>>,
}

/// Request to update a coupon.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateCouponRequest {
    pub max_uses: Option<i32>,
    pub valid_until: Option<DateTime<Utc>>,
    pub active: Option<bool>,
}

/// Referral statistics response.
#[derive(Debug, Clone, Serialize)]
pub struct ReferralStatsResponse {
    pub total_referrals: i64,
    pub converted_referrals: i64,
    pub conversion_rate: f64,
    pub top_referrers: Vec<TopReferrer>,
}

/// A top referrer entry.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct TopReferrer {
    pub user_id: Uuid,
    pub email: String,
    pub referral_count: i64,
    pub converted_count: i64,
}

// ─── Subscription / Billing Types ───────────────────────────────────────────────

/// Filters for listing subscriptions.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SubscriptionFilters {
    /// Filter by tier: "free", "pro", "teams".
    pub tier: Option<String>,
    /// Filter by status: "active", "past_due", "cancelled", etc.
    pub status: Option<String>,
    /// Filter by workspace_id.
    pub workspace_id: Option<Uuid>,
    /// Filter subscriptions with period_end after this date.
    pub period_end_after: Option<DateTime<Utc>>,
    /// Filter subscriptions with period_end before this date.
    pub period_end_before: Option<DateTime<Utc>>,
}

/// Summary subscription response for listing.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct AdminSubscriptionSummary {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub tier: String,
    pub status: String,
    pub period_start: Option<DateTime<Utc>>,
    pub period_end: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Detailed subscription view with billing history.
#[derive(Debug, Clone, Serialize)]
pub struct AdminSubscriptionDetail {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub tier: String,
    pub status: String,
    pub period_start: Option<DateTime<Utc>>,
    pub period_end: Option<DateTime<Utc>>,
    pub grace_period_end: Option<DateTime<Utc>>,
    pub payment_deadline: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub external_subscription_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub billing_events: Vec<BillingEventResponse>,
}

/// Internal DB row for subscription detail query.
#[derive(Debug, sqlx::FromRow)]
struct SubscriptionDetailRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub tier: String,
    pub status: String,
    pub period_start: Option<DateTime<Utc>>,
    pub period_end: Option<DateTime<Utc>>,
    pub grace_period_end: Option<DateTime<Utc>>,
    pub payment_deadline: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub external_subscription_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request to extend a subscription.
#[derive(Debug, Clone, Deserialize)]
pub struct ExtendSubscriptionRequest {
    pub months: Option<i32>,
    pub days: Option<i32>,
}

/// Request to override a subscription tier.
#[derive(Debug, Clone, Deserialize)]
pub struct OverrideTierRequest {
    pub tier: String,
}

/// Billing event response.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct BillingEventResponse {
    pub id: Uuid,
    pub external_event_id: String,
    pub event_type: String,
    pub workspace_id: Option<Uuid>,
    pub payload: serde_json::Value,
    pub processed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Filters for listing billing events.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BillingEventFilters {
    /// Filter by workspace_id.
    pub workspace_id: Option<Uuid>,
    /// Filter by event_type.
    pub event_type: Option<String>,
}

// ─── Tax Rate Types ─────────────────────────────────────────────────────────────

/// Tax rate response.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct TaxRateResponse {
    pub country_code: String,
    pub rate: Decimal,
    pub tax_name: String,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request to create a tax rate.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateTaxRateRequest {
    pub country_code: String,
    pub rate: Decimal,
    pub tax_name: String,
}

/// Request to update a tax rate.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTaxRateRequest {
    pub rate: Option<Decimal>,
    pub tax_name: Option<String>,
    pub active: Option<bool>,
}

// ─── Audit Log Types ────────────────────────────────────────────────────────────

/// Filters for the `list_audit_logs` endpoint.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuditLogFilters {
    pub admin_id: Option<Uuid>,
    pub action: Option<String>,
    pub target_resource: Option<String>,
    pub created_after: Option<DateTime<Utc>>,
    pub created_before: Option<DateTime<Utc>>,
}

/// Audit log response.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct AuditLogResponse {
    pub id: Uuid,
    pub admin_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub action: String,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub client_type: Option<String>,
    pub target_resource: Option<String>,
    pub target_id: Option<String>,
    pub result: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub trace_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

// ─── Feature Flag Types ─────────────────────────────────────────────────────────

/// Feature flag response.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct FeatureFlagResponse {
    pub name: String,
    pub enabled: bool,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request to create a feature flag.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateFeatureFlagRequest {
    pub name: String,
    pub enabled: Option<bool>,
    pub description: Option<String>,
}

/// Request to update a feature flag.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateFeatureFlagRequest {
    pub enabled: Option<bool>,
    pub description: Option<String>,
}

// ─── Admin Management Types ─────────────────────────────────────────────────────

/// Admin user info for admin listing.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct AdminUserInfo {
    pub id: Uuid,
    pub email: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub created_at: DateTime<Utc>,
}

// ─── Stats Types ────────────────────────────────────────────────────────────────

/// High-level platform overview statistics.
#[derive(Debug, Clone, Serialize)]
pub struct OverviewStats {
    pub total_users: i64,
    pub active_users: i64,
    pub suspended_users: i64,
    pub total_workspaces: i64,
    pub individual_workspaces: i64,
    pub team_workspaces: i64,
    pub subscriptions_free: i64,
    pub subscriptions_pro: i64,
    pub subscriptions_teams: i64,
    pub total_snippets: i64,
    pub total_folders: i64,
}

/// Referral analytics response.
#[derive(Debug, Clone, Serialize)]
pub struct ReferralAnalytics {
    pub total_referrals: i64,
    pub converted_referrals: i64,
    pub pending_referrals: i64,
    pub conversion_rate: f64,
    pub top_referrers: Vec<TopReferrer>,
}

// ─── Service ────────────────────────────────────────────────────────────────────

/// Admin service handling user and workspace management operations.
///
/// Requirements: 4.4–4.67
pub struct AdminService {
    pool: PgPool,
    session_registry: Option<Arc<SessionRegistry>>,
}

impl AdminService {
    /// Create a new `AdminService` instance.
    pub fn new(pool: PgPool, session_registry: Option<Arc<SessionRegistry>>) -> Self {
        Self {
            pool,
            session_registry,
        }
    }

    /// List users with pagination and optional filters.
    ///
    /// Supports search across email, first_name, last_name (case-insensitive),
    /// and filtering by role, subscription_tier, and status.
    ///
    /// Requirement 4.4
    pub async fn list_users(
        &self,
        pagination: &Pagination,
        filters: &UserFilters,
    ) -> Result<PaginatedResponse<AdminUserSummary>, AppError> {
        let offset = pagination.offset();
        let limit = pagination.limit();

        let search_pattern = filters
            .search
            .as_ref()
            .map(|s| format!("%{}%", s.to_lowercase()));

        let total: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)
            FROM users u
            LEFT JOIN workspace_members wm ON wm.user_id = u.id
            LEFT JOIN subscriptions s ON s.workspace_id = wm.workspace_id
            WHERE ($1::text IS NULL OR (
                LOWER(u.email) LIKE $1
                OR LOWER(COALESCE(u.first_name, '')) LIKE $1
                OR LOWER(COALESCE(u.last_name, '')) LIKE $1
            ))
            AND ($2::text IS NULL OR u.role = $2)
            AND ($3::text IS NULL OR s.tier = $3)
            AND ($4::text IS NULL OR u.status = $4)
            "#,
        )
        .bind(search_pattern.as_deref())
        .bind(filters.role.as_deref())
        .bind(filters.subscription_tier.as_deref())
        .bind(filters.status.as_deref())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to count users: {e}");
            AppError::InternalError
        })?;

        let users: Vec<AdminUserSummary> = sqlx::query_as(
            r#"
            SELECT DISTINCT ON (u.id)
                u.id,
                u.email,
                u.first_name,
                u.last_name,
                u.role,
                u.status,
                s.tier AS subscription_tier,
                u.created_at
            FROM users u
            LEFT JOIN workspace_members wm ON wm.user_id = u.id
            LEFT JOIN subscriptions s ON s.workspace_id = wm.workspace_id
            WHERE ($1::text IS NULL OR (
                LOWER(u.email) LIKE $1
                OR LOWER(COALESCE(u.first_name, '')) LIKE $1
                OR LOWER(COALESCE(u.last_name, '')) LIKE $1
            ))
            AND ($2::text IS NULL OR u.role = $2)
            AND ($3::text IS NULL OR s.tier = $3)
            AND ($4::text IS NULL OR u.status = $4)
            ORDER BY u.id, u.created_at DESC
            LIMIT $5 OFFSET $6
            "#,
        )
        .bind(search_pattern.as_deref())
        .bind(filters.role.as_deref())
        .bind(filters.subscription_tier.as_deref())
        .bind(filters.status.as_deref())
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to list users: {e}");
            AppError::InternalError
        })?;

        Ok(PaginatedResponse::new(users, total, pagination))
    }

    /// Get full user detail including subscriptions, workspaces, and referrals.
    ///
    /// Requirement 4.5
    pub async fn get_user(&self, user_id: Uuid) -> Result<AdminUserDetail, AppError> {
        let user: UserDetailRow = sqlx::query_as(
            r#"
            SELECT
                id, email, first_name, last_name, profile_picture_url,
                timezone, language, country_code, phone, role, status,
                referral_code, must_reset_password, deleted_at, created_at, updated_at
            FROM users
            WHERE id = $1
            "#,
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to fetch user: {e}");
            AppError::InternalError
        })?
        .ok_or(AppError::UserNotFound)?;

        let subscriptions: Vec<UserSubscriptionInfo> = sqlx::query_as(
            r#"
            SELECT
                s.id, s.workspace_id, s.tier, s.status,
                s.period_start, s.period_end, s.created_at
            FROM subscriptions s
            INNER JOIN workspace_members wm ON wm.workspace_id = s.workspace_id
            WHERE wm.user_id = $1
            ORDER BY s.created_at DESC
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to fetch user subscriptions: {e}");
            AppError::InternalError
        })?;

        let workspaces: Vec<UserWorkspaceInfo> = sqlx::query_as(
            r#"
            SELECT
                w.id AS workspace_id,
                w.name AS workspace_name,
                w.type AS workspace_type,
                wm.role,
                wm.joined_at
            FROM workspace_members wm
            INNER JOIN workspaces w ON w.id = wm.workspace_id
            WHERE wm.user_id = $1
            ORDER BY wm.joined_at DESC
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to fetch user workspaces: {e}");
            AppError::InternalError
        })?;

        let referrals: Vec<UserReferralInfo> = sqlx::query_as(
            r#"
            SELECT id, referred_user_id, status, created_at
            FROM referrals
            WHERE referrer_id = $1
            ORDER BY created_at DESC
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to fetch user referrals: {e}");
            AppError::InternalError
        })?;

        Ok(AdminUserDetail {
            id: user.id,
            email: user.email,
            first_name: user.first_name,
            last_name: user.last_name,
            profile_picture_url: user.profile_picture_url,
            timezone: user.timezone,
            language: user.language,
            country_code: user.country_code,
            phone: user.phone,
            role: user.role,
            status: user.status,
            referral_code: user.referral_code,
            must_reset_password: user.must_reset_password,
            deleted_at: user.deleted_at,
            created_at: user.created_at,
            updated_at: user.updated_at,
            subscriptions,
            workspaces,
            referrals,
        })
    }

    /// Suspend a user account.
    ///
    /// Sets status=suspended, revokes all refresh tokens, closes all WebSocket
    /// connections with code 1008. Blocks self-action and action on other admins.
    ///
    /// Requirements: 4.6, 4.7, 4.8
    pub async fn suspend_user(
        &self,
        admin_id: Uuid,
        user_id: Uuid,
    ) -> Result<AdminUserSummary, AppError> {
        if admin_id == user_id {
            return Err(AppError::CannotActOnSelf);
        }

        let target_role: Option<String> =
            sqlx::query_scalar("SELECT role FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to check target user role: {e}");
                    AppError::InternalError
                })?;

        match target_role.as_deref() {
            None => return Err(AppError::UserNotFound),
            Some("admin") => return Err(AppError::CannotActOnAdmin),
            _ => {}
        }

        sqlx::query("UPDATE users SET status = 'suspended', updated_at = NOW() WHERE id = $1")
            .bind(user_id)
            .execute(&self.pool)
            .await
            .map_err(|e| {
                tracing::error!("Failed to suspend user: {e}");
                AppError::InternalError
            })?;

        sqlx::query("UPDATE refresh_tokens SET revoked = true WHERE user_id = $1")
            .bind(user_id)
            .execute(&self.pool)
            .await
            .map_err(|e| {
                tracing::error!("Failed to revoke refresh tokens: {e}");
                AppError::InternalError
            })?;

        if let Some(ref registry) = self.session_registry {
            registry.close_user_sessions(user_id, 1008, "Account suspended".to_string());
        }

        let user: AdminUserSummary = sqlx::query_as(
            r#"
            SELECT DISTINCT ON (u.id)
                u.id, u.email, u.first_name, u.last_name, u.role, u.status,
                s.tier AS subscription_tier, u.created_at
            FROM users u
            LEFT JOIN workspace_members wm ON wm.user_id = u.id
            LEFT JOIN subscriptions s ON s.workspace_id = wm.workspace_id
            WHERE u.id = $1
            ORDER BY u.id, s.created_at DESC NULLS LAST
            "#,
        )
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to fetch suspended user: {e}");
            AppError::InternalError
        })?;

        Ok(user)
    }

    /// Unsuspend a user account by setting status back to active.
    ///
    /// Requirement 4.10
    pub async fn unsuspend_user(
        &self,
        admin_id: Uuid,
        user_id: Uuid,
    ) -> Result<AdminUserSummary, AppError> {
        if admin_id == user_id {
            return Err(AppError::CannotActOnSelf);
        }

        let target_role: Option<String> =
            sqlx::query_scalar("SELECT role FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to check target user role: {e}");
                    AppError::InternalError
                })?;

        match target_role.as_deref() {
            None => return Err(AppError::UserNotFound),
            Some("admin") => return Err(AppError::CannotActOnAdmin),
            _ => {}
        }

        sqlx::query("UPDATE users SET status = 'active', updated_at = NOW() WHERE id = $1")
            .bind(user_id)
            .execute(&self.pool)
            .await
            .map_err(|e| {
                tracing::error!("Failed to unsuspend user: {e}");
                AppError::InternalError
            })?;

        let user: AdminUserSummary = sqlx::query_as(
            r#"
            SELECT DISTINCT ON (u.id)
                u.id, u.email, u.first_name, u.last_name, u.role, u.status,
                s.tier AS subscription_tier, u.created_at
            FROM users u
            LEFT JOIN workspace_members wm ON wm.user_id = u.id
            LEFT JOIN subscriptions s ON s.workspace_id = wm.workspace_id
            WHERE u.id = $1
            ORDER BY u.id, s.created_at DESC NULLS LAST
            "#,
        )
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to fetch unsuspended user: {e}");
            AppError::InternalError
        })?;

        Ok(user)
    }

    /// Force a password reset on a user account.
    ///
    /// Sets `must_reset_password = true` on the user record.
    ///
    /// Requirement 4.11
    pub async fn force_password_reset(
        &self,
        admin_id: Uuid,
        user_id: Uuid,
    ) -> Result<(), AppError> {
        if admin_id == user_id {
            return Err(AppError::CannotActOnSelf);
        }

        let target_role: Option<String> =
            sqlx::query_scalar("SELECT role FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to check target user role: {e}");
                    AppError::InternalError
                })?;

        match target_role.as_deref() {
            None => return Err(AppError::UserNotFound),
            Some("admin") => return Err(AppError::CannotActOnAdmin),
            _ => {}
        }

        sqlx::query(
            "UPDATE users SET must_reset_password = true, updated_at = NOW() WHERE id = $1",
        )
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to force password reset: {e}");
            AppError::InternalError
        })?;

        Ok(())
    }

    /// Soft-delete a user account with 30-day retention.
    ///
    /// Sets `deleted_at` to the current timestamp. Blocks self-action and
    /// action on other admins.
    ///
    /// Requirements: 4.13, 4.14, 4.15
    pub async fn delete_user(&self, admin_id: Uuid, user_id: Uuid) -> Result<(), AppError> {
        if admin_id == user_id {
            return Err(AppError::CannotActOnSelf);
        }

        let target_role: Option<String> =
            sqlx::query_scalar("SELECT role FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to check target user role: {e}");
                    AppError::InternalError
                })?;

        match target_role.as_deref() {
            None => return Err(AppError::UserNotFound),
            Some("admin") => return Err(AppError::CannotActOnAdmin),
            _ => {}
        }

        sqlx::query("UPDATE users SET deleted_at = NOW(), updated_at = NOW() WHERE id = $1")
            .bind(user_id)
            .execute(&self.pool)
            .await
            .map_err(|e| {
                tracing::error!("Failed to soft-delete user: {e}");
                AppError::InternalError
            })?;

        Ok(())
    }

    // ─── Workspace Management ───────────────────────────────────────────────────

    /// List workspaces with pagination and optional filters.
    ///
    /// Supports filtering by workspace type and subscription status.
    ///
    /// Requirement 4.16
    pub async fn list_workspaces(
        &self,
        pagination: &Pagination,
        filters: &WorkspaceFilters,
    ) -> Result<PaginatedResponse<AdminWorkspaceSummary>, AppError> {
        let offset = pagination.offset();
        let limit = pagination.limit();

        let total: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(DISTINCT w.id)
            FROM workspaces w
            LEFT JOIN subscriptions s ON s.workspace_id = w.id
            WHERE ($1::text IS NULL OR w.type = $1)
            AND ($2::text IS NULL OR s.status = $2)
            "#,
        )
        .bind(filters.workspace_type.as_deref())
        .bind(filters.subscription_status.as_deref())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to count workspaces: {e}");
            AppError::InternalError
        })?;

        let workspaces: Vec<AdminWorkspaceSummary> = sqlx::query_as(
            r#"
            SELECT
                w.id,
                w.type AS workspace_type,
                w.name,
                w.owner_id,
                (SELECT COUNT(*) FROM workspace_members wm WHERE wm.workspace_id = w.id) AS member_count,
                s.tier AS subscription_tier,
                s.status AS subscription_status,
                w.created_at
            FROM workspaces w
            LEFT JOIN subscriptions s ON s.workspace_id = w.id
            WHERE ($1::text IS NULL OR w.type = $1)
            AND ($2::text IS NULL OR s.status = $2)
            ORDER BY w.created_at DESC
            LIMIT $3 OFFSET $4
            "#,
        )
        .bind(filters.workspace_type.as_deref())
        .bind(filters.subscription_status.as_deref())
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to list workspaces: {e}");
            AppError::InternalError
        })?;

        Ok(PaginatedResponse::new(workspaces, total, pagination))
    }

    /// Get full workspace detail including members, snippet/folder counts,
    /// and subscription info.
    ///
    /// Requirement 4.17
    pub async fn get_workspace(
        &self,
        workspace_id: Uuid,
    ) -> Result<AdminWorkspaceDetail, AppError> {
        let workspace: WorkspaceDetailRow = sqlx::query_as(
            r#"
            SELECT
                id,
                type AS workspace_type,
                name,
                owner_id,
                created_at
            FROM workspaces
            WHERE id = $1
            "#,
        )
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to fetch workspace: {e}");
            AppError::InternalError
        })?
        .ok_or(AppError::WorkspaceNotFound)?;

        let members: Vec<WorkspaceMemberInfo> = sqlx::query_as(
            r#"
            SELECT
                wm.user_id,
                u.email,
                wm.role,
                wm.joined_at
            FROM workspace_members wm
            INNER JOIN users u ON u.id = wm.user_id
            WHERE wm.workspace_id = $1
            ORDER BY wm.joined_at ASC
            "#,
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to fetch workspace members: {e}");
            AppError::InternalError
        })?;

        let snippet_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM snippets WHERE workspace_id = $1 AND deleted_at IS NULL",
        )
        .bind(workspace_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to count workspace snippets: {e}");
            AppError::InternalError
        })?;

        let folder_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM folders WHERE workspace_id = $1 AND deleted_at IS NULL",
        )
        .bind(workspace_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to count workspace folders: {e}");
            AppError::InternalError
        })?;

        let subscription: Option<WorkspaceSubscriptionInfo> = sqlx::query_as(
            r#"
            SELECT
                id, tier, status, period_start, period_end,
                grace_period_end, created_at
            FROM subscriptions
            WHERE workspace_id = $1
            ORDER BY created_at DESC
            LIMIT 1
            "#,
        )
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to fetch workspace subscription: {e}");
            AppError::InternalError
        })?;

        Ok(AdminWorkspaceDetail {
            id: workspace.id,
            workspace_type: workspace.workspace_type,
            name: workspace.name,
            owner_id: workspace.owner_id,
            created_at: workspace.created_at,
            members,
            snippet_count,
            folder_count,
            subscription,
        })
    }

    /// Deactivate a workspace.
    ///
    /// Sets subscription status to 'deactivated' and closes all WebSocket
    /// connections for that workspace with code 1008.
    ///
    /// Requirements: 4.18, 4.19
    pub async fn deactivate_workspace(
        &self,
        workspace_id: Uuid,
    ) -> Result<(), AppError> {
        // Verify workspace exists
        let exists: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM workspaces WHERE id = $1",
        )
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to check workspace existence: {e}");
            AppError::InternalError
        })?;

        if exists.is_none() {
            return Err(AppError::WorkspaceNotFound);
        }

        // Update subscription status to deactivated
        sqlx::query(
            "UPDATE subscriptions SET status = 'deactivated', updated_at = NOW() WHERE workspace_id = $1",
        )
        .bind(workspace_id)
        .execute(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to deactivate workspace subscription: {e}");
            AppError::InternalError
        })?;

        // Close all WebSocket connections for this workspace
        if let Some(ref registry) = self.session_registry {
            registry.close_workspace_sessions(
                workspace_id,
                1008,
                "Workspace deactivated".to_string(),
            );
        }

        Ok(())
    }

    /// Delete a workspace (hard-delete).
    ///
    /// Blocks deletion of individual workspaces (must delete user instead).
    /// Requires `confirm = true` to proceed.
    /// Closes all WebSocket connections before deleting.
    /// CASCADE foreign keys handle associated data cleanup.
    ///
    /// Requirements: 4.20, 4.21
    pub async fn delete_workspace(
        &self,
        workspace_id: Uuid,
        confirm: bool,
    ) -> Result<(), AppError> {
        if !confirm {
            return Err(AppError::ConfirmationRequired);
        }

        // Fetch workspace to check type
        let workspace_type: Option<String> = sqlx::query_scalar(
            "SELECT type FROM workspaces WHERE id = $1",
        )
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to fetch workspace for deletion: {e}");
            AppError::InternalError
        })?;

        match workspace_type.as_deref() {
            None => return Err(AppError::WorkspaceNotFound),
            Some("individual") => return Err(AppError::CannotDeleteIndividualWorkspace),
            _ => {}
        }

        // Close all WebSocket connections for this workspace first
        if let Some(ref registry) = self.session_registry {
            registry.close_workspace_sessions(
                workspace_id,
                1008,
                "Workspace deleted".to_string(),
            );
        }

        // Hard-delete the workspace; CASCADE handles associated data
        sqlx::query("DELETE FROM workspaces WHERE id = $1")
            .bind(workspace_id)
            .execute(&self.pool)
            .await
            .map_err(|e| {
                tracing::error!("Failed to delete workspace: {e}");
                AppError::InternalError
            })?;

        Ok(())
    }

    // ─── Discount Management ────────────────────────────────────────────────────

    /// List all discount records.
    ///
    /// Requirement 4.22
    pub async fn list_discounts(&self) -> Result<Vec<DiscountResponse>, AppError> {
        let discounts: Vec<DiscountResponse> = sqlx::query_as(
            r#"
            SELECT id, type, value, active, created_at
            FROM discounts
            ORDER BY created_at DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to list discounts: {e}");
            AppError::InternalError
        })?;

        Ok(discounts)
    }

    /// Create a new discount record.
    ///
    /// Requirement 4.23
    pub async fn create_discount(
        &self,
        req: &CreateDiscountRequest,
    ) -> Result<DiscountResponse, AppError> {
        let discount: DiscountResponse = sqlx::query_as(
            r#"
            INSERT INTO discounts (type, value)
            VALUES ($1, $2)
            RETURNING id, type, value, active, created_at
            "#,
        )
        .bind(&req.discount_type)
        .bind(req.value)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to create discount: {e}");
            AppError::InternalError
        })?;

        Ok(discount)
    }

    /// Update an existing discount (value, type, active).
    /// No hard-delete — deactivate only.
    ///
    /// Requirements: 4.24, 4.25, 4.26
    pub async fn update_discount(
        &self,
        discount_id: Uuid,
        req: &UpdateDiscountRequest,
    ) -> Result<DiscountResponse, AppError> {
        // Check existence first
        let exists: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM discounts WHERE id = $1")
                .bind(discount_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to check discount existence: {e}");
                    AppError::InternalError
                })?;

        if exists.is_none() {
            return Err(AppError::DiscountNotFound);
        }

        let discount: DiscountResponse = sqlx::query_as(
            r#"
            UPDATE discounts
            SET
                type = COALESCE($2, type),
                value = COALESCE($3, value),
                active = COALESCE($4, active)
            WHERE id = $1
            RETURNING id, type, value, active, created_at
            "#,
        )
        .bind(discount_id)
        .bind(req.discount_type.as_deref())
        .bind(req.value)
        .bind(req.active)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to update discount: {e}");
            AppError::InternalError
        })?;

        Ok(discount)
    }

    // ─── Coupon Management ──────────────────────────────────────────────────────

    /// List coupon codes with pagination and optional filters (type, active).
    ///
    /// Requirement 4.27
    pub async fn list_coupons(
        &self,
        pagination: &Pagination,
        filters: &CouponFilters,
    ) -> Result<PaginatedResponse<CouponResponse>, AppError> {
        let offset = pagination.offset();
        let limit = pagination.limit();

        let total: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)
            FROM coupon_codes
            WHERE ($1::text IS NULL OR type = $1)
              AND ($2::bool IS NULL OR active = $2)
            "#,
        )
        .bind(filters.coupon_type.as_deref())
        .bind(filters.active)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to count coupons: {e}");
            AppError::InternalError
        })?;

        let coupons: Vec<CouponResponse> = sqlx::query_as(
            r#"
            SELECT id, code, type, discount_id, owner_id, max_uses, times_used,
                   valid_from, valid_until, active, created_at
            FROM coupon_codes
            WHERE ($1::text IS NULL OR type = $1)
              AND ($2::bool IS NULL OR active = $2)
            ORDER BY created_at DESC
            LIMIT $3 OFFSET $4
            "#,
        )
        .bind(filters.coupon_type.as_deref())
        .bind(filters.active)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to list coupons: {e}");
            AppError::InternalError
        })?;

        Ok(PaginatedResponse::new(coupons, total, pagination))
    }

    /// Get a single coupon by ID.
    ///
    /// Requirement 4.31
    pub async fn get_coupon(&self, coupon_id: Uuid) -> Result<CouponResponse, AppError> {
        let coupon: CouponResponse = sqlx::query_as(
            r#"
            SELECT id, code, type, discount_id, owner_id, max_uses, times_used,
                   valid_from, valid_until, active, created_at
            FROM coupon_codes
            WHERE id = $1
            "#,
        )
        .bind(coupon_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to fetch coupon: {e}");
            AppError::InternalError
        })?
        .ok_or(AppError::CouponNotFound)?;

        Ok(coupon)
    }

    /// Create a new platform coupon code.
    ///
    /// Only type=platform can be created via admin API. Enforces case-insensitive
    /// code uniqueness. On unique constraint violation, returns CouponCodeAlreadyExists.
    ///
    /// Requirements: 4.28, 4.29
    pub async fn create_coupon(
        &self,
        req: &CreateCouponRequest,
    ) -> Result<CouponResponse, AppError> {
        let valid_from = req.valid_from.unwrap_or_else(Utc::now);

        let coupon: CouponResponse = sqlx::query_as(
            r#"
            INSERT INTO coupon_codes (code, type, discount_id, max_uses, valid_from, valid_until)
            VALUES ($1, 'platform', $2, $3, $4, $5)
            RETURNING id, code, type, discount_id, owner_id, max_uses, times_used,
                      valid_from, valid_until, active, created_at
            "#,
        )
        .bind(&req.code)
        .bind(req.discount_id)
        .bind(req.max_uses)
        .bind(valid_from)
        .bind(req.valid_until)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            // Check for unique constraint violation on LOWER(code)
            if let sqlx::Error::Database(ref db_err) = e {
                if db_err.constraint() == Some("idx_coupon_codes_code") {
                    return AppError::CouponCodeAlreadyExists;
                }
            }
            tracing::error!("Failed to create coupon: {e}");
            AppError::InternalError
        })?;

        Ok(coupon)
    }

    /// Update an existing coupon (active, max_uses, valid_until).
    ///
    /// Requirements: 4.30, 4.32
    pub async fn update_coupon(
        &self,
        coupon_id: Uuid,
        req: &UpdateCouponRequest,
    ) -> Result<CouponResponse, AppError> {
        // Check existence
        let exists: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM coupon_codes WHERE id = $1")
                .bind(coupon_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to check coupon existence: {e}");
                    AppError::InternalError
                })?;

        if exists.is_none() {
            return Err(AppError::CouponNotFound);
        }

        let coupon: CouponResponse = sqlx::query_as(
            r#"
            UPDATE coupon_codes
            SET
                active = COALESCE($2, active),
                max_uses = COALESCE($3, max_uses),
                valid_until = COALESCE($4, valid_until)
            WHERE id = $1
            RETURNING id, code, type, discount_id, owner_id, max_uses, times_used,
                      valid_from, valid_until, active, created_at
            "#,
        )
        .bind(coupon_id)
        .bind(req.active)
        .bind(req.max_uses)
        .bind(req.valid_until)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to update coupon: {e}");
            AppError::InternalError
        })?;

        Ok(coupon)
    }

    // ─── Referral Statistics ────────────────────────────────────────────────────

    // ─── Tax Rates ────────────────────────────────────────────────────────────

    /// List all tax rates.
    ///
    /// Requirements: 4.42, 4.43
    pub async fn list_tax_rates(&self) -> Result<Vec<TaxRateResponse>, AppError> {
        let rates: Vec<TaxRateResponse> = sqlx::query_as(
            r#"
            SELECT country_code, rate, tax_name, active, created_at, updated_at
            FROM tax_rates
            ORDER BY country_code ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to list tax rates: {e}");
            AppError::InternalError
        })?;

        Ok(rates)
    }

    /// Create a new tax rate.
    ///
    /// Returns `AppError::TaxRateAlreadyExists` if a rate for the given
    /// `country_code` already exists.
    ///
    /// Requirements: 4.44, 4.45
    pub async fn create_tax_rate(
        &self,
        req: &CreateTaxRateRequest,
    ) -> Result<TaxRateResponse, AppError> {
        let rate: TaxRateResponse = sqlx::query_as(
            r#"
            INSERT INTO tax_rates (country_code, rate, tax_name)
            VALUES ($1, $2, $3)
            RETURNING country_code, rate, tax_name, active, created_at, updated_at
            "#,
        )
        .bind(&req.country_code)
        .bind(req.rate)
        .bind(&req.tax_name)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref db_err) = e {
                if db_err.constraint() == Some("tax_rates_pkey") {
                    return AppError::TaxRateAlreadyExists;
                }
            }
            tracing::error!("Failed to create tax rate: {e}");
            AppError::InternalError
        })?;

        Ok(rate)
    }

    /// Update an existing tax rate (rate, tax_name, active).
    ///
    /// No hard-delete — deactivate only via `active = false`.
    ///
    /// Requirements: 4.46, 4.47
    pub async fn update_tax_rate(
        &self,
        country_code: &str,
        req: &UpdateTaxRateRequest,
    ) -> Result<TaxRateResponse, AppError> {
        let rate: TaxRateResponse = sqlx::query_as(
            r#"
            UPDATE tax_rates
            SET
                rate = COALESCE($2, rate),
                tax_name = COALESCE($3, tax_name),
                active = COALESCE($4, active),
                updated_at = NOW()
            WHERE country_code = $1
            RETURNING country_code, rate, tax_name, active, created_at, updated_at
            "#,
        )
        .bind(country_code)
        .bind(req.rate)
        .bind(req.tax_name.as_deref())
        .bind(req.active)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to update tax rate: {e}");
            AppError::InternalError
        })?
        .ok_or(AppError::UserNotFound)?; // tax rate not found — reuse generic not-found

        Ok(rate)
    }

    // ─── Audit Logs ─────────────────────────────────────────────────────────────

    /// List audit logs with pagination and optional filters.
    ///
    /// Filters by admin_id, action, target_resource, and date range.
    /// Audit logs are immutable — no update or delete.
    ///
    /// Requirements: 4.48, 4.49
    pub async fn list_audit_logs(
        &self,
        pagination: &Pagination,
        filters: &AuditLogFilters,
    ) -> Result<PaginatedResponse<AuditLogResponse>, AppError> {
        let offset = pagination.offset();
        let limit = pagination.limit();

        let total: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)
            FROM audit_logs
            WHERE ($1::uuid IS NULL OR admin_id = $1)
              AND ($2::text IS NULL OR action = $2)
              AND ($3::text IS NULL OR target_resource = $3)
              AND ($4::timestamptz IS NULL OR created_at >= $4)
              AND ($5::timestamptz IS NULL OR created_at <= $5)
            "#,
        )
        .bind(filters.admin_id)
        .bind(filters.action.as_deref())
        .bind(filters.target_resource.as_deref())
        .bind(filters.created_after)
        .bind(filters.created_before)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to count audit logs: {e}");
            AppError::InternalError
        })?;

        let logs: Vec<AuditLogResponse> = sqlx::query_as(
            r#"
            SELECT id, admin_id, user_id, action, ip_address, user_agent,
                   client_type, target_resource, target_id, result,
                   metadata, trace_id, created_at
            FROM audit_logs
            WHERE ($1::uuid IS NULL OR admin_id = $1)
              AND ($2::text IS NULL OR action = $2)
              AND ($3::text IS NULL OR target_resource = $3)
              AND ($4::timestamptz IS NULL OR created_at >= $4)
              AND ($5::timestamptz IS NULL OR created_at <= $5)
            ORDER BY created_at DESC
            LIMIT $6 OFFSET $7
            "#,
        )
        .bind(filters.admin_id)
        .bind(filters.action.as_deref())
        .bind(filters.target_resource.as_deref())
        .bind(filters.created_after)
        .bind(filters.created_before)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to list audit logs: {e}");
            AppError::InternalError
        })?;

        Ok(PaginatedResponse::new(logs, total, pagination))
    }

    /// Get a single audit log entry by ID.
    ///
    /// Requirements: 4.50, 4.51
    pub async fn get_audit_log(&self, log_id: Uuid) -> Result<AuditLogResponse, AppError> {
        let log: AuditLogResponse = sqlx::query_as(
            r#"
            SELECT id, admin_id, user_id, action, ip_address, user_agent,
                   client_type, target_resource, target_id, result,
                   metadata, trace_id, created_at
            FROM audit_logs
            WHERE id = $1
            "#,
        )
        .bind(log_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to fetch audit log: {e}");
            AppError::InternalError
        })?
        .ok_or(AppError::AuditLogNotFound)?;

        Ok(log)
    }

    // ─── Feature Flags ──────────────────────────────────────────────────────────

    /// List all feature flags.
    ///
    /// Requirement 4.52
    pub async fn list_feature_flags(&self) -> Result<Vec<FeatureFlagResponse>, AppError> {
        let flags: Vec<FeatureFlagResponse> = sqlx::query_as(
            r#"
            SELECT name, enabled, description, created_at, updated_at
            FROM feature_flags
            ORDER BY name ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to list feature flags: {e}");
            AppError::InternalError
        })?;

        Ok(flags)
    }

    /// Create a new feature flag.
    ///
    /// Validates name is kebab-case (`^[a-z][a-z0-9]*(-[a-z0-9]+)*$`), max 100 chars.
    /// Returns `AppError::InvalidFlagName` on bad name,
    /// `AppError::FeatureFlagAlreadyExists` on duplicate.
    ///
    /// Requirements: 4.53, 4.54, 4.55
    pub async fn create_feature_flag(
        &self,
        req: &CreateFeatureFlagRequest,
    ) -> Result<FeatureFlagResponse, AppError> {
        // Validate kebab-case name: a-z start, then segments of a-z0-9 separated by hyphens
        if !Self::is_valid_kebab_case(&req.name) {
            return Err(AppError::InvalidFlagName);
        }

        let enabled = req.enabled.unwrap_or(false);

        let flag: FeatureFlagResponse = sqlx::query_as(
            r#"
            INSERT INTO feature_flags (name, enabled, description)
            VALUES ($1, $2, $3)
            RETURNING name, enabled, description, created_at, updated_at
            "#,
        )
        .bind(&req.name)
        .bind(enabled)
        .bind(req.description.as_deref())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref db_err) = e {
                if db_err.constraint() == Some("feature_flags_pkey") {
                    return AppError::FeatureFlagAlreadyExists;
                }
            }
            tracing::error!("Failed to create feature flag: {e}");
            AppError::InternalError
        })?;

        Ok(flag)
    }

    /// Update a feature flag (enabled and/or description).
    ///
    /// Requirements: 4.56, 4.57
    pub async fn update_feature_flag(
        &self,
        name: &str,
        req: &UpdateFeatureFlagRequest,
    ) -> Result<FeatureFlagResponse, AppError> {
        let flag: FeatureFlagResponse = sqlx::query_as(
            r#"
            UPDATE feature_flags
            SET
                enabled = COALESCE($2, enabled),
                description = COALESCE($3, description),
                updated_at = NOW()
            WHERE name = $1
            RETURNING name, enabled, description, created_at, updated_at
            "#,
        )
        .bind(name)
        .bind(req.enabled)
        .bind(req.description.as_deref())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to update feature flag: {e}");
            AppError::InternalError
        })?
        .ok_or(AppError::FeatureFlagNotFound)?;

        Ok(flag)
    }

    /// Delete a feature flag by name (hard-delete).
    ///
    /// Returns `AppError::FeatureFlagNotFound` if the flag does not exist.
    ///
    /// Requirements: 4.58, 4.59
    pub async fn delete_feature_flag(&self, name: &str) -> Result<(), AppError> {
        let result = sqlx::query("DELETE FROM feature_flags WHERE name = $1")
            .bind(name)
            .execute(&self.pool)
            .await
            .map_err(|e| {
                tracing::error!("Failed to delete feature flag: {e}");
                AppError::InternalError
            })?;

        if result.rows_affected() == 0 {
            return Err(AppError::FeatureFlagNotFound);
        }

        Ok(())
    }

    // ─── Admin Management ───────────────────────────────────────────────────────

    /// List all admin users.
    ///
    /// Requirement 4.60
    pub async fn list_admins(&self) -> Result<Vec<AdminUserInfo>, AppError> {
        let admins: Vec<AdminUserInfo> = sqlx::query_as(
            r#"
            SELECT id, email, first_name, last_name, created_at
            FROM users
            WHERE role = 'admin'
            ORDER BY created_at ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to list admins: {e}");
            AppError::InternalError
        })?;

        Ok(admins)
    }

    /// Demote an admin to a regular user.
    ///
    /// Blocks self-demote and removal of the last admin.
    ///
    /// Requirements: 4.61, 4.62, 4.63
    pub async fn demote_admin(
        &self,
        admin_id: Uuid,
        target_id: Uuid,
    ) -> Result<(), AppError> {
        // Block self-demote
        if admin_id == target_id {
            return Err(AppError::CannotDemoteSelf);
        }

        // Check the target is actually an admin
        let target_role: Option<String> =
            sqlx::query_scalar("SELECT role FROM users WHERE id = $1")
                .bind(target_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to check target admin role: {e}");
                    AppError::InternalError
                })?;

        match target_role.as_deref() {
            None => return Err(AppError::UserNotFound),
            Some(role) if role != "admin" => return Err(AppError::UserNotFound),
            _ => {}
        }

        // Block removal of the last admin
        let admin_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE role = 'admin'")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to count admins: {e}");
                    AppError::InternalError
                })?;

        if admin_count <= 1 {
            return Err(AppError::LastAdminCannotBeRemoved);
        }

        // Set role to 'user'
        sqlx::query("UPDATE users SET role = 'user', updated_at = NOW() WHERE id = $1")
            .bind(target_id)
            .execute(&self.pool)
            .await
            .map_err(|e| {
                tracing::error!("Failed to demote admin: {e}");
                AppError::InternalError
            })?;

        Ok(())
    }

    // ─── Stats ──────────────────────────────────────────────────────────────────

    /// Get high-level platform overview statistics computed on-demand.
    ///
    /// Requirements: 4.64, 4.65
    pub async fn get_overview_stats(&self) -> Result<OverviewStats, AppError> {
        let total_users: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE deleted_at IS NULL")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to count total users: {e}");
                    AppError::InternalError
                })?;

        let active_users: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM users WHERE status = 'active' AND deleted_at IS NULL",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to count active users: {e}");
            AppError::InternalError
        })?;

        let suspended_users: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM users WHERE status = 'suspended' AND deleted_at IS NULL",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to count suspended users: {e}");
            AppError::InternalError
        })?;

        let total_workspaces: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM workspaces")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to count workspaces: {e}");
                    AppError::InternalError
                })?;

        let individual_workspaces: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM workspaces WHERE type = 'individual'",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to count individual workspaces: {e}");
            AppError::InternalError
        })?;

        let team_workspaces: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM workspaces WHERE type = 'team'")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to count team workspaces: {e}");
                    AppError::InternalError
                })?;

        let subscriptions_free: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM subscriptions WHERE tier = 'free'")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to count free subscriptions: {e}");
                    AppError::InternalError
                })?;

        let subscriptions_pro: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM subscriptions WHERE tier = 'pro'")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to count pro subscriptions: {e}");
                    AppError::InternalError
                })?;

        let subscriptions_teams: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM subscriptions WHERE tier = 'teams'")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to count teams subscriptions: {e}");
                    AppError::InternalError
                })?;

        let total_snippets: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM snippets WHERE deleted_at IS NULL",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to count snippets: {e}");
            AppError::InternalError
        })?;

        let total_folders: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM folders WHERE deleted_at IS NULL",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to count folders: {e}");
            AppError::InternalError
        })?;

        Ok(OverviewStats {
            total_users,
            active_users,
            suspended_users,
            total_workspaces,
            individual_workspaces,
            team_workspaces,
            subscriptions_free,
            subscriptions_pro,
            subscriptions_teams,
            total_snippets,
            total_folders,
        })
    }

    /// Get referral analytics computed on-demand.
    ///
    /// Includes total, converted, pending referrals, conversion rate,
    /// and top referrers.
    ///
    /// Requirements: 4.66, 4.67
    pub async fn get_referral_analytics(&self) -> Result<ReferralAnalytics, AppError> {
        let total_referrals: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM referrals")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to count referrals: {e}");
                    AppError::InternalError
                })?;

        let converted_referrals: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM referrals WHERE status = 'converted'")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to count converted referrals: {e}");
                    AppError::InternalError
                })?;

        let pending_referrals: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM referrals WHERE status = 'pending'")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to count pending referrals: {e}");
                    AppError::InternalError
                })?;

        let conversion_rate = if total_referrals > 0 {
            (converted_referrals as f64 / total_referrals as f64) * 100.0
        } else {
            0.0
        };

        let top_referrers: Vec<TopReferrer> = sqlx::query_as(
            r#"
            SELECT
                u.id AS user_id,
                u.email,
                COUNT(r.id) AS referral_count,
                COUNT(CASE WHEN r.status = 'converted' THEN 1 END) AS converted_count
            FROM referrals r
            INNER JOIN users u ON u.id = r.referrer_id
            GROUP BY u.id, u.email
            ORDER BY converted_count DESC, referral_count DESC
            LIMIT 10
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to fetch top referrers: {e}");
            AppError::InternalError
        })?;

        Ok(ReferralAnalytics {
            total_referrals,
            converted_referrals,
            pending_referrals,
            conversion_rate,
            top_referrers,
        })
    }

    // ─── Referral Statistics ────────────────────────────────────────────────────

    /// Get referral statistics computed on-demand from the referrals table.
    ///
    /// Requirement 4.33
    pub async fn get_referral_stats(&self) -> Result<ReferralStatsResponse, AppError> {
        let total_referrals: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM referrals")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to count referrals: {e}");
                    AppError::InternalError
                })?;

        let converted_referrals: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM referrals WHERE status = 'converted'")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to count converted referrals: {e}");
                    AppError::InternalError
                })?;

        let conversion_rate = if total_referrals > 0 {
            (converted_referrals as f64 / total_referrals as f64) * 100.0
        } else {
            0.0
        };

        // Top 10 referrers by converted count
        let top_referrers: Vec<TopReferrer> = sqlx::query_as(
            r#"
            SELECT
                u.id AS user_id,
                u.email,
                COUNT(r.id) AS referral_count,
                COUNT(CASE WHEN r.status = 'converted' THEN 1 END) AS converted_count
            FROM referrals r
            INNER JOIN users u ON u.id = r.referrer_id
            GROUP BY u.id, u.email
            ORDER BY converted_count DESC, referral_count DESC
            LIMIT 10
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to fetch top referrers: {e}");
            AppError::InternalError
        })?;

        Ok(ReferralStatsResponse {
            total_referrals,
            converted_referrals,
            conversion_rate,
            top_referrers,
        })
    }

    // ─── Subscription & Billing Oversight ───────────────────────────────────────

    /// List subscriptions with pagination and optional filters.
    ///
    /// Supports filtering by tier, status, workspace_id, and period_end range.
    ///
    /// Requirement 4.34
    pub async fn list_subscriptions(
        &self,
        pagination: &Pagination,
        filters: &SubscriptionFilters,
    ) -> Result<PaginatedResponse<AdminSubscriptionSummary>, AppError> {
        let offset = pagination.offset();
        let limit = pagination.limit();

        let total: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)
            FROM subscriptions
            WHERE ($1::text IS NULL OR tier = $1)
              AND ($2::text IS NULL OR status = $2)
              AND ($3::uuid IS NULL OR workspace_id = $3)
              AND ($4::timestamptz IS NULL OR period_end >= $4)
              AND ($5::timestamptz IS NULL OR period_end <= $5)
            "#,
        )
        .bind(filters.tier.as_deref())
        .bind(filters.status.as_deref())
        .bind(filters.workspace_id)
        .bind(filters.period_end_after)
        .bind(filters.period_end_before)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to count subscriptions: {e}");
            AppError::InternalError
        })?;

        let subscriptions: Vec<AdminSubscriptionSummary> = sqlx::query_as(
            r#"
            SELECT id, workspace_id, tier, status, period_start, period_end, created_at
            FROM subscriptions
            WHERE ($1::text IS NULL OR tier = $1)
              AND ($2::text IS NULL OR status = $2)
              AND ($3::uuid IS NULL OR workspace_id = $3)
              AND ($4::timestamptz IS NULL OR period_end >= $4)
              AND ($5::timestamptz IS NULL OR period_end <= $5)
            ORDER BY created_at DESC
            LIMIT $6 OFFSET $7
            "#,
        )
        .bind(filters.tier.as_deref())
        .bind(filters.status.as_deref())
        .bind(filters.workspace_id)
        .bind(filters.period_end_after)
        .bind(filters.period_end_before)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to list subscriptions: {e}");
            AppError::InternalError
        })?;

        Ok(PaginatedResponse::new(subscriptions, total, pagination))
    }

    /// Get full subscription detail including billing event history.
    ///
    /// Requirement 4.35
    pub async fn get_subscription(
        &self,
        subscription_id: Uuid,
    ) -> Result<AdminSubscriptionDetail, AppError> {
        let row: SubscriptionDetailRow = sqlx::query_as(
            r#"
            SELECT
                id, workspace_id, tier, status, period_start, period_end,
                grace_period_end, payment_deadline, cancelled_at,
                external_subscription_id, created_at, updated_at
            FROM subscriptions
            WHERE id = $1
            "#,
        )
        .bind(subscription_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to fetch subscription: {e}");
            AppError::InternalError
        })?
        .ok_or(AppError::SubscriptionNotFound)?;

        let billing_events: Vec<BillingEventResponse> = sqlx::query_as(
            r#"
            SELECT id, external_event_id, event_type, workspace_id, payload, processed_at, created_at
            FROM billing_events
            WHERE workspace_id = $1
            ORDER BY created_at DESC
            "#,
        )
        .bind(row.workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to fetch billing events for subscription: {e}");
            AppError::InternalError
        })?;

        Ok(AdminSubscriptionDetail {
            id: row.id,
            workspace_id: row.workspace_id,
            tier: row.tier,
            status: row.status,
            period_start: row.period_start,
            period_end: row.period_end,
            grace_period_end: row.grace_period_end,
            payment_deadline: row.payment_deadline,
            cancelled_at: row.cancelled_at,
            external_subscription_id: row.external_subscription_id,
            created_at: row.created_at,
            updated_at: row.updated_at,
            billing_events,
        })
    }

    /// Extend a subscription's period_end by the specified months and/or days.
    ///
    /// If `period_end` is NULL, starts from the current time.
    /// Audit log entry should be written by the handler layer.
    ///
    /// Requirement 4.36
    pub async fn extend_subscription(
        &self,
        subscription_id: Uuid,
        req: &ExtendSubscriptionRequest,
    ) -> Result<AdminSubscriptionSummary, AppError> {
        // Fetch current period_end (or use now if NULL)
        let current_period_end: Option<DateTime<Utc>> = sqlx::query_scalar(
            "SELECT period_end FROM subscriptions WHERE id = $1",
        )
        .bind(subscription_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to fetch subscription period_end: {e}");
            AppError::InternalError
        })?
        .ok_or(AppError::SubscriptionNotFound)?;

        let base = current_period_end.unwrap_or_else(Utc::now);

        // Add months first, then days
        let mut new_period_end = base;
        if let Some(months) = req.months {
            if months > 0 {
                new_period_end = new_period_end
                    .checked_add_months(Months::new(months as u32))
                    .unwrap_or(new_period_end);
            }
        }
        if let Some(days) = req.days {
            if days > 0 {
                new_period_end = new_period_end + chrono::Duration::days(days as i64);
            }
        }

        let subscription: AdminSubscriptionSummary = sqlx::query_as(
            r#"
            UPDATE subscriptions
            SET period_end = $2, updated_at = NOW()
            WHERE id = $1
            RETURNING id, workspace_id, tier, status, period_start, period_end, created_at
            "#,
        )
        .bind(subscription_id)
        .bind(new_period_end)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to extend subscription: {e}");
            AppError::InternalError
        })?;

        // NOTE: Audit log entry (action: "extend_subscription") is written by the handler layer.

        Ok(subscription)
    }

    /// Cancel a subscription immediately (no grace period).
    ///
    /// Sets status='cancelled', cancelled_at=NOW(), clears grace_period_end,
    /// and closes all WebSocket connections for workspace members.
    ///
    /// Requirement 4.37
    pub async fn cancel_subscription(
        &self,
        subscription_id: Uuid,
    ) -> Result<AdminSubscriptionSummary, AppError> {
        // Fetch subscription to get workspace_id and verify existence
        let workspace_id: Uuid = sqlx::query_scalar(
            "SELECT workspace_id FROM subscriptions WHERE id = $1",
        )
        .bind(subscription_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to fetch subscription for cancellation: {e}");
            AppError::InternalError
        })?
        .ok_or(AppError::SubscriptionNotFound)?;

        // Set status to cancelled, set cancelled_at, clear grace_period_end (immediate cancellation)
        let subscription: AdminSubscriptionSummary = sqlx::query_as(
            r#"
            UPDATE subscriptions
            SET status = 'cancelled',
                cancelled_at = NOW(),
                grace_period_end = NULL,
                updated_at = NOW()
            WHERE id = $1
            RETURNING id, workspace_id, tier, status, period_start, period_end, created_at
            "#,
        )
        .bind(subscription_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to cancel subscription: {e}");
            AppError::InternalError
        })?;

        // Soft-lock: close all WS connections for workspace members
        if let Some(ref registry) = self.session_registry {
            registry.close_workspace_sessions(
                workspace_id,
                1008,
                "Subscription cancelled".to_string(),
            );
        }

        Ok(subscription)
    }

    /// Override a subscription's tier without billing provider interaction.
    ///
    /// Validates tier is one of 'free', 'pro', 'teams'. Returns `AppError::InvalidTier`
    /// on invalid tier value.
    ///
    /// Requirement 4.38
    pub async fn override_tier(
        &self,
        subscription_id: Uuid,
        req: &OverrideTierRequest,
    ) -> Result<AdminSubscriptionSummary, AppError> {
        // Validate tier value
        match req.tier.as_str() {
            "free" | "pro" | "teams" => {}
            _ => return Err(AppError::InvalidTier),
        }

        // Verify subscription exists
        let exists: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM subscriptions WHERE id = $1")
                .bind(subscription_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to check subscription existence: {e}");
                    AppError::InternalError
                })?;

        if exists.is_none() {
            return Err(AppError::SubscriptionNotFound);
        }

        let subscription: AdminSubscriptionSummary = sqlx::query_as(
            r#"
            UPDATE subscriptions
            SET tier = $2, updated_at = NOW()
            WHERE id = $1
            RETURNING id, workspace_id, tier, status, period_start, period_end, created_at
            "#,
        )
        .bind(subscription_id)
        .bind(&req.tier)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to override subscription tier: {e}");
            AppError::InternalError
        })?;

        Ok(subscription)
    }

    /// List billing events with pagination and optional filters.
    ///
    /// Supports filtering by workspace_id and event_type.
    ///
    /// Requirements: 4.39, 4.40, 4.41
    pub async fn list_billing_events(
        &self,
        pagination: &Pagination,
        filters: &BillingEventFilters,
    ) -> Result<PaginatedResponse<BillingEventResponse>, AppError> {
        let offset = pagination.offset();
        let limit = pagination.limit();

        let total: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)
            FROM billing_events
            WHERE ($1::uuid IS NULL OR workspace_id = $1)
              AND ($2::text IS NULL OR event_type = $2)
            "#,
        )
        .bind(filters.workspace_id)
        .bind(filters.event_type.as_deref())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to count billing events: {e}");
            AppError::InternalError
        })?;

        let events: Vec<BillingEventResponse> = sqlx::query_as(
            r#"
            SELECT id, external_event_id, event_type, workspace_id, payload, processed_at, created_at
            FROM billing_events
            WHERE ($1::uuid IS NULL OR workspace_id = $1)
              AND ($2::text IS NULL OR event_type = $2)
            ORDER BY created_at DESC
            LIMIT $3 OFFSET $4
            "#,
        )
        .bind(filters.workspace_id)
        .bind(filters.event_type.as_deref())
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to list billing events: {e}");
            AppError::InternalError
        })?;

        Ok(PaginatedResponse::new(events, total, pagination))
    }

    // ─── Helpers ────────────────────────────────────────────────────────────────

    /// Validate that a string is valid kebab-case.
    ///
    /// Rules: starts with a-z, then segments of a-z0-9 separated by single hyphens,
    /// no leading/trailing hyphens, max 100 characters.
    /// Pattern: `^[a-z][a-z0-9]*(-[a-z0-9]+)*$`
    fn is_valid_kebab_case(name: &str) -> bool {
        if name.is_empty() || name.len() > 100 {
            return false;
        }

        let bytes = name.as_bytes();

        // Must start with a-z
        if !bytes[0].is_ascii_lowercase() {
            return false;
        }

        let mut prev_was_hyphen = false;
        for &b in &bytes[1..] {
            match b {
                b'a'..=b'z' | b'0'..=b'9' => {
                    prev_was_hyphen = false;
                }
                b'-' => {
                    if prev_was_hyphen {
                        return false; // no consecutive hyphens
                    }
                    prev_was_hyphen = true;
                }
                _ => return false, // invalid character
            }
        }

        // Must not end with a hyphen
        !prev_was_hyphen
    }
}
