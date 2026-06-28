use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{error, info, warn};

use crate::errors::AppError;

// ─── Constants ──────────────────────────────────────────────────────────────────

/// Retry backoff durations for transient failures (Requirement 7.13).
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(5),
    Duration::from_secs(30),
];

/// Maximum number of retry attempts.
const MAX_RETRIES: usize = 3;

// ─── Task Intervals ─────────────────────────────────────────────────────────────

const DELTA_PURGE_INTERVAL: Duration = Duration::from_secs(3600);         // 1 hour
const SOFT_DELETE_CLEANUP_INTERVAL: Duration = Duration::from_secs(21600); // 6 hours
const EXPIRED_TOKEN_CLEANUP_INTERVAL: Duration = Duration::from_secs(3600); // 1 hour
const GRACE_PERIOD_CHECK_INTERVAL: Duration = Duration::from_secs(3600);    // 1 hour
const PAYMENT_DEADLINE_CHECK_INTERVAL: Duration = Duration::from_secs(3600); // 1 hour
const ACCOUNT_HARD_DELETE_CHECK_INTERVAL: Duration = Duration::from_secs(86400); // 24 hours

// ─── SchedulerService ───────────────────────────────────────────────────────────

/// Background task scheduler that runs recurring maintenance tasks at defined intervals.
///
/// Supports:
/// - Interval-based task execution using tokio timers
/// - Retry with exponential backoff for transient failures (Requirement 7.13)
/// - Idempotent execution via timestamp-based filtering
/// - Graceful shutdown: stops spawning new runs and waits for in-progress tasks
pub struct SchedulerService {
    pool: PgPool,
    shutdown_timeout: Duration,
}

impl SchedulerService {
    /// Create a new SchedulerService.
    ///
    /// # Arguments
    /// - `pool` - Database connection pool
    /// - `shutdown_timeout_secs` - Max seconds to wait for in-progress tasks on shutdown
    pub fn new(pool: PgPool, shutdown_timeout_secs: u64) -> Self {
        Self {
            pool,
            shutdown_timeout: Duration::from_secs(shutdown_timeout_secs),
        }
    }

    /// Start all scheduled tasks. Returns a shutdown sender and a join handle.
    ///
    /// Send `true` on the sender to signal shutdown. The join handle completes
    /// once all task loops have stopped (or the shutdown timeout is reached).
    pub fn start(self: Arc<Self>) -> (watch::Sender<bool>, JoinHandle<()>) {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let handle = tokio::spawn(async move {
            let mut handles = Vec::new();

            // Register all 6 recurring tasks
            handles.push(self.spawn_task(
                "delta_purge",
                DELTA_PURGE_INTERVAL,
                shutdown_rx.clone(),
                |svc| Box::pin(async move { svc.delta_purge().await.map(|_| ()) }),
            ));

            handles.push(self.spawn_task(
                "soft_delete_cleanup",
                SOFT_DELETE_CLEANUP_INTERVAL,
                shutdown_rx.clone(),
                |svc| Box::pin(async move { svc.soft_delete_cleanup().await.map(|_| ()) }),
            ));

            handles.push(self.spawn_task(
                "expired_token_cleanup",
                EXPIRED_TOKEN_CLEANUP_INTERVAL,
                shutdown_rx.clone(),
                |svc| Box::pin(async move { svc.expired_token_cleanup().await.map(|_| ()) }),
            ));

            handles.push(self.spawn_task(
                "grace_period_check",
                GRACE_PERIOD_CHECK_INTERVAL,
                shutdown_rx.clone(),
                |svc| Box::pin(async move { svc.grace_period_check().await.map(|_| ()) }),
            ));

            handles.push(self.spawn_task(
                "payment_deadline_check",
                PAYMENT_DEADLINE_CHECK_INTERVAL,
                shutdown_rx.clone(),
                |svc| Box::pin(async move { svc.payment_deadline_check().await.map(|_| ()) }),
            ));

            handles.push(self.spawn_task(
                "account_hard_delete_check",
                ACCOUNT_HARD_DELETE_CHECK_INTERVAL,
                shutdown_rx.clone(),
                |svc| Box::pin(async move { svc.account_hard_delete_check().await.map(|_| ()) }),
            ));

            info!("Scheduler started with {} tasks", handles.len());

            // Wait for all task loops to complete
            for handle in handles {
                let _ = handle.await;
            }

            info!("Scheduler: all task loops stopped");
        });

        (shutdown_tx, handle)
    }

    /// Signal shutdown and wait for in-progress tasks to complete (up to timeout).
    pub async fn shutdown(
        shutdown_tx: watch::Sender<bool>,
        handle: JoinHandle<()>,
        shutdown_timeout: Duration,
    ) {
        info!("Scheduler: initiating graceful shutdown");

        // Signal all task loops to stop
        let _ = shutdown_tx.send(true);

        // Wait for completion up to the timeout
        match time::timeout(shutdown_timeout, handle).await {
            Ok(_) => info!("Scheduler: graceful shutdown complete"),
            Err(_) => warn!(
                "Scheduler: shutdown timed out after {:?}, some tasks may still be running",
                shutdown_timeout
            ),
        }
    }

    /// Spawn a single task loop that runs at the given interval.
    fn spawn_task<F>(
        self: &Arc<Self>,
        name: &'static str,
        interval: Duration,
        mut shutdown_rx: watch::Receiver<bool>,
        handler: F,
    ) -> JoinHandle<()>
    where
        F: Fn(Arc<SchedulerService>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), AppError>> + Send>>
            + Send
            + Sync
            + 'static,
    {
        let svc = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = time::interval(interval);
            // The first tick fires immediately — skip it so tasks don't all run on startup
            ticker.tick().await;

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        // Check shutdown before executing
                        if *shutdown_rx.borrow() {
                            info!("Scheduler task '{}': shutdown received, stopping", name);
                            break;
                        }

                        // Execute with retry
                        Self::run_with_retry(name, &handler, &svc).await;
                    }
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            info!("Scheduler task '{}': shutdown signal received", name);
                            break;
                        }
                    }
                }
            }
        })
    }

    /// Execute a task handler with retry on transient failures.
    ///
    /// Retries up to 3 times with exponential backoff: 1s → 5s → 30s.
    /// Non-transient failures are logged immediately without retry.
    async fn run_with_retry<F>(name: &'static str, handler: &F, svc: &Arc<SchedulerService>)
    where
        F: Fn(Arc<SchedulerService>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), AppError>> + Send>>
            + Send
            + Sync
            + 'static,
    {
        for attempt in 0..MAX_RETRIES {
            match handler(Arc::clone(svc)).await {
                Ok(()) => {
                    return;
                }
                Err(ref err) if Self::is_transient(err) => {
                    if attempt < MAX_RETRIES - 1 {
                        let delay = RETRY_DELAYS[attempt];
                        warn!(
                            task = name,
                            attempt = attempt + 1,
                            max_retries = MAX_RETRIES,
                            delay_secs = delay.as_secs(),
                            error = %err,
                            "Scheduler task failed with transient error, retrying"
                        );
                        time::sleep(delay).await;
                    } else {
                        error!(
                            task = name,
                            attempts = MAX_RETRIES,
                            error = %err,
                            "Scheduler task failed after all retry attempts"
                        );
                    }
                }
                Err(ref err) => {
                    // Non-transient failure — log immediately, do not retry
                    error!(
                        task = name,
                        error = %err,
                        "Scheduler task failed with non-transient error (no retry)"
                    );
                    return;
                }
            }
        }
    }

    /// Classify whether an error is transient (eligible for retry).
    ///
    /// Transient: DatabaseTimeout, ServiceUnavailable, InternalError (DB connection issues).
    /// Non-transient: validation errors, not-found errors, etc.
    fn is_transient(err: &AppError) -> bool {
        matches!(
            err,
            AppError::DatabaseTimeout | AppError::ServiceUnavailable | AppError::InternalError
        )
    }

    // ─── Task Handlers ──────────────────────────────────────────────────────────

    /// Purge sync deltas older than 30 days.
    ///
    /// Idempotent: uses `created_at < NOW() - INTERVAL '30 days'` timestamp filter.
    async fn delta_purge(&self) -> Result<u64, AppError> {
        let result = sqlx::query(
            r#"
            DELETE FROM sync_deltas
            WHERE created_at < NOW() - INTERVAL '30 days'
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Self::map_db_error(e))?;

        let count = result.rows_affected();
        info!(count = count, "delta_purge: removed old sync deltas");
        Ok(count)
    }

    /// Remove soft-deleted snippets and folders older than 30 days.
    ///
    /// Idempotent: uses `deleted_at < NOW() - INTERVAL '30 days'` timestamp filter.
    async fn soft_delete_cleanup(&self) -> Result<u64, AppError> {
        // Delete old soft-deleted snippets
        let snippets_result = sqlx::query(
            r#"
            DELETE FROM snippets
            WHERE deleted_at IS NOT NULL
              AND deleted_at < NOW() - INTERVAL '30 days'
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Self::map_db_error(e))?;

        // Delete old soft-deleted folders
        let folders_result = sqlx::query(
            r#"
            DELETE FROM folders
            WHERE deleted_at IS NOT NULL
              AND deleted_at < NOW() - INTERVAL '30 days'
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Self::map_db_error(e))?;

        let total = snippets_result.rows_affected() + folders_result.rows_affected();
        info!(
            snippets = snippets_result.rows_affected(),
            folders = folders_result.rows_affected(),
            total = total,
            "soft_delete_cleanup: removed permanently deleted items"
        );
        Ok(total)
    }

    /// Remove expired and old revoked tokens.
    ///
    /// Idempotent: uses timestamp-based filtering.
    /// - Refresh tokens: expired OR (revoked AND older than 7 days)
    /// - Password reset tokens: expired OR (used AND older than 7 days)
    async fn expired_token_cleanup(&self) -> Result<u64, AppError> {
        // Clean up refresh tokens
        let refresh_result = sqlx::query(
            r#"
            DELETE FROM refresh_tokens
            WHERE expires_at < NOW()
               OR (revoked = TRUE AND created_at < NOW() - INTERVAL '7 days')
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Self::map_db_error(e))?;

        // Clean up password reset tokens
        let reset_result = sqlx::query(
            r#"
            DELETE FROM password_reset_tokens
            WHERE expires_at < NOW()
               OR (used = TRUE AND created_at < NOW() - INTERVAL '7 days')
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Self::map_db_error(e))?;

        let total = refresh_result.rows_affected() + reset_result.rows_affected();
        info!(
            refresh_tokens = refresh_result.rows_affected(),
            password_reset_tokens = reset_result.rows_affected(),
            total = total,
            "expired_token_cleanup: removed expired/revoked tokens"
        );
        Ok(total)
    }

    /// Check subscriptions with expired grace periods and transition to cancelled.
    ///
    /// Idempotent: only operates on `status = 'past_due' AND grace_period_end < NOW()`.
    async fn grace_period_check(&self) -> Result<u64, AppError> {
        let result = sqlx::query(
            r#"
            UPDATE subscriptions
            SET status = 'cancelled', cancelled_at = NOW(), updated_at = NOW()
            WHERE status = 'past_due'
              AND grace_period_end IS NOT NULL
              AND grace_period_end < NOW()
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Self::map_db_error(e))?;

        let count = result.rows_affected();
        info!(
            count = count,
            "grace_period_check: transitioned past_due subscriptions to cancelled"
        );
        Ok(count)
    }

    /// Check subscriptions past their payment deadline and deactivate.
    ///
    /// Idempotent: only operates on `status = 'pending_payment' AND payment_deadline < NOW()`.
    async fn payment_deadline_check(&self) -> Result<u64, AppError> {
        let result = sqlx::query(
            r#"
            UPDATE subscriptions
            SET status = 'deactivated', updated_at = NOW()
            WHERE status = 'pending_payment'
              AND payment_deadline IS NOT NULL
              AND payment_deadline < NOW()
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Self::map_db_error(e))?;

        let count = result.rows_affected();
        info!(
            count = count,
            "payment_deadline_check: deactivated subscriptions past payment deadline"
        );
        Ok(count)
    }

    /// Hard-delete user data for accounts soft-deleted more than 30 days ago.
    ///
    /// Idempotent: uses `deleted_at < NOW() - INTERVAL '30 days'` timestamp filter.
    /// Cascading deletes handle related data (workspaces, tokens, etc.) via FK constraints.
    async fn account_hard_delete_check(&self) -> Result<u64, AppError> {
        // First, find users eligible for hard deletion
        let user_ids: Vec<uuid::Uuid> = sqlx::query_scalar(
            r#"
            SELECT id FROM users
            WHERE deleted_at IS NOT NULL
              AND deleted_at < NOW() - INTERVAL '30 days'
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Self::map_db_error(e))?;

        if user_ids.is_empty() {
            info!("account_hard_delete_check: no accounts to hard-delete");
            return Ok(0);
        }

        let mut deleted_count: u64 = 0;

        for user_id in &user_ids {
            // Delete user's owned workspaces (cascades to workspace_members, snippets, folders, etc.)
            // Note: ON DELETE RESTRICT on workspaces.owner_id means we need to handle this carefully.
            // First remove workspace members referencing this user in other workspaces
            sqlx::query(
                r#"
                DELETE FROM workspace_members
                WHERE user_id = $1
                "#,
            )
            .bind(user_id)
            .execute(&self.pool)
            .await
            .map_err(|e| Self::map_db_error(e))?;

            // Delete workspaces owned by this user (CASCADE handles child tables)
            sqlx::query(
                r#"
                DELETE FROM workspaces
                WHERE owner_id = $1
                "#,
            )
            .bind(user_id)
            .execute(&self.pool)
            .await
            .map_err(|e| Self::map_db_error(e))?;

            // Delete the user record (CASCADE handles refresh_tokens, oauth_accounts, etc.)
            let result = sqlx::query(
                r#"
                DELETE FROM users
                WHERE id = $1
                  AND deleted_at IS NOT NULL
                  AND deleted_at < NOW() - INTERVAL '30 days'
                "#,
            )
            .bind(user_id)
            .execute(&self.pool)
            .await
            .map_err(|e| Self::map_db_error(e))?;

            deleted_count += result.rows_affected();
        }

        info!(
            count = deleted_count,
            "account_hard_delete_check: hard-deleted user accounts"
        );
        Ok(deleted_count)
    }

    // ─── Helpers ────────────────────────────────────────────────────────────────

    /// Map a sqlx database error to an appropriate AppError.
    ///
    /// Connection/timeout errors → DatabaseTimeout (transient, will be retried).
    /// Other errors → InternalError (transient, will be retried).
    fn map_db_error(err: sqlx::Error) -> AppError {
        match &err {
            sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => {
                warn!(error = %err, "Database pool error (transient)");
                AppError::DatabaseTimeout
            }
            sqlx::Error::Io(_) => {
                warn!(error = %err, "Database IO error (transient)");
                AppError::DatabaseTimeout
            }
            _ => {
                error!(error = %err, "Database error in scheduler task");
                AppError::InternalError
            }
        }
    }
}
