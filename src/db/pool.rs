use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool};
use tracing::error;

use crate::config::AppConfig;

/// Initialize a PostgreSQL connection pool from application configuration.
///
/// Eagerly connects to the database. If the initial connection fails,
/// logs a FATAL error and terminates the process with exit code 1.
///
/// Sets `statement_timeout` on every new connection via `after_connect`.
pub async fn init_pool(config: &AppConfig) -> PgPool {
    let statement_timeout_ms = config.database_statement_timeout_secs * 1000;

    let pool = PgPoolOptions::new()
        .max_connections(config.database_max_connections)
        .min_connections(config.database_min_connections)
        .acquire_timeout(Duration::from_secs(config.database_connect_timeout_secs))
        .idle_timeout(Duration::from_secs(config.database_idle_timeout_secs))
        .after_connect(move |conn, _meta| {
            Box::pin(async move {
                conn.execute(
                    format!("SET statement_timeout = '{}'", statement_timeout_ms).as_str(),
                )
                .await?;
                Ok(())
            })
        })
        .connect(&config.database_url)
        .await;

    match pool {
        Ok(pool) => pool,
        Err(e) => {
            error!("FATAL: failed to connect to database: {}", e);
            std::process::exit(1);
        }
    }
}
