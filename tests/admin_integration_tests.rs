//! Integration tests for the Admin Service.
//!
//! These tests exercise the full HTTP request/response cycle through the Axum
//! router, covering user management, workspace management, coupon/discount CRUD,
//! subscription management, feature flag CRUD, and admin demote logic.
//!
//! Requirements: 4.1–4.67
//!
//! Run with: `cargo test --test admin_integration_tests -- --ignored`
//!
//! Requires a test PostgreSQL database. Set DATABASE_URL env var.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::Extension,
    http::{Request, StatusCode},
    middleware,
    routing::{delete, get, patch, post, put},
    Router,
};
use chrono::Utc;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use ursnip_backend::admin::handlers::*;
use ursnip_backend::admin::service::AdminService;
use ursnip_backend::middleware::admin_guard::admin_guard;
use ursnip_backend::middleware::auth_extractor::AccessTokenClaims;
use ursnip_backend::models::common::{ClientType, Role, Tier};
use ursnip_backend::sync::session_registry::SessionRegistry;

// ─── Test Constants ─────────────────────────────────────────────────────────────

const TEST_JWT_SECRET: &str = "test-admin-integration-jwt-secret";

// ─── Test Helpers ───────────────────────────────────────────────────────────────

/// Create admin JWT claims for a given user ID.
fn admin_claims(user_id: Uuid) -> AccessTokenClaims {
    AccessTokenClaims {
        sub: user_id,
        client_type: ClientType::Web,
        role: Role::Admin,
        permissions: vec!["admin:all".to_string()],
        subscription_tier: Tier::Pro,
        status: "active".to_string(),
        must_reset_password: false,
        exp: Utc::now().timestamp() + 3600,
    }
}

/// Generate a signed JWT token for test requests.
fn make_token(claims: &AccessTokenClaims) -> String {
    encode(
        &Header::new(Algorithm::HS256),
        claims,
        &EncodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
    )
    .unwrap()
}

/// Build the admin test router with full middleware stack.
fn build_test_app(pool: PgPool) -> Router {
    let session_registry = Arc::new(SessionRegistry::new(10_000));
    let admin_service = Arc::new(AdminService::new(pool.clone(), Some(session_registry)));
    let jwt_secret = Arc::new(TEST_JWT_SECRET.to_string());

    Router::new()
        // Users
        .route("/admin/users", get(list_users_handler))
        .route(
            "/admin/users/:user_id",
            get(get_user_handler).delete(delete_user_handler),
        )
        .route("/admin/users/:user_id/suspend", post(suspend_user_handler))
        .route(
            "/admin/users/:user_id/unsuspend",
            post(unsuspend_user_handler),
        )
        .route(
            "/admin/users/:user_id/force-password-reset",
            post(force_password_reset_handler),
        )
        // Workspaces
        .route("/admin/workspaces", get(list_workspaces_handler))
        .route(
            "/admin/workspaces/:workspace_id",
            get(get_workspace_handler).delete(delete_workspace_handler),
        )
        .route(
            "/admin/workspaces/:workspace_id/deactivate",
            post(deactivate_workspace_handler),
        )
        // Discounts
        .route(
            "/admin/discounts",
            get(list_discounts_handler).post(create_discount_handler),
        )
        .route("/admin/discounts/:id", patch(update_discount_handler))
        // Coupons
        .route(
            "/admin/coupons",
            get(list_coupons_handler).post(create_coupon_handler),
        )
        .route(
            "/admin/coupons/:id",
            get(get_coupon_handler).patch(update_coupon_handler),
        )
        // Subscriptions
        .route("/admin/subscriptions", get(list_subscriptions_handler))
        .route("/admin/subscriptions/:id", get(get_subscription_handler))
        .route(
            "/admin/subscriptions/:id/extend",
            post(extend_subscription_handler),
        )
        .route(
            "/admin/subscriptions/:id/cancel",
            post(cancel_subscription_handler),
        )
        .route("/admin/subscriptions/:id/tier", patch(override_tier_handler))
        // Feature Flags
        .route(
            "/admin/feature-flags",
            get(list_feature_flags_handler).post(create_feature_flag_handler),
        )
        .route(
            "/admin/feature-flags/:name",
            put(update_feature_flag_handler).delete(delete_feature_flag_handler),
        )
        // Admin Management
        .route("/admin/admins", get(list_admins_handler))
        .route("/admin/admins/:admin_id", delete(demote_admin_handler))
        .layer(Extension(admin_service))
        .layer(Extension(pool.clone()))
        .layer(middleware::from_fn(admin_guard))
        .layer(middleware::from_fn(move |req, next| {
            let secret = jwt_secret.clone();
            ursnip_backend::middleware::auth_extractor::auth_middleware(req, next, secret)
        }))
}

/// Create a test user directly in the database, returning the user ID.
async fn create_test_user(pool: &PgPool, email: &str, role: &str) -> Uuid {
    let user_id = Uuid::new_v4();
    let referral_code = format!("ref_{}", Uuid::new_v4().to_string().split('-').next().unwrap());
    sqlx::query(
        r#"INSERT INTO users (id, email, password_hash, role, status, referral_code)
           VALUES ($1, $2, 'hashed_pw', $3, 'active', $4)"#,
    )
    .bind(user_id)
    .bind(email)
    .bind(role)
    .bind(&referral_code)
    .execute(pool)
    .await
    .unwrap();
    user_id
}

/// Create a workspace and subscription for a user, returning (workspace_id, subscription_id).
async fn create_workspace_with_subscription(
    pool: &PgPool,
    owner_id: Uuid,
    ws_type: &str,
    tier: &str,
    status: &str,
) -> (Uuid, Uuid) {
    let workspace_id = Uuid::new_v4();
    let subscription_id = Uuid::new_v4();

    sqlx::query(
        r#"INSERT INTO workspaces (id, type, owner_id, name)
           VALUES ($1, $2, $3, $4)"#,
    )
    .bind(workspace_id)
    .bind(ws_type)
    .bind(owner_id)
    .bind(format!("{}'s workspace", ws_type))
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        r#"INSERT INTO workspace_members (workspace_id, user_id, role)
           VALUES ($1, $2, 'owner')"#,
    )
    .bind(workspace_id)
    .bind(owner_id)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        r#"INSERT INTO subscriptions (id, workspace_id, tier, status, period_end)
           VALUES ($1, $2, $3, $4, NOW() + INTERVAL '30 days')"#,
    )
    .bind(subscription_id)
    .bind(workspace_id)
    .bind(tier)
    .bind(status)
    .execute(pool)
    .await
    .unwrap();

    (workspace_id, subscription_id)
}

/// Helper to make an authenticated GET request.
fn auth_get(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("Authorization", format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap()
}

/// Helper to make an authenticated POST request with JSON body.
fn auth_post(uri: &str, token: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("Authorization", format!("Bearer {}", token))
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

/// Helper to make an authenticated PATCH request with JSON body.
fn auth_patch(uri: &str, token: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("PATCH")
        .uri(uri)
        .header("Authorization", format!("Bearer {}", token))
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

/// Helper to make an authenticated PUT request with JSON body.
fn auth_put(uri: &str, token: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header("Authorization", format!("Bearer {}", token))
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

/// Helper to make an authenticated DELETE request.
fn auth_delete(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .header("Authorization", format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap()
}

/// Extract response body as JSON Value.
async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

/// Get a test database pool (uses DATABASE_URL env var).
/// Each test function should use a transaction or unique data to avoid conflicts.
async fn test_pool() -> PgPool {
    let url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set for integration tests");
    let pool = PgPool::connect(&url).await.unwrap();
    // Run migrations
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    pool
}

// ═══════════════════════════════════════════════════════════════════════════════
// User Management Tests (suspend, unsuspend, force-reset, delete)
// Requirements: 4.4–4.15
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore]
async fn test_suspend_user_success() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_suspend@test.com", "admin").await;
    let user_id = create_test_user(&pool, "user_to_suspend@test.com", "user").await;
    create_workspace_with_subscription(&pool, user_id, "individual", "free", "active").await;

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));

    let resp = app
        .oneshot(auth_post(
            &format!("/admin/users/{}/suspend", user_id),
            &token,
            json!({}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["status"], "suspended");
    assert_eq!(body["id"], user_id.to_string());
}

#[tokio::test]
#[ignore]
async fn test_suspend_user_blocks_self_action() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_self_sus@test.com", "admin").await;
    create_workspace_with_subscription(&pool, admin_id, "individual", "free", "active").await;

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));

    let resp = app
        .oneshot(auth_post(
            &format!("/admin/users/{}/suspend", admin_id),
            &token,
            json!({}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_json(resp).await;
    assert_eq!(body["error"]["code"], "CANNOT_ACT_ON_SELF");
}

#[tokio::test]
#[ignore]
async fn test_suspend_user_blocks_action_on_admin() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin1_sus@test.com", "admin").await;
    let other_admin = create_test_user(&pool, "admin2_sus@test.com", "admin").await;
    create_workspace_with_subscription(&pool, admin_id, "individual", "free", "active").await;

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));

    let resp = app
        .oneshot(auth_post(
            &format!("/admin/users/{}/suspend", other_admin),
            &token,
            json!({}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_json(resp).await;
    assert_eq!(body["error"]["code"], "CANNOT_ACT_ON_ADMIN");
}

#[tokio::test]
#[ignore]
async fn test_unsuspend_user_success() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_unsus@test.com", "admin").await;
    let user_id = create_test_user(&pool, "user_to_unsus@test.com", "user").await;
    create_workspace_with_subscription(&pool, user_id, "individual", "free", "active").await;

    // First suspend the user
    sqlx::query("UPDATE users SET status = 'suspended' WHERE id = $1")
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));

    let resp = app
        .oneshot(auth_post(
            &format!("/admin/users/{}/unsuspend", user_id),
            &token,
            json!({}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["status"], "active");
}

#[tokio::test]
#[ignore]
async fn test_force_password_reset_success() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_fpr@test.com", "admin").await;
    let user_id = create_test_user(&pool, "user_fpr@test.com", "user").await;

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));

    let resp = app
        .oneshot(auth_post(
            &format!("/admin/users/{}/force-password-reset", user_id),
            &token,
            json!({}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Verify in DB
    let must_reset: bool =
        sqlx::query_scalar("SELECT must_reset_password FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(must_reset);
}

#[tokio::test]
#[ignore]
async fn test_delete_user_success() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_del@test.com", "admin").await;
    let user_id = create_test_user(&pool, "user_del@test.com", "user").await;

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));

    let resp = app
        .oneshot(auth_delete(
            &format!("/admin/users/{}", user_id),
            &token,
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Verify soft-delete
    let deleted_at: Option<chrono::DateTime<Utc>> =
        sqlx::query_scalar("SELECT deleted_at FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(deleted_at.is_some());
}

#[tokio::test]
#[ignore]
async fn test_delete_user_not_found() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_del_nf@test.com", "admin").await;

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));

    let resp = app
        .oneshot(auth_delete(
            &format!("/admin/users/{}", Uuid::new_v4()),
            &token,
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ═══════════════════════════════════════════════════════════════════════════════
// Workspace Management Tests (deactivate, hard-delete with confirm)
// Requirements: 4.16–4.21
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore]
async fn test_deactivate_workspace_success() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_deact@test.com", "admin").await;
    let user_id = create_test_user(&pool, "user_deact@test.com", "user").await;
    let (workspace_id, subscription_id) =
        create_workspace_with_subscription(&pool, user_id, "team", "pro", "active").await;

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));

    let resp = app
        .oneshot(auth_post(
            &format!("/admin/workspaces/{}/deactivate", workspace_id),
            &token,
            json!({}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Verify subscription is deactivated
    let status: String =
        sqlx::query_scalar("SELECT status FROM subscriptions WHERE id = $1")
            .bind(subscription_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(status, "deactivated");
}

#[tokio::test]
#[ignore]
async fn test_delete_workspace_requires_confirm() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_ws_del_c@test.com", "admin").await;
    let user_id = create_test_user(&pool, "user_ws_del_c@test.com", "user").await;
    let (workspace_id, _) =
        create_workspace_with_subscription(&pool, user_id, "team", "free", "active").await;

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));

    // Without confirm=true
    let resp = app
        .oneshot(auth_delete(
            &format!("/admin/workspaces/{}", workspace_id),
            &token,
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_json(resp).await;
    assert_eq!(body["error"]["code"], "CONFIRMATION_REQUIRED");
}

#[tokio::test]
#[ignore]
async fn test_delete_workspace_with_confirm() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_ws_del_ok@test.com", "admin").await;
    let user_id = create_test_user(&pool, "user_ws_del_ok@test.com", "user").await;
    let (workspace_id, _) =
        create_workspace_with_subscription(&pool, user_id, "team", "free", "active").await;

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));

    let resp = app
        .oneshot(auth_delete(
            &format!("/admin/workspaces/{}?confirm=true", workspace_id),
            &token,
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Verify workspace is gone
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM workspaces WHERE id = $1")
            .bind(workspace_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
#[ignore]
async fn test_delete_individual_workspace_blocked() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_ws_indiv@test.com", "admin").await;
    let user_id = create_test_user(&pool, "user_ws_indiv@test.com", "user").await;
    let (workspace_id, _) =
        create_workspace_with_subscription(&pool, user_id, "individual", "free", "active").await;

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));

    let resp = app
        .oneshot(auth_delete(
            &format!("/admin/workspaces/{}?confirm=true", workspace_id),
            &token,
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_json(resp).await;
    assert_eq!(body["error"]["code"], "CANNOT_DELETE_INDIVIDUAL_WORKSPACE");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Coupon/Discount CRUD Tests
// Requirements: 4.22–4.33
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore]
async fn test_discount_crud() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_disc@test.com", "admin").await;

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));

    // Create discount
    let resp = app
        .clone()
        .oneshot(auth_post(
            "/admin/discounts",
            &token,
            json!({"discount_type": "percentage", "value": "20.00"}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = body_json(resp).await;
    assert_eq!(body["discount_type"], "percentage");
    assert_eq!(body["active"], true);
    let discount_id = body["id"].as_str().unwrap().to_string();

    // Update discount (deactivate)
    let resp = app
        .clone()
        .oneshot(auth_patch(
            &format!("/admin/discounts/{}", discount_id),
            &token,
            json!({"active": false}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["active"], false);

    // List discounts
    let resp = app
        .oneshot(auth_get("/admin/discounts", &token))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert!(body.is_array());
}

#[tokio::test]
#[ignore]
async fn test_coupon_crud() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_coupon@test.com", "admin").await;

    // Create a discount for the coupon to reference
    let discount_id = Uuid::new_v4();
    sqlx::query("INSERT INTO discounts (id, type, value) VALUES ($1, 'percentage', 15)")
        .bind(discount_id)
        .execute(&pool)
        .await
        .unwrap();

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));
    let coupon_code = format!("TESTCOUP{}", Uuid::new_v4().to_string().split('-').next().unwrap());

    // Create coupon
    let resp = app
        .clone()
        .oneshot(auth_post(
            "/admin/coupons",
            &token,
            json!({
                "code": coupon_code,
                "discount_id": discount_id.to_string(),
                "max_uses": 100
            }),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = body_json(resp).await;
    assert_eq!(body["code"], coupon_code);
    assert_eq!(body["coupon_type"], "platform");
    assert_eq!(body["max_uses"], 100);
    let coupon_id = body["id"].as_str().unwrap().to_string();

    // Get coupon
    let resp = app
        .clone()
        .oneshot(auth_get(
            &format!("/admin/coupons/{}", coupon_id),
            &token,
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["code"], coupon_code);

    // Update coupon
    let resp = app
        .clone()
        .oneshot(auth_patch(
            &format!("/admin/coupons/{}", coupon_id),
            &token,
            json!({"active": false}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["active"], false);
}

#[tokio::test]
#[ignore]
async fn test_coupon_duplicate_code_rejected() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_coup_dup@test.com", "admin").await;

    let discount_id = Uuid::new_v4();
    sqlx::query("INSERT INTO discounts (id, type, value) VALUES ($1, 'flat', 5)")
        .bind(discount_id)
        .execute(&pool)
        .await
        .unwrap();

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));
    let code = format!("DUP{}", Uuid::new_v4().to_string().split('-').next().unwrap());

    // First create
    let resp = app
        .clone()
        .oneshot(auth_post(
            "/admin/coupons",
            &token,
            json!({"code": code, "discount_id": discount_id.to_string()}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Duplicate create (case-insensitive)
    let resp = app
        .oneshot(auth_post(
            "/admin/coupons",
            &token,
            json!({"code": code.to_uppercase(), "discount_id": discount_id.to_string()}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = body_json(resp).await;
    assert_eq!(body["error"]["code"], "COUPON_CODE_ALREADY_EXISTS");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Subscription Management Tests (extend, cancel, tier-override)
// Requirements: 4.34–4.41
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore]
async fn test_extend_subscription() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_ext@test.com", "admin").await;
    let user_id = create_test_user(&pool, "user_ext@test.com", "user").await;
    let (_, subscription_id) =
        create_workspace_with_subscription(&pool, user_id, "individual", "pro", "active").await;

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));

    let resp = app
        .oneshot(auth_post(
            &format!("/admin/subscriptions/{}/extend", subscription_id),
            &token,
            json!({"months": 3, "days": 15}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["tier"], "pro");
    assert_eq!(body["status"], "active");
    // period_end should be extended
    assert!(body["period_end"].is_string());
}

#[tokio::test]
#[ignore]
async fn test_cancel_subscription() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_cancel@test.com", "admin").await;
    let user_id = create_test_user(&pool, "user_cancel@test.com", "user").await;
    let (_, subscription_id) =
        create_workspace_with_subscription(&pool, user_id, "individual", "pro", "active").await;

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));

    let resp = app
        .oneshot(auth_post(
            &format!("/admin/subscriptions/{}/cancel", subscription_id),
            &token,
            json!({}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["status"], "cancelled");
}

#[tokio::test]
#[ignore]
async fn test_override_tier() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_tier@test.com", "admin").await;
    let user_id = create_test_user(&pool, "user_tier@test.com", "user").await;
    let (_, subscription_id) =
        create_workspace_with_subscription(&pool, user_id, "individual", "free", "active").await;

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));

    let resp = app
        .oneshot(auth_patch(
            &format!("/admin/subscriptions/{}/tier", subscription_id),
            &token,
            json!({"tier": "pro"}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["tier"], "pro");
}

#[tokio::test]
#[ignore]
async fn test_override_tier_invalid_tier_rejected() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_tier_inv@test.com", "admin").await;
    let user_id = create_test_user(&pool, "user_tier_inv@test.com", "user").await;
    let (_, subscription_id) =
        create_workspace_with_subscription(&pool, user_id, "individual", "free", "active").await;

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));

    let resp = app
        .oneshot(auth_patch(
            &format!("/admin/subscriptions/{}/tier", subscription_id),
            &token,
            json!({"tier": "enterprise"}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_json(resp).await;
    assert_eq!(body["error"]["code"], "INVALID_TIER");
}

#[tokio::test]
#[ignore]
async fn test_subscription_not_found() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_sub_nf@test.com", "admin").await;

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));

    let resp = app
        .oneshot(auth_post(
            &format!("/admin/subscriptions/{}/cancel", Uuid::new_v4()),
            &token,
            json!({}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ═══════════════════════════════════════════════════════════════════════════════
// Feature Flag CRUD Tests
// Requirements: 4.52–4.59
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore]
async fn test_feature_flag_crud() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_ff@test.com", "admin").await;

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));
    let flag_name = format!(
        "test-flag-{}",
        Uuid::new_v4().to_string().split('-').next().unwrap()
    );

    // Create feature flag
    let resp = app
        .clone()
        .oneshot(auth_post(
            "/admin/feature-flags",
            &token,
            json!({
                "name": flag_name,
                "enabled": true,
                "description": "A test flag"
            }),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = body_json(resp).await;
    assert_eq!(body["name"], flag_name);
    assert_eq!(body["enabled"], true);
    assert_eq!(body["description"], "A test flag");

    // Update feature flag
    let resp = app
        .clone()
        .oneshot(auth_put(
            &format!("/admin/feature-flags/{}", flag_name),
            &token,
            json!({"enabled": false, "description": "Updated description"}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["enabled"], false);
    assert_eq!(body["description"], "Updated description");

    // List feature flags
    let resp = app
        .clone()
        .oneshot(auth_get("/admin/feature-flags", &token))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert!(body.is_array());

    // Delete feature flag
    let resp = app
        .oneshot(auth_delete(
            &format!("/admin/feature-flags/{}", flag_name),
            &token,
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
#[ignore]
async fn test_feature_flag_invalid_name_rejected() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_ff_inv@test.com", "admin").await;

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));

    // Invalid: uppercase
    let resp = app
        .clone()
        .oneshot(auth_post(
            "/admin/feature-flags",
            &token,
            json!({"name": "InvalidName", "enabled": true}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_json(resp).await;
    assert_eq!(body["error"]["code"], "INVALID_FLAG_NAME");
}

#[tokio::test]
#[ignore]
async fn test_feature_flag_duplicate_rejected() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_ff_dup@test.com", "admin").await;

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));
    let flag_name = format!(
        "dup-flag-{}",
        Uuid::new_v4().to_string().split('-').next().unwrap()
    );

    // First create
    let resp = app
        .clone()
        .oneshot(auth_post(
            "/admin/feature-flags",
            &token,
            json!({"name": flag_name}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Duplicate
    let resp = app
        .oneshot(auth_post(
            "/admin/feature-flags",
            &token,
            json!({"name": flag_name}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = body_json(resp).await;
    assert_eq!(body["error"]["code"], "FEATURE_FLAG_ALREADY_EXISTS");
}

#[tokio::test]
#[ignore]
async fn test_feature_flag_delete_not_found() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_ff_dnf@test.com", "admin").await;

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));

    let resp = app
        .oneshot(auth_delete(
            "/admin/feature-flags/nonexistent-flag",
            &token,
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ═══════════════════════════════════════════════════════════════════════════════
// Admin Demote Tests (self-demote blocked, last admin blocked)
// Requirements: 4.60–4.63
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore]
async fn test_demote_admin_success() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_demote_a@test.com", "admin").await;
    let target_admin = create_test_user(&pool, "admin_demote_b@test.com", "admin").await;

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));

    let resp = app
        .oneshot(auth_delete(
            &format!("/admin/admins/{}", target_admin),
            &token,
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Verify role changed to user
    let role: String = sqlx::query_scalar("SELECT role FROM users WHERE id = $1")
        .bind(target_admin)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(role, "user");
}

#[tokio::test]
#[ignore]
async fn test_demote_admin_self_demote_blocked() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_self_dem@test.com", "admin").await;
    // Need another admin so we're not the last
    let _ = create_test_user(&pool, "admin_other_dem@test.com", "admin").await;

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));

    let resp = app
        .oneshot(auth_delete(
            &format!("/admin/admins/{}", admin_id),
            &token,
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_json(resp).await;
    assert_eq!(body["error"]["code"], "CANNOT_DEMOTE_SELF");
}

#[tokio::test]
#[ignore]
async fn test_demote_last_admin_blocked() {
    let pool = test_pool().await;

    // Create a caller and target admin.
    let caller_id = create_test_user(&pool, "admin_last_caller@test.com", "admin").await;
    let target_id = create_test_user(&pool, "admin_last_target2@test.com", "admin").await;

    // Demote ALL admins except the target so the target is the sole remaining admin.
    // With --test-threads=1, no concurrent test will create admins between this
    // UPDATE and the service's count query.
    sqlx::query("UPDATE users SET role = 'user' WHERE role = 'admin' AND id != $1")
        .bind(target_id)
        .execute(&pool)
        .await
        .unwrap();

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(caller_id));

    // Attempt to demote the last admin → must be blocked
    let resp = app
        .oneshot(auth_delete(
            &format!("/admin/admins/{}", target_id),
            &token,
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_json(resp).await;
    assert_eq!(body["error"]["code"], "LAST_ADMIN_CANNOT_BE_REMOVED");

    // Restore admins so other tests aren't affected
    sqlx::query("UPDATE users SET role = 'admin' WHERE id IN ($1, $2)")
        .bind(caller_id)
        .bind(target_id)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("UPDATE users SET role = 'admin' WHERE email = 'admin@example.com'")
        .execute(&pool)
        .await
        .ok();
}

#[tokio::test]
#[ignore]
async fn test_list_admins() {
    let pool = test_pool().await;
    let admin_id = create_test_user(&pool, "admin_list_a@test.com", "admin").await;
    let _ = create_test_user(&pool, "admin_list_b@test.com", "admin").await;

    let app = build_test_app(pool.clone());
    let token = make_token(&admin_claims(admin_id));

    let resp = app
        .oneshot(auth_get("/admin/admins", &token))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert!(body.is_array());
    let admins = body.as_array().unwrap();
    assert!(admins.len() >= 2);
}

// ═══════════════════════════════════════════════════════════════════════════════
// Authentication / Authorization Guard Tests
// Requirements: 4.1–4.3
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore]
async fn test_admin_endpoints_reject_unauthenticated() {
    let pool = test_pool().await;
    let app = build_test_app(pool);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/users")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore]
async fn test_admin_endpoints_reject_non_admin_role() {
    let pool = test_pool().await;
    let user_id = create_test_user(&pool, "regular_user_guard@test.com", "user").await;

    let app = build_test_app(pool);

    // Create a token with role=user (not admin)
    let claims = AccessTokenClaims {
        sub: user_id,
        client_type: ClientType::Web,
        role: Role::User,
        permissions: vec![],
        subscription_tier: Tier::Free,
        status: "active".to_string(),
        must_reset_password: false,
        exp: Utc::now().timestamp() + 3600,
    };
    let token = make_token(&claims);

    let resp = app
        .oneshot(auth_get("/admin/users", &token))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}
