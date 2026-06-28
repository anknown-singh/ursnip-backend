//! Subscription context injection middleware.
//!
//! Loads the workspace subscription tier, status, and period_end into request
//! extensions so downstream handlers can make tier-based decisions without
//! repeating the lookup logic.
//!
//! Skips injection for admin routes (`/admin/*`) and public routes
//! (`/health`, `/ready`, `/auth/*`, `/webhooks/*`).

use axum::{
    extract::Request,
    middleware::Next,
    response::Response,
};
use chrono::{DateTime, Utc};

use crate::middleware::auth_extractor::AccessTokenClaims;
use crate::models::common::{SubscriptionStatus, Tier};

/// Subscription context injected into request extensions.
///
/// Provides downstream handlers with the current workspace subscription state
/// for tier-gating, quota enforcement, and feature access decisions.
#[derive(Debug, Clone)]
pub struct SubscriptionContext {
    /// The subscription tier (free, pro, teams).
    pub tier: Tier,
    /// Current subscription status.
    pub status: SubscriptionStatus,
    /// When the current billing period ends (if applicable).
    pub period_end: Option<DateTime<Utc>>,
}

/// Routes that should skip subscription context injection (admin prefix).
const ADMIN_PREFIX: &str = "/admin/";

/// Exact public routes that skip subscription context injection.
const SKIP_EXACT_ROUTES: &[&str] = &["/health", "/ready"];

/// Prefix-based public routes that skip subscription context injection.
const SKIP_PREFIX_ROUTES: &[&str] = &["/auth/", "/webhooks/"];

/// Returns `true` if the given path should skip subscription context injection.
fn should_skip(path: &str) -> bool {
    // Admin routes
    if path.starts_with(ADMIN_PREFIX) || path == "/admin" {
        return true;
    }

    // Exact public routes
    if SKIP_EXACT_ROUTES.contains(&path) {
        return true;
    }

    // Prefix-based public routes
    for prefix in SKIP_PREFIX_ROUTES {
        if path.starts_with(prefix) {
            return true;
        }
    }

    false
}

/// Middleware that injects `SubscriptionContext` into request extensions.
///
/// This middleware runs after `auth_middleware`, so `AccessTokenClaims` are
/// available in request extensions for authenticated routes.
///
/// For now, the subscription context is derived directly from the JWT claims'
/// `subscription_tier` field. When the subscription service is fully integrated,
/// this will be replaced with a database lookup for real-time subscription state.
///
/// # Usage
///
/// ```rust,ignore
/// use axum::{middleware, Router};
/// use crate::middleware::subscription_context::subscription_context_middleware;
///
/// let app = Router::new()
///     .route("/snippets", get(list_snippets))
///     .layer(middleware::from_fn(subscription_context_middleware));
/// ```
pub async fn subscription_context_middleware(
    mut request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();

    // Skip for admin and public routes.
    if should_skip(&path) {
        return next.run(request).await;
    }

    // Extract claims from extensions (inserted by auth_middleware).
    // If claims are not present (e.g., the route somehow bypassed auth),
    // just pass through without injecting subscription context.
    if let Some(claims) = request.extensions().get::<AccessTokenClaims>().cloned() {
        let context = SubscriptionContext {
            tier: claims.subscription_tier,
            status: SubscriptionStatus::Active, // Default until DB lookup is wired
            period_end: None,                   // Default until DB lookup is wired
        };
        request.extensions_mut().insert(context);
    }

    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, middleware, routing::get, Extension, Router};
    use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
    use std::sync::Arc;
    use tower::ServiceExt;
    use uuid::Uuid;

    use crate::middleware::auth_extractor::{auth_middleware, AccessTokenClaims};
    use crate::models::common::{ClientType, Role, Tier};

    const TEST_SECRET: &str = "test-jwt-secret-key-for-unit-tests";

    fn make_token(claims: &AccessTokenClaims) -> String {
        encode(
            &Header::new(Algorithm::HS256),
            claims,
            &EncodingKey::from_secret(TEST_SECRET.as_bytes()),
        )
        .unwrap()
    }

    fn default_claims() -> AccessTokenClaims {
        AccessTokenClaims {
            sub: Uuid::new_v4(),
            client_type: ClientType::Native,
            role: Role::User,
            permissions: vec![],
            subscription_tier: Tier::Pro,
            status: "active".to_string(),
            must_reset_password: false,
            exp: chrono::Utc::now().timestamp() + 3600,
        }
    }

    /// Handler that extracts SubscriptionContext and returns tier info.
    async fn tier_handler(
        Extension(ctx): Extension<SubscriptionContext>,
    ) -> String {
        format!("{:?}:{:?}", ctx.tier, ctx.status)
    }

    /// Handler for routes that should NOT have subscription context.
    async fn plain_handler() -> &'static str {
        "ok"
    }

    /// Build a test router with both auth and subscription context middleware.
    fn app() -> Router {
        let secret = Arc::new(TEST_SECRET.to_string());
        Router::new()
            .route("/snippets", get(tier_handler))
            .route("/admin/users", get(plain_handler))
            .route("/health", get(plain_handler))
            .route("/auth/login", get(plain_handler))
            .route("/webhooks/stripe", get(plain_handler))
            .layer(middleware::from_fn(subscription_context_middleware))
            .layer(middleware::from_fn(move |req, next| {
                auth_middleware(req, next, secret.clone())
            }))
    }

    #[tokio::test]
    async fn injects_subscription_context_for_authenticated_route() {
        let claims = default_claims();
        let token = make_token(&claims);

        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/snippets")
                    .header("Authorization", format!("Bearer {}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert_eq!(body_str, "Pro:Active");
    }

    #[tokio::test]
    async fn free_tier_injects_correctly() {
        let mut claims = default_claims();
        claims.subscription_tier = Tier::Free;
        let token = make_token(&claims);

        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/snippets")
                    .header("Authorization", format!("Bearer {}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert_eq!(body_str, "Free:Active");
    }

    #[tokio::test]
    async fn teams_tier_injects_correctly() {
        let mut claims = default_claims();
        claims.subscription_tier = Tier::Teams;
        let token = make_token(&claims);

        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/snippets")
                    .header("Authorization", format!("Bearer {}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert_eq!(body_str, "Teams:Active");
    }

    #[tokio::test]
    async fn skips_health_route() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn skips_auth_routes() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/auth/login")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn skips_webhook_routes() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/webhooks/stripe")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn skips_admin_routes() {
        // Admin routes need auth but should skip subscription context.
        // Since admin_guard isn't applied here, just verify the route responds
        // without subscription context being required.
        let mut claims = default_claims();
        claims.role = Role::Admin;
        let token = make_token(&claims);

        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/admin/users")
                    .header("Authorization", format!("Bearer {}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[test]
    fn should_skip_identifies_admin_routes() {
        assert!(should_skip("/admin/users"));
        assert!(should_skip("/admin/subscriptions"));
        assert!(should_skip("/admin"));
    }

    #[test]
    fn should_skip_identifies_public_routes() {
        assert!(should_skip("/health"));
        assert!(should_skip("/ready"));
        assert!(should_skip("/auth/login"));
        assert!(should_skip("/auth/register"));
        assert!(should_skip("/auth/oauth/google"));
        assert!(should_skip("/webhooks/stripe"));
        assert!(should_skip("/webhooks/paddle/events"));
    }

    #[test]
    fn should_skip_returns_false_for_protected_routes() {
        assert!(!should_skip("/snippets"));
        assert!(!should_skip("/sync/push"));
        assert!(!should_skip("/ai/complete"));
        assert!(!should_skip("/subscriptions/checkout"));
        assert!(!should_skip("/teams/invite"));
    }
}
