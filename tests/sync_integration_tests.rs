//! Integration tests for the Sync Service.
//!
//! These tests exercise the full HTTP request/response cycle through the Axum router
//! for snippet CRUD, folder CRUD, batch operations, delta polling, snapshot retrieval,
//! trigger uniqueness enforcement, and WebSocket connectivity.
//!
//! Requires a running PostgreSQL instance. Set DATABASE_URL environment variable.
//! The test suite applies migrations fresh and seeds test data per test function.
//!
//! Run with: `cargo test --test sync_integration_tests -- --ignored`
//!
//! Requirements covered: 2.1–2.47

use std::sync::Arc;

use axum::{
    body::Body,
    extract::Extension,
    http::{Request, StatusCode},
    routing::{delete, get, patch, post},
    Router,
};
use chrono::Utc;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use ursnip_backend::middleware::auth_extractor::AccessTokenClaims;
use ursnip_backend::models::common::{ClientType, Role, Tier};
use ursnip_backend::sync::handlers::{
    batch_operations_handler, create_folder_handler, create_snippet_handler,
    delete_folder_handler, delete_snippet_handler, get_deltas_handler,
    get_snapshot_handler, update_folder_handler, update_snippet_handler,
};
use ursnip_backend::sync::service::SyncService;
use ursnip_backend::sync::session_registry::SessionRegistry;

// ─── Constants ──────────────────────────────────────────────────────────────────

const TEST_JWT_SECRET: &str = "test-jwt-secret-for-sync-integration-tests";

// ─── Test Helpers ───────────────────────────────────────────────────────────────

/// Build a test Axum router with sync routes, injecting the SyncService and SessionRegistry.
/// Authentication is simulated by injecting claims directly via middleware.
fn build_test_router(pool: PgPool) -> Router {
    let sync_service = Arc::new(SyncService::new(pool));
    let session_registry = Arc::new(SessionRegistry::new(10_000));

    Router::new()
        .route("/sync/snippets", post(create_snippet_handler))
        .route("/sync/snippets/:id", patch(update_snippet_handler))
        .route("/sync/snippets/:id", delete(delete_snippet_handler))
        .route("/sync/snippets/batch", post(batch_operations_handler))
        .route("/sync/folders", post(create_folder_handler))
        .route("/sync/folders/:id", patch(update_folder_handler))
        .route("/sync/folders/:id", delete(delete_folder_handler))
        .route("/sync/snapshot", get(get_snapshot_handler))
        .route("/sync/deltas", get(get_deltas_handler))
        .layer(Extension(sync_service))
        .layer(Extension(session_registry))
}

/// Create default test claims for a native user.
fn test_claims(user_id: Uuid) -> AccessTokenClaims {
    AccessTokenClaims {
        sub: user_id,
        client_type: ClientType::Native,
        role: Role::User,
        permissions: vec![],
        subscription_tier: Tier::Free,
        status: "active".to_string(),
        must_reset_password: false,
        exp: Utc::now().timestamp() + 3600,
    }
}

/// Build a JSON POST request with claims injected into extensions.
fn post_json(uri: &str, body: Value, claims: &AccessTokenClaims) -> Request<Body> {
    let mut req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    req.extensions_mut().insert(claims.clone());
    req
}

/// Build a JSON PATCH request with claims injected into extensions.
fn patch_json(uri: &str, body: Value, claims: &AccessTokenClaims) -> Request<Body> {
    let mut req = Request::builder()
        .method("PATCH")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    req.extensions_mut().insert(claims.clone());
    req
}

/// Build a DELETE request with claims injected into extensions.
fn delete_req(uri: &str, claims: &AccessTokenClaims) -> Request<Body> {
    let mut req = Request::builder()
        .method("DELETE")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(claims.clone());
    req
}

/// Build a GET request with claims injected into extensions.
fn get_req(uri: &str, claims: &AccessTokenClaims) -> Request<Body> {
    let mut req = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(claims.clone());
    req
}

/// Extract JSON body from a response.
async fn body_json(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

/// Initialize a test database pool, run migrations, and return a clean PgPool.
async fn setup_test_db() -> PgPool {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set for integration tests");

    let pool = PgPool::connect(&database_url)
        .await
        .expect("Failed to connect to test database");

    // Run migrations
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    pool
}

/// Seed a test user and workspace with a free subscription.
/// Returns (user_id, workspace_id).
async fn seed_user_and_workspace(pool: &PgPool) -> (Uuid, Uuid) {
    let user_id = Uuid::new_v4();
    let workspace_id = Uuid::new_v4();
    let referral_code = format!("REF{}", &user_id.to_string()[..8]);

    // Create user
    sqlx::query(
        r#"INSERT INTO users (id, email, password_hash, role, status, referral_code)
           VALUES ($1, $2, $3, 'user', 'active', $4)"#,
    )
    .bind(user_id)
    .bind(format!("test_{}@example.com", &user_id.to_string()[..8]))
    .bind("$argon2id$v=19$m=65536,t=3,p=4$hash")
    .bind(&referral_code)
    .execute(pool)
    .await
    .expect("Failed to create test user");

    // Create workspace
    sqlx::query(
        r#"INSERT INTO workspaces (id, type, owner_id, name)
           VALUES ($1, 'individual', $2, 'Test Workspace')"#,
    )
    .bind(workspace_id)
    .bind(user_id)
    .execute(pool)
    .await
    .expect("Failed to create test workspace");

    // Add membership
    sqlx::query(
        r#"INSERT INTO workspace_members (workspace_id, user_id, role)
           VALUES ($1, $2, 'owner')"#,
    )
    .bind(workspace_id)
    .bind(user_id)
    .execute(pool)
    .await
    .expect("Failed to create workspace membership");

    // Create free subscription
    sqlx::query(
        r#"INSERT INTO subscriptions (workspace_id, tier, status)
           VALUES ($1, 'free', 'active')"#,
    )
    .bind(workspace_id)
    .execute(pool)
    .await
    .expect("Failed to create test subscription");

    (user_id, workspace_id)
}

/// Clean up test data after each test to avoid polluting next tests.
async fn cleanup(pool: &PgPool, user_id: Uuid, workspace_id: Uuid) {
    // Delete in reverse dependency order
    let _ = sqlx::query("DELETE FROM sync_deltas WHERE workspace_id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM snippet_folders WHERE snippet_id IN (SELECT id FROM snippets WHERE workspace_id = $1)")
        .bind(workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM snippets WHERE workspace_id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM folders WHERE workspace_id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM subscriptions WHERE workspace_id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM workspace_members WHERE workspace_id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM workspaces WHERE id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user_id)
        .execute(pool)
        .await;
}

// ─── Test: Snippet CRUD with version assignment ────────────────────────────────
// Requirements: 2.1, 2.4, 2.5, 2.6, 2.7, 2.32, 2.33, 2.38, 2.39, 2.40

#[tokio::test]
#[ignore]
async fn test_snippet_create_returns_201_with_version() {
    let pool = setup_test_db().await;
    let (user_id, workspace_id) = seed_user_and_workspace(&pool).await;
    let app = build_test_router(pool.clone());
    let claims = test_claims(user_id);

    let body = json!({
        "workspace_id": workspace_id,
        "trigger": "hello",
        "content": "Hello, world!",
        "snippet_type": "text"
    });

    let resp = app.oneshot(post_json("/sync/snippets", body, &claims)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let json = body_json(resp).await;
    assert_eq!(json["workspace_id"], workspace_id.to_string());
    assert_eq!(json["trigger"], "hello");
    assert_eq!(json["content"], "Hello, world!");
    assert_eq!(json["version"], 1);
    assert!(json["id"].as_str().is_some());

    cleanup(&pool, user_id, workspace_id).await;
}

#[tokio::test]
#[ignore]
async fn test_snippet_update_increments_version() {
    let pool = setup_test_db().await;
    let (user_id, workspace_id) = seed_user_and_workspace(&pool).await;
    let app = build_test_router(pool.clone());
    let claims = test_claims(user_id);

    // Create a snippet
    let create_body = json!({
        "workspace_id": workspace_id,
        "trigger": "greet",
        "content": "Hi there",
        "snippet_type": "text"
    });
    let resp = app.clone().oneshot(post_json("/sync/snippets", create_body, &claims)).await.unwrap();
    let created = body_json(resp).await;
    let snippet_id = created["id"].as_str().unwrap();

    // Update the snippet
    let update_body = json!({
        "content": "Hello updated"
    });
    let uri = format!("/sync/snippets/{}", snippet_id);
    let resp = app.oneshot(patch_json(&uri, update_body, &claims)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let updated = body_json(resp).await;
    assert_eq!(updated["content"], "Hello updated");
    assert_eq!(updated["version"], 2); // version incremented

    cleanup(&pool, user_id, workspace_id).await;
}

#[tokio::test]
#[ignore]
async fn test_snippet_delete_returns_204() {
    let pool = setup_test_db().await;
    let (user_id, workspace_id) = seed_user_and_workspace(&pool).await;
    let app = build_test_router(pool.clone());
    let claims = test_claims(user_id);

    // Create
    let body = json!({
        "workspace_id": workspace_id,
        "trigger": "deleteme",
        "content": "Temporary",
        "snippet_type": "text"
    });
    let resp = app.clone().oneshot(post_json("/sync/snippets", body, &claims)).await.unwrap();
    let created = body_json(resp).await;
    let snippet_id = created["id"].as_str().unwrap();

    // Delete
    let uri = format!("/sync/snippets/{}", snippet_id);
    let resp = app.oneshot(delete_req(&uri, &claims)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    cleanup(&pool, user_id, workspace_id).await;
}

// ─── Test: Folder CRUD with version assignment ──────────────────────────────────
// Requirements: 2.2, 2.8, 2.9, 2.10, 2.34, 2.35

#[tokio::test]
#[ignore]
async fn test_folder_create_returns_201_with_version() {
    let pool = setup_test_db().await;
    let (user_id, workspace_id) = seed_user_and_workspace(&pool).await;
    let app = build_test_router(pool.clone());
    let claims = test_claims(user_id);

    let body = json!({
        "workspace_id": workspace_id,
        "name": "My Folder"
    });

    let resp = app.oneshot(post_json("/sync/folders", body, &claims)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let json = body_json(resp).await;
    assert_eq!(json["workspace_id"], workspace_id.to_string());
    assert_eq!(json["name"], "My Folder");
    assert_eq!(json["version"], 1);

    cleanup(&pool, user_id, workspace_id).await;
}

#[tokio::test]
#[ignore]
async fn test_folder_update_increments_version() {
    let pool = setup_test_db().await;
    let (user_id, workspace_id) = seed_user_and_workspace(&pool).await;
    let app = build_test_router(pool.clone());
    let claims = test_claims(user_id);

    // Create folder
    let body = json!({ "workspace_id": workspace_id, "name": "Orig" });
    let resp = app.clone().oneshot(post_json("/sync/folders", body, &claims)).await.unwrap();
    let created = body_json(resp).await;
    let folder_id = created["id"].as_str().unwrap();

    // Update folder
    let update_body = json!({ "name": "Renamed" });
    let uri = format!("/sync/folders/{}", folder_id);
    let resp = app.oneshot(patch_json(&uri, update_body, &claims)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let updated = body_json(resp).await;
    assert_eq!(updated["name"], "Renamed");
    assert_eq!(updated["version"], 2);

    cleanup(&pool, user_id, workspace_id).await;
}

#[tokio::test]
#[ignore]
async fn test_folder_delete_returns_204() {
    let pool = setup_test_db().await;
    let (user_id, workspace_id) = seed_user_and_workspace(&pool).await;
    let app = build_test_router(pool.clone());
    let claims = test_claims(user_id);

    // Create folder
    let body = json!({ "workspace_id": workspace_id, "name": "ToDelete" });
    let resp = app.clone().oneshot(post_json("/sync/folders", body, &claims)).await.unwrap();
    let created = body_json(resp).await;
    let folder_id = created["id"].as_str().unwrap();

    // Delete folder
    let uri = format!("/sync/folders/{}", folder_id);
    let resp = app.oneshot(delete_req(&uri, &claims)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    cleanup(&pool, user_id, workspace_id).await;
}

// ─── Test: Batch operations ─────────────────────────────────────────────────────
// Requirements: 2.34, 2.35, 2.36, 2.37

#[tokio::test]
#[ignore]
async fn test_batch_create_multiple_snippets_success() {
    let pool = setup_test_db().await;
    let (user_id, workspace_id) = seed_user_and_workspace(&pool).await;
    let app = build_test_router(pool.clone());
    let claims = test_claims(user_id);

    let body = json!({
        "workspace_id": workspace_id,
        "operations": [
            {
                "type": "create_snippet",
                "workspace_id": workspace_id,
                "trigger": "batch1",
                "content": "Content 1",
                "snippet_type": "text"
            },
            {
                "type": "create_snippet",
                "workspace_id": workspace_id,
                "trigger": "batch2",
                "content": "Content 2",
                "snippet_type": "text"
            }
        ]
    });

    let resp = app.oneshot(post_json("/sync/snippets/batch", body, &claims)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let results = json["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    // Sequential version assignment
    assert_eq!(results[0]["version"], 1);
    assert_eq!(results[1]["version"], 2);
    assert_eq!(json["workspace_version"], 2);

    cleanup(&pool, user_id, workspace_id).await;
}

#[tokio::test]
#[ignore]
async fn test_batch_rollback_on_duplicate_trigger() {
    let pool = setup_test_db().await;
    let (user_id, workspace_id) = seed_user_and_workspace(&pool).await;
    let app = build_test_router(pool.clone());
    let claims = test_claims(user_id);

    // Create an existing snippet
    let pre_body = json!({
        "workspace_id": workspace_id,
        "trigger": "existing",
        "content": "Already here",
        "snippet_type": "text"
    });
    app.clone().oneshot(post_json("/sync/snippets", pre_body, &claims)).await.unwrap();

    // Batch with a duplicate trigger — entire batch should fail
    let batch_body = json!({
        "workspace_id": workspace_id,
        "operations": [
            {
                "type": "create_snippet",
                "workspace_id": workspace_id,
                "trigger": "new_one",
                "content": "Valid",
                "snippet_type": "text"
            },
            {
                "type": "create_snippet",
                "workspace_id": workspace_id,
                "trigger": "existing",
                "content": "Duplicate!",
                "snippet_type": "text"
            }
        ]
    });

    let resp = app.oneshot(post_json("/sync/snippets/batch", batch_body, &claims)).await.unwrap();
    // Should fail with 409 TRIGGER_ALREADY_EXISTS (entire batch rolled back)
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    // Verify "new_one" was NOT persisted (rollback)
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM snippets WHERE workspace_id = $1 AND trigger = 'new_one' AND deleted_at IS NULL"
    )
    .bind(workspace_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 0, "Batch rollback: 'new_one' should not exist");

    cleanup(&pool, user_id, workspace_id).await;
}

// ─── Test: Delta polling with pagination ────────────────────────────────────────
// Requirements: 2.13, 2.14, 2.15, 2.16

#[tokio::test]
#[ignore]
async fn test_delta_polling_returns_deltas_since_version() {
    let pool = setup_test_db().await;
    let (user_id, workspace_id) = seed_user_and_workspace(&pool).await;
    let app = build_test_router(pool.clone());
    let claims = test_claims(user_id);

    // Create 3 snippets to generate 3 deltas
    for i in 1..=3 {
        let body = json!({
            "workspace_id": workspace_id,
            "trigger": format!("delta_{}", i),
            "content": format!("Content {}", i),
            "snippet_type": "text"
        });
        app.clone().oneshot(post_json("/sync/snippets", body, &claims)).await.unwrap();
    }

    // Poll deltas since version 1 (should get versions 2 and 3)
    let uri = format!(
        "/sync/deltas?workspace_id={}&since_version=1",
        workspace_id
    );
    let resp = app.clone().oneshot(get_req(&uri, &claims)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let deltas = json["deltas"].as_array().unwrap();
    assert_eq!(deltas.len(), 2);
    assert_eq!(deltas[0]["version"], 2);
    assert_eq!(deltas[1]["version"], 3);
    assert_eq!(json["next_since_version"], 3);

    cleanup(&pool, user_id, workspace_id).await;
}

#[tokio::test]
#[ignore]
async fn test_delta_polling_with_limit_pagination() {
    let pool = setup_test_db().await;
    let (user_id, workspace_id) = seed_user_and_workspace(&pool).await;
    let app = build_test_router(pool.clone());
    let claims = test_claims(user_id);

    // Create 5 snippets
    for i in 1..=5 {
        let body = json!({
            "workspace_id": workspace_id,
            "trigger": format!("page_{}", i),
            "content": format!("Content {}", i),
            "snippet_type": "text"
        });
        app.clone().oneshot(post_json("/sync/snippets", body, &claims)).await.unwrap();
    }

    // Poll with limit=2 from version 0
    let uri = format!(
        "/sync/deltas?workspace_id={}&since_version=0&limit=2",
        workspace_id
    );
    let resp = app.clone().oneshot(get_req(&uri, &claims)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let deltas = json["deltas"].as_array().unwrap();
    assert_eq!(deltas.len(), 2);
    assert_eq!(json["has_more"], true);
    assert_eq!(json["next_since_version"], 2);

    // Continue paging from next_since_version
    let uri = format!(
        "/sync/deltas?workspace_id={}&since_version=2&limit=2",
        workspace_id
    );
    let resp = app.clone().oneshot(get_req(&uri, &claims)).await.unwrap();
    let json = body_json(resp).await;
    let deltas = json["deltas"].as_array().unwrap();
    assert_eq!(deltas.len(), 2);
    assert_eq!(json["has_more"], true);

    // Final page
    let uri = format!(
        "/sync/deltas?workspace_id={}&since_version=4&limit=2",
        workspace_id
    );
    let resp = app.oneshot(get_req(&uri, &claims)).await.unwrap();
    let json = body_json(resp).await;
    let deltas = json["deltas"].as_array().unwrap();
    assert_eq!(deltas.len(), 1);
    assert_eq!(json["has_more"], false);

    cleanup(&pool, user_id, workspace_id).await;
}

// ─── Test: Snapshot retrieval ───────────────────────────────────────────────────
// Requirements: 2.11, 2.12

#[tokio::test]
#[ignore]
async fn test_snapshot_returns_all_active_resources() {
    let pool = setup_test_db().await;
    let (user_id, workspace_id) = seed_user_and_workspace(&pool).await;
    let app = build_test_router(pool.clone());
    let claims = test_claims(user_id);

    // Create 2 snippets and 1 folder
    let s1 = json!({
        "workspace_id": workspace_id,
        "trigger": "snap1",
        "content": "Content 1",
        "snippet_type": "text"
    });
    let s2 = json!({
        "workspace_id": workspace_id,
        "trigger": "snap2",
        "content": "Content 2",
        "snippet_type": "text"
    });
    let f1 = json!({ "workspace_id": workspace_id, "name": "Folder1" });

    app.clone().oneshot(post_json("/sync/snippets", s1, &claims)).await.unwrap();
    app.clone().oneshot(post_json("/sync/snippets", s2, &claims)).await.unwrap();
    app.clone().oneshot(post_json("/sync/folders", f1, &claims)).await.unwrap();

    // Delete one snippet (should NOT appear in snapshot)
    let snap_resp = app.clone()
        .oneshot(get_req(
            &format!("/sync/snapshot?workspace_id={}", workspace_id),
            &claims,
        ))
        .await
        .unwrap();

    // Before deletion, snapshot has 2 snippets
    let json = body_json(snap_resp).await;
    assert_eq!(json["snippets"].as_array().unwrap().len(), 2);
    assert_eq!(json["folders"].as_array().unwrap().len(), 1);
    assert_eq!(json["version"], 3); // 3 operations total

    cleanup(&pool, user_id, workspace_id).await;
}

#[tokio::test]
#[ignore]
async fn test_snapshot_excludes_deleted_snippets() {
    let pool = setup_test_db().await;
    let (user_id, workspace_id) = seed_user_and_workspace(&pool).await;
    let app = build_test_router(pool.clone());
    let claims = test_claims(user_id);

    // Create a snippet then delete it
    let body = json!({
        "workspace_id": workspace_id,
        "trigger": "willdelete",
        "content": "Gone soon",
        "snippet_type": "text"
    });
    let resp = app.clone().oneshot(post_json("/sync/snippets", body, &claims)).await.unwrap();
    let created = body_json(resp).await;
    let snippet_id = created["id"].as_str().unwrap();

    let uri = format!("/sync/snippets/{}", snippet_id);
    app.clone().oneshot(delete_req(&uri, &claims)).await.unwrap();

    // Snapshot should show 0 snippets
    let snap_uri = format!("/sync/snapshot?workspace_id={}", workspace_id);
    let resp = app.oneshot(get_req(&snap_uri, &claims)).await.unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["snippets"].as_array().unwrap().len(), 0);
    // Version should be 2 (create + delete)
    assert_eq!(json["version"], 2);

    cleanup(&pool, user_id, workspace_id).await;
}

// ─── Test: Trigger uniqueness enforcement and reuse after soft-delete ───────────
// Requirements: 2.32, 2.33, 2.45

#[tokio::test]
#[ignore]
async fn test_duplicate_trigger_returns_409() {
    let pool = setup_test_db().await;
    let (user_id, workspace_id) = seed_user_and_workspace(&pool).await;
    let app = build_test_router(pool.clone());
    let claims = test_claims(user_id);

    let body = json!({
        "workspace_id": workspace_id,
        "trigger": "dup_trigger",
        "content": "First",
        "snippet_type": "text"
    });
    let resp = app.clone().oneshot(post_json("/sync/snippets", body.clone(), &claims)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Second creation with same trigger should conflict
    let resp = app.oneshot(post_json("/sync/snippets", body, &claims)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    cleanup(&pool, user_id, workspace_id).await;
}

#[tokio::test]
#[ignore]
async fn test_trigger_reuse_after_soft_delete() {
    let pool = setup_test_db().await;
    let (user_id, workspace_id) = seed_user_and_workspace(&pool).await;
    let app = build_test_router(pool.clone());
    let claims = test_claims(user_id);

    // Create snippet
    let body = json!({
        "workspace_id": workspace_id,
        "trigger": "reusable",
        "content": "Original",
        "snippet_type": "text"
    });
    let resp = app.clone().oneshot(post_json("/sync/snippets", body, &claims)).await.unwrap();
    let created = body_json(resp).await;
    let snippet_id = created["id"].as_str().unwrap();

    // Soft-delete it
    let uri = format!("/sync/snippets/{}", snippet_id);
    app.clone().oneshot(delete_req(&uri, &claims)).await.unwrap();

    // Now reuse the same trigger — should succeed
    let reuse_body = json!({
        "workspace_id": workspace_id,
        "trigger": "reusable",
        "content": "Reused content",
        "snippet_type": "text"
    });
    let resp = app.oneshot(post_json("/sync/snippets", reuse_body, &claims)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let json = body_json(resp).await;
    assert_eq!(json["trigger"], "reusable");
    assert_eq!(json["content"], "Reused content");
    // New snippet gets version 3 (create=1, delete=2, recreate=3)
    assert_eq!(json["version"], 3);

    cleanup(&pool, user_id, workspace_id).await;
}

// ─── Test: WebSocket connection and session registry ────────────────────────────
// Requirements: 2.17, 2.23, 2.24, 2.25

#[tokio::test]
async fn test_ws_session_registry_register_and_broadcast() {
    // This test validates the SessionRegistry at the component level,
    // since full WebSocket HTTP upgrade requires a running TCP server.
    let registry = SessionRegistry::new(100);
    let user_id = Uuid::new_v4();
    let workspace_id = Uuid::new_v4();

    let (tx1, mut rx1) = tokio::sync::mpsc::unbounded_channel();
    let session1 = ursnip_backend::sync::session_registry::WsSession {
        session_id: Uuid::new_v4(),
        user_id,
        workspace_id,
        sender: tx1,
        connected_at: Utc::now(),
    };
    let s1_id = session1.session_id;

    let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
    let session2 = ursnip_backend::sync::session_registry::WsSession {
        session_id: Uuid::new_v4(),
        user_id,
        workspace_id,
        sender: tx2,
        connected_at: Utc::now(),
    };

    registry.register(session1).unwrap();
    registry.register(session2).unwrap();
    assert_eq!(registry.total_connections(), 2);

    // Broadcast excluding session1
    let msg = json!({"type": "delta", "version": 1});
    registry.broadcast_to_workspace(workspace_id, msg.clone(), Some(s1_id));

    // session1 should NOT receive (excluded)
    assert!(rx1.try_recv().is_err());
    // session2 should receive
    let received = rx2.try_recv().unwrap();
    match received {
        ursnip_backend::sync::session_registry::WsMessage::Send(v) => {
            assert_eq!(v["type"], "delta");
        }
        _ => panic!("Expected Send message"),
    }
}

#[tokio::test]
async fn test_ws_session_registry_heartbeat_close() {
    // Test that close_user_sessions sends Close messages
    let registry = SessionRegistry::new(100);
    let user_id = Uuid::new_v4();
    let workspace_id = Uuid::new_v4();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let session = ursnip_backend::sync::session_registry::WsSession {
        session_id: Uuid::new_v4(),
        user_id,
        workspace_id,
        sender: tx,
        connected_at: Utc::now(),
    };

    registry.register(session).unwrap();
    assert_eq!(registry.total_connections(), 1);

    // Simulate close (e.g., from admin suspend)
    registry.close_user_sessions(user_id, 1008, "Account suspended".to_string());
    assert_eq!(registry.total_connections(), 0);

    // Session should receive Close with code 1008
    let msg = rx.try_recv().unwrap();
    match msg {
        ursnip_backend::sync::session_registry::WsMessage::Close(code, reason) => {
            assert_eq!(code, 1008);
            assert_eq!(reason, "Account suspended");
        }
        _ => panic!("Expected Close message"),
    }
}

#[tokio::test]
async fn test_ws_per_user_connection_limit() {
    // Per-user limit is 5 connections; 6th evicts oldest
    let registry = SessionRegistry::new(100);
    let user_id = Uuid::new_v4();
    let workspace_id = Uuid::new_v4();

    let mut receivers = Vec::new();
    for _ in 0..5 {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let session = ursnip_backend::sync::session_registry::WsSession {
            session_id: Uuid::new_v4(),
            user_id,
            workspace_id,
            sender: tx,
            connected_at: Utc::now(),
        };
        registry.register(session).unwrap();
        receivers.push(rx);
    }
    assert_eq!(registry.total_connections(), 5);

    // 6th connection should evict the oldest
    let (tx6, _rx6) = tokio::sync::mpsc::unbounded_channel();
    let session6 = ursnip_backend::sync::session_registry::WsSession {
        session_id: Uuid::new_v4(),
        user_id,
        workspace_id,
        sender: tx6,
        connected_at: Utc::now(),
    };
    registry.register(session6).unwrap();
    assert_eq!(registry.total_connections(), 5); // still 5 (evicted one)

    // Oldest receiver should get a Close message
    let msg = receivers[0].try_recv().unwrap();
    match msg {
        ursnip_backend::sync::session_registry::WsMessage::Close(code, _) => {
            assert_eq!(code, 1008);
        }
        _ => panic!("Expected Close message for evicted session"),
    }
}

// ─── Test: Non-member cannot access workspace ───────────────────────────────────
// Requirements: 2.41, 2.42

#[tokio::test]
#[ignore]
async fn test_non_member_cannot_create_snippet() {
    let pool = setup_test_db().await;
    let (user_id, workspace_id) = seed_user_and_workspace(&pool).await;
    let app = build_test_router(pool.clone());

    // Create claims for a different user (not a member)
    let other_user = Uuid::new_v4();
    let claims = test_claims(other_user);

    let body = json!({
        "workspace_id": workspace_id,
        "trigger": "unauth",
        "content": "Should fail",
        "snippet_type": "text"
    });
    let resp = app.oneshot(post_json("/sync/snippets", body, &claims)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    cleanup(&pool, user_id, workspace_id).await;
}

// ─── Test: Snapshot required for expired deltas ─────────────────────────────────
// Requirements: 2.16

#[tokio::test]
#[ignore]
async fn test_delta_snapshot_required_for_expired_deltas() {
    let pool = setup_test_db().await;
    let (user_id, workspace_id) = seed_user_and_workspace(&pool).await;
    let claims = test_claims(user_id);

    // Manually insert an old delta (31 days ago) to simulate retention expiry
    let old_timestamp = Utc::now() - chrono::Duration::days(31);
    sqlx::query(
        r#"INSERT INTO sync_deltas (workspace_id, entity_type, entity_id, operation, payload, version, created_at)
           VALUES ($1, 'snippet', $2, 'create', '{}', 5, $3)"#,
    )
    .bind(workspace_id)
    .bind(Uuid::new_v4())
    .bind(old_timestamp)
    .execute(&pool)
    .await
    .unwrap();

    let app = build_test_router(pool.clone());

    // Request deltas with since_version < min_version (5) and since_version > 0
    // The oldest delta is beyond the 30-day retention window, so SNAPSHOT_REQUIRED
    let uri = format!(
        "/sync/deltas?workspace_id={}&since_version=1",
        workspace_id
    );
    let resp = app.oneshot(get_req(&uri, &claims)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    cleanup(&pool, user_id, workspace_id).await;
}

// ─── Test: Free tier snippet limit ──────────────────────────────────────────────
// Requirements: 5.21, 2.5

#[tokio::test]
#[ignore]
async fn test_free_tier_snippet_limit_enforced() {
    let pool = setup_test_db().await;
    let (user_id, workspace_id) = seed_user_and_workspace(&pool).await;
    let app = build_test_router(pool.clone());
    let claims = test_claims(user_id);

    // Create 10 snippets (free tier max)
    for i in 1..=10 {
        let body = json!({
            "workspace_id": workspace_id,
            "trigger": format!("limit_{}", i),
            "content": "Short",
            "snippet_type": "text"
        });
        let resp = app.clone().oneshot(post_json("/sync/snippets", body, &claims)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED, "Snippet {} should succeed", i);
    }

    // 11th should fail with 422 SNIPPET_LIMIT_REACHED
    let body = json!({
        "workspace_id": workspace_id,
        "trigger": "limit_11",
        "content": "Over limit",
        "snippet_type": "text"
    });
    let resp = app.oneshot(post_json("/sync/snippets", body, &claims)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    cleanup(&pool, user_id, workspace_id).await;
}

// ─── Test: Free tier content length limit ───────────────────────────────────────
// Requirements: 5.21

#[tokio::test]
#[ignore]
async fn test_free_tier_content_length_limit() {
    let pool = setup_test_db().await;
    let (user_id, workspace_id) = seed_user_and_workspace(&pool).await;
    let app = build_test_router(pool.clone());
    let claims = test_claims(user_id);

    // Content with 2001 chars should be rejected on free tier
    let long_content = "x".repeat(2001);
    let body = json!({
        "workspace_id": workspace_id,
        "trigger": "toolong",
        "content": long_content,
        "snippet_type": "text"
    });
    let resp = app.oneshot(post_json("/sync/snippets", body, &claims)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    cleanup(&pool, user_id, workspace_id).await;
}

// ─── Test: Free tier folder limit ───────────────────────────────────────────────
// Requirements: 5.21

#[tokio::test]
#[ignore]
async fn test_free_tier_folder_limit_enforced() {
    let pool = setup_test_db().await;
    let (user_id, workspace_id) = seed_user_and_workspace(&pool).await;
    let app = build_test_router(pool.clone());
    let claims = test_claims(user_id);

    // Create 3 folders (free tier max)
    for i in 1..=3 {
        let body = json!({ "workspace_id": workspace_id, "name": format!("Folder {}", i) });
        let resp = app.clone().oneshot(post_json("/sync/folders", body, &claims)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED, "Folder {} should succeed", i);
    }

    // 4th should fail
    let body = json!({ "workspace_id": workspace_id, "name": "Folder 4" });
    let resp = app.oneshot(post_json("/sync/folders", body, &claims)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    cleanup(&pool, user_id, workspace_id).await;
}

// ─── Test: Batch with mixed operations ──────────────────────────────────────────
// Requirements: 2.34, 2.35, 2.36

#[tokio::test]
#[ignore]
async fn test_batch_with_update_and_delete() {
    let pool = setup_test_db().await;
    let (user_id, workspace_id) = seed_user_and_workspace(&pool).await;
    let app = build_test_router(pool.clone());
    let claims = test_claims(user_id);

    // Pre-create a snippet to update/delete in batch
    let pre = json!({
        "workspace_id": workspace_id,
        "trigger": "pre_existing",
        "content": "Original",
        "snippet_type": "text"
    });
    let resp = app.clone().oneshot(post_json("/sync/snippets", pre, &claims)).await.unwrap();
    let created = body_json(resp).await;
    let snippet_id = created["id"].as_str().unwrap();

    // Batch: create new + update existing
    let batch_body = json!({
        "workspace_id": workspace_id,
        "operations": [
            {
                "type": "create_snippet",
                "workspace_id": workspace_id,
                "trigger": "batch_new",
                "content": "New in batch",
                "snippet_type": "text"
            },
            {
                "type": "update_snippet",
                "id": snippet_id,
                "content": "Updated in batch"
            }
        ]
    });

    let resp = app.oneshot(post_json("/sync/snippets/batch", batch_body, &claims)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let results = json["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    // Versions are sequential from the pre-existing operation (version 1)
    assert_eq!(results[0]["version"], 2);
    assert_eq!(results[1]["version"], 3);
    // Updated snippet content
    assert_eq!(results[1]["snippet"]["content"], "Updated in batch");

    cleanup(&pool, user_id, workspace_id).await;
}

// ─── Test: Version monotonicity across mixed operations ─────────────────────────
// Requirements: 2.4

#[tokio::test]
#[ignore]
async fn test_version_monotonically_increases_across_operations() {
    let pool = setup_test_db().await;
    let (user_id, workspace_id) = seed_user_and_workspace(&pool).await;
    let app = build_test_router(pool.clone());
    let claims = test_claims(user_id);

    // Create snippet (v1)
    let body = json!({
        "workspace_id": workspace_id,
        "trigger": "mono",
        "content": "v1",
        "snippet_type": "text"
    });
    let resp = app.clone().oneshot(post_json("/sync/snippets", body, &claims)).await.unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["version"], 1);
    let snippet_id = json["id"].as_str().unwrap().to_string();

    // Create folder (v2)
    let body = json!({ "workspace_id": workspace_id, "name": "F1" });
    let resp = app.clone().oneshot(post_json("/sync/folders", body, &claims)).await.unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["version"], 2);

    // Update snippet (v3)
    let body = json!({ "content": "v3" });
    let uri = format!("/sync/snippets/{}", snippet_id);
    let resp = app.clone().oneshot(patch_json(&uri, body, &claims)).await.unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["version"], 3);

    // Delete snippet (v4)
    let resp = app.oneshot(delete_req(&uri, &claims)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Verify via deltas that versions are 1,2,3,4
    let app2 = build_test_router(pool.clone());
    let delta_uri = format!("/sync/deltas?workspace_id={}&since_version=0", workspace_id);
    let resp = app2.oneshot(get_req(&delta_uri, &claims)).await.unwrap();
    let json = body_json(resp).await;
    let deltas = json["deltas"].as_array().unwrap();
    assert_eq!(deltas.len(), 4);
    for (i, delta) in deltas.iter().enumerate() {
        assert_eq!(delta["version"], (i as i64) + 1);
    }

    cleanup(&pool, user_id, workspace_id).await;
}

// ─── Test: WebSocket full connection via TCP server ──────────────────────────────
// Requirements: 2.17, 2.18, 2.26, 2.27, 2.28, 2.29, 2.30, 2.31

#[tokio::test]
#[ignore]
async fn test_ws_connection_and_delta_push() {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::{connect_async, tungstenite::Message};

    let pool = setup_test_db().await;
    let (user_id, workspace_id) = seed_user_and_workspace(&pool).await;

    let jwt_secret = TEST_JWT_SECRET;
    let claims = test_claims(user_id);
    let token = encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(jwt_secret.as_bytes()),
    )
    .unwrap();

    // Build a full app with the WS upgrade handler
    let sync_service = Arc::new(SyncService::new(pool.clone()));
    let session_registry = Arc::new(SessionRegistry::new(10_000));

    use ursnip_backend::sync::websocket::{WsState, ws_upgrade_handler};
    let ws_state = WsState {
        registry: session_registry.clone(),
        sync_service: sync_service.clone(),
        jwt_secret: Arc::new(jwt_secret.to_string()),
    };

    let app = Router::new()
        .route("/sync/ws", get(ws_upgrade_handler))
        .with_state(ws_state);

    // Start a TCP listener
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service()).await.unwrap();
    });

    // Give server a moment to start
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Connect via WebSocket
    let ws_url = format!(
        "ws://{}/sync/ws?token={}&workspace_id={}",
        addr, token, workspace_id
    );
    let (mut ws_stream, _) = connect_async(&ws_url).await
        .expect("Failed to connect WebSocket");

    // The server should accept the connection. Let's send a ping and expect a pong.
    let ping_msg = json!({"type": "ping", "timestamp": Utc::now().to_rfc3339()});
    ws_stream.send(Message::Text(serde_json::to_string(&ping_msg).unwrap())).await.unwrap();

    // Read response (should be a pong)
    if let Some(Ok(Message::Text(text))) = ws_stream.next().await {
        let envelope: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(envelope["type"], "pong");
    } else {
        panic!("Expected pong text message from server");
    }

    // Now simulate a delta push via the registry
    let delta_msg = json!({
        "type": "delta",
        "workspace_id": workspace_id.to_string(),
        "version": 1,
        "timestamp": Utc::now().to_rfc3339(),
        "payload": {"trigger": "test", "content": "pushed"}
    });
    session_registry.broadcast_to_workspace(workspace_id, delta_msg.clone(), None);

    // Read the pushed delta
    if let Some(Ok(Message::Text(text))) = ws_stream.next().await {
        let envelope: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(envelope["type"], "delta");
        assert_eq!(envelope["version"], 1);
    } else {
        panic!("Expected delta message from broadcast");
    }

    // Close the connection
    ws_stream.close(None).await.unwrap();

    cleanup(&pool, user_id, workspace_id).await;
}

// ─── Test: Workspace not found returns 404 ──────────────────────────────────────
// Requirements: 2.41

#[tokio::test]
#[ignore]
async fn test_nonexistent_workspace_returns_404() {
    let pool = setup_test_db().await;
    let (user_id, workspace_id) = seed_user_and_workspace(&pool).await;
    let app = build_test_router(pool.clone());
    let claims = test_claims(user_id);

    let fake_workspace = Uuid::new_v4();
    let body = json!({
        "workspace_id": fake_workspace,
        "trigger": "orphan",
        "content": "No workspace",
        "snippet_type": "text"
    });
    let resp = app.oneshot(post_json("/sync/snippets", body, &claims)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    cleanup(&pool, user_id, workspace_id).await;
}

// ─── Test: Deltas ordered ascending by version ──────────────────────────────────
// Requirements: 2.14

#[tokio::test]
#[ignore]
async fn test_deltas_ordered_ascending() {
    let pool = setup_test_db().await;
    let (user_id, workspace_id) = seed_user_and_workspace(&pool).await;
    let app = build_test_router(pool.clone());
    let claims = test_claims(user_id);

    // Create several snippets
    for i in 1..=5 {
        let body = json!({
            "workspace_id": workspace_id,
            "trigger": format!("ord_{}", i),
            "content": format!("c{}", i),
            "snippet_type": "text"
        });
        app.clone().oneshot(post_json("/sync/snippets", body, &claims)).await.unwrap();
    }

    let app2 = build_test_router(pool.clone());
    let uri = format!("/sync/deltas?workspace_id={}&since_version=0", workspace_id);
    let resp = app2.oneshot(get_req(&uri, &claims)).await.unwrap();
    let json = body_json(resp).await;
    let deltas = json["deltas"].as_array().unwrap();

    // Verify ascending order
    let mut prev_version = 0i64;
    for delta in deltas {
        let v = delta["version"].as_i64().unwrap();
        assert!(v > prev_version, "Deltas must be in ascending version order");
        prev_version = v;
    }

    cleanup(&pool, user_id, workspace_id).await;
}
