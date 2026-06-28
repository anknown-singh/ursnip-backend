//! Sliding window rate limiter middleware.
//!
//! Provides per-IP, per-user, per-admin, sync-mutation, sync-read, and
//! forgot-password rate limiting using in-memory sliding windows backed by
//! DashMap for concurrent access.
//!
//! ## IP Resolution
//!
//! When the TCP peer address falls within a trusted proxy CIDR, the rightmost
//! untrusted IP from the `X-Forwarded-For` header is used. Otherwise, the TCP
//! peer address is used directly.

use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request};
use axum::http::header::HeaderMap;
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;
use ipnet::IpNet;

use crate::AppError;

// ─── Rate Limit Configuration ──────────────────────────────────────────────────

/// IP-based rate limit: 100 requests per minute.
const IP_MAX_REQUESTS: usize = 100;
const IP_WINDOW: Duration = Duration::from_secs(60);

/// Authenticated user rate limit: 500 requests per minute.
const USER_MAX_REQUESTS: usize = 500;
const USER_WINDOW: Duration = Duration::from_secs(60);

/// Admin user rate limit: 300 requests per minute.
const ADMIN_MAX_REQUESTS: usize = 300;
const ADMIN_WINDOW: Duration = Duration::from_secs(60);

/// Sync mutation rate limit: 60 requests per minute.
const SYNC_MUTATION_MAX_REQUESTS: usize = 60;
const SYNC_MUTATION_WINDOW: Duration = Duration::from_secs(60);

/// Sync read rate limit: 120 requests per minute.
const SYNC_READ_MAX_REQUESTS: usize = 120;
const SYNC_READ_WINDOW: Duration = Duration::from_secs(60);

/// Forgot password rate limit: 3 requests per hour.
const FORGOT_PASSWORD_MAX_REQUESTS: usize = 3;
const FORGOT_PASSWORD_WINDOW: Duration = Duration::from_secs(3600);

// ─── Sliding Window ────────────────────────────────────────────────────────────

/// A sliding window that tracks request timestamps and enforces a maximum
/// request count within a configurable time window.
#[derive(Debug, Clone)]
pub struct SlidingWindow {
    /// Timestamps of recent requests within the window.
    timestamps: VecDeque<Instant>,
}

impl SlidingWindow {
    /// Creates a new empty sliding window.
    pub fn new() -> Self {
        Self {
            timestamps: VecDeque::new(),
        }
    }

    /// Checks whether a new request is allowed and records it if so.
    ///
    /// Removes expired entries (older than `window_duration`), then checks if
    /// the current count is below `max_requests`. If allowed, records the
    /// current timestamp and returns `true`. Otherwise returns `false`.
    pub fn check_and_record(&mut self, window_duration: Duration, max_requests: usize) -> bool {
        let now = Instant::now();
        let cutoff = now - window_duration;

        // Remove expired timestamps from the front.
        while let Some(&front) = self.timestamps.front() {
            if front < cutoff {
                self.timestamps.pop_front();
            } else {
                break;
            }
        }

        if self.timestamps.len() < max_requests {
            self.timestamps.push_back(now);
            true
        } else {
            false
        }
    }

    /// Returns the duration until the next request would be allowed
    /// (time until the oldest entry in the window expires).
    pub fn retry_after(&self, window_duration: Duration) -> Duration {
        if let Some(&oldest) = self.timestamps.front() {
            let elapsed = oldest.elapsed();
            if elapsed < window_duration {
                window_duration - elapsed
            } else {
                Duration::ZERO
            }
        } else {
            Duration::ZERO
        }
    }
}

impl Default for SlidingWindow {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Rate Limiter ──────────────────────────────────────────────────────────────

/// Application-wide rate limiter holding concurrent maps for each limiter category.
#[derive(Clone)]
pub struct RateLimiter {
    /// Per-IP rate limiter (100 req/min).
    ip_limiter: Arc<DashMap<String, SlidingWindow>>,
    /// Per-user rate limiter (500 req/min).
    user_limiter: Arc<DashMap<String, SlidingWindow>>,
    /// Per-admin rate limiter (300 req/min).
    admin_limiter: Arc<DashMap<String, SlidingWindow>>,
    /// Per-user sync mutation rate limiter (60 req/min).
    sync_mutation_limiter: Arc<DashMap<String, SlidingWindow>>,
    /// Per-user sync read rate limiter (120 req/min).
    sync_read_limiter: Arc<DashMap<String, SlidingWindow>>,
    /// Per-IP/email forgot password rate limiter (3 req/hour).
    forgot_password_limiter: Arc<DashMap<String, SlidingWindow>>,
    /// Parsed trusted proxy CIDRs for IP resolution.
    trusted_proxy_cidrs: Arc<Vec<IpNet>>,
}

impl RateLimiter {
    /// Creates a new `RateLimiter` from the provided trusted proxy CIDR strings.
    ///
    /// Invalid CIDR strings are logged as warnings and skipped.
    pub fn new(trusted_proxy_cidrs: &[String]) -> Self {
        let cidrs: Vec<IpNet> = trusted_proxy_cidrs
            .iter()
            .filter_map(|cidr_str| {
                cidr_str.parse::<IpNet>().map_err(|e| {
                    tracing::warn!(cidr = %cidr_str, error = %e, "Invalid trusted proxy CIDR, skipping");
                    e
                }).ok()
            })
            .collect();

        Self {
            ip_limiter: Arc::new(DashMap::new()),
            user_limiter: Arc::new(DashMap::new()),
            admin_limiter: Arc::new(DashMap::new()),
            sync_mutation_limiter: Arc::new(DashMap::new()),
            sync_read_limiter: Arc::new(DashMap::new()),
            forgot_password_limiter: Arc::new(DashMap::new()),
            trusted_proxy_cidrs: Arc::new(cidrs),
        }
    }

    /// Check IP-based rate limit (100 req/min).
    ///
    /// Returns `Ok(())` if allowed, or `Err(AppError::RateLimitExceeded)` with
    /// the appropriate `retry_after_secs`.
    pub fn check_ip(&self, ip: &str) -> Result<(), AppError> {
        self.check_limit(&self.ip_limiter, ip, IP_WINDOW, IP_MAX_REQUESTS)
    }

    /// Check user-based rate limit (500 req/min).
    pub fn check_user(&self, user_id: &str) -> Result<(), AppError> {
        self.check_limit(&self.user_limiter, user_id, USER_WINDOW, USER_MAX_REQUESTS)
    }

    /// Check admin-based rate limit (300 req/min).
    pub fn check_admin(&self, user_id: &str) -> Result<(), AppError> {
        self.check_limit(&self.admin_limiter, user_id, ADMIN_WINDOW, ADMIN_MAX_REQUESTS)
    }

    /// Check sync mutation rate limit (60 req/min).
    pub fn check_sync_mutation(&self, user_id: &str) -> Result<(), AppError> {
        self.check_limit(
            &self.sync_mutation_limiter,
            user_id,
            SYNC_MUTATION_WINDOW,
            SYNC_MUTATION_MAX_REQUESTS,
        )
    }

    /// Check sync read rate limit (120 req/min).
    pub fn check_sync_read(&self, user_id: &str) -> Result<(), AppError> {
        self.check_limit(
            &self.sync_read_limiter,
            user_id,
            SYNC_READ_WINDOW,
            SYNC_READ_MAX_REQUESTS,
        )
    }

    /// Check forgot password rate limit (3 req/hour).
    pub fn check_forgot_password(&self, key: &str) -> Result<(), AppError> {
        self.check_limit(
            &self.forgot_password_limiter,
            key,
            FORGOT_PASSWORD_WINDOW,
            FORGOT_PASSWORD_MAX_REQUESTS,
        )
    }

    /// Internal helper to check a limiter map for the given key.
    fn check_limit(
        &self,
        limiter: &DashMap<String, SlidingWindow>,
        key: &str,
        window: Duration,
        max_requests: usize,
    ) -> Result<(), AppError> {
        let mut entry = limiter.entry(key.to_string()).or_insert_with(SlidingWindow::new);
        let window_ref = entry.value_mut();

        if window_ref.check_and_record(window, max_requests) {
            Ok(())
        } else {
            let retry_after = window_ref.retry_after(window);
            let retry_after_secs = retry_after.as_secs().max(1);
            Err(AppError::RateLimitExceeded { retry_after_secs })
        }
    }

    /// Resolves the client IP address from request headers and connection info.
    ///
    /// Strategy:
    /// 1. Get the TCP peer address from `ConnectInfo`.
    /// 2. If the peer address is within a trusted proxy CIDR, parse the
    ///    `X-Forwarded-For` header and select the rightmost IP that is NOT
    ///    in a trusted CIDR.
    /// 3. If no untrusted IP is found in `X-Forwarded-For`, fall back to the
    ///    peer address.
    /// 4. If the peer address is not trusted, use it directly.
    pub fn resolve_client_ip(
        &self,
        peer_addr: Option<&std::net::SocketAddr>,
        headers: &HeaderMap,
    ) -> String {
        let peer_ip = peer_addr.map(|addr| addr.ip());

        // Check if the direct peer is a trusted proxy.
        let peer_is_trusted = peer_ip
            .map(|ip| self.is_trusted_ip(ip))
            .unwrap_or(false);

        if peer_is_trusted {
            // Parse X-Forwarded-For and find the rightmost untrusted IP.
            if let Some(forwarded_for) = headers.get("x-forwarded-for") {
                if let Ok(xff_str) = forwarded_for.to_str() {
                    let ips: Vec<&str> = xff_str.split(',').map(|s| s.trim()).collect();

                    // Walk from right to left, find the first untrusted IP.
                    for ip_str in ips.iter().rev() {
                        if let Ok(ip) = ip_str.parse::<IpAddr>() {
                            if !self.is_trusted_ip(ip) {
                                return ip.to_string();
                            }
                        }
                    }
                }
            }
        }

        // Fall back to peer address.
        peer_ip
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// Returns `true` if the given IP falls within any trusted proxy CIDR.
    fn is_trusted_ip(&self, ip: IpAddr) -> bool {
        self.trusted_proxy_cidrs.iter().any(|cidr| cidr.contains(&ip))
    }
}

// ─── Middleware Functions ──────────────────────────────────────────────────────

/// Axum middleware layer that enforces IP-based rate limiting.
///
/// Should be applied globally to the router. Extracts the client IP via
/// `ConnectInfo<SocketAddr>` and X-Forwarded-For resolution, then checks
/// the IP rate limiter.
pub async fn ip_rate_limit(
    ConnectInfo(peer_addr): ConnectInfo<std::net::SocketAddr>,
    axum::extract::State(rate_limiter): axum::extract::State<RateLimiter>,
    request: Request,
    next: axum::middleware::Next,
) -> Response {
    let client_ip = rate_limiter.resolve_client_ip(Some(&peer_addr), request.headers());

    if let Err(err) = rate_limiter.check_ip(&client_ip) {
        return err.into_response();
    }

    next.run(request).await
}

/// Axum middleware layer that enforces user-based rate limiting.
///
/// Should be applied to authenticated routes. Expects a `UserId` extension
/// to be set by the authentication middleware. Falls back to admin limiter
/// if the `IsAdmin` extension is set.
pub async fn user_rate_limit(
    axum::extract::State(rate_limiter): axum::extract::State<RateLimiter>,
    request: Request,
    next: axum::middleware::Next,
) -> Response {
    // Extract user ID from request extensions (set by auth middleware).
    let user_id = request
        .extensions()
        .get::<UserId>()
        .map(|id| id.0.clone());

    let is_admin = request.extensions().get::<IsAdmin>().is_some();

    if let Some(ref uid) = user_id {
        let result = if is_admin {
            rate_limiter.check_admin(uid)
        } else {
            rate_limiter.check_user(uid)
        };

        if let Err(err) = result {
            return err.into_response();
        }
    }

    next.run(request).await
}

// ─── Extension Types ───────────────────────────────────────────────────────────

/// Request extension for the authenticated user ID (set by auth middleware).
#[derive(Clone, Debug)]
pub struct UserId(pub String);

/// Request extension marker for admin users (set by auth middleware).
#[derive(Clone, Debug)]
pub struct IsAdmin;

// ─── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::Duration;

    #[test]
    fn sliding_window_allows_within_limit() {
        let mut window = SlidingWindow::new();
        for _ in 0..5 {
            assert!(window.check_and_record(Duration::from_secs(60), 5));
        }
    }

    #[test]
    fn sliding_window_denies_over_limit() {
        let mut window = SlidingWindow::new();
        for _ in 0..5 {
            assert!(window.check_and_record(Duration::from_secs(60), 5));
        }
        // 6th request should be denied
        assert!(!window.check_and_record(Duration::from_secs(60), 5));
    }

    #[test]
    fn sliding_window_retry_after_is_positive_when_full() {
        let mut window = SlidingWindow::new();
        let win_dur = Duration::from_secs(60);
        for _ in 0..5 {
            window.check_and_record(win_dur, 5);
        }
        let retry = window.retry_after(win_dur);
        assert!(retry > Duration::ZERO);
        assert!(retry <= win_dur);
    }

    #[test]
    fn rate_limiter_check_ip_allows_within_limit() {
        let rl = RateLimiter::new(&[]);
        for _ in 0..IP_MAX_REQUESTS {
            assert!(rl.check_ip("192.168.1.1").is_ok());
        }
    }

    #[test]
    fn rate_limiter_check_ip_denies_over_limit() {
        let rl = RateLimiter::new(&[]);
        for _ in 0..IP_MAX_REQUESTS {
            rl.check_ip("10.0.0.1").unwrap();
        }
        let result = rl.check_ip("10.0.0.1");
        assert!(result.is_err());
    }

    #[test]
    fn rate_limiter_different_keys_independent() {
        let rl = RateLimiter::new(&[]);
        for _ in 0..IP_MAX_REQUESTS {
            rl.check_ip("10.0.0.1").unwrap();
        }
        // Different IP should still be allowed
        assert!(rl.check_ip("10.0.0.2").is_ok());
    }

    #[test]
    fn resolve_client_ip_uses_peer_when_not_trusted() {
        let rl = RateLimiter::new(&[]);
        let peer = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 50)), 12345);
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "10.0.0.1, 172.16.0.1".parse().unwrap());

        let resolved = rl.resolve_client_ip(Some(&peer), &headers);
        assert_eq!(resolved, "203.0.113.50");
    }

    #[test]
    fn resolve_client_ip_uses_xff_rightmost_untrusted_when_peer_is_trusted() {
        let rl = RateLimiter::new(&["10.0.0.0/8".to_string()]);
        let peer = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 12345);
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "203.0.113.50, 10.0.0.5".parse().unwrap(),
        );

        let resolved = rl.resolve_client_ip(Some(&peer), &headers);
        // Rightmost untrusted is 203.0.113.50 (10.0.0.5 is trusted)
        assert_eq!(resolved, "203.0.113.50");
    }

    #[test]
    fn resolve_client_ip_falls_back_to_peer_if_all_xff_trusted() {
        let rl = RateLimiter::new(&["10.0.0.0/8".to_string(), "172.16.0.0/12".to_string()]);
        let peer = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 12345);
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "10.0.0.2, 172.16.0.5".parse().unwrap());

        let resolved = rl.resolve_client_ip(Some(&peer), &headers);
        // All XFF IPs are trusted, fall back to peer
        assert_eq!(resolved, "10.0.0.1");
    }

    #[test]
    fn resolve_client_ip_no_xff_header_uses_peer() {
        let rl = RateLimiter::new(&["10.0.0.0/8".to_string()]);
        let peer = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 12345);
        let headers = HeaderMap::new();

        let resolved = rl.resolve_client_ip(Some(&peer), &headers);
        assert_eq!(resolved, "10.0.0.1");
    }

    #[test]
    fn check_forgot_password_limit() {
        let rl = RateLimiter::new(&[]);
        for _ in 0..FORGOT_PASSWORD_MAX_REQUESTS {
            assert!(rl.check_forgot_password("user@example.com").is_ok());
        }
        let result = rl.check_forgot_password("user@example.com");
        assert!(result.is_err());
    }

    #[test]
    fn check_sync_mutation_limit() {
        let rl = RateLimiter::new(&[]);
        for _ in 0..SYNC_MUTATION_MAX_REQUESTS {
            assert!(rl.check_sync_mutation("user-123").is_ok());
        }
        let result = rl.check_sync_mutation("user-123");
        assert!(result.is_err());
    }

    #[test]
    fn check_sync_read_limit() {
        let rl = RateLimiter::new(&[]);
        for _ in 0..SYNC_READ_MAX_REQUESTS {
            assert!(rl.check_sync_read("user-456").is_ok());
        }
        let result = rl.check_sync_read("user-456");
        assert!(result.is_err());
    }

    #[test]
    fn check_admin_limit() {
        let rl = RateLimiter::new(&[]);
        for _ in 0..ADMIN_MAX_REQUESTS {
            assert!(rl.check_admin("admin-1").is_ok());
        }
        let result = rl.check_admin("admin-1");
        assert!(result.is_err());
    }
}
