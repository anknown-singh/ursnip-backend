//! Integration tests for the middleware stack.
//!
//! These tests exercise the full HTTP request/response cycle through the Axum
//! router, covering client type enforcement, rate limiting, CORS preflight
//! handling, security headers, body size limit enforcement, and error response
//! format consistency.
//!
//! Requirements: 1.5, 1.6, 7.22–7.27, 7.47–7.53, 7.83, 7.84
//!
//! Property-based tests validate:
//! - Property 1: Client type endpoint enforcement
//! - Property 22: Error response format consistency
//! - Property 23: Rate limiting sliding window
//! - Property 24: Client IP resolution

use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
    middleware,
    routing::{get, post},
    Router,
};
use chrono::Utc;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

use ursnip_backend::middleware::auth_extractor::AccessTokenClaims;
use ursnip_backend::middleware::body_limit::{body_limit_layer, DEFAULT_BODY_LIMIT, SYNC_BODY_LIMIT};
use ursnip_backend::middleware::client_type_guard::client_type_guard;
use ursnip_backend::middleware::cors::cors_middleware;
use ursnip_backend::middleware::rate_limit::{RateLimiter, SlidingWindow};
use ursnip_backend::middleware::security_headers::security_headers;
use ursnip_backend::middleware::trace_id::trace_id_layer;
use ursnip_backend::models::common::{ClientType, Role, Tier};

// ─── Test Constants ─────────────────────────────────────────────────────────────

const TEST_JWT_SECRET: &str = "test-middleware-integration-jwt-secret";

// ─── Test Helpers ───────────────────────────────────────────────────────────────

/// Create claims for a given user ID and client type.
fn make_claims(client_type: ClientType) -> AccessTokenClaims {
    AccessTokenClaims {
        sub: Uuid::new_v4(),
        client_type,
        role: Role::User,
        permissions: vec![],
        subscription_tier: Tier::Free,
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

/// Extract response body as JSON Value.
async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

/// Build a test router with auth + client_type_guard middleware layered.
/// Includes routes for all restricted endpoint groups.
fn build_client_type_test_app() -> Router {
    let jwt_secret = Arc::new(TEST_JWT_SECRET.to_string());

    Router::new()
        .route("/sync/snippets", get(|| async { "sync" }))
        .route("/ai/expand", post(|| async { "ai" }))
        .route("/subscriptions/current", get(|| async { "subs" }))
        .route("/teams/list", get(|| async { "teams" }))
        .route("/admin/users", get(|| async { "admin" }))
        .route("/auth/login", post(|| async { "login" }))
        .route("/health", get(|| async { "ok" }))
        .layer(middleware::from_fn(client_type_guard))
        .layer(middleware::from_fn(move |req, next| {
            let secret = jwt_secret.clone();
            ursnip_backend::middleware::auth_extractor::auth_middleware(req, next, secret)
        }))
}

/// Build a test router with full global middleware stack (trace_id, security_headers, cors, body_limit).
fn build_full_middleware_app() -> Router {
    let jwt_secret = Arc::new(TEST_JWT_SECRET.to_string());
    let allowed_origins = Arc::new(vec!["https://app.ursnip.com".to_string()]);

    let app = Router::new()
        .route("/sync/snippets", post(|| async { "sync" }))
        .route("/ai/expand", post(|| async { "ai" }))
        .route("/subscriptions/current", get(|| async { "subs" }))
        .route("/teams/list", get(|| async { "teams" }))
        .route("/admin/users", get(|| async { "admin" }))
        .route("/health", get(|| async { "ok" }))
        .layer(middleware::from_fn(client_type_guard))
        .layer(middleware::from_fn(move |req, next| {
            let secret = jwt_secret.clone();
            ursnip_backend::middleware::auth_extractor::auth_middleware(req, next, secret)
        }));

    let origins_clone = allowed_origins.clone();
    app.layer(body_limit_layer(DEFAULT_BODY_LIMIT))
        .layer(middleware::from_fn(move |req, next| {
            cors_middleware(origins_clone.clone(), req, next)
        }))
        .layer(middleware::from_fn(security_headers))
        .layer(middleware::from_fn(trace_id_layer))
}

// ═══════════════════════════════════════════════════════════════════════════════
// Client Type Enforcement Tests
// Requirements: 1.5, 1.6
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_sync_allows_native_rejects_web() {
    let app = build_client_type_test_app();

    // Native can access /sync/*
    let claims = make_claims(ClientType::Native);
    let token = make_token(&claims);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/sync/snippets")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Web is rejected from /sync/*
    let claims = make_claims(ClientType::Web);
    let token = make_token(&claims);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/sync/snippets")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_ai_allows_native_rejects_web() {
    let app = build_client_type_test_app();

    // Native can access /ai/*
    let claims = make_claims(ClientType::Native);
    let token = make_token(&claims);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ai/expand")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Web is rejected from /ai/*
    let claims = make_claims(ClientType::Web);
    let token = make_token(&claims);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ai/expand")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_subscriptions_allows_web_rejects_native() {
    let app = build_client_type_test_app();

    // Web can access /subscriptions/*
    let claims = make_claims(ClientType::Web);
    let token = make_token(&claims);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/subscriptions/current")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Native is rejected from /subscriptions/*
    let claims = make_claims(ClientType::Native);
    let token = make_token(&claims);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/subscriptions/current")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_teams_allows_web_rejects_native() {
    let app = build_client_type_test_app();

    // Web can access /teams/*
    let claims = make_claims(ClientType::Web);
    let token = make_token(&claims);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/teams/list")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Native is rejected from /teams/*
    let claims = make_claims(ClientType::Native);
    let token = make_token(&claims);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/teams/list")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_admin_allows_web_rejects_native() {
    let app = build_client_type_test_app();

    // Web can access /admin/*
    let claims = make_claims(ClientType::Web);
    let token = make_token(&claims);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/users")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Native is rejected from /admin/*
    let claims = make_claims(ClientType::Native);
    let token = make_token(&claims);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/admin/users")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ═══════════════════════════════════════════════════════════════════════════════
// Rate Limiting Tests (IP and user-level)
// Requirements: 7.50, 7.51, 7.52, 7.53, 7.83, 7.84
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_ip_rate_limit_allows_within_threshold() {
    let rl = RateLimiter::new(&[]);
    // 100 requests should all succeed
    for _ in 0..100 {
        assert!(rl.check_ip("192.168.1.1").is_ok());
    }
}

#[test]
fn test_ip_rate_limit_denies_over_threshold() {
    let rl = RateLimiter::new(&[]);
    for _ in 0..100 {
        rl.check_ip("10.0.0.1").unwrap();
    }
    // 101st request should fail
    let result = rl.check_ip("10.0.0.1");
    assert!(result.is_err());
}

#[test]
fn test_user_rate_limit_allows_within_threshold() {
    let rl = RateLimiter::new(&[]);
    for _ in 0..500 {
        assert!(rl.check_user("user-1").is_ok());
    }
}

#[test]
fn test_user_rate_limit_denies_over_threshold() {
    let rl = RateLimiter::new(&[]);
    for _ in 0..500 {
        rl.check_user("user-1").unwrap();
    }
    let result = rl.check_user("user-1");
    assert!(result.is_err());
}

#[test]
fn test_different_ips_are_independent() {
    let rl = RateLimiter::new(&[]);
    // Fill up one IP
    for _ in 0..100 {
        rl.check_ip("10.0.0.1").unwrap();
    }
    // Another IP should still be allowed
    assert!(rl.check_ip("10.0.0.2").is_ok());
}

// ═══════════════════════════════════════════════════════════════════════════════
// CORS Preflight Handling Tests
// Requirements: 7.15–7.21
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_cors_preflight_allowed_origin_returns_204() {
    let origins = Arc::new(vec!["https://app.ursnip.com".to_string()]);
    let origins_clone = origins.clone();

    let app = Router::new()
        .route("/test", get(|| async { "ok" }))
        .layer(middleware::from_fn(move |req, next| {
            cors_middleware(origins_clone.clone(), req, next)
        }));

    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/test")
                .header(header::ORIGIN, "https://app.ursnip.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        resp.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .unwrap(),
        "https://app.ursnip.com"
    );
    assert!(resp
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_METHODS)
        .is_some());
    assert!(resp
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_HEADERS)
        .is_some());
    assert_eq!(
        resp.headers()
            .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
            .unwrap(),
        "true"
    );
}

#[tokio::test]
async fn test_cors_preflight_disallowed_origin_no_headers() {
    let origins = Arc::new(vec!["https://app.ursnip.com".to_string()]);
    let origins_clone = origins.clone();

    let app = Router::new()
        .route("/test", get(|| async { "ok" }))
        .layer(middleware::from_fn(move |req, next| {
            cors_middleware(origins_clone.clone(), req, next)
        }));

    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/test")
                .header(header::ORIGIN, "https://evil.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(resp
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .is_none());
}

#[tokio::test]
async fn test_cors_empty_origins_rejects_all() {
    let origins = Arc::new(vec![]);
    let origins_clone = origins.clone();

    let app = Router::new()
        .route("/test", get(|| async { "ok" }))
        .layer(middleware::from_fn(move |req, next| {
            cors_middleware(origins_clone.clone(), req, next)
        }));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/test")
                .header(header::ORIGIN, "https://app.ursnip.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .is_none());
}

// ═══════════════════════════════════════════════════════════════════════════════
// Security Headers Tests
// Requirements: 7.47, 7.48, 7.49
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_security_headers_present_in_responses() {
    let app = build_full_middleware_app();

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let headers = resp.headers();
    assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(headers.get("x-frame-options").unwrap(), "DENY");
    assert_eq!(
        headers.get("referrer-policy").unwrap(),
        "strict-origin-when-cross-origin"
    );
    assert_eq!(headers.get("x-xss-protection").unwrap(), "0");
    assert_eq!(
        headers.get("strict-transport-security").unwrap(),
        "max-age=31536000; includeSubDomains"
    );
    assert_eq!(
        headers.get("content-security-policy").unwrap(),
        "default-src 'none'"
    );
}

#[tokio::test]
async fn test_trace_id_header_present() {
    let app = build_full_middleware_app();

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let trace_id_header = resp.headers().get("x-trace-id");
    assert!(trace_id_header.is_some());
    // Verify it's a valid UUID
    let trace_id_str = trace_id_header.unwrap().to_str().unwrap();
    assert!(Uuid::parse_str(trace_id_str).is_ok());
}

// ═══════════════════════════════════════════════════════════════════════════════
// Body Size Limit Enforcement Tests
// Requirements: 7.22, 7.23, 7.24
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_body_limit_rejects_oversized_request() {
    let app = Router::new()
        .route("/test", post(|| async { "ok" }))
        .layer(body_limit_layer(DEFAULT_BODY_LIMIT));

    let over_limit = DEFAULT_BODY_LIMIT + 1;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/test")
                .header("content-length", over_limit.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn test_body_limit_allows_within_limit() {
    let app = Router::new()
        .route("/test", post(|| async { "ok" }))
        .layer(body_limit_layer(DEFAULT_BODY_LIMIT));

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/test")
                .header("content-length", "1000")
                .body(Body::from("x".repeat(1000)))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_sync_routes_allow_elevated_10mb_limit() {
    let app = Router::new()
        .route("/sync/push", post(|| async { "ok" }))
        .layer(body_limit_layer(SYNC_BODY_LIMIT));

    // 5 MB should be fine for sync routes
    let five_mb = 5 * 1024 * 1024;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sync/push")
                .header("content-length", five_mb.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

// ═══════════════════════════════════════════════════════════════════════════════
// Error Response Format Consistency Tests
// Requirements: 7.25, 7.26, 7.27
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_error_response_has_trace_id_and_error_code() {
    let app = build_client_type_test_app();

    // Trigger a 403 error via client type mismatch
    let claims = make_claims(ClientType::Web);
    let token = make_token(&claims);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/sync/snippets")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = body_json(resp).await;

    // Must have trace_id as a UUID
    assert!(body["trace_id"].is_string());
    let trace_id_str = body["trace_id"].as_str().unwrap();
    assert!(Uuid::parse_str(trace_id_str).is_ok());

    // Must have error.code
    assert!(body["error"]["code"].is_string());
    assert_eq!(body["error"]["code"], "CLIENT_TYPE_NOT_ALLOWED");

    // Must have error.message
    assert!(body["error"]["message"].is_string());
}

#[tokio::test]
async fn test_error_response_401_format() {
    let app = build_client_type_test_app();

    // Missing auth on protected route → 401
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/sync/snippets")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = body_json(resp).await;

    assert!(body["trace_id"].is_string());
    assert!(body["error"]["code"].is_string());
    assert!(body["error"]["message"].is_string());
}

#[tokio::test]
async fn test_error_response_413_format() {
    let app = Router::new()
        .route("/test", post(|| async { "ok" }))
        .layer(body_limit_layer(1024));

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/test")
                .header("content-length", "2048")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = body_json(resp).await;

    assert!(body["trace_id"].is_string());
    assert_eq!(body["error"]["code"], "REQUEST_BODY_TOO_LARGE");
    assert!(body["error"]["message"].is_string());
}

// ═══════════════════════════════════════════════════════════════════════════════
// Property-Based Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod property_tests {
    use super::*;
    use proptest::prelude::*;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    // ─────────────────────────────────────────────────────────────────────────
    // Property 1: Client type endpoint enforcement
    // **Validates: Requirements 1.5, 1.6**
    //
    // For any combination of client_type (native/web) and endpoint prefix,
    // the guard correctly allows or denies:
    //   - /sync/*, /ai/* → native only
    //   - /subscriptions/*, /teams/*, /admin/* → web only
    //   - Other paths → both allowed
    // ─────────────────────────────────────────────────────────────────────────

    /// Strategy for generating a client type.
    fn client_type_strategy() -> impl Strategy<Value = ClientType> {
        prop_oneof![Just(ClientType::Native), Just(ClientType::Web),]
    }

    /// Strategy for generating endpoint paths with their expected client type.
    /// Returns (path, required_client_type_or_none).
    fn endpoint_strategy() -> impl Strategy<Value = (&'static str, Option<ClientType>)> {
        prop_oneof![
            Just(("/sync/snippets", Some(ClientType::Native))),
            Just(("/sync/snapshot", Some(ClientType::Native))),
            Just(("/ai/expand", Some(ClientType::Native))),
            Just(("/subscriptions/current", Some(ClientType::Web))),
            Just(("/teams/list", Some(ClientType::Web))),
            Just(("/admin/users", Some(ClientType::Web))),
            Just(("/health", None)),
            Just(("/auth/login", None)),
        ]
    }

    proptest! {
        #[test]
        fn prop_client_type_endpoint_enforcement(
            (endpoint, required_type) in endpoint_strategy(),
            client_type in client_type_strategy()
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let app = build_client_type_test_app();

                let claims = make_claims(client_type.clone());
                let token = make_token(&claims);

                let resp = app
                    .oneshot(
                        Request::builder()
                            .uri(endpoint)
                            .header("Authorization", format!("Bearer {}", token))
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();

                match required_type {
                    Some(required) => {
                        if client_type == required {
                            // Correct client type → should be allowed
                            prop_assert_ne!(
                                resp.status(),
                                StatusCode::FORBIDDEN,
                                "Expected access for {:?} on {}",
                                client_type,
                                endpoint
                            );
                        } else {
                            // Wrong client type → must be 403
                            prop_assert_eq!(
                                resp.status(),
                                StatusCode::FORBIDDEN,
                                "Expected 403 for {:?} on {}",
                                client_type,
                                endpoint
                            );
                        }
                    }
                    None => {
                        // Unrestricted endpoint → any client type is allowed
                        prop_assert_ne!(
                            resp.status(),
                            StatusCode::FORBIDDEN,
                            "Unrestricted endpoint {} should not return 403",
                            endpoint
                        );
                    }
                }

                Ok(())
            })?;
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Property 22: Error response format consistency
    // **Validates: Requirements 7.25, 7.26, 7.27**
    //
    // All error responses must contain:
    //   - trace_id: a valid UUID string
    //   - error.code: a SCREAMING_SNAKE_CASE string
    //   - error.message: a non-empty string
    // ─────────────────────────────────────────────────────────────────────────

    /// Strategy that generates various error-triggering scenarios.
    fn error_scenario_strategy() -> impl Strategy<Value = (&'static str, &'static str, &'static str)>
    {
        prop_oneof![
            // (method, uri, description) — all should produce errors
            Just(("GET", "/sync/snippets", "no_auth_on_protected")),
            Just(("GET", "/sync/snippets", "web_on_native_only")),
            Just(("GET", "/admin/users", "native_on_web_only")),
            Just(("GET", "/subscriptions/current", "native_on_web_only")),
            Just(("GET", "/teams/list", "native_on_web_only")),
        ]
    }

    proptest! {
        #[test]
        fn prop_error_response_format_consistency(
            (method, uri, scenario) in error_scenario_strategy()
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let app = build_client_type_test_app();

                let request = match scenario {
                    "no_auth_on_protected" => {
                        Request::builder()
                            .method(method)
                            .uri(uri)
                            .body(Body::empty())
                            .unwrap()
                    }
                    "web_on_native_only" => {
                        let claims = make_claims(ClientType::Web);
                        let token = make_token(&claims);
                        Request::builder()
                            .method(method)
                            .uri(uri)
                            .header("Authorization", format!("Bearer {}", token))
                            .body(Body::empty())
                            .unwrap()
                    }
                    "native_on_web_only" => {
                        let claims = make_claims(ClientType::Native);
                        let token = make_token(&claims);
                        Request::builder()
                            .method(method)
                            .uri(uri)
                            .header("Authorization", format!("Bearer {}", token))
                            .body(Body::empty())
                            .unwrap()
                    }
                    _ => unreachable!(),
                };

                let resp = app.oneshot(request).await.unwrap();
                let status = resp.status();

                // Only check error responses (4xx/5xx)
                if status.as_u16() >= 400 {
                    let body = body_json(resp).await;

                    // Must have trace_id
                    prop_assert!(
                        body["trace_id"].is_string(),
                        "Error response must have trace_id, got: {:?}",
                        body
                    );
                    let trace_id_str = body["trace_id"].as_str().unwrap();
                    prop_assert!(
                        Uuid::parse_str(trace_id_str).is_ok(),
                        "trace_id must be valid UUID, got: {}",
                        trace_id_str
                    );

                    // Must have error.code (SCREAMING_SNAKE_CASE)
                    prop_assert!(
                        body["error"]["code"].is_string(),
                        "Error response must have error.code"
                    );
                    let code = body["error"]["code"].as_str().unwrap();
                    prop_assert!(
                        code.chars().all(|c| c.is_uppercase() || c == '_'),
                        "error.code must be SCREAMING_SNAKE_CASE, got: {}",
                        code
                    );

                    // Must have error.message
                    prop_assert!(
                        body["error"]["message"].is_string(),
                        "Error response must have error.message"
                    );
                    let msg = body["error"]["message"].as_str().unwrap();
                    prop_assert!(
                        !msg.is_empty(),
                        "error.message must not be empty"
                    );
                }

                Ok(())
            })?;
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Property 23: Rate limiting sliding window
    // **Validates: Requirements 7.50, 7.51, 7.52, 7.53, 7.83, 7.84**
    //
    // For any number of requests N within a window:
    //   - The first min(N, limit) requests succeed
    //   - Requests beyond the limit are rejected
    //   - Different keys are independent
    // ─────────────────────────────────────────────────────────────────────────

    proptest! {
        #[test]
        fn prop_rate_limiting_sliding_window(
            num_requests in 1usize..200,
            max_limit in 1usize..50,
            window_secs in 1u64..120,
        ) {
            let mut window = SlidingWindow::new();
            let window_duration = Duration::from_secs(window_secs);

            let mut allowed = 0;
            let mut denied = 0;

            for _ in 0..num_requests {
                if window.check_and_record(window_duration, max_limit) {
                    allowed += 1;
                } else {
                    denied += 1;
                }
            }

            // Exactly min(num_requests, max_limit) should be allowed
            let expected_allowed = num_requests.min(max_limit);
            prop_assert_eq!(
                allowed, expected_allowed,
                "Expected {} allowed, got {} (requests={}, limit={})",
                expected_allowed, allowed, num_requests, max_limit
            );

            // The rest should be denied
            let expected_denied = num_requests.saturating_sub(max_limit);
            prop_assert_eq!(
                denied, expected_denied,
                "Expected {} denied, got {} (requests={}, limit={})",
                expected_denied, denied, num_requests, max_limit
            );
        }
    }

    proptest! {
        #[test]
        fn prop_rate_limiting_keys_independent(
            key1 in "[a-z]{3,8}",
            key2 in "[a-z]{3,8}",
            requests_key1 in 1usize..150,
        ) {
            // Skip if keys happen to be the same
            prop_assume!(key1 != key2);

            let rl = RateLimiter::new(&[]);

            // Fill up key1
            for _ in 0..requests_key1.min(100) {
                let _ = rl.check_ip(&key1);
            }

            // key2 should always be allowed for its first request
            prop_assert!(
                rl.check_ip(&key2).is_ok(),
                "Independent key should not be affected by other key's rate limit"
            );
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Property 24: Client IP resolution
    // **Validates: Requirements 7.83, 7.84**
    //
    // IP resolution rules:
    //   - If peer is NOT in trusted CIDRs → use peer IP directly
    //   - If peer IS in trusted CIDRs → use rightmost untrusted IP from XFF
    //   - If all XFF IPs are trusted → fall back to peer IP
    //   - If no XFF header → use peer IP
    // ─────────────────────────────────────────────────────────────────────────

    /// Strategy for generating IPv4 addresses.
    fn ipv4_strategy() -> impl Strategy<Value = Ipv4Addr> {
        (1u8..255, 0u8..255, 0u8..255, 1u8..255)
            .prop_map(|(a, b, c, d)| Ipv4Addr::new(a, b, c, d))
    }

    /// Strategy for generating IPs within the 10.0.0.0/8 range (trusted).
    fn trusted_ip_strategy() -> impl Strategy<Value = Ipv4Addr> {
        (0u8..255, 0u8..255, 1u8..255)
            .prop_map(|(b, c, d)| Ipv4Addr::new(10, b, c, d))
    }

    /// Strategy for generating IPs outside trusted ranges.
    fn untrusted_ip_strategy() -> impl Strategy<Value = Ipv4Addr> {
        (11u8..255, 0u8..255, 0u8..255, 1u8..255)
            .prop_filter("not in 10.0.0.0/8", |(a, _, _, _)| *a != 10)
            .prop_map(|(a, b, c, d)| Ipv4Addr::new(a, b, c, d))
    }

    proptest! {
        #[test]
        fn prop_client_ip_untrusted_peer_uses_peer_directly(
            peer_ip in untrusted_ip_strategy(),
            xff_ip in ipv4_strategy(),
        ) {
            let rl = RateLimiter::new(&["10.0.0.0/8".to_string()]);
            let peer = SocketAddr::new(IpAddr::V4(peer_ip), 12345);
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(
                "x-forwarded-for",
                xff_ip.to_string().parse().unwrap(),
            );

            let resolved = rl.resolve_client_ip(Some(&peer), &headers);

            // When peer is untrusted, XFF is ignored
            prop_assert_eq!(
                resolved,
                peer_ip.to_string(),
                "Untrusted peer should use peer IP directly, not XFF"
            );
        }
    }

    proptest! {
        #[test]
        fn prop_client_ip_trusted_peer_uses_xff_rightmost_untrusted(
            peer_ip in trusted_ip_strategy(),
            client_ip in untrusted_ip_strategy(),
            proxy_ip in trusted_ip_strategy(),
        ) {
            let rl = RateLimiter::new(&["10.0.0.0/8".to_string()]);
            let peer = SocketAddr::new(IpAddr::V4(peer_ip), 12345);
            let mut headers = axum::http::HeaderMap::new();
            // XFF: client_ip, proxy_ip (rightmost untrusted is client_ip)
            let xff_value = format!("{}, {}", client_ip, proxy_ip);
            headers.insert("x-forwarded-for", xff_value.parse().unwrap());

            let resolved = rl.resolve_client_ip(Some(&peer), &headers);

            // Should resolve to the rightmost untrusted IP (client_ip, since proxy_ip is trusted)
            prop_assert_eq!(
                resolved,
                client_ip.to_string(),
                "Trusted peer with XFF should resolve to rightmost untrusted IP"
            );
        }
    }

    proptest! {
        #[test]
        fn prop_client_ip_trusted_peer_all_xff_trusted_falls_back(
            peer_ip in trusted_ip_strategy(),
            xff_ip1 in trusted_ip_strategy(),
            xff_ip2 in trusted_ip_strategy(),
        ) {
            let rl = RateLimiter::new(&["10.0.0.0/8".to_string()]);
            let peer = SocketAddr::new(IpAddr::V4(peer_ip), 12345);
            let mut headers = axum::http::HeaderMap::new();
            let xff_value = format!("{}, {}", xff_ip1, xff_ip2);
            headers.insert("x-forwarded-for", xff_value.parse().unwrap());

            let resolved = rl.resolve_client_ip(Some(&peer), &headers);

            // All XFF IPs are trusted, fall back to peer
            prop_assert_eq!(
                resolved,
                peer_ip.to_string(),
                "When all XFF IPs are trusted, should fall back to peer IP"
            );
        }
    }

    proptest! {
        #[test]
        fn prop_client_ip_trusted_peer_no_xff_uses_peer(
            peer_ip in trusted_ip_strategy(),
        ) {
            let rl = RateLimiter::new(&["10.0.0.0/8".to_string()]);
            let peer = SocketAddr::new(IpAddr::V4(peer_ip), 12345);
            let headers = axum::http::HeaderMap::new();

            let resolved = rl.resolve_client_ip(Some(&peer), &headers);

            prop_assert_eq!(
                resolved,
                peer_ip.to_string(),
                "Trusted peer with no XFF should fall back to peer IP"
            );
        }
    }
}
