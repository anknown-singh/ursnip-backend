use std::sync::Arc;

use chrono::{DateTime, Months, Utc};
use rand::Rng;
use rust_decimal::Decimal;
use sqlx::PgPool;
use tracing::info;
use uuid::Uuid;

use crate::config::AppConfig;
use crate::errors::AppError;
use crate::models::common::{SubscriptionStatus, Tier};
use crate::subscription::invoice::{compute_invoice, Invoice, InvoiceRequest};

// ─── Constants ──────────────────────────────────────────────────────────────────

/// Free tier limits (Requirement 5.21).
const FREE_MAX_SNIPPETS: i64 = 10;
const FREE_MAX_FOLDERS: i64 = 3;
const FREE_MAX_CONTENT_CHARS: usize = 2000;

/// Minimum billing cycle in months (Requirement 5.26).
const MINIMUM_BILLING_CYCLE_MONTHS: i32 = 12;

/// Base prices per tier (annual).
const PRO_BASE_PRICE: &str = "99.00";
const TEAMS_BASE_PRICE: &str = "199.00";

// ─── Internal Row Types ─────────────────────────────────────────────────────────

#[derive(Debug, sqlx::FromRow)]
struct SubscriptionRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub tier: String,
    pub status: String,
    pub period_start: Option<DateTime<Utc>>,
    pub period_end: Option<DateTime<Utc>>,
    pub grace_period_end: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
}

#[derive(Debug, sqlx::FromRow)]
struct CouponRow {
    pub id: Uuid,
    pub code: String,
    pub coupon_type: String,
    pub discount_id: Uuid,
    pub owner_id: Option<Uuid>,
    pub max_uses: Option<i32>,
    pub times_used: i32,
    pub valid_from: DateTime<Utc>,
    pub valid_until: Option<DateTime<Utc>>,
    pub active: bool,
}

#[derive(Debug, sqlx::FromRow)]
struct DiscountRow {
    pub id: Uuid,
    pub discount_type: String,
    pub value: Decimal,
    pub active: bool,
}

#[derive(Debug, sqlx::FromRow)]
struct ReferralRow {
    pub id: Uuid,
    pub referrer_id: Uuid,
    pub referred_user_id: Uuid,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

// ─── Public Types ───────────────────────────────────────────────────────────────

/// Validated coupon with its linked discount info.
#[derive(Debug, Clone)]
pub struct ValidatedCoupon {
    pub coupon_id: Uuid,
    pub discount_type: String,
    pub discount_value: Decimal,
}

/// Checkout request parameters.
#[derive(Debug, Clone)]
pub struct CheckoutRequest {
    pub workspace_id: Uuid,
    pub tier: String,
    pub billing_cycle_months: i32,
    pub coupon_code: Option<String>,
    pub discount_id: Option<Uuid>,
    pub country_code: Option<String>,
    pub success_url: Option<String>,
    pub cancel_url: Option<String>,
}

/// Checkout response with URL and invoice breakdown.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CheckoutResponse {
    pub checkout_url: String,
    pub invoice: Invoice,
}

/// Serializable subscription response for the current subscription endpoint.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CurrentSubscriptionResponse {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub tier: String,
    pub status: String,
    pub period_start: Option<DateTime<Utc>>,
    pub period_end: Option<DateTime<Utc>>,
    pub grace_period_end: Option<DateTime<Utc>>,
}

// ─── Service ────────────────────────────────────────────────────────────────────

/// Subscription service handling tier management and limit enforcement.
pub struct SubscriptionService {
    pool: PgPool,
    config: Arc<AppConfig>,
}

impl SubscriptionService {
    /// Create a new SubscriptionService instance.
    pub fn new(pool: PgPool, config: Arc<AppConfig>) -> Self {
        Self { pool, config }
    }

    /// Create a free subscription for a workspace with status=active.
    ///
    /// Called during workspace creation (Requirement 5.6).
    pub async fn create_free_subscription(
        &self,
        workspace_id: Uuid,
    ) -> Result<(), AppError> {
        sqlx::query(
            r#"
            INSERT INTO subscriptions (workspace_id, tier, status)
            VALUES ($1, 'free', 'active')
            "#,
        )
        .bind(workspace_id)
        .execute(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        Ok(())
    }

    /// Initiate an upgrade from free to pro tier.
    ///
    /// Validates that the current tier is free; returns 422 ALREADY_UPGRADED if not.
    /// Transitions the subscription status to pending_payment so the handler can
    /// proceed to checkout (Requirement 5.7).
    pub async fn initiate_upgrade(
        &self,
        workspace_id: Uuid,
    ) -> Result<(), AppError> {
        let subscription = self.get_subscription(workspace_id).await?;

        if subscription.tier != "free" {
            return Err(AppError::AlreadyUpgraded);
        }

        sqlx::query(
            r#"
            UPDATE subscriptions
            SET status = 'pending_payment', updated_at = now()
            WHERE workspace_id = $1
            "#,
        )
        .bind(workspace_id)
        .execute(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        Ok(())
    }

    /// Enforce tier limits for a workspace.
    ///
    /// For free tier: max 10 snippets, max 3 folders, max 2000 chars content.
    /// For pro/teams: no limits (Requirement 5.21, 5.23).
    ///
    /// `content_length` is provided when creating/updating a snippet to validate
    /// the content character limit.
    ///
    /// This should be called before creating a new snippet or folder.
    pub async fn enforce_tier_limits(
        &self,
        workspace_id: Uuid,
        content_length: Option<usize>,
    ) -> Result<(), AppError> {
        let subscription = self.get_subscription(workspace_id).await?;
        let tier = self.parse_tier(&subscription.tier);

        // Pro and Teams tiers have no limits (Requirement 5.23)
        if tier != Tier::Free {
            return Ok(());
        }

        // Check content length limit
        if let Some(len) = content_length {
            if len > FREE_MAX_CONTENT_CHARS {
                return Err(AppError::SnippetContentTooLong);
            }
        }

        // Check snippet count
        let snippet_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM snippets WHERE workspace_id = $1 AND deleted_at IS NULL",
        )
        .bind(workspace_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        if snippet_count >= FREE_MAX_SNIPPETS {
            return Err(AppError::SnippetLimitReached);
        }

        // Check folder count
        let folder_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM folders WHERE workspace_id = $1 AND deleted_at IS NULL",
        )
        .bind(workspace_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        if folder_count >= FREE_MAX_FOLDERS {
            return Err(AppError::FolderLimitReached);
        }

        Ok(())
    }

    /// Enforce snippet-specific tier limits (count + content length).
    ///
    /// Call this before creating a new snippet. Checks both the snippet count
    /// limit and content character limit for the free tier.
    pub async fn enforce_snippet_limits(
        &self,
        workspace_id: Uuid,
        content_length: usize,
    ) -> Result<(), AppError> {
        let subscription = self.get_subscription(workspace_id).await?;
        let tier = self.parse_tier(&subscription.tier);

        if tier != Tier::Free {
            return Ok(());
        }

        // Check content length
        if content_length > FREE_MAX_CONTENT_CHARS {
            return Err(AppError::SnippetContentTooLong);
        }

        // Check snippet count
        let snippet_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM snippets WHERE workspace_id = $1 AND deleted_at IS NULL",
        )
        .bind(workspace_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        if snippet_count >= FREE_MAX_SNIPPETS {
            return Err(AppError::SnippetLimitReached);
        }

        Ok(())
    }

    /// Enforce folder-specific tier limits (count only).
    ///
    /// Call this before creating a new folder.
    pub async fn enforce_folder_limits(
        &self,
        workspace_id: Uuid,
    ) -> Result<(), AppError> {
        let subscription = self.get_subscription(workspace_id).await?;
        let tier = self.parse_tier(&subscription.tier);

        if tier != Tier::Free {
            return Ok(());
        }

        let folder_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM folders WHERE workspace_id = $1 AND deleted_at IS NULL",
        )
        .bind(workspace_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        if folder_count >= FREE_MAX_FOLDERS {
            return Err(AppError::FolderLimitReached);
        }

        Ok(())
    }

    /// Check if the workspace is in a soft-locked state.
    ///
    /// A workspace is soft-locked when the subscription is cancelled or expired
    /// (period_end has passed) AND the existing content exceeds free tier limits.
    /// In this state, writes that would add content are rejected with 422
    /// CONTENT_SOFT_LOCKED (Requirements 5.24, 5.25).
    ///
    /// Call this before any write operation (create/update snippet, create folder).
    pub async fn check_soft_lock(
        &self,
        workspace_id: Uuid,
    ) -> Result<(), AppError> {
        let subscription = self.get_subscription(workspace_id).await?;
        let status = self.parse_status(&subscription.status);
        let tier = self.parse_tier(&subscription.tier);

        // Only check soft-lock for non-free tiers that have been cancelled/expired.
        // Free tier uses normal limit enforcement, not soft-lock.
        if tier == Tier::Free {
            return Ok(());
        }

        let is_cancelled_or_expired = match status {
            SubscriptionStatus::Cancelled | SubscriptionStatus::Deactivated => true,
            SubscriptionStatus::Active => {
                // Check if the subscription period has ended
                if let Some(period_end) = subscription.period_end {
                    Utc::now() > period_end
                } else {
                    false
                }
            }
            _ => false,
        };

        if !is_cancelled_or_expired {
            return Ok(());
        }

        // Subscription is cancelled/expired — check if content exceeds free limits
        let snippet_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM snippets WHERE workspace_id = $1 AND deleted_at IS NULL",
        )
        .bind(workspace_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        let folder_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM folders WHERE workspace_id = $1 AND deleted_at IS NULL",
        )
        .bind(workspace_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        // If content exceeds free limits, the workspace is soft-locked
        if snippet_count > FREE_MAX_SNIPPETS || folder_count > FREE_MAX_FOLDERS {
            return Err(AppError::ContentSoftLocked);
        }

        Ok(())
    }

    // ─── Coupon Validation and Checkout ────────────────────────────────────────

    /// Validate a coupon code case-insensitively and return its linked discount info.
    ///
    /// Checks: exists, active, valid_from <= now, valid_until IS NULL OR > now,
    /// max_uses IS NULL OR times_used < max_uses (Requirements 5.37, 5.38).
    pub async fn validate_coupon(&self, code: &str) -> Result<ValidatedCoupon, AppError> {
        // Step 1: Case-insensitive lookup
        let coupon = sqlx::query_as::<_, CouponRow>(
            r#"
            SELECT id, code, type AS coupon_type, discount_id, owner_id,
                   max_uses, times_used, valid_from, valid_until, active
            FROM coupon_codes
            WHERE LOWER(code) = LOWER($1)
            "#,
        )
        .bind(code)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?
        .ok_or(AppError::CouponNotFound)?;

        // Step 2: Check active
        if !coupon.active {
            return Err(AppError::CouponInactive);
        }

        // Step 3: Check valid_from <= now
        let now = Utc::now();
        if coupon.valid_from > now {
            return Err(AppError::CouponNotYetValid);
        }

        // Step 4: Check valid_until IS NULL OR > now
        if let Some(valid_until) = coupon.valid_until {
            if valid_until <= now {
                return Err(AppError::CouponExpired);
            }
        }

        // Step 5: Check max_uses IS NULL OR times_used < max_uses
        if let Some(max_uses) = coupon.max_uses {
            if coupon.times_used >= max_uses {
                return Err(AppError::CouponUsageLimitReached);
            }
        }

        // Step 6: Fetch linked discount
        let discount = sqlx::query_as::<_, DiscountRow>(
            r#"
            SELECT id, type AS discount_type, value, active
            FROM discounts
            WHERE id = $1
            "#,
        )
        .bind(coupon.discount_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?
        .ok_or(AppError::DiscountNotFound)?;

        if !discount.active {
            return Err(AppError::DiscountNotFound);
        }

        Ok(ValidatedCoupon {
            coupon_id: coupon.id,
            discount_type: discount.discount_type,
            discount_value: discount.value,
        })
    }

    /// Process a checkout request: validate, compute invoice, initiate billing session.
    ///
    /// Steps:
    /// 1. Validate billing_cycle_months >= 12 (Requirement 5.26)
    /// 2. No stacking: reject if both coupon_code and discount_id present (Requirement 5.35)
    /// 3. If coupon_code: validate_coupon(), get discount info
    /// 4. If discount_id: fetch discount, check active (Requirement 5.34)
    /// 5. Compute invoice
    /// 6. Within transaction: update subscription, increment coupon times_used if applicable (Requirement 5.39)
    /// 7. Return mock checkout URL + invoice (Requirement 5.28)
    pub async fn checkout(&self, request: CheckoutRequest) -> Result<CheckoutResponse, AppError> {
        // Step 1: Validate minimum billing cycle
        if request.billing_cycle_months < MINIMUM_BILLING_CYCLE_MONTHS {
            return Err(AppError::MinimumBillingCycleNotMet);
        }

        // Step 2: Enforce no stacking (Requirement 5.35)
        if request.coupon_code.is_some() && request.discount_id.is_some() {
            return Err(AppError::MultipleDiscountsNotAllowed);
        }

        // Step 3 & 4: Resolve discount source
        let mut coupon_id: Option<Uuid> = None;
        let mut discount_type: Option<String> = None;
        let mut discount_value: Option<Decimal> = None;

        if let Some(ref code) = request.coupon_code {
            // Validate coupon and extract discount info
            let validated = self.validate_coupon(code).await?;
            coupon_id = Some(validated.coupon_id);
            discount_type = Some(validated.discount_type);
            discount_value = Some(validated.discount_value);
        } else if let Some(disc_id) = request.discount_id {
            // Fetch discount directly (Requirement 5.34)
            let discount = sqlx::query_as::<_, DiscountRow>(
                r#"
                SELECT id, type AS discount_type, value, active
                FROM discounts
                WHERE id = $1
                "#,
            )
            .bind(disc_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| AppError::InternalError)?
            .ok_or(AppError::DiscountNotFound)?;

            if !discount.active {
                return Err(AppError::DiscountNotFound);
            }

            discount_type = Some(discount.discount_type);
            discount_value = Some(discount.value);
        }

        // Determine base price from tier
        let base_price: Decimal = match request.tier.as_str() {
            "pro" => PRO_BASE_PRICE.parse().unwrap(),
            "teams" => TEAMS_BASE_PRICE.parse().unwrap(),
            _ => return Err(AppError::InvalidTier),
        };

        // Step 5: Compute invoice
        let invoice_request = InvoiceRequest {
            base_price,
            billing_cycle_months: request.billing_cycle_months,
            discount_type: discount_type.clone(),
            discount_value,
            country_code: request.country_code.clone(),
        };

        let invoice = compute_invoice(&self.pool, &invoice_request).await?;

        // Step 6: Within transaction — update subscription and increment coupon times_used
        let mut tx = self.pool.begin().await.map_err(|_| AppError::InternalError)?;

        // Update subscription to pending_payment
        sqlx::query(
            r#"
            UPDATE subscriptions
            SET tier = $1, status = 'pending_payment', updated_at = now()
            WHERE workspace_id = $2
            "#,
        )
        .bind(&request.tier)
        .bind(request.workspace_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Increment times_used atomically if a coupon was used (Requirement 5.39)
        if let Some(c_id) = coupon_id {
            sqlx::query(
                r#"
                UPDATE coupon_codes
                SET times_used = times_used + 1
                WHERE id = $1
                "#,
            )
            .bind(c_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::InternalError)?;
        }

        tx.commit().await.map_err(|_| AppError::InternalError)?;

        // Step 7: Create Paddle checkout transaction
        let paddle_params = crate::subscription::paddle::CreateTransactionParams {
            tier: request.tier.clone(),
            total_amount_cents: crate::subscription::paddle::dollars_to_cents(invoice.total_amount),
            currency: invoice.currency.clone(),
            workspace_id: request.workspace_id.to_string(),
            success_url: request.success_url.or_else(|| self.config.billing_success_url.clone()),
            country_code: request.country_code.clone(),
        };

        let paddle_result =
            crate::subscription::paddle::create_checkout_transaction(&self.config, paddle_params)
                .await?;

        let mut checkout_url = paddle_result.checkout_url;

        // Append cancel_url as a query parameter if provided
        let cancel_url = request
            .cancel_url
            .or_else(|| self.config.billing_cancel_url.clone());
        if let Some(ref url) = cancel_url {
            let separator = if checkout_url.contains('?') { "&" } else { "?" };
            checkout_url = format!(
                "{}{}cancel_url={}",
                checkout_url,
                separator,
                urlencoding::encode(url)
            );
        }

        Ok(CheckoutResponse {
            checkout_url,
            invoice,
        })
    }

    // ─── Grace Period and Payment Deadline Checks ─────────────────────────────

    /// Check all past_due subscriptions for expired grace periods.
    ///
    /// Finds subscriptions where status='past_due' AND grace_period_end < NOW(),
    /// transitions them to status='cancelled', sets cancelled_at = NOW().
    /// This is called periodically by the background scheduler (Requirement 5.31).
    ///
    /// Returns the count of subscriptions transitioned.
    pub async fn check_grace_periods(&self) -> Result<u64, AppError> {
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
        .map_err(|_| AppError::InternalError)?;

        let count = result.rows_affected();
        info!(
            count = count,
            "check_grace_periods: transitioned past_due subscriptions to cancelled"
        );

        Ok(count)
    }

    /// Check all team workspaces with pending_payment past their deadline.
    ///
    /// Finds subscriptions where status='pending_payment' AND payment_deadline < NOW(),
    /// transitions them to status='deactivated'.
    /// This is called periodically by the background scheduler (Requirement 5.10, 5.11).
    ///
    /// Returns the count of subscriptions deactivated.
    pub async fn check_payment_deadlines(&self) -> Result<u64, AppError> {
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
        .map_err(|_| AppError::InternalError)?;

        let count = result.rows_affected();
        info!(
            count = count,
            "check_payment_deadlines: deactivated subscriptions past payment deadline"
        );

        Ok(count)
    }

    // ─── Referral System ──────────────────────────────────────────────────────

    /// Generate a referral code and create associated coupon_codes record.
    ///
    /// Called during user registration (Requirement 5.41).
    /// 1. Generate an 8-char alphanumeric code (retry on collision)
    /// 2. Create a discount with type=percentage, value=0.20 (20%), active=true
    /// 3. Create a coupon_codes record with type=referral, owner_id=user_id
    pub async fn create_referral_code(&self, user_id: Uuid) -> Result<String, AppError> {
        // Generate a unique 8-char alphanumeric code (retry up to 10 times on collision)
        let code = self.generate_unique_coupon_code(8).await?;

        // Create a discount of 20% for the referral
        let discount_id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO discounts (type, value, active)
            VALUES ('percentage', 0.20, true)
            RETURNING id
            "#,
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Create the coupon_codes record with type=referral, linked to the discount
        sqlx::query(
            r#"
            INSERT INTO coupon_codes (code, type, discount_id, owner_id, max_uses, valid_from, active)
            VALUES ($1, 'referral', $2, $3, NULL, now(), true)
            "#,
        )
        .bind(&code)
        .bind(discount_id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        Ok(code)
    }

    /// Validate and record a referral during registration (Requirements 5.42–5.44, 5.49).
    ///
    /// 1. Find coupon_codes where LOWER(code) = LOWER(referrer_code) AND type='referral'
    /// 2. If not found → ReferralCodeNotFound
    /// 3. Get owner_id from coupon → that's the referrer
    /// 4. If referrer == referred_user_id → SelfReferralNotAllowed
    /// 5. Check if referral already exists for (referrer, referred_user_id) → ReferralAlreadyUsed
    /// 6. INSERT into referrals with status='pending'. Use ON CONFLICT DO NOTHING for idempotency.
    pub async fn record_referral(
        &self,
        referrer_code: &str,
        referred_user_id: Uuid,
    ) -> Result<(), AppError> {
        // Step 1: Case-insensitive lookup for referral coupon
        let coupon = sqlx::query_as::<_, CouponRow>(
            r#"
            SELECT id, code, type AS coupon_type, discount_id, owner_id,
                   max_uses, times_used, valid_from, valid_until, active
            FROM coupon_codes
            WHERE LOWER(code) = LOWER($1) AND type = 'referral'
            "#,
        )
        .bind(referrer_code)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Step 2: If not found → ReferralCodeNotFound
        let coupon = coupon.ok_or(AppError::ReferralCodeNotFound)?;

        // Step 3: Get the referrer (owner of the coupon)
        let referrer_id = coupon.owner_id.ok_or(AppError::ReferralCodeNotFound)?;

        // Step 4: Self-referral check
        if referrer_id == referred_user_id {
            return Err(AppError::SelfReferralNotAllowed);
        }

        // Step 5: Check if referral already exists for this pair
        let existing: bool = sqlx::query_scalar(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM referrals
                WHERE referrer_id = $1 AND referred_user_id = $2
            )
            "#,
        )
        .bind(referrer_id)
        .bind(referred_user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        if existing {
            return Err(AppError::ReferralAlreadyUsed);
        }

        // Step 6: Insert referral with ON CONFLICT DO NOTHING for idempotency
        sqlx::query(
            r#"
            INSERT INTO referrals (referrer_id, referred_user_id, status)
            VALUES ($1, $2, 'pending')
            ON CONFLICT (referrer_id, referred_user_id) DO NOTHING
            "#,
        )
        .bind(referrer_id)
        .bind(referred_user_id)
        .execute(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        Ok(())
    }

    /// Apply referral reward when referred user's first paid subscription activates
    /// (Requirements 5.45, 5.46, 5.48).
    ///
    /// 1. Find referral where referred_user_id = $1 AND status = 'pending'
    /// 2. If none found, return Ok (no-op, idempotent)
    /// 3. Mark referral status = 'converted'
    /// 4. Find referrer's subscription (via their individual workspace)
    /// 5. If referrer has active paid sub with period_end → add 1 month to period_end
    /// 6. If referrer has no active paid sub → INSERT into referral_credits
    pub async fn apply_referral_reward(
        &self,
        referred_user_id: Uuid,
    ) -> Result<(), AppError> {
        // Step 1: Find pending referral for this referred user
        let referral = sqlx::query_as::<_, ReferralRow>(
            r#"
            SELECT id, referrer_id, referred_user_id, status, created_at
            FROM referrals
            WHERE referred_user_id = $1 AND status = 'pending'
            "#,
        )
        .bind(referred_user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Step 2: If no pending referral found, return Ok (idempotent no-op)
        let referral = match referral {
            Some(r) => r,
            None => return Ok(()),
        };

        // Step 3: Mark referral as converted
        sqlx::query(
            r#"
            UPDATE referrals
            SET status = 'converted'
            WHERE id = $1
            "#,
        )
        .bind(referral.id)
        .execute(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Step 4: Find referrer's individual workspace subscription
        let referrer_sub = sqlx::query_as::<_, SubscriptionRow>(
            r#"
            SELECT s.id, s.workspace_id, s.tier, s.status, s.period_start,
                   s.period_end, s.grace_period_end, s.cancelled_at
            FROM subscriptions s
            INNER JOIN workspaces w ON w.id = s.workspace_id
            WHERE w.owner_id = $1
              AND w.type = 'individual'
              AND s.tier IN ('pro', 'teams')
              AND s.status = 'active'
            LIMIT 1
            "#,
        )
        .bind(referral.referrer_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        match referrer_sub {
            Some(sub) => {
                // Step 5: Referrer has active paid sub → add 1 month to period_end
                if let Some(period_end) = sub.period_end {
                    let new_period_end = period_end
                        .checked_add_months(Months::new(1))
                        .unwrap_or(period_end);

                    sqlx::query(
                        r#"
                        UPDATE subscriptions
                        SET period_end = $1, updated_at = now()
                        WHERE id = $2
                        "#,
                    )
                    .bind(new_period_end)
                    .bind(sub.id)
                    .execute(&self.pool)
                    .await
                    .map_err(|_| AppError::InternalError)?;
                } else {
                    // Active sub but no period_end set — store credit instead
                    sqlx::query(
                        r#"
                        INSERT INTO referral_credits (user_id, months, redeemed)
                        VALUES ($1, 1, false)
                        "#,
                    )
                    .bind(referral.referrer_id)
                    .execute(&self.pool)
                    .await
                    .map_err(|_| AppError::InternalError)?;
                }
            }
            None => {
                // Step 6: No active paid subscription → store 1 month credit
                sqlx::query(
                    r#"
                    INSERT INTO referral_credits (user_id, months, redeemed)
                    VALUES ($1, 1, false)
                    "#,
                )
                .bind(referral.referrer_id)
                .execute(&self.pool)
                .await
                .map_err(|_| AppError::InternalError)?;
            }
        }

        Ok(())
    }

    // ─── Public Query ─────────────────────────────────────────────────────────

    /// Get the current subscription for a workspace in a serializable format.
    ///
    /// Used by the GET /subscriptions/current handler.
    pub async fn get_current_subscription(
        &self,
        workspace_id: Uuid,
    ) -> Result<CurrentSubscriptionResponse, AppError> {
        let row = self.get_subscription(workspace_id).await?;
        Ok(CurrentSubscriptionResponse {
            id: row.id,
            workspace_id: row.workspace_id,
            tier: row.tier,
            status: row.status,
            period_start: row.period_start,
            period_end: row.period_end,
            grace_period_end: row.grace_period_end,
        })
    }

    // ─── Private Helpers ────────────────────────────────────────────────────────

    /// Fetch the subscription for a workspace.
    async fn get_subscription(
        &self,
        workspace_id: Uuid,
    ) -> Result<SubscriptionRow, AppError> {
        let row = sqlx::query_as::<_, SubscriptionRow>(
            r#"
            SELECT id, workspace_id, tier, status, period_start, period_end,
                   grace_period_end, cancelled_at
            FROM subscriptions
            WHERE workspace_id = $1
            "#,
        )
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        row.ok_or(AppError::SubscriptionNotFound)
    }

    /// Parse a tier string into the Tier enum.
    fn parse_tier(&self, tier: &str) -> Tier {
        match tier {
            "free" => Tier::Free,
            "pro" => Tier::Pro,
            "teams" => Tier::Teams,
            _ => Tier::Free, // Default to free for safety
        }
    }

    /// Parse a status string into the SubscriptionStatus enum.
    fn parse_status(&self, status: &str) -> SubscriptionStatus {
        match status {
            "active" => SubscriptionStatus::Active,
            "past_due" => SubscriptionStatus::PastDue,
            "cancelled" => SubscriptionStatus::Cancelled,
            "pending_payment" => SubscriptionStatus::PendingPayment,
            "deactivated" => SubscriptionStatus::Deactivated,
            _ => SubscriptionStatus::Active, // Default for safety
        }
    }

    /// Generate a unique alphanumeric coupon code of the given length.
    /// Retries on collision (up to 10 attempts).
    async fn generate_unique_coupon_code(&self, len: usize) -> Result<String, AppError> {
        for _ in 0..10 {
            let code = generate_random_code(len);
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM coupon_codes WHERE LOWER(code) = LOWER($1))",
            )
            .bind(&code)
            .fetch_one(&self.pool)
            .await
            .map_err(|_| AppError::InternalError)?;

            if !exists {
                return Ok(code);
            }
        }

        Err(AppError::InternalError)
    }
}

/// Generate a random alphanumeric code of the given length.
fn generate_random_code(len: usize) -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}
