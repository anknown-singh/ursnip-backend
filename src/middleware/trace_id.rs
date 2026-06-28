use axum::{
    extract::Request,
    http::header::HeaderName,
    middleware::Next,
    response::Response,
};
use uuid::Uuid;

/// Header name for the trace ID propagated to clients.
pub static X_TRACE_ID: HeaderName = HeaderName::from_static("x-trace-id");

/// Axum middleware that generates a UUID v4 trace ID for each request,
/// stores it in request extensions (as `Uuid`), and adds an `X-Trace-Id`
/// response header for client-side correlation.
pub async fn trace_id_layer(mut request: Request, next: Next) -> Response {
    let trace_id = Uuid::new_v4();

    // Store the trace ID in request extensions so downstream handlers
    // and the error handler can retrieve it.
    request.extensions_mut().insert(trace_id);

    let mut response = next.run(request).await;

    // Add X-Trace-Id header to every response.
    response
        .headers_mut()
        .insert(X_TRACE_ID.clone(), trace_id.to_string().parse().unwrap());

    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, middleware, routing::get, Extension, Router};
    use tower::ServiceExt;

    /// Handler that reads the trace_id from extensions and returns it as the body.
    async fn echo_trace_id(Extension(trace_id): Extension<Uuid>) -> String {
        trace_id.to_string()
    }

    fn app() -> Router {
        Router::new()
            .route("/test", get(echo_trace_id))
            .layer(middleware::from_fn(trace_id_layer))
    }

    #[tokio::test]
    async fn response_contains_x_trace_id_header() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        let header_value = response
            .headers()
            .get("x-trace-id")
            .expect("X-Trace-Id header must be present");

        // Verify it's a valid UUID v4
        let parsed: Uuid = header_value.to_str().unwrap().parse().unwrap();
        assert_eq!(parsed.get_version_num(), 4);
    }

    #[tokio::test]
    async fn trace_id_in_extensions_matches_response_header() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // The handler echoes the trace_id from extensions as the response body.
        let header_value = response
            .headers()
            .get("x-trace-id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body_bytes.to_vec()).unwrap();

        assert_eq!(header_value, body_str);
    }

    #[tokio::test]
    async fn each_request_gets_unique_trace_id() {
        let app = app();

        let resp1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let resp2 = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let id1 = resp1.headers().get("x-trace-id").unwrap().to_str().unwrap().to_string();
        let id2 = resp2.headers().get("x-trace-id").unwrap().to_str().unwrap().to_string();

        assert_ne!(id1, id2, "Each request must get a unique trace_id");
    }
}
