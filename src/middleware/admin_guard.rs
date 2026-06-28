//! Admin role guard middleware.
//!
//! Enforces that requests targeting `/admin/*` routes are made by users
//! with the `Admin` role. Returns HTTP 403 (Forbidden) if the caller
//! does not have admin privileges.
//!
//! This middleware expects `AccessTokenClaims` to already be present in
//! request extensions (inserted by the auth extractor middleware).

use axum::{
    extract::Request,
    middleware::Next,
    response::Response,
};

use crate::errors::AppError;
use crate::middleware::auth_extractor::AccessTokenClaims;
use crate::models::common::Role;

/// Middleware function that guards `/admin/*` routes.
///
/// If the request path starts with `/admin`, the middleware checks that
/// `AccessTokenClaims` are present in request extensions and that the
/// user's role is `Admin`. If either condition fails, the request is
/// rejected with `AppError::Forbidden` (HTTP 403).
///
/// Requests to non-admin paths pass through unconditionally.
///
/// # Usage
///
/// ```rust,ignore
/// use axum::{Router, middleware};
/// use crate::middleware::admin_guard::admin_guard;
///
/// let app = Router::new()
///     .nest("/admin", admin_routes())
///     .layer(middleware::from_fn(admin_guard));
/// ```
pub async fn admin_guard(request: Request, next: Next) -> Result<Response, AppError> {
    if request.uri().path().starts_with("/admin") {
        let claims = request
            .extensions()
            .get::<AccessTokenClaims>()
            .ok_or(AppError::Forbidden)?;

        if claims.role != Role::Admin {
            return Err(AppError::Forbidden);
        }
    }

    Ok(next.run(request).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request as HttpRequest, StatusCode},
        middleware,
        routing::get,
        Router,
    };
    use tower::ServiceExt;
    use uuid::Uuid;

    use crate::models::common::{ClientType, Tier};

    /// Helper to build a test router with the admin guard middleware.
    fn test_router() -> Router {
        Router::new()
            .route("/admin/users", get(|| async { "admin" }))
            .route("/health", get(|| async { "ok" }))
            .layer(middleware::from_fn(admin_guard))
    }

    /// Helper to create AccessTokenClaims with a given role.
    fn make_claims(role: Role) -> AccessTokenClaims {
        AccessTokenClaims {
            sub: Uuid::new_v4(),
            client_type: ClientType::Web,
            role,
            permissions: vec![],
            subscription_tier: Tier::Free,
            status: "active".to_string(),
            must_reset_password: false,
            exp: 9999999999,
        }
    }

    #[tokio::test]
    async fn allows_admin_user_to_access_admin_routes() {
        let app = test_router();
        let claims = make_claims(Role::Admin);

        let mut request = HttpRequest::builder()
            .method("GET")
            .uri("/admin/users")
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(claims);

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn rejects_non_admin_user_on_admin_routes() {
        let app = test_router();
        let claims = make_claims(Role::User);

        let mut request = HttpRequest::builder()
            .method("GET")
            .uri("/admin/users")
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(claims);

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn rejects_request_without_claims_on_admin_routes() {
        let app = test_router();

        let request = HttpRequest::builder()
            .method("GET")
            .uri("/admin/users")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn allows_non_admin_routes_without_claims() {
        let app = test_router();

        let request = HttpRequest::builder()
            .method("GET")
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn allows_non_admin_routes_for_regular_user() {
        let app = test_router();
        let claims = make_claims(Role::User);

        let mut request = HttpRequest::builder()
            .method("GET")
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(claims);

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
