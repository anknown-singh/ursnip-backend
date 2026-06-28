use axum::{
    body::Body,
    extract::Request,
    http::{header, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::Response,
};
use std::sync::Arc;

/// Allowed HTTP methods for CORS responses.
const ALLOWED_METHODS: &str = "GET, POST, PUT, PATCH, DELETE, OPTIONS";

/// Allowed headers for CORS responses.
const ALLOWED_HEADERS: &str = "Authorization, Content-Type, X-Trace-Id";

/// CORS middleware that checks the `Origin` header against a configured allow-list.
///
/// Behavior:
/// - If `allowed_origins` is empty, no CORS headers are set (browser will block cross-origin requests).
/// - For preflight (OPTIONS) requests from an allowed origin: returns 204 with CORS headers.
/// - For regular requests from an allowed origin: adds CORS headers to the response.
/// - For requests from a disallowed origin: no CORS headers are set.
pub async fn cors_middleware(
    allowed_origins: Arc<Vec<String>>,
    request: Request,
    next: Next,
) -> Response {
    // If no origins are configured, skip all CORS handling (requirement 7.3 #21).
    if allowed_origins.is_empty() {
        return next.run(request).await;
    }

    // Extract the Origin header value from the request.
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // Check if the origin is in the allow-list.
    let is_allowed = origin
        .as_ref()
        .map(|o| allowed_origins.iter().any(|allowed| allowed == o))
        .unwrap_or(false);

    // If this is a preflight OPTIONS request from an allowed origin, short-circuit with 204.
    if request.method() == Method::OPTIONS && is_allowed {
        let origin_value = origin.unwrap(); // safe: is_allowed implies origin is Some
        let mut response = Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .unwrap();

        set_cors_headers(response.headers_mut(), &origin_value);
        return response;
    }

    // Run the inner handler.
    let mut response = next.run(request).await;

    // For non-preflight requests from an allowed origin, attach CORS headers to the response.
    if is_allowed {
        if let Some(origin_value) = origin {
            set_cors_headers(response.headers_mut(), &origin_value);
        }
    }

    response
}

/// Sets the standard CORS response headers.
fn set_cors_headers(headers: &mut axum::http::HeaderMap, origin: &str) {
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_str(origin).unwrap_or_else(|_| HeaderValue::from_static("*")),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static(ALLOWED_METHODS),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static(ALLOWED_HEADERS),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
        HeaderValue::from_static("true"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{middleware, routing::get, Router};
    use tower::ServiceExt;

    /// Helper to build a test router with the CORS middleware.
    fn app(origins: Vec<String>) -> Router {
        let origins = Arc::new(origins);
        Router::new()
            .route("/test", get(|| async { "hello" }))
            .layer(middleware::from_fn(move |req, next| {
                let origins = Arc::clone(&origins);
                cors_middleware(origins, req, next)
            }))
    }

    #[tokio::test]
    async fn allowed_origin_gets_cors_headers() {
        let app = app(vec!["https://example.com".to_string()]);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header(header::ORIGIN, "https://example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .unwrap(),
            "https://example.com"
        );
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
                .unwrap(),
            "true"
        );
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_METHODS)
                .unwrap(),
            ALLOWED_METHODS
        );
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_HEADERS)
                .unwrap(),
            ALLOWED_HEADERS
        );
    }

    #[tokio::test]
    async fn disallowed_origin_gets_no_cors_headers() {
        let app = app(vec!["https://example.com".to_string()]);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header(header::ORIGIN, "https://evil.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none());
    }

    #[tokio::test]
    async fn empty_origins_rejects_all_cors() {
        let app = app(vec![]);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header(header::ORIGIN, "https://example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none());
    }

    #[tokio::test]
    async fn preflight_returns_204_for_allowed_origin() {
        let app = app(vec!["https://example.com".to_string()]);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/test")
                    .header(header::ORIGIN, "https://example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .unwrap(),
            "https://example.com"
        );
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_METHODS)
                .unwrap(),
            ALLOWED_METHODS
        );
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_HEADERS)
                .unwrap(),
            ALLOWED_HEADERS
        );
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
                .unwrap(),
            "true"
        );
    }

    #[tokio::test]
    async fn preflight_from_disallowed_origin_has_no_cors_headers() {
        let app = app(vec!["https://example.com".to_string()]);

        let response = app
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

        // The request passes through to the handler (no short-circuit for disallowed origins)
        assert!(response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none());
    }

    #[tokio::test]
    async fn no_origin_header_gets_no_cors_headers() {
        let app = app(vec!["https://example.com".to_string()]);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none());
    }

    #[tokio::test]
    async fn multiple_allowed_origins() {
        let app = app(vec![
            "https://app.example.com".to_string(),
            "https://admin.example.com".to_string(),
        ]);

        // First origin works
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header(header::ORIGIN, "https://app.example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .unwrap(),
            "https://app.example.com"
        );

        // Second origin works
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header(header::ORIGIN, "https://admin.example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .unwrap(),
            "https://admin.example.com"
        );
    }
}
