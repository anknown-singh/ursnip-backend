use axum::{
    extract::Request,
    middleware::Next,
    response::Response,
};
use jsonwebtoken::{decode, DecodingKey, Validation, Algorithm};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::errors::AppError;
use crate::models::common::{ClientType, Role, Tier};

/// Claims extracted from a validated access token JWT.
///
/// Injected into request extensions for downstream handlers to consume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessTokenClaims {
    /// User ID (JWT `sub` claim).
    pub sub: Uuid,
    /// Client type: "native" or "web".
    pub client_type: ClientType,
    /// User role: "user" or "admin".
    pub role: Role,
    /// Granted permissions.
    pub permissions: Vec<String>,
    /// Subscription tier.
    pub subscription_tier: Tier,
    /// Account status: "active" or "suspended".
    pub status: String,
    /// Whether the user must reset their password before proceeding.
    pub must_reset_password: bool,
    /// Token expiration (Unix timestamp).
    pub exp: i64,
}

/// Routes that do not require authentication.
const PUBLIC_EXACT_ROUTES: &[&str] = &[
    "/health",
    "/ready",
    "/auth/register",
    "/auth/login",
    "/auth/refresh",
    "/auth/forgot-password",
    "/auth/reset-password",
    "/auth/verify-email-change",
];

/// Route prefixes that do not require authentication.
const PUBLIC_PREFIX_ROUTES: &[&str] = &[
    "/auth/oauth/",
    "/webhooks/",
];

/// Returns `true` if the given path is a public route that skips authentication.
fn is_public_route(path: &str) -> bool {
    if PUBLIC_EXACT_ROUTES.contains(&path) {
        return true;
    }
    for prefix in PUBLIC_PREFIX_ROUTES {
        if path.starts_with(prefix) {
            return true;
        }
    }
    false
}

/// Axum middleware layer that extracts and validates a JWT from the
/// `Authorization: Bearer <token>` header.
///
/// Public routes (health, auth, webhooks) bypass authentication.
/// On success, `AccessTokenClaims` is inserted into request extensions.
///
/// # Usage
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use axum::middleware;
///
/// let jwt_secret = Arc::new(config.jwt_secret.clone());
/// let app = Router::new()
///     .layer(middleware::from_fn(move |req, next| {
///         auth_middleware(req, next, jwt_secret.clone())
///     }));
/// ```
pub async fn auth_middleware(
    mut request: Request,
    next: Next,
    jwt_secret: Arc<String>,
) -> Result<Response, AppError> {
    let path = request.uri().path().to_string();

    // Skip authentication for public routes.
    if is_public_route(&path) {
        return Ok(next.run(request).await);
    }

    // Extract the Authorization header.
    let auth_header = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::Unauthorized)?;

    // Expect "Bearer <token>" format.
    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or(AppError::Unauthorized)?;

    if token.is_empty() {
        return Err(AppError::Unauthorized);
    }

    // Decode and validate the JWT.
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;

    let token_data = decode::<AccessTokenClaims>(
        token,
        &DecodingKey::from_secret(jwt_secret.as_bytes()),
        &validation,
    )
    .map_err(|_| AppError::Unauthorized)?;

    let claims = token_data.claims;

    // Check if the account is suspended.
    if claims.status == "suspended" {
        return Err(AppError::AccountSuspended);
    }

    // Check if password reset is required.
    if claims.must_reset_password {
        return Err(AppError::PasswordResetRequired);
    }

    // Inject claims into request extensions for downstream handlers.
    request.extensions_mut().insert(claims);

    Ok(next.run(request).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, middleware, routing::get, Extension, Router};
    use jsonwebtoken::{encode, EncodingKey, Header};
    use tower::ServiceExt;

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
            permissions: vec!["snippets:read".to_string()],
            subscription_tier: Tier::Free,
            status: "active".to_string(),
            must_reset_password: false,
            exp: chrono::Utc::now().timestamp() + 3600,
        }
    }

    fn app() -> Router {
        let secret = Arc::new(TEST_SECRET.to_string());
        Router::new()
            .route("/protected", get(handler))
            .route("/health", get(|| async { "ok" }))
            .route("/auth/login", get(|| async { "login" }))
            .route("/auth/oauth/google", get(|| async { "oauth" }))
            .route("/webhooks/stripe", get(|| async { "webhook" }))
            .layer(middleware::from_fn(move |req, next| {
                auth_middleware(req, next, secret.clone())
            }))
    }

    async fn handler(Extension(claims): Extension<AccessTokenClaims>) -> String {
        claims.sub.to_string()
    }

    #[tokio::test]
    async fn public_routes_bypass_auth() {
        let app = app();

        // /health
        let resp = app
            .clone()
            .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        // /auth/login
        let resp = app
            .clone()
            .oneshot(Request::builder().uri("/auth/login").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        // /auth/oauth/google (prefix match)
        let resp = app
            .clone()
            .oneshot(Request::builder().uri("/auth/oauth/google").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        // /webhooks/stripe (prefix match)
        let resp = app
            .oneshot(Request::builder().uri("/webhooks/stripe").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn missing_auth_header_returns_401() {
        let resp = app()
            .oneshot(Request::builder().uri("/protected").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn malformed_auth_header_returns_401() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", "Basic abc123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn invalid_token_returns_401() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", "Bearer invalid.token.here")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn expired_token_returns_401() {
        let mut claims = default_claims();
        claims.exp = chrono::Utc::now().timestamp() - 3600; // expired 1 hour ago
        let token = make_token(&claims);

        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn suspended_account_returns_403() {
        let mut claims = default_claims();
        claims.status = "suspended".to_string();
        let token = make_token(&claims);

        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 403);
    }

    #[tokio::test]
    async fn must_reset_password_returns_403() {
        let mut claims = default_claims();
        claims.must_reset_password = true;
        let token = make_token(&claims);

        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 403);
    }

    #[tokio::test]
    async fn valid_token_injects_claims_into_extensions() {
        let claims = default_claims();
        let user_id = claims.sub;
        let token = make_token(&claims);

        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);

        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body_bytes.to_vec()).unwrap();
        assert_eq!(body_str, user_id.to_string());
    }

    #[tokio::test]
    async fn is_public_route_exact_matches() {
        assert!(is_public_route("/health"));
        assert!(is_public_route("/ready"));
        assert!(is_public_route("/auth/register"));
        assert!(is_public_route("/auth/login"));
        assert!(is_public_route("/auth/refresh"));
        assert!(is_public_route("/auth/forgot-password"));
        assert!(is_public_route("/auth/reset-password"));
        assert!(is_public_route("/auth/verify-email-change"));
    }

    #[tokio::test]
    async fn is_public_route_prefix_matches() {
        assert!(is_public_route("/auth/oauth/google"));
        assert!(is_public_route("/auth/oauth/github/callback"));
        assert!(is_public_route("/webhooks/stripe"));
        assert!(is_public_route("/webhooks/paddle/events"));
    }

    #[tokio::test]
    async fn is_public_route_non_matches() {
        assert!(!is_public_route("/protected"));
        assert!(!is_public_route("/api/snippets"));
        assert!(!is_public_route("/auth/change-password"));
        assert!(!is_public_route("/users/me"));
    }
}
