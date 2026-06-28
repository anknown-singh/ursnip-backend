use ursnip_backend::config;
use ursnip_backend::db;
use ursnip_backend::email;
use ursnip_backend::logging;
use ursnip_backend::middleware;
use ursnip_backend::router;
use ursnip_backend::scheduler;
use ursnip_backend::sync;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use config::AppConfig;
use email::EmailService;
use middleware::rate_limit::RateLimiter;
use router::{AppState, build_router};
use scheduler::service::SchedulerService;
use sync::session_registry::SessionRegistry;
use tokio::signal;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() {
    // ── 1. Load environment & config ────────────────────────────────────────
    dotenvy::dotenv().ok();

    // ── 2. Initialize structured JSON logging ───────────────────────────────
    let log_level = std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    logging::init_logging(&log_level);

    info!("ursnip-backend starting up");

    // Load configuration from environment
    let config = AppConfig::from_env();
    let config = Arc::new(config);
    let port = config.port;
    let shutdown_timeout_secs = config.shutdown_timeout_secs;

    // ── 3. Create PgPool and run pending migrations ─────────────────────────
    let pool = db::init_pool(&config).await;

    info!("Running database migrations");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .unwrap_or_else(|e| {
            error!("FATAL: database migration failed: {}", e);
            std::process::exit(1);
        });
    info!("Database migrations complete");

    // ── 4. Initialize all services ──────────────────────────────────────────
    let session_registry = Arc::new(SessionRegistry::new(config.ws_max_connections));

    // Email service (warmup — just create the service; no connection test needed)
    let _email_service = EmailService::new(config.clone());
    info!("Email service initialized");

    // ── 5. Start background scheduler ───────────────────────────────────────
    let scheduler = Arc::new(SchedulerService::new(pool.clone(), shutdown_timeout_secs));
    let (scheduler_shutdown_tx, scheduler_handle) = scheduler.clone().start();
    info!("Background scheduler started");

    // ── 6. Set readiness flag ───────────────────────────────────────────────
    let ready = Arc::new(AtomicBool::new(false));

    // All init steps complete — mark ready
    ready.store(true, Ordering::Release);
    info!("All readiness checks passed, service is ready");

    // ── 7. Initialize rate limiter and build router ─────────────────────────
    let rate_limiter = RateLimiter::new(&config.trusted_proxy_cidrs);

    let state = AppState {
        pool: pool.clone(),
        config: config.clone(),
        rate_limiter,
        ready: ready.clone(),
        session_registry: session_registry.clone(),
    };

    let app = build_router(state);

    // ── 8. Bind TCP listener ────────────────────────────────────────────────
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port))
        .await
        .unwrap_or_else(|e| {
            error!("FATAL: failed to bind TCP listener on port {}: {}", port, e);
            std::process::exit(1);
        });

    info!("Listening on 0.0.0.0:{}", port);

    // ── 9. Serve with graceful shutdown ─────────────────────────────────────
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal(
        shutdown_timeout_secs,
        session_registry,
        scheduler_shutdown_tx,
        scheduler_handle,
        pool,
    ))
    .await
    .unwrap_or_else(|e| {
        error!("Server error: {}", e);
        std::process::exit(1);
    });

    info!("shutdown complete");
}

/// Graceful shutdown handler.
///
/// Listens for SIGTERM/SIGINT, then:
/// 1. Stops accepting new HTTP connections (handled by axum's graceful shutdown)
/// 2. Sends WebSocket Close frame (code 1001) to all active sessions
/// 3. Signals the background scheduler to stop
/// 4. Waits up to SHUTDOWN_TIMEOUT_SECS for in-flight requests and background tasks
/// 5. Closes the database pool
/// 6. Logs "shutdown complete" and returns (process exits 0)
async fn shutdown_signal(
    shutdown_timeout_secs: u64,
    session_registry: Arc<SessionRegistry>,
    scheduler_shutdown_tx: watch::Sender<bool>,
    scheduler_handle: JoinHandle<()>,
    pool: sqlx::PgPool,
) {
    let shutdown_timeout = Duration::from_secs(shutdown_timeout_secs);

    // Wait for SIGTERM or SIGINT
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            info!("Received SIGINT, initiating graceful shutdown");
        }
        _ = terminate => {
            info!("Received SIGTERM, initiating graceful shutdown");
        }
    }

    // ── Step 1: Stop accepting new connections ──────────────────────────────
    // (Handled automatically by axum's with_graceful_shutdown — returning from
    // this future stops the listener.)

    // ── Step 2: Close all WebSocket sessions with code 1001 ─────────────────
    info!("Closing all WebSocket sessions");
    session_registry.close_all_sessions(1001, "Server shutting down".to_string());

    // ── Step 3: Signal scheduler to stop ────────────────────────────────────
    info!("Signaling background scheduler to stop");
    let _ = scheduler_shutdown_tx.send(true);

    // ── Step 4: Wait for scheduler tasks to complete (up to timeout) ────────
    match tokio::time::timeout(shutdown_timeout, scheduler_handle).await {
        Ok(Ok(())) => {
            info!("Scheduler shutdown complete");
        }
        Ok(Err(e)) => {
            warn!("Scheduler task panicked during shutdown: {}", e);
        }
        Err(_) => {
            warn!(
                "Scheduler did not stop within {} seconds, force-terminating",
                shutdown_timeout_secs
            );
        }
    }

    // ── Step 5: Close database pool ─────────────────────────────────────────
    info!("Closing database connection pool");
    pool.close().await;

    // ── Step 6: Done ────────────────────────────────────────────────────────
    // The "shutdown complete" log is emitted by main() after this function returns.
}
