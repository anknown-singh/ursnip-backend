//! Panic recovery middleware.
//!
//! Catches panics from downstream handlers, logs the panic message/backtrace
//! at ERROR level with the request's trace_id, and returns a 500 response
//! using the standard `ErrorResponse` format.

use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use futures::FutureExt;
use std::panic::AssertUnwindSafe;
use uuid::Uuid;

use crate::errors::{ErrorBody, ErrorResponse};

/// Axum middleware that wraps the downstream handler with panic recovery.
///
/// When a panic is caught:
/// 1. Extracts the trace_id from request extensions (if available).
/// 2. Logs the panic message at ERROR level with the trace_id.
/// 3. Returns HTTP 500 with the standard `ErrorResponse` body.
///
/// # Usage
///
/// ```rust,ignore
/// use axum::{Router, middleware};
/// use crate::middleware::panic_recovery::panic_recovery_layer;
///
/// let app = Router::new()
///     .layer(middleware::from_fn(panic_recovery_layer));
/// ```
pub async fn panic_recovery_layer(request: Request, next: Next) -> Response {
    // Extract trace_id before passing the request downstream.
    // The trace_id middleware runs before this one and stores it in extensions.
    let trace_id = request
        .extensions()
        .get::<Uuid>()
        .copied()
        .unwrap_or_else(Uuid::new_v4);

    // Wrap the future in AssertUnwindSafe so we can use catch_unwind on it.
    let response = AssertUnwindSafe(next.run(request)).catch_unwind().await;

    match response {
        Ok(response) => response,
        Err(panic_payload) => {
            // Extract the panic message from the payload.
            let panic_message = extract_panic_message(&panic_payload);

            tracing::error!(
                trace_id = %trace_id,
                panic.message = %panic_message,
                "Handler panicked"
            );

            // Build the standard error response.
            let body = ErrorResponse {
                error: ErrorBody {
                    code: "INTERNAL_ERROR".to_string(),
                    message: "Internal error".to_string(),
                    details: None,
                },
                trace_id,
            };

            (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response()
        }
    }
}

/// Extracts a human-readable message from a panic payload.
fn extract_panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::Request as HttpRequest,
        middleware,
        routing::get,
        Router,
    };
    use tower::ServiceExt;

    /// Handler that panics with a string message.
    async fn panicking_handler() -> &'static str {
        panic!("something went wrong");
    }

    /// Handler that completes normally.
    async fn ok_handler() -> &'static str {
        "ok"
    }

    /// Helper to build a test router with trace_id + panic_recovery middleware.
    fn test_router_with_handler(handler: axum::routing::MethodRouter) -> Router {
        Router::new()
            .route("/test", handler)
            .layer(middleware::from_fn(panic_recovery_layer))
            .layer(middleware::from_fn(crate::middleware::trace_id::trace_id_layer))
    }

    #[tokio::test]
    async fn returns_500_on_panic() {
        let app = test_router_with_handler(get(panicking_handler));

        let request = HttpRequest::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn returns_error_response_format_on_panic() {
        let app = test_router_with_handler(get(panicking_handler));

        let request = HttpRequest::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(body["error"]["code"], "INTERNAL_ERROR");
        assert_eq!(body["error"]["message"], "Internal error");
        // trace_id should be present and be a valid UUID
        let trace_id_str = body["trace_id"].as_str().unwrap();
        assert!(Uuid::parse_str(trace_id_str).is_ok());
    }

    #[tokio::test]
    async fn passes_through_normal_responses() {
        let app = test_router_with_handler(get(ok_handler));

        let request = HttpRequest::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body_bytes[..], b"ok");
    }
}
