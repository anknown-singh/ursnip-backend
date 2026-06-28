//! HTTP handlers for all admin endpoints.
//!
//! Each handler extracts the request body/params, delegates to the appropriate
//! admin service method, writes an audit log entry, and formats the HTTP response.
//! Audit logging is best-effort — failures do not abort the request.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, Query},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::admin::service::{
    AdminService, AuditLogFilters, BillingEventFilters, CouponFilters,
    CreateCouponRequest, CreateDiscountRequest, CreateFeatureFlagRequest, CreateTaxRateRequest,
    ExtendSubscriptionRequest, OverrideTierRequest, SubscriptionFilters,
    UpdateCouponRequest, UpdateDiscountRequest, UpdateFeatureFlagRequest, UpdateTaxRateRequest,
    UserFilters, WorkspaceFilters,
};
use crate::errors::AppError;
use crate::middleware::auth_extractor::AccessTokenClaims;
use crate::models::common::Pagination;

// ─── Helper ─────────────────────────────────────────────────────────────────────

/// Write an audit log entry (best-effort).
async fn write_audit_log(
    pool: &PgPool,
    admin_id: Uuid,
    action: &str,
    target_resource: &str,
    target_id: &str,
) {
    sqlx::query(
        r#"
        INSERT INTO audit_logs (admin_id, action, target_resource, target_id, result, trace_id)
        VALUES ($1, $2, $3, $4, 'success', $5)
        "#,
    )
    .bind(admin_id)
    .bind(action)
    .bind(target_resource)
    .bind(target_id)
    .bind(Uuid::new_v4())
    .execute(pool)
    .await
    .ok();
}

// ─── Delete Workspace Request ───────────────────────────────────────────────────

/// Query parameters for workspace deletion (requires confirm=true).
#[derive(Debug, Deserialize)]
pub struct DeleteWorkspaceQuery {
    pub confirm: Option<bool>,
}


// ─── User Handlers ──────────────────────────────────────────────────────────────

/// GET /admin/users
///
/// List users with pagination and optional filters.
pub async fn list_users_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Query(pagination): Query<Pagination>,
    Query(filters): Query<UserFilters>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.list_users(&pagination, &filters).await?;
    write_audit_log(&pool, claims.sub, "list_users", "user", "all").await;
    Ok(Json(response))
}

/// GET /admin/users/{user_id}
///
/// Get detailed user information.
pub async fn get_user_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Path(user_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.get_user(user_id).await?;
    write_audit_log(&pool, claims.sub, "get_user", "user", &user_id.to_string()).await;
    Ok(Json(response))
}

/// POST /admin/users/{user_id}/suspend
///
/// Suspend a user account.
pub async fn suspend_user_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Path(user_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.suspend_user(claims.sub, user_id).await?;
    write_audit_log(&pool, claims.sub, "suspend_user", "user", &user_id.to_string()).await;
    Ok(Json(response))
}

/// POST /admin/users/{user_id}/unsuspend
///
/// Unsuspend a user account.
pub async fn unsuspend_user_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Path(user_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.unsuspend_user(claims.sub, user_id).await?;
    write_audit_log(&pool, claims.sub, "unsuspend_user", "user", &user_id.to_string()).await;
    Ok(Json(response))
}

/// POST /admin/users/{user_id}/force-password-reset
///
/// Force a password reset on a user account.
pub async fn force_password_reset_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Path(user_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    service.force_password_reset(claims.sub, user_id).await?;
    write_audit_log(&pool, claims.sub, "force_password_reset", "user", &user_id.to_string()).await;
    Ok(StatusCode::NO_CONTENT)
}

/// DELETE /admin/users/{user_id}
///
/// Soft-delete a user account.
pub async fn delete_user_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Path(user_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    service.delete_user(claims.sub, user_id).await?;
    write_audit_log(&pool, claims.sub, "delete_user", "user", &user_id.to_string()).await;
    Ok(StatusCode::NO_CONTENT)
}

// ─── Workspace Handlers ─────────────────────────────────────────────────────────

/// GET /admin/workspaces
///
/// List workspaces with pagination and optional filters.
pub async fn list_workspaces_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Query(pagination): Query<Pagination>,
    Query(filters): Query<WorkspaceFilters>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.list_workspaces(&pagination, &filters).await?;
    write_audit_log(&pool, claims.sub, "list_workspaces", "workspace", "all").await;
    Ok(Json(response))
}

/// GET /admin/workspaces/{workspace_id}
///
/// Get detailed workspace information.
pub async fn get_workspace_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Path(workspace_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.get_workspace(workspace_id).await?;
    write_audit_log(&pool, claims.sub, "get_workspace", "workspace", &workspace_id.to_string()).await;
    Ok(Json(response))
}

/// POST /admin/workspaces/{workspace_id}/deactivate
///
/// Deactivate a workspace (sets subscription to deactivated, closes WS connections).
pub async fn deactivate_workspace_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Path(workspace_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    service.deactivate_workspace(workspace_id).await?;
    write_audit_log(&pool, claims.sub, "deactivate_workspace", "workspace", &workspace_id.to_string()).await;
    Ok(StatusCode::NO_CONTENT)
}

/// DELETE /admin/workspaces/{workspace_id}
///
/// Hard-delete a workspace (requires confirm=true query parameter).
pub async fn delete_workspace_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Path(workspace_id): Path<Uuid>,
    Query(query): Query<DeleteWorkspaceQuery>,
) -> Result<impl IntoResponse, AppError> {
    let confirm = query.confirm.unwrap_or(false);
    service.delete_workspace(workspace_id, confirm).await?;
    write_audit_log(&pool, claims.sub, "delete_workspace", "workspace", &workspace_id.to_string()).await;
    Ok(StatusCode::NO_CONTENT)
}

// ─── Discount Handlers ──────────────────────────────────────────────────────────

/// GET /admin/discounts
///
/// List all discounts.
pub async fn list_discounts_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.list_discounts().await?;
    write_audit_log(&pool, claims.sub, "list_discounts", "discount", "all").await;
    Ok(Json(response))
}

/// POST /admin/discounts
///
/// Create a new discount.
pub async fn create_discount_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Json(body): Json<CreateDiscountRequest>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.create_discount(&body).await?;
    write_audit_log(&pool, claims.sub, "create_discount", "discount", &response.id.to_string()).await;
    Ok((StatusCode::CREATED, Json(response)))
}

/// PATCH /admin/discounts/{id}
///
/// Update an existing discount.
pub async fn update_discount_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateDiscountRequest>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.update_discount(id, &body).await?;
    write_audit_log(&pool, claims.sub, "update_discount", "discount", &id.to_string()).await;
    Ok(Json(response))
}


// ─── Coupon Handlers ────────────────────────────────────────────────────────────

/// GET /admin/coupons
///
/// List coupons with pagination and optional filters.
pub async fn list_coupons_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Query(pagination): Query<Pagination>,
    Query(filters): Query<CouponFilters>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.list_coupons(&pagination, &filters).await?;
    write_audit_log(&pool, claims.sub, "list_coupons", "coupon", "all").await;
    Ok(Json(response))
}

/// GET /admin/coupons/{id}
///
/// Get a single coupon by ID.
pub async fn get_coupon_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.get_coupon(id).await?;
    write_audit_log(&pool, claims.sub, "get_coupon", "coupon", &id.to_string()).await;
    Ok(Json(response))
}

/// POST /admin/coupons
///
/// Create a new platform coupon.
pub async fn create_coupon_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Json(body): Json<CreateCouponRequest>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.create_coupon(&body).await?;
    write_audit_log(&pool, claims.sub, "create_coupon", "coupon", &response.id.to_string()).await;
    Ok((StatusCode::CREATED, Json(response)))
}

/// PATCH /admin/coupons/{id}
///
/// Update an existing coupon.
pub async fn update_coupon_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateCouponRequest>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.update_coupon(id, &body).await?;
    write_audit_log(&pool, claims.sub, "update_coupon", "coupon", &id.to_string()).await;
    Ok(Json(response))
}

// ─── Referral Handlers ──────────────────────────────────────────────────────────

/// GET /admin/referrals
///
/// Get referral statistics.
pub async fn get_referral_stats_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.get_referral_stats().await?;
    write_audit_log(&pool, claims.sub, "get_referral_stats", "referral", "all").await;
    Ok(Json(response))
}

// ─── Subscription Handlers ──────────────────────────────────────────────────────

/// GET /admin/subscriptions
///
/// List subscriptions with pagination and optional filters.
pub async fn list_subscriptions_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Query(pagination): Query<Pagination>,
    Query(filters): Query<SubscriptionFilters>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.list_subscriptions(&pagination, &filters).await?;
    write_audit_log(&pool, claims.sub, "list_subscriptions", "subscription", "all").await;
    Ok(Json(response))
}

/// GET /admin/subscriptions/{id}
///
/// Get detailed subscription information.
pub async fn get_subscription_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.get_subscription(id).await?;
    write_audit_log(&pool, claims.sub, "get_subscription", "subscription", &id.to_string()).await;
    Ok(Json(response))
}

/// POST /admin/subscriptions/{id}/extend
///
/// Extend a subscription's period_end.
pub async fn extend_subscription_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Path(id): Path<Uuid>,
    Json(body): Json<ExtendSubscriptionRequest>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.extend_subscription(id, &body).await?;
    write_audit_log(&pool, claims.sub, "extend_subscription", "subscription", &id.to_string()).await;
    Ok(Json(response))
}

/// POST /admin/subscriptions/{id}/cancel
///
/// Cancel a subscription immediately.
pub async fn cancel_subscription_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.cancel_subscription(id).await?;
    write_audit_log(&pool, claims.sub, "cancel_subscription", "subscription", &id.to_string()).await;
    Ok(Json(response))
}

/// PATCH /admin/subscriptions/{id}/tier
///
/// Override a subscription's tier.
pub async fn override_tier_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Path(id): Path<Uuid>,
    Json(body): Json<OverrideTierRequest>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.override_tier(id, &body).await?;
    write_audit_log(&pool, claims.sub, "override_tier", "subscription", &id.to_string()).await;
    Ok(Json(response))
}

// ─── Billing Event Handlers ─────────────────────────────────────────────────────

/// GET /admin/billing-events
///
/// List billing events with pagination and optional filters.
pub async fn list_billing_events_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Query(pagination): Query<Pagination>,
    Query(filters): Query<BillingEventFilters>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.list_billing_events(&pagination, &filters).await?;
    write_audit_log(&pool, claims.sub, "list_billing_events", "billing_event", "all").await;
    Ok(Json(response))
}

// ─── Tax Rate Handlers ──────────────────────────────────────────────────────────

/// GET /admin/tax-rates
///
/// List all tax rates.
pub async fn list_tax_rates_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.list_tax_rates().await?;
    write_audit_log(&pool, claims.sub, "list_tax_rates", "tax_rate", "all").await;
    Ok(Json(response))
}

/// POST /admin/tax-rates
///
/// Create a new tax rate.
pub async fn create_tax_rate_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Json(body): Json<CreateTaxRateRequest>,
) -> Result<impl IntoResponse, AppError> {
    let country_code = body.country_code.clone();
    let response = service.create_tax_rate(&body).await?;
    write_audit_log(&pool, claims.sub, "create_tax_rate", "tax_rate", &country_code).await;
    Ok((StatusCode::CREATED, Json(response)))
}

/// PATCH /admin/tax-rates/{country_code}
///
/// Update an existing tax rate.
pub async fn update_tax_rate_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Path(country_code): Path<String>,
    Json(body): Json<UpdateTaxRateRequest>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.update_tax_rate(&country_code, &body).await?;
    write_audit_log(&pool, claims.sub, "update_tax_rate", "tax_rate", &country_code).await;
    Ok(Json(response))
}

// ─── Audit Log Handlers ─────────────────────────────────────────────────────────

/// GET /admin/audit-logs
///
/// List audit logs with pagination and optional filters.
pub async fn list_audit_logs_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Query(pagination): Query<Pagination>,
    Query(filters): Query<AuditLogFilters>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.list_audit_logs(&pagination, &filters).await?;
    write_audit_log(&pool, claims.sub, "list_audit_logs", "audit_log", "all").await;
    Ok(Json(response))
}

/// GET /admin/audit-logs/{id}
///
/// Get a single audit log entry.
pub async fn get_audit_log_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.get_audit_log(id).await?;
    write_audit_log(&pool, claims.sub, "get_audit_log", "audit_log", &id.to_string()).await;
    Ok(Json(response))
}

// ─── Feature Flag Handlers ──────────────────────────────────────────────────────

/// GET /admin/feature-flags
///
/// List all feature flags.
pub async fn list_feature_flags_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.list_feature_flags().await?;
    write_audit_log(&pool, claims.sub, "list_feature_flags", "feature_flag", "all").await;
    Ok(Json(response))
}

/// POST /admin/feature-flags
///
/// Create a new feature flag.
pub async fn create_feature_flag_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Json(body): Json<CreateFeatureFlagRequest>,
) -> Result<impl IntoResponse, AppError> {
    let name = body.name.clone();
    let response = service.create_feature_flag(&body).await?;
    write_audit_log(&pool, claims.sub, "create_feature_flag", "feature_flag", &name).await;
    Ok((StatusCode::CREATED, Json(response)))
}

/// PUT /admin/feature-flags/{name}
///
/// Update a feature flag.
pub async fn update_feature_flag_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Path(name): Path<String>,
    Json(body): Json<UpdateFeatureFlagRequest>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.update_feature_flag(&name, &body).await?;
    write_audit_log(&pool, claims.sub, "update_feature_flag", "feature_flag", &name).await;
    Ok(Json(response))
}

/// DELETE /admin/feature-flags/{name}
///
/// Delete a feature flag.
pub async fn delete_feature_flag_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    service.delete_feature_flag(&name).await?;
    write_audit_log(&pool, claims.sub, "delete_feature_flag", "feature_flag", &name).await;
    Ok(StatusCode::NO_CONTENT)
}

// ─── Admin Management Handlers ──────────────────────────────────────────────────

/// GET /admin/admins
///
/// List all admin users.
pub async fn list_admins_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.list_admins().await?;
    write_audit_log(&pool, claims.sub, "list_admins", "admin", "all").await;
    Ok(Json(response))
}

/// DELETE /admin/admins/{admin_id}
///
/// Demote an admin to regular user.
pub async fn demote_admin_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
    Path(admin_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    service.demote_admin(claims.sub, admin_id).await?;
    write_audit_log(&pool, claims.sub, "demote_admin", "admin", &admin_id.to_string()).await;
    Ok(StatusCode::NO_CONTENT)
}

// ─── Stats Handlers ─────────────────────────────────────────────────────────────

/// GET /admin/stats/overview
///
/// Get platform overview statistics.
pub async fn overview_stats_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.get_overview_stats().await?;
    write_audit_log(&pool, claims.sub, "overview_stats", "stats", "overview").await;
    Ok(Json(response))
}

/// GET /admin/stats/referrals
///
/// Get referral analytics.
pub async fn referral_analytics_handler(
    Extension(service): Extension<Arc<AdminService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Extension(pool): Extension<PgPool>,
) -> Result<impl IntoResponse, AppError> {
    let response = service.get_referral_analytics().await?;
    write_audit_log(&pool, claims.sub, "referral_analytics", "stats", "referrals").await;
    Ok(Json(response))
}
