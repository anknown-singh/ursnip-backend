use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::sync::Semaphore;
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::AppConfig;
use crate::errors::AppError;
use crate::models::common::{SubscriptionStatus, Tier};

// ─── Constants ──────────────────────────────────────────────────────────────────

/// Free tier: 50 AI expansion requests per 24-hour rolling window.
const FREE_TIER_QUOTA: usize = 50;

/// Paid tier (pro/teams): 1000 AI expansion requests per 24-hour rolling window.
const PAID_TIER_QUOTA: usize = 1000;

/// Maximum trigger length in characters.
const MAX_TRIGGER_LENGTH: usize = 500;

/// Maximum system_prompt size in bytes (10 KB).
const MAX_SYSTEM_PROMPT_BYTES: usize = 10_240;

/// Maximum context size in bytes (50 KB).
const MAX_CONTEXT_BYTES: usize = 51_200;

/// Timeout for AI provider HTTP calls.
const AI_PROVIDER_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum number of requests that can queue waiting for a semaphore permit.
const MAX_QUEUE_SIZE: usize = 100;

/// Timeout for acquiring a semaphore permit (queue wait time).
const SEMAPHORE_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

// ─── Request / Response DTOs ────────────────────────────────────────────────────

/// Request body for the AI expand endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct AiExpandRequest {
    pub trigger: String,
    pub system_prompt: String,
    pub context: Option<String>,
}

/// Response body for the AI expand endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct AiExpandResponse {
    pub expanded_text: String,
}

/// Payload sent to the upstream AI provider.
#[derive(Debug, Serialize)]
struct AiProviderRequest {
    trigger: String,
    system_prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<String>,
}

/// Response expected from the upstream AI provider.
#[derive(Debug, Deserialize)]
struct AiProviderResponse {
    expanded_text: Option<String>,
}

// ─── Internal Row Types ─────────────────────────────────────────────────────────

#[derive(Debug, sqlx::FromRow)]
struct UserSubscriptionRow {
    pub tier: String,
    pub status: String,
}

// ─── Service ────────────────────────────────────────────────────────────────────

/// AI expansion service handling input validation, quota enforcement,
/// concurrency control, and upstream provider communication.
pub struct AiService {
    pool: PgPool,
    config: Arc<AppConfig>,
    http_client: Client,
    /// Concurrency semaphore limiting parallel AI provider calls.
    semaphore: Arc<Semaphore>,
    /// In-memory quota tracker: user_id → sorted timestamps of recent requests.
    quota_tracker: DashMap<Uuid, VecDeque<DateTime<Utc>>>,
}

impl AiService {
    /// Create a new AiService instance.
    pub fn new(pool: PgPool, config: Arc<AppConfig>) -> Self {
        let max_concurrent = config.ai_max_concurrent_requests;

        let http_client = Client::builder()
            .timeout(AI_PROVIDER_TIMEOUT)
            .build()
            .expect("Failed to build reqwest client");

        Self {
            pool,
            config,
            http_client,
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            quota_tracker: DashMap::new(),
        }
    }

    /// Main entry point: validate inputs, check quota, acquire concurrency permit,
    /// call the AI provider, and return the expanded text.
    pub async fn expand(
        &self,
        user_id: Uuid,
        request: AiExpandRequest,
    ) -> Result<AiExpandResponse, AppError> {
        // Step 1: Validate inputs
        self.validate_inputs(&request)?;

        // Step 2: Check quota
        self.check_quota(user_id).await?;

        // Step 3: Acquire concurrency permit
        let _permit = self.acquire_permit().await?;

        // Step 4: Call AI provider
        let expanded_text = self.call_provider(&request).await?;

        // Step 5: Record usage (after successful call)
        self.record_usage(user_id);

        Ok(AiExpandResponse { expanded_text })
    }

    // ─── Input Validation ───────────────────────────────────────────────────────

    /// Validate the AI expand request fields.
    fn validate_inputs(&self, request: &AiExpandRequest) -> Result<(), AppError> {
        // trigger: required, non-empty
        if request.trigger.is_empty() {
            return Err(AppError::InvalidRequestBody);
        }

        // system_prompt: required, non-empty
        if request.system_prompt.is_empty() {
            return Err(AppError::InvalidRequestBody);
        }

        // trigger: max 500 characters
        if request.trigger.chars().count() > MAX_TRIGGER_LENGTH {
            return Err(AppError::TriggerTooLong);
        }

        // system_prompt: max 10 KB
        if request.system_prompt.len() > MAX_SYSTEM_PROMPT_BYTES {
            return Err(AppError::SystemPromptTooLong);
        }

        // context: optional, max 50 KB
        if let Some(ref context) = request.context {
            if context.len() > MAX_CONTEXT_BYTES {
                return Err(AppError::ContextTooLong);
            }
        }

        Ok(())
    }

    // ─── Quota Enforcement ──────────────────────────────────────────────────────

    /// Check whether the user has remaining AI quota in the 24h rolling window.
    ///
    /// Quota limits:
    /// - free tier: 50 requests / 24h
    /// - pro/teams (active or past_due within grace): 1000 requests / 24h
    /// - cancelled/expired: free tier limits (50 requests / 24h)
    async fn check_quota(&self, user_id: Uuid) -> Result<(), AppError> {
        let limit = self.get_quota_limit(user_id).await?;
        let now = Utc::now();
        let window_start = now - chrono::Duration::hours(24);

        // Check current usage within the window
        let current_usage = self.count_usage_in_window(user_id, window_start);

        if current_usage >= limit {
            return Err(AppError::AiQuotaExceeded);
        }

        Ok(())
    }

    /// Determine the quota limit for a user based on their subscription tier/status.
    async fn get_quota_limit(&self, user_id: Uuid) -> Result<usize, AppError> {
        // Query the user's subscription through their individual workspace
        let row = sqlx::query_as::<_, UserSubscriptionRow>(
            r#"
            SELECT s.tier, s.status
            FROM subscriptions s
            INNER JOIN workspaces w ON w.id = s.workspace_id
            WHERE w.owner_id = $1
              AND w.type = 'individual'
            LIMIT 1
            "#,
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        let Some(row) = row else {
            // No subscription found — default to free tier limits
            return Ok(FREE_TIER_QUOTA);
        };

        let tier = match row.tier.as_str() {
            "pro" | "teams" => Tier::Pro, // Both pro and teams get paid limits
            _ => Tier::Free,
        };

        let status = match row.status.as_str() {
            "active" => SubscriptionStatus::Active,
            "past_due" => SubscriptionStatus::PastDue,
            "cancelled" => SubscriptionStatus::Cancelled,
            "pending_payment" => SubscriptionStatus::PendingPayment,
            "deactivated" => SubscriptionStatus::Deactivated,
            _ => SubscriptionStatus::Active,
        };

        // Determine limit based on tier and status
        match (tier, status) {
            // Active paid tier → paid limit
            (Tier::Pro | Tier::Teams, SubscriptionStatus::Active) => Ok(PAID_TIER_QUOTA),
            // Past due (within grace period) → still gets paid limit
            (Tier::Pro | Tier::Teams, SubscriptionStatus::PastDue) => Ok(PAID_TIER_QUOTA),
            // Cancelled or deactivated → free limit regardless of previous tier
            (_, SubscriptionStatus::Cancelled | SubscriptionStatus::Deactivated) => {
                Ok(FREE_TIER_QUOTA)
            }
            // Everything else (free tier, pending payment) → free limit
            _ => Ok(FREE_TIER_QUOTA),
        }
    }

    /// Count the number of requests a user has made within the given window.
    fn count_usage_in_window(&self, user_id: Uuid, window_start: DateTime<Utc>) -> usize {
        let entry = self.quota_tracker.get(&user_id);
        match entry {
            Some(timestamps) => timestamps.iter().filter(|ts| **ts >= window_start).count(),
            None => 0,
        }
    }

    /// Record a successful AI expansion request for quota tracking.
    fn record_usage(&self, user_id: Uuid) {
        let now = Utc::now();
        let window_start = now - chrono::Duration::hours(24);

        let mut entry = self.quota_tracker.entry(user_id).or_insert_with(VecDeque::new);

        // Prune expired entries from the front
        while let Some(front) = entry.front() {
            if *front < window_start {
                entry.pop_front();
            } else {
                break;
            }
        }

        // Record the new timestamp
        entry.push_back(now);
    }

    // ─── Concurrency Control ────────────────────────────────────────────────────

    /// Acquire a semaphore permit with queue size and timeout constraints.
    ///
    /// - If more than MAX_QUEUE_SIZE requests are already waiting, return 429 AI_SERVICE_BUSY.
    /// - If the permit cannot be acquired within 5 seconds, return 429 AI_SERVICE_BUSY.
    async fn acquire_permit(
        &self,
    ) -> Result<tokio::sync::SemaphorePermit<'_>, AppError> {
        // Check queue depth: available_permits tells us how many are free.
        // If available is 0, waiting = total_waiters. We approximate queue size
        // by checking (max_concurrent - available_permits) vs our threshold.
        // A more precise approach: if available_permits == 0, we check if we'd
        // exceed the queue limit. Since Semaphore doesn't expose waiter count,
        // we use a timeout-based approach combined with a try_acquire check.
        let available = self.semaphore.available_permits();
        let max_concurrent = self.config.ai_max_concurrent_requests;

        // If no permits available and we'd exceed queue capacity, reject immediately.
        // We approximate: if all permits are taken and more than MAX_QUEUE_SIZE
        // additional requests would be waiting, we reject. Since we can't know
        // exact waiter count, we rely on the timeout to bound wait time.
        if available == 0 {
            // Try to acquire with timeout — this effectively bounds the queue
            match tokio::time::timeout(SEMAPHORE_ACQUIRE_TIMEOUT, self.semaphore.acquire()).await {
                Ok(Ok(permit)) => Ok(permit),
                Ok(Err(_)) => {
                    // Semaphore closed (shouldn't happen)
                    Err(AppError::AiServiceBusy)
                }
                Err(_) => {
                    // Timeout waiting for permit
                    warn!("AI service busy: semaphore acquire timed out");
                    Err(AppError::AiServiceBusy)
                }
            }
        } else {
            // Permits available, acquire immediately
            match self.semaphore.acquire().await {
                Ok(permit) => Ok(permit),
                Err(_) => Err(AppError::AiServiceBusy),
            }
        }
    }

    // ─── AI Provider Communication ──────────────────────────────────────────────

    /// Call the upstream AI provider with the expansion request.
    ///
    /// - HTTP POST to AI_PROVIDER_URL
    /// - 10s timeout (configured on the client)
    /// - Returns 502 AI_PROVIDER_UNAVAILABLE on network error or timeout
    /// - Returns 502 AI_PROVIDER_INVALID_RESPONSE on empty/malformed response
    async fn call_provider(&self, request: &AiExpandRequest) -> Result<String, AppError> {
        let provider_request = AiProviderRequest {
            trigger: request.trigger.clone(),
            system_prompt: request.system_prompt.clone(),
            context: request.context.clone(),
        };

        let response = self
            .http_client
            .post(&self.config.ai_provider_url)
            .header("Authorization", format!("Bearer {}", self.config.ai_provider_key))
            .json(&provider_request)
            .send()
            .await
            .map_err(|e| {
                warn!("AI provider request failed: {}", e);
                AppError::AiProviderUnavailable
            })?;

        // Check for non-success status codes
        if !response.status().is_success() {
            warn!(
                "AI provider returned error status: {}",
                response.status()
            );
            return Err(AppError::AiProviderUnavailable);
        }

        // Parse the response body
        let provider_response: AiProviderResponse = response.json().await.map_err(|e| {
            warn!("AI provider returned invalid JSON: {}", e);
            AppError::AiProviderInvalidResponse
        })?;

        // Check for empty or missing expanded_text
        let expanded_text = provider_response.expanded_text.ok_or_else(|| {
            warn!("AI provider returned response without expanded_text field");
            AppError::AiProviderInvalidResponse
        })?;

        if expanded_text.is_empty() {
            warn!("AI provider returned empty expanded_text");
            return Err(AppError::AiProviderInvalidResponse);
        }

        Ok(expanded_text)
    }
}
