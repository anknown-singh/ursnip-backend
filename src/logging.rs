use tracing_subscriber::{fmt, EnvFilter};

/// Initialize the structured JSON logging subscriber.
///
/// Configures `tracing-subscriber` with:
/// - JSON formatting for structured log output
/// - Configurable log level filter (falls back to env `RUST_LOG` if set)
/// - Span events (new + close) for trace ID context propagation
/// - Current span context included in log records
///
/// # Arguments
/// * `log_level` - The default log level filter string (e.g. "info", "debug", "warn").
///   If the `RUST_LOG` environment variable is set, it takes precedence.
pub fn init_logging(log_level: &str) {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(log_level));

    fmt()
        .with_env_filter(env_filter)
        .json()
        .with_current_span(true)
        .with_span_list(true)
        .with_span_events(fmt::format::FmtSpan::NEW | fmt::format::FmtSpan::CLOSE)
        .with_target(true)
        .with_thread_ids(true)
        .with_file(true)
        .with_line_number(true)
        .init();
}
