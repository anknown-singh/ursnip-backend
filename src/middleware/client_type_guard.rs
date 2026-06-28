//! Client type enforcement middleware.
//!
//! Restricts access to endpoint groups based on the client type (native vs web)
//! encoded in the authenticated user's access token claims.
//!
//! Route prefix rules:
//! - `/sync/*`          → native only
//! - `/subscriptions/*` → web only
//! - `/teams/*`         → web only
//! - `/admin/*`         → web only
//! - `/ai/*`            → native only
//!
//! Returns 403 `CLIENT_TYPE_NOT_ALLOWED` on mismatch.
//! If no claims are present in request extensions (unauthenticated/public route),
//! the middleware passes through without enforcement.

use axum::{
    extract::Request,
    middleware::Next,
    response::Response,
};

use crate::errors::AppError;
use crate::middleware::auth_extractor::AccessTokenClaims;
use crate::models::ClientType;

/// Rules mapping path prefixes to the required client type.
const CLIENT_TYPE_RULES: &[(&str, ClientType)] = &[
    ("/sync/", ClientType::Native),
    ("/subscriptions/", ClientType::Web),
    ("/teams/", ClientType::Web),
    ("/admin/", ClientType::Web),
    ("/ai/", ClientType::Native),
];

/// Middleware function that enforces client type restrictions per route prefix.
///
/// Must run after the auth middleware that inserts `AccessTokenClaims` into
/// request extensions.
///
/// # Behavior
///
/// - If claims are present, checks the request path against `CLIENT_TYPE_RULES`.
/// - If the path matches a rule and the token's `client_type` differs from the
///   required type, returns `AppError::ClientTypeNotAllowed` (HTTP 403).
/// - If no claims are present (public/unauthenticated route), passes through.
/// - If the path doesn't match any rule, passes through.
pub async fn client_type_guard(request: Request, next: Next) -> Result<Response, AppError> {
    // If no claims in extensions, this is a public route — pass through.
    let claims = request.extensions().get::<AccessTokenClaims>().cloned();

    if let Some(claims) = claims {
        let path = request.uri().path();

        for (prefix, required_type) in CLIENT_TYPE_RULES {
            if path.starts_with(prefix) && claims.client_type != *required_type {
                return Err(AppError::ClientTypeNotAllowed);
            }
        }
    }

    Ok(next.run(request).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request as HttpRequest, http::StatusCode, routing::get, Router};
    use tower::ServiceExt;
    use uuid::Uuid;

    use crate::models::common::{Role, Tier};

    /// Helper: build a test router with the client_type_guard middleware.
    fn test_router() -> Router {
        Router::new()
            .route("/sync/push", get(|| async { "ok" }))
            .route("/subscriptions/plans", get(|| async { "ok" }))
            .route("/teams/list", get(|| async { "ok" }))
            .route("/admin/users", get(|| async { "ok" }))
            .route("/ai/expand", get(|| async { "ok" }))
            .route("/health", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(client_type_guard))
    }

    /// Helper: create test claims with the given client type.
    fn make_claims(client_type: ClientType) -> AccessTokenClaims {
        AccessTokenClaims {
            sub: Uuid::new_v4(),
            client_type,
            role: Role::User,
            permissions: vec![],
            subscription_tier: Tier::Free,
            status: "active".to_string(),
            must_reset_password: false,
            exp: chrono::Utc::now().timestamp() + 3600,
        }
    }

    /// Helper: build a request to `uri` with claims inserted into extensions.
    fn request_with_claims(uri: &str, client_type: ClientType) -> HttpRequest<Body> {
        let mut req = HttpRequest::builder()
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(make_claims(client_type));
        req
    }

    /// Helper: build a request to `uri` without claims (public route).
    fn request_without_claims(uri: &str) -> HttpRequest<Body> {
        HttpRequest::builder()
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    }

    // --- /sync/* tests ---

    #[tokio::test]
    async fn sync_allows_native_client() {
        let app = test_router();
        let req = request_with_claims("/sync/push", ClientType::Native);
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn sync_rejects_web_client() {
        let app = test_router();
        let req = request_with_claims("/sync/push", ClientType::Web);
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    // --- /subscriptions/* tests ---

    #[tokio::test]
    async fn subscriptions_allows_web_client() {
        let app = test_router();
        let req = request_with_claims("/subscriptions/plans", ClientType::Web);
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn subscriptions_rejects_native_client() {
        let app = test_router();
        let req = request_with_claims("/subscriptions/plans", ClientType::Native);
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    // --- /teams/* tests ---

    #[tokio::test]
    async fn teams_allows_web_client() {
        let app = test_router();
        let req = request_with_claims("/teams/list", ClientType::Web);
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn teams_rejects_native_client() {
        let app = test_router();
        let req = request_with_claims("/teams/list", ClientType::Native);
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    // --- /admin/* tests ---

    #[tokio::test]
    async fn admin_allows_web_client() {
        let app = test_router();
        let req = request_with_claims("/admin/users", ClientType::Web);
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn admin_rejects_native_client() {
        let app = test_router();
        let req = request_with_claims("/admin/users", ClientType::Native);
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    // --- /ai/* tests ---

    #[tokio::test]
    async fn ai_allows_native_client() {
        let app = test_router();
        let req = request_with_claims("/ai/expand", ClientType::Native);
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn ai_rejects_web_client() {
        let app = test_router();
        let req = request_with_claims("/ai/expand", ClientType::Web);
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    // --- Pass-through tests ---

    #[tokio::test]
    async fn unmatched_path_passes_through() {
        let app = test_router();
        let req = request_with_claims("/health", ClientType::Native);
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn no_claims_passes_through() {
        let app = test_router();
        let req = request_without_claims("/sync/push");
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
