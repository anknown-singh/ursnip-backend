//! Request body size enforcement middleware.
//!
//! Provides configurable body size limits that reject requests exceeding the
//! threshold with HTTP 413 (Payload Too Large) via `AppError::RequestBodyTooLarge`.

use axum::{
    extract::Request,
    http::header::CONTENT_LENGTH,
    response::{IntoResponse, Response},
};

use crate::AppError;

/// Default body size limit: 1 MB.
pub const DEFAULT_BODY_LIMIT: usize = 1_048_576; // 1 * 1024 * 1024

/// Elevated body size limit for sync routes: 10 MB.
pub const SYNC_BODY_LIMIT: usize = 10_485_760; // 10 * 1024 * 1024

/// Creates a middleware layer that enforces a request body size limit.
///
/// Inspects the `Content-Length` header and rejects requests that exceed
/// `limit_bytes` with `AppError::RequestBodyTooLarge` (HTTP 413).
///
/// Requests without a `Content-Length` header are passed through.
///
/// # Arguments
///
/// * `limit_bytes` - Maximum allowed body size in bytes.
///
/// # Usage
///
/// ```rust,ignore
/// use axum::Router;
/// use crate::middleware::body_limit::{body_limit_layer, DEFAULT_BODY_LIMIT, SYNC_BODY_LIMIT};
///
/// // Default 1 MB limit for all routes
/// let app = Router::new()
///     .route("/health", get(health))
///     .layer(body_limit_layer(DEFAULT_BODY_LIMIT));
///
/// // 10 MB override for sync routes
/// let sync_routes = Router::new()
///     .route("/sync/push", post(sync_push))
///     .layer(body_limit_layer(SYNC_BODY_LIMIT));
/// ```
pub fn body_limit_layer(limit_bytes: usize) -> BodyLimitLayer {
    BodyLimitLayer { limit_bytes }
}

/// A tower [`Layer`](tower::Layer) that applies body size limit enforcement.
#[derive(Clone, Copy)]
pub struct BodyLimitLayer {
    limit_bytes: usize,
}

impl<S> tower::Layer<S> for BodyLimitLayer {
    type Service = BodyLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        BodyLimitService {
            inner,
            limit_bytes: self.limit_bytes,
        }
    }
}

/// A tower [`Service`](tower::Service) that checks the `Content-Length` header
/// before forwarding the request to the inner service.
///
/// If the content length exceeds the configured limit, the request is rejected
/// with HTTP 413 Payload Too Large (`AppError::RequestBodyTooLarge`).
#[derive(Clone)]
pub struct BodyLimitService<S> {
    inner: S,
    limit_bytes: usize,
}

impl<S> tower::Service<Request> for BodyLimitService<S>
where
    S: tower::Service<Request, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future =
        std::pin::Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let limit_bytes = self.limit_bytes;
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);

        Box::pin(async move {
            if let Some(content_length) = extract_content_length(&request) {
                if content_length > limit_bytes {
                    return Ok(AppError::RequestBodyTooLarge.into_response());
                }
            }
            inner.call(request).await
        })
    }
}

/// Extracts the `Content-Length` header value from a request, if present and valid.
fn extract_content_length(request: &Request) -> Option<usize> {
    request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request as HttpRequest, http::StatusCode, routing::post, Router};
    use tower::ServiceExt;

    /// Helper to build a test router with the body limit middleware.
    fn test_router(limit: usize) -> Router {
        Router::new()
            .route("/test", post(|| async { "ok" }))
            .layer(body_limit_layer(limit))
    }

    #[tokio::test]
    async fn allows_request_within_limit() {
        let app = test_router(DEFAULT_BODY_LIMIT);

        let request = HttpRequest::builder()
            .method("POST")
            .uri("/test")
            .header(CONTENT_LENGTH, "1000")
            .body(Body::from("x".repeat(1000)))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn allows_request_at_exact_limit() {
        let app = test_router(1024);

        let request = HttpRequest::builder()
            .method("POST")
            .uri("/test")
            .header(CONTENT_LENGTH, "1024")
            .body(Body::from("x".repeat(1024)))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn rejects_request_exceeding_limit() {
        let app = test_router(1024);

        let request = HttpRequest::builder()
            .method("POST")
            .uri("/test")
            .header(CONTENT_LENGTH, "1025")
            .body(Body::from("x".repeat(1025)))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn rejects_request_exceeding_default_1mb_limit() {
        let app = test_router(DEFAULT_BODY_LIMIT);
        let over_limit = DEFAULT_BODY_LIMIT + 1;

        let request = HttpRequest::builder()
            .method("POST")
            .uri("/test")
            .header(CONTENT_LENGTH, over_limit.to_string())
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn allows_request_without_content_length() {
        let app = test_router(1024);

        let request = HttpRequest::builder()
            .method("POST")
            .uri("/test")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn sync_limit_allows_larger_bodies() {
        let app = test_router(SYNC_BODY_LIMIT);

        // 5 MB — within the 10 MB sync limit
        let five_mb = 5 * 1024 * 1024;
        let request = HttpRequest::builder()
            .method("POST")
            .uri("/test")
            .header(CONTENT_LENGTH, five_mb.to_string())
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn sync_limit_rejects_over_10mb() {
        let app = test_router(SYNC_BODY_LIMIT);
        let over_sync = SYNC_BODY_LIMIT + 1;

        let request = HttpRequest::builder()
            .method("POST")
            .uri("/test")
            .header(CONTENT_LENGTH, over_sync.to_string())
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
