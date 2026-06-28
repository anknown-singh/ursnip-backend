//! Library root for the ursnip-backend crate.
//!
//! This module re-exports internal modules to make them accessible from
//! integration tests in the `tests/` directory.

pub mod admin;
pub mod ai;
pub mod auth;
pub mod config;
pub mod db;
pub mod email;
pub mod errors;
pub mod logging;
pub mod middleware;
pub mod models;
pub mod router;
pub mod scheduler;
pub mod subscription;
pub mod sync;
pub mod workspace;

pub use errors::AppError;
