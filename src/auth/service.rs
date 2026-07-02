use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio::time::Instant;
use uuid::Uuid;

use crate::auth::jwt::encode_access_token;
use crate::auth::password::{hash_password, verify_password};
use crate::config::AppConfig;
use crate::errors::AppError;
use crate::middleware::auth_extractor::AccessTokenClaims;
use crate::models::common::{ClientType, Role, Tier};

// ─── Profile & Account Management DTOs ──────────────────────────────────────────

/// Request payload for updating user profile fields.
/// All fields are optional — only provided fields will be updated.
#[derive(Debug, Deserialize)]
pub struct UpdateProfileRequest {
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub profile_picture_url: Option<String>,
    pub timezone: Option<String>,
    pub language: Option<String>,
    pub country_code: Option<String>,
    pub phone: Option<String>,
}

/// Response payload for profile operations.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct ProfileResponse {
    pub id: Uuid,
    pub email: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub profile_picture_url: Option<String>,
    pub timezone: Option<String>,
    pub language: Option<String>,
    pub country_code: Option<String>,
    pub phone: Option<String>,
    pub role: Role,
    pub referral_code: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request payload for initiating an email change.
#[derive(Debug, Deserialize)]
pub struct ChangeEmailRequest {
    pub new_email: String,
}

/// Request payload for verifying an email change (query parameter).
#[derive(Debug, Deserialize)]
pub struct VerifyEmailChangeRequest {
    pub token: String,
}

/// Request payload for changing password.
#[derive(Debug, Deserialize)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

// ─── DTOs ───────────────────────────────────────────────────────────────────────

/// Registration request payload.
#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
    pub client_type: ClientType,
    pub referral_code: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
}

/// Login request payload.
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
    pub client_type: ClientType,
}

/// Authentication response returned after register/login.
#[derive(Debug, Serialize)]
pub struct AuthResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub user: UserInfo,
}

/// Basic user information returned in auth responses.
#[derive(Debug, Serialize)]
pub struct UserInfo {
    pub id: Uuid,
    pub email: String,
    pub role: Role,
    pub referral_code: String,
    pub workspace_id: Uuid,
}

/// Refresh token request payload.
#[derive(Debug, Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
    pub client_type: ClientType,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
}

/// Logout request payload.
#[derive(Debug, Deserialize)]
pub struct LogoutRequest {
    pub refresh_token: String,
}

/// Session information returned by list_sessions.
#[derive(Debug, Serialize)]
pub struct SessionInfo {
    pub session_id: Uuid,
    pub client_type: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
}

/// Internal row type for session listing.
#[derive(Debug, sqlx::FromRow)]
struct SessionRow {
    pub id: Uuid,
    pub client_type: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
}

/// Forgot password request payload.
#[derive(Debug, Deserialize)]
pub struct ForgotPasswordRequest {
    pub email: String,
}

/// Reset password request payload.
#[derive(Debug, Deserialize)]
pub struct ResetPasswordRequest {
    pub token: String,
    pub password: String,
}

/// Internal row type for refresh token lookup.
#[derive(Debug, sqlx::FromRow)]
struct RefreshTokenRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub token_hash: String,
    pub client_type: String,
    pub expires_at: DateTime<Utc>,
    pub revoked: bool,
}

/// Internal row type for user lookup during login.
#[derive(Debug, sqlx::FromRow)]
struct UserRow {
    pub id: Uuid,
    pub email: String,
    pub password_hash: String,
    pub role: Role,
    pub status: String,
    pub deleted_at: Option<DateTime<Utc>>,
    pub referral_code: String,
    pub must_reset_password: bool,
}

/// Request payload for creating an admin invite.
#[derive(Debug, Deserialize)]
pub struct CreateAdminInviteRequest {
    pub email: String,
}

/// Request payload for registering via an admin invite.
#[derive(Debug, Deserialize)]
pub struct RegisterViaInviteRequest {
    pub token: String,
    pub password: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub client_type: ClientType,
}

/// Response payload for a created admin invite.
#[derive(Debug, Serialize)]
pub struct AdminInviteResponse {
    pub id: Uuid,
    pub email: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// Internal row type for admin invite lookup.
#[derive(Debug, sqlx::FromRow)]
struct AdminInviteRow {
    pub id: Uuid,
    pub email: String,
    pub token_hash: String,
    pub expires_at: DateTime<Utc>,
    pub used: bool,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
}

/// Internal row type for password reset token lookup.
#[derive(Debug, sqlx::FromRow)]
struct PasswordResetTokenRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub token_hash: String,
    pub expires_at: DateTime<Utc>,
    pub used: bool,
}

// ─── Brute-force tracking ────────────────────────────────────────────────────

/// In-memory state for tracking failed login attempts per email.
#[derive(Debug, Clone)]
struct BruteForceEntry {
    /// Number of consecutive failed login attempts.
    count: u32,
    /// If set, the account is locked until this time.
    locked_until: Option<DateTime<Utc>>,
}

// ─── Constants ───────────────────────────────────────────────────────────────────

/// Maximum consecutive failed login attempts before lockout.
const MAX_FAILED_ATTEMPTS: u32 = 5;

/// Lockout duration in minutes after reaching max failed attempts.
const LOCKOUT_DURATION_MINUTES: i64 = 15;

/// Minimum response time in milliseconds to resist timing attacks.
const MIN_RESPONSE_MS: u64 = 100;

/// Maximum active refresh token sessions per user.
const MAX_SESSIONS: i64 = 5;

/// Maximum pending (unused, unexpired) admin invites at any time.
const MAX_PENDING_ADMIN_INVITES: i64 = 5;

// ─── Service ────────────────────────────────────────────────────────────────────

/// Authentication service handling registration and login flows.
pub struct AuthService {
    pool: PgPool,
    config: Arc<AppConfig>,
    /// In-memory brute-force attempt tracker. Key: email, Value: BruteForceEntry.
    failed_attempts: DashMap<String, BruteForceEntry>,
}

impl AuthService {
    /// Create a new AuthService instance.
    pub fn new(pool: PgPool, config: Arc<AppConfig>) -> Self {
        Self {
            pool,
            config,
            failed_attempts: DashMap::new(),
        }
    }

    /// Register a new user account.
    ///
    /// Performs the following steps within a transaction:
    /// 1. Validate password length (min 8 chars)
    /// 2. Check email uniqueness
    /// 3. Hash password with Argon2id
    /// 4. Generate unique referral code
    /// 5. Persist user with role=user
    /// 6. Create individual workspace + free subscription
    /// 7. Handle optional referral code
    /// 8. Generate token pair (access + refresh)
    pub async fn register(&self, req: RegisterRequest) -> Result<AuthResponse, AppError> {
        // 1. Validate password length
        if req.password.len() < 8 {
            return Err(AppError::PasswordTooShort);
        }

        // 2. Check email uniqueness
        let email_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM users WHERE email = $1 AND deleted_at IS NULL)",
        )
        .bind(&req.email)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        if email_exists {
            return Err(AppError::EmailAlreadyRegistered);
        }

        // 3. Hash password with Argon2id
        let password_hash = hash_password(&req.password)?;

        // 4. Generate unique referral code (8 alphanumeric chars, retry on collision)
        let referral_code = self.generate_unique_referral_code().await?;

        // 5–7. Execute registration within a transaction
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AppError::InternalError)?;

        // 5. Insert user with role=user (blocks admin creation)
        let user_id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO users (email, password_hash, role, status, referral_code, first_name, last_name)
            VALUES ($1, $2, 'user', 'active', $3, $4, $5)
            RETURNING id
            "#,
        )
        .bind(&req.email)
        .bind(&password_hash)
        .bind(&referral_code)
        .bind(&req.first_name)
        .bind(&req.last_name)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| {
            // Handle unique constraint violation on email (race condition)
            if let sqlx::Error::Database(ref db_err) = e {
                if db_err.constraint() == Some("idx_users_email") {
                    return AppError::EmailAlreadyRegistered;
                }
            }
            AppError::InternalError
        })?;

        // 6a. Create individual workspace
        let workspace_name = format!("{}'s Workspace", req.email);
        let workspace_id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO workspaces (type, owner_id, name)
            VALUES ('individual', $1, $2)
            RETURNING id
            "#,
        )
        .bind(user_id)
        .bind(&workspace_name)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        // 6b. Add workspace_members entry with role=owner
        sqlx::query(
            r#"
            INSERT INTO workspace_members (workspace_id, user_id, role)
            VALUES ($1, $2, 'owner')
            "#,
        )
        .bind(workspace_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        // 6c. Create free subscription for the workspace
        sqlx::query(
            r#"
            INSERT INTO subscriptions (workspace_id, tier, status)
            VALUES ($1, 'free', 'active')
            "#,
        )
        .bind(workspace_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        // 7. Handle referral code if provided
        if let Some(ref code) = req.referral_code {
            self.handle_referral(code, user_id, &mut tx).await?;
        }

        // 8. Generate refresh token
        let raw_refresh_token = generate_refresh_token();
        let token_hash = hash_token(&raw_refresh_token);
        let expires_at = Utc::now() + Duration::days(30);

        sqlx::query(
            r#"
            INSERT INTO refresh_tokens (user_id, token_hash, client_type, expires_at)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(user_id)
        .bind(&token_hash)
        .bind(client_type_to_str(&req.client_type))
        .bind(expires_at)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Commit transaction
        tx.commit().await.map_err(|_| AppError::InternalError)?;

        // Generate access token JWT
        let claims = AccessTokenClaims {
            sub: user_id,
            client_type: req.client_type,
            role: Role::User,
            permissions: default_user_permissions(),
            subscription_tier: Tier::Free,
            status: "active".to_string(),
            must_reset_password: false,
            exp: 0, // will be set by encode_access_token
        };

        let access_token = encode_access_token(claims, &self.config.jwt_secret);

        Ok(AuthResponse {
            access_token,
            refresh_token: raw_refresh_token,
            user: UserInfo {
                id: user_id,
                email: req.email,
                role: Role::User,
                referral_code,
                workspace_id,
            },
        })
    }

    /// Generate a unique 8-character alphanumeric referral code.
    /// Retries on collision (up to 10 attempts).
    async fn generate_unique_referral_code(&self) -> Result<String, AppError> {
        for _ in 0..10 {
            let code = generate_random_code(8);
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM users WHERE referral_code = $1)",
            )
            .bind(&code)
            .fetch_one(&self.pool)
            .await
            .map_err(|_| AppError::InternalError)?;

            if !exists {
                return Ok(code);
            }
        }

        // Extremely unlikely to reach here with 8-char alphanumeric codes
        Err(AppError::InternalError)
    }

    /// Validate and record a referral during registration.
    async fn handle_referral(
        &self,
        referral_code: &str,
        referred_user_id: Uuid,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<(), AppError> {
        // Look up the referrer by their referral_code
        let referrer_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM users WHERE referral_code = $1 AND deleted_at IS NULL",
        )
        .bind(referral_code)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        let referrer_id = referrer_id.ok_or(AppError::ReferralCodeNotFound)?;

        // Check not self-referral
        if referrer_id == referred_user_id {
            return Err(AppError::SelfReferralNotAllowed);
        }

        // Record in referrals table with status=pending
        sqlx::query(
            r#"
            INSERT INTO referrals (referrer_id, referred_user_id, status)
            VALUES ($1, $2, 'pending')
            "#,
        )
        .bind(referrer_id)
        .bind(referred_user_id)
        .execute(&mut **tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        Ok(())
    }

    // ─── Login Flow ─────────────────────────────────────────────────────────────

    /// Authenticate a user and issue a token pair.
    ///
    /// Performs the following steps:
    /// 1. Check brute-force lockout
    /// 2. Look up user by email (including soft-deleted within 30-day window)
    /// 3. Verify Argon2id password hash
    /// 4. Reset failed attempt counter on success
    /// 5. Reactivate soft-deleted account if within retention window
    /// 6. Enforce session limit (max 5 active refresh tokens)
    /// 7. Generate token pair and return response with role
    ///
    /// Enforces a minimum 100ms response time to resist timing attacks.
    pub async fn login(&self, req: LoginRequest) -> Result<AuthResponse, AppError> {
        let start = Instant::now();

        // 1. Check brute-force lockout
        self.check_brute_force(&req.email)?;

        // 2. Look up user by email (include soft-deleted within 30-day retention window)
        let thirty_days_ago = Utc::now() - Duration::days(30);
        let user_row = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT id, email, password_hash, role, status, deleted_at, referral_code, must_reset_password
            FROM users
            WHERE email = $1
              AND (deleted_at IS NULL OR deleted_at > $2)
            "#,
        )
        .bind(&req.email)
        .bind(thirty_days_ago)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        let user = match user_row {
            Some(u) => u,
            None => {
                self.record_failed_attempt(&req.email);
                self.enforce_min_response_time(start).await;
                return Err(AppError::InvalidCredentials);
            }
        };

        // 3. Verify password
        let password_valid = verify_password(&req.password, &user.password_hash)?;
        if !password_valid {
            self.record_failed_attempt(&req.email);
            self.enforce_min_response_time(start).await;
            return Err(AppError::InvalidCredentials);
        }

        // 4. Reset failed attempts on successful authentication
        self.reset_failed_attempts(&req.email);

        // 5–7. Proceed within a transaction
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AppError::InternalError)?;

        // 5. Reactivate soft-deleted account if within retention window
        if user.deleted_at.is_some() {
            sqlx::query("UPDATE users SET deleted_at = NULL WHERE id = $1")
                .bind(user.id)
                .execute(&mut *tx)
                .await
                .map_err(|_| AppError::InternalError)?;
        }

        // 6. Enforce session limit (revoke oldest if >= MAX_SESSIONS active)
        self.enforce_session_limit(user.id, &mut tx).await?;

        // 7. Generate refresh token and persist
        let raw_refresh_token = generate_refresh_token();
        let token_hash = hash_token(&raw_refresh_token);
        let expires_at = Utc::now() + Duration::days(30);

        sqlx::query(
            r#"
            INSERT INTO refresh_tokens (user_id, token_hash, client_type, expires_at)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(user.id)
        .bind(&token_hash)
        .bind(client_type_to_str(&req.client_type))
        .bind(expires_at)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Commit transaction
        tx.commit().await.map_err(|_| AppError::InternalError)?;

        // Look up subscription tier for access token claims
        let subscription_tier: Option<String> = sqlx::query_scalar(
            r#"
            SELECT s.tier
            FROM subscriptions s
            JOIN workspaces w ON w.id = s.workspace_id
            WHERE w.owner_id = $1 AND w.type = 'individual'
            LIMIT 1
            "#,
        )
        .bind(user.id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        let tier = match subscription_tier.as_deref() {
            Some("pro") => Tier::Pro,
            Some("teams") => Tier::Teams,
            _ => Tier::Free,
        };

        // Look up the user's individual workspace ID
        let workspace_id: Uuid = sqlx::query_scalar(
            r#"
            SELECT id FROM workspaces WHERE owner_id = $1 AND type = 'individual' LIMIT 1
            "#,
        )
        .bind(user.id)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Generate access token JWT
        let claims = AccessTokenClaims {
            sub: user.id,
            client_type: req.client_type,
            role: user.role.clone(),
            permissions: default_user_permissions(),
            subscription_tier: tier,
            status: user.status.clone(),
            must_reset_password: user.must_reset_password,
            exp: 0, // will be set by encode_access_token
        };

        let access_token = encode_access_token(claims, &self.config.jwt_secret);

        Ok(AuthResponse {
            access_token,
            refresh_token: raw_refresh_token,
            user: UserInfo {
                id: user.id,
                email: user.email,
                role: user.role,
                referral_code: user.referral_code,
                workspace_id,
            },
        })
    }

    // ─── Token Refresh ────────────────────────────────────────────────────────────

    /// Refresh an access/refresh token pair using token rotation.
    ///
    /// Implements the following logic:
    /// 1. Hash the presented token and look it up in `refresh_tokens`
    /// 2. If not found → InvalidRefreshToken
    /// 3. If found AND revoked → token reuse detected (theft):
    ///    - Revoke ALL tokens for that user
    ///    - Log security event
    ///    - Return TokenReuseDetected
    /// 4. If found AND expired → InvalidRefreshToken
    /// 5. If valid → atomically invalidate old token and issue new pair
    pub async fn refresh_token(&self, req: RefreshRequest) -> Result<AuthResponse, AppError> {
        // 1. Hash the presented token
        let token_hash = hash_token(&req.refresh_token);

        // 2. Look up by token_hash
        let token_row = sqlx::query_as::<_, RefreshTokenRow>(
            r#"
            SELECT id, user_id, token_hash, client_type, expires_at, revoked
            FROM refresh_tokens
            WHERE token_hash = $1
            "#,
        )
        .bind(&token_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        let token_row = match token_row {
            Some(row) => row,
            None => return Err(AppError::InvalidRefreshToken),
        };

        // 3. Reuse detection: if already revoked, this is token theft
        if token_row.revoked {
            // Revoke ALL refresh tokens for this user
            sqlx::query("UPDATE refresh_tokens SET revoked = true WHERE user_id = $1")
                .bind(token_row.user_id)
                .execute(&self.pool)
                .await
                .map_err(|_| AppError::InternalError)?;

            // Log security event
            tracing::warn!(
                user_id = %token_row.user_id,
                ip_address = req.ip_address.as_deref().unwrap_or("unknown"),
                user_agent = req.user_agent.as_deref().unwrap_or("unknown"),
                timestamp = %Utc::now(),
                "Token reuse detected — possible token theft. All refresh tokens revoked for user."
            );

            return Err(AppError::TokenReuseDetected);
        }

        // 4. Check expiration (after reuse check)
        if token_row.expires_at < Utc::now() {
            return Err(AppError::InvalidRefreshToken);
        }

        // 5. Valid token — rotate: invalidate old, issue new (atomically)
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AppError::InternalError)?;

        // Invalidate the presented token
        sqlx::query("UPDATE refresh_tokens SET revoked = true WHERE id = $1")
            .bind(token_row.id)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::InternalError)?;

        // Generate new refresh token
        let new_raw_refresh_token = generate_refresh_token();
        let new_token_hash = hash_token(&new_raw_refresh_token);
        let new_expires_at = Utc::now() + Duration::days(30);

        sqlx::query(
            r#"
            INSERT INTO refresh_tokens (user_id, token_hash, client_type, expires_at)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(token_row.user_id)
        .bind(&new_token_hash)
        .bind(client_type_to_str(&req.client_type))
        .bind(new_expires_at)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Commit transaction
        tx.commit().await.map_err(|_| AppError::InternalError)?;

        // Look up user details
        let user = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT id, email, password_hash, role, status, deleted_at, referral_code, must_reset_password
            FROM users
            WHERE id = $1
            "#,
        )
        .bind(token_row.user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Look up subscription tier
        let subscription_tier: Option<String> = sqlx::query_scalar(
            r#"
            SELECT s.tier
            FROM subscriptions s
            JOIN workspaces w ON w.id = s.workspace_id
            WHERE w.owner_id = $1 AND w.type = 'individual'
            LIMIT 1
            "#,
        )
        .bind(user.id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        let tier = match subscription_tier.as_deref() {
            Some("pro") => Tier::Pro,
            Some("teams") => Tier::Teams,
            _ => Tier::Free,
        };

        // Generate new access token JWT
        let claims = AccessTokenClaims {
            sub: user.id,
            client_type: req.client_type,
            role: user.role.clone(),
            permissions: default_user_permissions(),
            subscription_tier: tier,
            status: user.status.clone(),
            must_reset_password: user.must_reset_password,
            exp: 0, // will be set by encode_access_token
        };

        let access_token = encode_access_token(claims, &self.config.jwt_secret);

        // Look up the user's individual workspace ID
        let workspace_id: Uuid = sqlx::query_scalar(
            r#"
            SELECT id FROM workspaces WHERE owner_id = $1 AND type = 'individual' LIMIT 1
            "#,
        )
        .bind(user.id)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        Ok(AuthResponse {
            access_token,
            refresh_token: new_raw_refresh_token,
            user: UserInfo {
                id: user.id,
                email: user.email,
                role: user.role,
                referral_code: user.referral_code,
                workspace_id,
            },
        })
    }

    // ─── Logout ─────────────────────────────────────────────────────────────────

    /// Invalidate a refresh token (logout).
    ///
    /// Hashes the presented token, looks it up in `refresh_tokens`, and marks it
    /// as revoked. Returns `Ok(())` on success (caller returns HTTP 204).
    ///
    /// Errors:
    /// - `InvalidRefreshToken` if the token is not found or already revoked.
    pub async fn logout(&self, req: LogoutRequest) -> Result<(), AppError> {
        let token_hash = hash_token(&req.refresh_token);

        // Look up the token row
        let token_row = sqlx::query_as::<_, RefreshTokenRow>(
            r#"
            SELECT id, user_id, token_hash, client_type, expires_at, revoked
            FROM refresh_tokens
            WHERE token_hash = $1
            "#,
        )
        .bind(&token_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        let token_row = match token_row {
            Some(row) => row,
            None => return Err(AppError::InvalidRefreshToken),
        };

        // If already revoked, treat as invalid
        if token_row.revoked {
            return Err(AppError::InvalidRefreshToken);
        }

        // Revoke the token
        sqlx::query("UPDATE refresh_tokens SET revoked = true WHERE id = $1")
            .bind(token_row.id)
            .execute(&self.pool)
            .await
            .map_err(|_| AppError::InternalError)?;

        Ok(())
    }

    // ─── Forgot Password ──────────────────────────────────────────────────────────

    /// Initiate a password reset flow.
    ///
    /// Generates a cryptographically secure token, stores its SHA-256 hash
    /// in `password_reset_tokens` with a 30-minute TTL, invalidates any
    /// previously issued (unused) tokens for that user, and triggers an email
    /// send (placeholder until email service is implemented).
    ///
    /// Always returns `Ok(())` regardless of whether the email exists to prevent
    /// email enumeration attacks.
    pub async fn forgot_password(&self, req: ForgotPasswordRequest) -> Result<(), AppError> {
        // Look up user by email (only active, non-deleted users)
        let user_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM users WHERE email = $1 AND deleted_at IS NULL",
        )
        .bind(&req.email)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        // If user doesn't exist, return 200 anyway (prevent email enumeration)
        let user_id = match user_id {
            Some(id) => id,
            None => return Ok(()),
        };

        // Generate crypto-secure random token (32 bytes, hex-encoded)
        let raw_token = generate_refresh_token(); // reuses the same crypto-secure generator
        let token_hash = hash_token(&raw_token);
        let expires_at = Utc::now() + Duration::minutes(30);

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AppError::InternalError)?;

        // Invalidate any previously issued (unused) reset tokens for this user
        sqlx::query(
            "UPDATE password_reset_tokens SET used = true WHERE user_id = $1 AND used = false",
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Store new token hash with 30-minute TTL
        sqlx::query(
            r#"
            INSERT INTO password_reset_tokens (user_id, token_hash, expires_at)
            VALUES ($1, $2, $3)
            "#,
        )
        .bind(user_id)
        .bind(&token_hash)
        .bind(expires_at)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        tx.commit().await.map_err(|_| AppError::InternalError)?;

        // TODO: Send password reset email via EmailService (task 14)
        // For now, log the raw token as a placeholder
        tracing::info!(
            user_id = %user_id,
            email = %req.email,
            "Password reset token generated (email sending not yet implemented). Token: {}",
            raw_token
        );

        Ok(())
    }

    // ─── Reset Password ───────────────────────────────────────────────────────────

    /// Reset a user's password using a valid reset token.
    ///
    /// Validates the token (must exist, not expired, not already used), hashes
    /// the new password with Argon2id, updates the user's password, marks the
    /// token as used, and revokes ALL refresh tokens for that user.
    pub async fn reset_password(&self, req: ResetPasswordRequest) -> Result<(), AppError> {
        // Validate password length (minimum 8 characters)
        if req.password.len() < 8 {
            return Err(AppError::PasswordTooShort);
        }

        // Hash the presented token to look it up
        let token_hash = hash_token(&req.token);

        // Look up the reset token row
        let token_row = sqlx::query_as::<_, PasswordResetTokenRow>(
            r#"
            SELECT id, user_id, token_hash, expires_at, used
            FROM password_reset_tokens
            WHERE token_hash = $1
            "#,
        )
        .bind(&token_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        let token_row = match token_row {
            Some(row) => row,
            None => return Err(AppError::InvalidResetToken),
        };

        // Check if token is already used
        if token_row.used {
            return Err(AppError::InvalidResetToken);
        }

        // Check if token is expired
        if token_row.expires_at < Utc::now() {
            return Err(AppError::InvalidResetToken);
        }

        // Hash the new password with Argon2id
        let new_password_hash = hash_password(&req.password)?;

        // Perform all updates atomically
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AppError::InternalError)?;

        // Update the user's password
        sqlx::query("UPDATE users SET password_hash = $1, updated_at = NOW() WHERE id = $2")
            .bind(&new_password_hash)
            .bind(token_row.user_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::InternalError)?;

        // Mark the reset token as used
        sqlx::query("UPDATE password_reset_tokens SET used = true WHERE id = $1")
            .bind(token_row.id)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::InternalError)?;

        // Revoke ALL refresh tokens for this user
        sqlx::query("UPDATE refresh_tokens SET revoked = true WHERE user_id = $1")
            .bind(token_row.user_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::InternalError)?;

        tx.commit().await.map_err(|_| AppError::InternalError)?;

        // Also clear the must_reset_password flag if it was set
        sqlx::query("UPDATE users SET must_reset_password = false WHERE id = $1 AND must_reset_password = true")
            .bind(token_row.user_id)
            .execute(&self.pool)
            .await
            .map_err(|_| AppError::InternalError)?;

        tracing::info!(
            user_id = %token_row.user_id,
            "Password successfully reset and all refresh tokens revoked."
        );

        Ok(())
    }

    // ─── Account Deletion ─────────────────────────────────────────────────────────

    /// Soft-delete a user account.
    ///
    /// Checks that the user does not own any team workspaces. If they do,
    /// returns `TransferOwnershipRequired` (HTTP 422). Otherwise, sets
    /// `deleted_at` to the current UTC timestamp.
    pub async fn delete_account(&self, user_id: Uuid) -> Result<(), AppError> {
        // Check if user owns any team workspaces
        let team_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM workspaces WHERE owner_id = $1 AND type = 'team'",
        )
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        if team_count > 0 {
            return Err(AppError::TransferOwnershipRequired);
        }

        // Soft-delete by setting deleted_at
        sqlx::query("UPDATE users SET deleted_at = NOW() WHERE id = $1")
            .bind(user_id)
            .execute(&self.pool)
            .await
            .map_err(|_| AppError::InternalError)?;

        Ok(())
    }

    // ─── Session Management ─────────────────────────────────────────────────────

    /// List active sessions for a user.
    ///
    /// Returns all non-revoked, non-expired refresh tokens for the user,
    /// each represented as a `SessionInfo` with session_id, client_type,
    /// created_at, and last_used_at.
    pub async fn list_sessions(&self, user_id: Uuid) -> Result<Vec<SessionInfo>, AppError> {
        let rows = sqlx::query_as::<_, SessionRow>(
            r#"
            SELECT id, client_type, created_at, last_used_at
            FROM refresh_tokens
            WHERE user_id = $1
              AND revoked = false
              AND expires_at > NOW()
            ORDER BY created_at DESC
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        let sessions = rows
            .into_iter()
            .map(|row| SessionInfo {
                session_id: row.id,
                client_type: row.client_type,
                created_at: row.created_at,
                last_used_at: row.last_used_at,
            })
            .collect();

        Ok(sessions)
    }

    /// Revoke a specific session (refresh token) for a user.
    ///
    /// Verifies that the session belongs to the user before revoking.
    /// Returns `InvalidRefreshToken` if the session is not found or does not
    /// belong to the user.
    pub async fn revoke_session(
        &self,
        user_id: Uuid,
        session_id: Uuid,
    ) -> Result<(), AppError> {
        // Verify the session belongs to this user and is active
        let result = sqlx::query_scalar::<_, Uuid>(
            r#"
            SELECT id FROM refresh_tokens
            WHERE id = $1
              AND user_id = $2
              AND revoked = false
              AND expires_at > NOW()
            "#,
        )
        .bind(session_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        if result.is_none() {
            return Err(AppError::InvalidRefreshToken);
        }

        // Revoke the session
        sqlx::query("UPDATE refresh_tokens SET revoked = true WHERE id = $1")
            .bind(session_id)
            .execute(&self.pool)
            .await
            .map_err(|_| AppError::InternalError)?;

        Ok(())
    }

    // ─── Admin Invite Flow ──────────────────────────────────────────────────────

    /// Create an admin invite.
    ///
    /// Enforces a maximum of 5 pending (unused, unexpired) invites at any time.
    /// Generates a crypto-secure token with 24-hour TTL, stores its SHA-256 hash
    /// in `admin_invites`, and sends an invite email (placeholder).
    ///
    /// Returns HTTP 201 with invite details on success.
    pub async fn create_admin_invite(
        &self,
        email: String,
        created_by: Uuid,
    ) -> Result<AdminInviteResponse, AppError> {
        // 1. Enforce max 5 pending invites
        let pending_count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)
            FROM admin_invites
            WHERE used = false
              AND expires_at > NOW()
            "#,
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        if pending_count >= MAX_PENDING_ADMIN_INVITES {
            return Err(AppError::MaxPendingInvitesReached);
        }

        // 2. Generate crypto-secure token (32 bytes, hex-encoded)
        let raw_token = generate_refresh_token();
        let token_hash = hash_token(&raw_token);
        let expires_at = Utc::now() + Duration::hours(24);

        // 3. Store hashed token in admin_invites
        let row = sqlx::query_as::<_, AdminInviteRow>(
            r#"
            INSERT INTO admin_invites (email, token_hash, expires_at, created_by)
            VALUES ($1, $2, $3, $4)
            RETURNING id, email, token_hash, expires_at, used, created_by, created_at
            "#,
        )
        .bind(&email)
        .bind(&token_hash)
        .bind(expires_at)
        .bind(created_by)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        // 4. Send invite email (placeholder — email service not yet implemented)
        tracing::info!(
            invite_id = %row.id,
            email = %email,
            created_by = %created_by,
            "Admin invite created (email sending not yet implemented). Token: {}",
            raw_token
        );

        Ok(AdminInviteResponse {
            id: row.id,
            email: row.email,
            expires_at: row.expires_at,
            created_at: row.created_at,
        })
    }

    /// Register a new admin user via a valid invite token.
    ///
    /// Validates the token (must exist, not expired, not already used), creates
    /// the user account with `role = admin`, generates a referral code, creates
    /// an individual workspace + free subscription (same pattern as regular
    /// register but with role=admin), marks the invite as used, and issues a
    /// token pair.
    pub async fn register_via_invite(
        &self,
        req: RegisterViaInviteRequest,
    ) -> Result<AuthResponse, AppError> {
        // 1. Validate password length
        if req.password.len() < 8 {
            return Err(AppError::PasswordTooShort);
        }

        // 2. Hash the presented token and look up the invite
        let token_hash = hash_token(&req.token);

        let invite_row = sqlx::query_as::<_, AdminInviteRow>(
            r#"
            SELECT id, email, token_hash, expires_at, used, created_by, created_at
            FROM admin_invites
            WHERE token_hash = $1
            "#,
        )
        .bind(&token_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        let invite = match invite_row {
            Some(row) => row,
            None => return Err(AppError::InviteExpired),
        };

        // 3. Validate: not used and not expired
        if invite.used || invite.expires_at < Utc::now() {
            return Err(AppError::InviteExpired);
        }

        // 4. Check email uniqueness
        let email_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM users WHERE email = $1 AND deleted_at IS NULL)",
        )
        .bind(&invite.email)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        if email_exists {
            return Err(AppError::EmailAlreadyRegistered);
        }

        // 5. Hash password with Argon2id
        let password_hash = hash_password(&req.password)?;

        // 6. Generate unique referral code
        let referral_code = self.generate_unique_referral_code().await?;

        // 7. Execute registration within a transaction
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AppError::InternalError)?;

        // 7a. Create user with role=admin
        let user_id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO users (email, password_hash, role, status, referral_code, first_name, last_name)
            VALUES ($1, $2, 'admin', 'active', $3, $4, $5)
            RETURNING id
            "#,
        )
        .bind(&invite.email)
        .bind(&password_hash)
        .bind(&referral_code)
        .bind(&req.first_name)
        .bind(&req.last_name)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref db_err) = e {
                if db_err.constraint() == Some("idx_users_email") {
                    return AppError::EmailAlreadyRegistered;
                }
            }
            AppError::InternalError
        })?;

        // 7b. Create individual workspace
        let workspace_name = format!("{}'s Workspace", invite.email);
        let workspace_id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO workspaces (type, owner_id, name)
            VALUES ('individual', $1, $2)
            RETURNING id
            "#,
        )
        .bind(user_id)
        .bind(&workspace_name)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        // 7c. Add workspace_members entry with role=owner
        sqlx::query(
            r#"
            INSERT INTO workspace_members (workspace_id, user_id, role)
            VALUES ($1, $2, 'owner')
            "#,
        )
        .bind(workspace_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        // 7d. Create free subscription for the workspace
        sqlx::query(
            r#"
            INSERT INTO subscriptions (workspace_id, tier, status)
            VALUES ($1, 'free', 'active')
            "#,
        )
        .bind(workspace_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        // 7e. Mark the invite as used
        sqlx::query("UPDATE admin_invites SET used = true WHERE id = $1")
            .bind(invite.id)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::InternalError)?;

        // 7f. Generate refresh token
        let raw_refresh_token = generate_refresh_token();
        let refresh_token_hash = hash_token(&raw_refresh_token);
        let refresh_expires_at = Utc::now() + Duration::days(30);

        sqlx::query(
            r#"
            INSERT INTO refresh_tokens (user_id, token_hash, client_type, expires_at)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(user_id)
        .bind(&refresh_token_hash)
        .bind(client_type_to_str(&req.client_type))
        .bind(refresh_expires_at)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        // Commit transaction
        tx.commit().await.map_err(|_| AppError::InternalError)?;

        // Generate access token JWT
        let claims = AccessTokenClaims {
            sub: user_id,
            client_type: req.client_type,
            role: Role::Admin,
            permissions: default_user_permissions(),
            subscription_tier: Tier::Free,
            status: "active".to_string(),
            must_reset_password: false,
            exp: 0, // will be set by encode_access_token
        };

        let access_token = encode_access_token(claims, &self.config.jwt_secret);

        Ok(AuthResponse {
            access_token,
            refresh_token: raw_refresh_token,
            user: UserInfo {
                id: user_id,
                email: invite.email,
                role: Role::Admin,
                referral_code,
                workspace_id,
            },
        })
    }

    // ─── Brute-Force Protection ─────────────────────────────────────────────────

    /// Check if the given email is currently locked due to brute-force protection.
    ///
    /// Returns `AppError::AccountLocked` with remaining lockout seconds if locked.
    fn check_brute_force(&self, email: &str) -> Result<(), AppError> {
        if let Some(entry) = self.failed_attempts.get(email) {
            if let Some(locked_until) = entry.locked_until {
                let now = Utc::now();
                if now < locked_until {
                    let remaining = (locked_until - now).num_seconds().max(1) as u64;
                    return Err(AppError::AccountLocked {
                        retry_after_secs: remaining,
                    });
                }
                // Lockout expired — will be cleared on next successful login or attempt reset
            }
        }
        Ok(())
    }

    /// Record a failed login attempt for the given email.
    ///
    /// After MAX_FAILED_ATTEMPTS consecutive failures, locks the account for
    /// LOCKOUT_DURATION_MINUTES.
    fn record_failed_attempt(&self, email: &str) {
        let mut entry = self
            .failed_attempts
            .entry(email.to_string())
            .or_insert(BruteForceEntry {
                count: 0,
                locked_until: None,
            });

        entry.count += 1;

        if entry.count >= MAX_FAILED_ATTEMPTS {
            entry.locked_until =
                Some(Utc::now() + Duration::minutes(LOCKOUT_DURATION_MINUTES));
        }
    }

    /// Reset the failed attempt counter for the given email (called on successful login).
    fn reset_failed_attempts(&self, email: &str) {
        self.failed_attempts.remove(email);
    }

    // ─── Session Limit ──────────────────────────────────────────────────────────

    /// Enforce the maximum session limit per user.
    ///
    /// If there are already MAX_SESSIONS active (non-revoked, non-expired) refresh tokens,
    /// revoke the oldest one to make room for the new session.
    async fn enforce_session_limit(
        &self,
        user_id: Uuid,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<(), AppError> {
        // Count active sessions
        let active_count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)
            FROM refresh_tokens
            WHERE user_id = $1
              AND revoked = false
              AND expires_at > NOW()
            "#,
        )
        .bind(user_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        if active_count >= MAX_SESSIONS {
            // Revoke the oldest active refresh token
            sqlx::query(
                r#"
                UPDATE refresh_tokens
                SET revoked = true
                WHERE id = (
                    SELECT id
                    FROM refresh_tokens
                    WHERE user_id = $1
                      AND revoked = false
                      AND expires_at > NOW()
                    ORDER BY created_at ASC
                    LIMIT 1
                )
                "#,
            )
            .bind(user_id)
            .execute(&mut **tx)
            .await
            .map_err(|_| AppError::InternalError)?;
        }

        Ok(())
    }

    // ─── Timing Protection ──────────────────────────────────────────────────────

    /// Enforce minimum response time to resist timing attacks.
    async fn enforce_min_response_time(&self, start: Instant) {
        let elapsed = start.elapsed();
        let min_duration = std::time::Duration::from_millis(MIN_RESPONSE_MS);
        if elapsed < min_duration {
            tokio::time::sleep(min_duration - elapsed).await;
        }
    }

    // ─── Profile Management ─────────────────────────────────────────────────────

    /// Update the authenticated user's profile fields.
    ///
    /// Only fields that are `Some` in the request will be updated.
    /// Builds a dynamic SQL UPDATE query to avoid overwriting unset fields.
    pub async fn update_profile(
        &self,
        user_id: Uuid,
        req: UpdateProfileRequest,
    ) -> Result<ProfileResponse, AppError> {
        // Build dynamic SET clause — only include fields that are Some
        let mut set_clauses: Vec<String> = Vec::new();
        let mut param_index: usize = 1; // $1 is reserved for user_id in WHERE clause

        // We'll collect the values to bind in order
        struct DynParams {
            first_name: Option<String>,
            last_name: Option<String>,
            profile_picture_url: Option<String>,
            timezone: Option<String>,
            language: Option<String>,
            country_code: Option<String>,
            phone: Option<String>,
        }

        let params = DynParams {
            first_name: req.first_name.clone(),
            last_name: req.last_name.clone(),
            profile_picture_url: req.profile_picture_url.clone(),
            timezone: req.timezone.clone(),
            language: req.language.clone(),
            country_code: req.country_code.clone(),
            phone: req.phone.clone(),
        };

        // Track which fields to bind (in order)
        let mut bind_order: Vec<&str> = Vec::new();

        if req.first_name.is_some() {
            param_index += 1;
            set_clauses.push(format!("first_name = ${}", param_index));
            bind_order.push("first_name");
        }
        if req.last_name.is_some() {
            param_index += 1;
            set_clauses.push(format!("last_name = ${}", param_index));
            bind_order.push("last_name");
        }
        if req.profile_picture_url.is_some() {
            param_index += 1;
            set_clauses.push(format!("profile_picture_url = ${}", param_index));
            bind_order.push("profile_picture_url");
        }
        if req.timezone.is_some() {
            param_index += 1;
            set_clauses.push(format!("timezone = ${}", param_index));
            bind_order.push("timezone");
        }
        if req.language.is_some() {
            param_index += 1;
            set_clauses.push(format!("language = ${}", param_index));
            bind_order.push("language");
        }
        if req.country_code.is_some() {
            param_index += 1;
            set_clauses.push(format!("country_code = ${}", param_index));
            bind_order.push("country_code");
        }
        if req.phone.is_some() {
            param_index += 1;
            set_clauses.push(format!("phone = ${}", param_index));
            bind_order.push("phone");
        }

        // If no fields to update, just return current profile
        if set_clauses.is_empty() {
            return self.get_profile(user_id).await;
        }

        // Always update updated_at
        set_clauses.push("updated_at = NOW()".to_string());

        let sql = format!(
            "UPDATE users SET {} WHERE id = $1 RETURNING id, email, first_name, last_name, profile_picture_url, timezone, language, country_code, phone, role, referral_code, created_at, updated_at",
            set_clauses.join(", ")
        );

        // Build and execute the query with dynamic bindings
        let mut query = sqlx::query_as::<_, ProfileResponse>(&sql).bind(user_id);

        for field in &bind_order {
            match *field {
                "first_name" => query = query.bind(params.first_name.as_deref()),
                "last_name" => query = query.bind(params.last_name.as_deref()),
                "profile_picture_url" => query = query.bind(params.profile_picture_url.as_deref()),
                "timezone" => query = query.bind(params.timezone.as_deref()),
                "language" => query = query.bind(params.language.as_deref()),
                "country_code" => query = query.bind(params.country_code.as_deref()),
                "phone" => query = query.bind(params.phone.as_deref()),
                _ => {}
            }
        }

        let profile = query
            .fetch_one(&self.pool)
            .await
            .map_err(|_| AppError::InternalError)?;

        Ok(profile)
    }

    /// Fetch the current profile for a user.
    pub async fn get_profile(&self, user_id: Uuid) -> Result<ProfileResponse, AppError> {
        let profile = sqlx::query_as::<_, ProfileResponse>(
            r#"
            SELECT id, email, first_name, last_name, profile_picture_url, timezone, language, country_code, phone, role, referral_code, created_at, updated_at
            FROM users
            WHERE id = $1 AND deleted_at IS NULL
            "#,
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?
        .ok_or(AppError::UserNotFound)?;

        Ok(profile)
    }

    // ─── Email Change ───────────────────────────────────────────────────────────

    /// Initiate an email change by generating a verification token.
    ///
    /// Steps:
    /// 1. Generate a crypto-secure random token
    /// 2. Store token_hash + new_email in email_change_requests (TTL: 24h)
    /// 3. Send verification email to new address (placeholder)
    ///
    /// Returns the raw token (for the email link).
    pub async fn initiate_email_change(
        &self,
        user_id: Uuid,
        req: ChangeEmailRequest,
    ) -> Result<(), AppError> {
        // Check if new email is already in use
        let email_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM users WHERE email = $1 AND deleted_at IS NULL)",
        )
        .bind(&req.new_email)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        if email_exists {
            return Err(AppError::EmailAlreadyRegistered);
        }

        // Generate verification token
        let raw_token = generate_refresh_token(); // reuse crypto-secure random generator
        let token_hash = hash_token(&raw_token);
        let expires_at = Utc::now() + Duration::hours(24);

        // Store in email_change_requests
        sqlx::query(
            r#"
            INSERT INTO email_change_requests (user_id, new_email, token_hash, expires_at)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(user_id)
        .bind(&req.new_email)
        .bind(&token_hash)
        .bind(expires_at)
        .execute(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        // TODO: Send verification email to new address
        // For now, log the token so it can be used during development
        tracing::info!(
            user_id = %user_id,
            new_email = %req.new_email,
            token = %raw_token,
            "Email change verification token generated. Send verification email to new address."
        );

        Ok(())
    }

    /// Verify an email change token and update the user's email.
    ///
    /// Steps:
    /// 1. Hash the presented token and look it up in email_change_requests
    /// 2. Validate: not used, not expired
    /// 3. Update the user's email
    /// 4. Mark the token as used
    /// 5. Send notification to the old email (placeholder)
    pub async fn verify_email_change(
        &self,
        req: VerifyEmailChangeRequest,
    ) -> Result<(), AppError> {
        let token_hash = hash_token(&req.token);

        // Look up the email change request
        let row = sqlx::query_as::<_, EmailChangeRow>(
            r#"
            SELECT id, user_id, new_email, expires_at, used
            FROM email_change_requests
            WHERE token_hash = $1
            "#,
        )
        .bind(&token_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        let row = match row {
            Some(r) => r,
            None => return Err(AppError::InvalidResetToken),
        };

        // Check if already used
        if row.used {
            return Err(AppError::InvalidResetToken);
        }

        // Check if expired
        if row.expires_at < Utc::now() {
            return Err(AppError::InvalidResetToken);
        }

        // Get old email for notification
        let old_email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
            .bind(row.user_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|_| AppError::InternalError)?;

        // Execute update in a transaction
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AppError::InternalError)?;

        // Update the user's email
        sqlx::query("UPDATE users SET email = $1, updated_at = NOW() WHERE id = $2")
            .bind(&row.new_email)
            .bind(row.user_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::InternalError)?;

        // Mark token as used
        sqlx::query("UPDATE email_change_requests SET used = true WHERE id = $1")
            .bind(row.id)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::InternalError)?;

        // Commit transaction
        tx.commit().await.map_err(|_| AppError::InternalError)?;

        // TODO: Send notification email to old address
        tracing::info!(
            user_id = %row.user_id,
            old_email = %old_email,
            new_email = %row.new_email,
            "Email changed successfully. Send notification to previous email address."
        );

        Ok(())
    }

    // ─── Password Change ────────────────────────────────────────────────────────

    /// Change the authenticated user's password.
    ///
    /// Steps:
    /// 1. Validate new password length (min 8 chars)
    /// 2. Fetch current password hash
    /// 3. Verify current password matches
    /// 4. Hash new password with Argon2id
    /// 5. Update user record
    /// 6. Revoke ALL refresh tokens for this user
    pub async fn change_password(
        &self,
        user_id: Uuid,
        req: ChangePasswordRequest,
    ) -> Result<(), AppError> {
        // 1. Validate new password length
        if req.new_password.len() < 8 {
            return Err(AppError::PasswordTooShort);
        }

        // 2. Fetch current password hash
        let current_hash: String = sqlx::query_scalar(
            "SELECT password_hash FROM users WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?
        .ok_or(AppError::UserNotFound)?;

        // 3. Verify current password
        let password_valid = verify_password(&req.current_password, &current_hash)?;
        if !password_valid {
            return Err(AppError::InvalidCurrentPassword);
        }

        // 4. Hash new password with Argon2id
        let new_hash = hash_password(&req.new_password)?;

        // 5–6. Update password and revoke tokens in a transaction
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AppError::InternalError)?;

        // 5. Update password hash
        sqlx::query("UPDATE users SET password_hash = $1, updated_at = NOW() WHERE id = $2")
            .bind(&new_hash)
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::InternalError)?;

        // 6. Revoke ALL refresh tokens for this user
        sqlx::query("UPDATE refresh_tokens SET revoked = true WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::InternalError)?;

        // Commit transaction
        tx.commit().await.map_err(|_| AppError::InternalError)?;

        tracing::info!(user_id = %user_id, "Password changed successfully. All refresh tokens revoked.");

        Ok(())
    }
}

// ─── Helper Types ────────────────────────────────────────────────────────────

/// Internal row type for email change request lookup.
#[derive(Debug, sqlx::FromRow)]
struct EmailChangeRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub new_email: String,
    pub expires_at: DateTime<Utc>,
    pub used: bool,
}

// ─── Helper Functions ───────────────────────────────────────────────────────────

/// Generate a cryptographically random 32-byte refresh token, returned as hex string.
pub(crate) fn generate_refresh_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill(&mut bytes);
    hex::encode(bytes)
}

/// Compute SHA-256 hash of a token string, returned as hex.
pub(crate) fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

/// Generate a random alphanumeric code of the given length.
pub(crate) fn generate_random_code(len: usize) -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

/// Convert ClientType enum to its database string representation.
pub(crate) fn client_type_to_str(ct: &ClientType) -> &'static str {
    match ct {
        ClientType::Native => "native",
        ClientType::Web => "web",
    }
}

/// Default permissions granted to a new user.
pub(crate) fn default_user_permissions() -> Vec<String> {
    vec![
        "snippets:read".to_string(),
        "snippets:write".to_string(),
        "sync:read".to_string(),
        "sync:write".to_string(),
    ]
}

// ─── Property-Based Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod property_tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::Arc;

    /// Helper: create an AuthService with a lazy PgPool for brute-force testing.
    /// The brute-force logic is entirely in-memory (DashMap) so no DB connection is made.
    /// We need a Tokio runtime context because PgPool's Drop impl requires one.
    fn brute_force_service() -> AuthService {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent_test_db")
            .unwrap();

        let config = Arc::new(AppConfig {
            database_url: String::new(),
            jwt_secret: "test-secret".to_string(),
            google_client_id: String::new(),
            google_client_secret: String::new(),
            github_client_id: String::new(),
            github_client_secret: String::new(),
            oauth_redirect_base_url: String::new(),
            ai_provider_url: String::new(),
            ai_provider_key: String::new(),
            billing_webhook_secret: String::new(),
            email_from_address: String::new(),
            seed_admin_email: String::new(),
            seed_admin_password: String::new(),
            email_provider: crate::config::EmailProviderType::Smtp,
            email_smtp_host: Some(String::new()),
            email_smtp_port: Some(587),
            email_smtp_user: Some(String::new()),
            email_smtp_password: Some(String::new()),
            email_api_key: None,
            email_api_url: None,
            email_from_name: String::new(),
            port: 8080,
            log_level: "info".to_string(),
            database_max_connections: 5,
            database_min_connections: 1,
            database_connect_timeout_secs: 5,
            database_idle_timeout_secs: 300,
            database_statement_timeout_secs: 30,
            cors_allowed_origins: vec![],
            trusted_proxy_cidrs: vec![],
            ws_max_connections: 100,
            ai_max_concurrent_requests: 10,
            shutdown_timeout_secs: 30,
            billing_success_url: None,
            billing_cancel_url: None,
        });

        AuthService {
            pool,
            config,
            failed_attempts: DashMap::new(),
        }
    }

    /// Strategy to generate valid email-like strings for brute-force testing.
    fn email_strategy() -> impl Strategy<Value = String> {
        "[a-z]{3,10}@[a-z]{3,8}\\.[a-z]{2,4}".prop_map(|s| s)
    }

    /// Strategy to generate a sequence of login attempt actions.
    /// `true` = failed attempt, `false` = successful attempt.
    fn attempt_sequence_strategy() -> impl Strategy<Value = Vec<bool>> {
        prop::collection::vec(any::<bool>(), 1..30)
    }

    // ─── Property 10: Brute-force lockout state machine ─────────────────────────
    //
    // **Validates: Requirements 1.50, 1.51, 1.52**
    //
    // For any email address, after 5 consecutive failed login attempts the account
    // SHALL be locked for 15 minutes (returning 429 ACCOUNT_LOCKED). A successful
    // login SHALL reset the counter to zero.

    /// We need a Tokio runtime for the PgPool (even though we never use it).
    /// This wrapper creates a runtime for proptest closures.
    fn with_runtime<F: FnOnce() -> Result<(), proptest::test_runner::TestCaseError>>(
        f: F,
    ) -> Result<(), proptest::test_runner::TestCaseError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = rt.enter();
        f()
    }

    proptest! {
        /// **Validates: Requirements 1.50, 1.51, 1.52**
        ///
        /// Property 10a: After exactly 5 consecutive failures, the account is locked.
        #[test]
        fn prop_lockout_after_five_failures(email in email_strategy()) {
            with_runtime(|| {
                let service = brute_force_service();

                // First 4 failures should NOT lock the account
                for _ in 0..4 {
                    service.record_failed_attempt(&email);
                    prop_assert!(service.check_brute_force(&email).is_ok(),
                        "Account should NOT be locked before 5 failures");
                }

                // 5th failure should trigger lockout
                service.record_failed_attempt(&email);
                let result = service.check_brute_force(&email);
                prop_assert!(result.is_err(), "Account MUST be locked after 5 failures");

                match result.unwrap_err() {
                    AppError::AccountLocked { retry_after_secs } => {
                        prop_assert!(retry_after_secs > 0 && retry_after_secs <= 900,
                            "Retry-after should be between 1 and 900 seconds, got {}", retry_after_secs);
                    }
                    other => prop_assert!(false, "Expected AccountLocked, got {:?}", other),
                }
                Ok(())
            })?;
        }

        /// **Validates: Requirements 1.50, 1.51, 1.52**
        ///
        /// Property 10b: Successful login resets the counter — no lockout after reset.
        #[test]
        fn prop_successful_login_resets_counter(email in email_strategy()) {
            with_runtime(|| {
                let service = brute_force_service();

                // Record 4 failures (one short of lockout)
                for _ in 0..4 {
                    service.record_failed_attempt(&email);
                }

                // Simulate a successful login (resets counter)
                service.reset_failed_attempts(&email);

                // Now 4 more failures should still NOT lock
                for _ in 0..4 {
                    service.record_failed_attempt(&email);
                    prop_assert!(service.check_brute_force(&email).is_ok(),
                        "Account should NOT be locked after reset + 4 failures");
                }

                // 5th failure after reset SHOULD lock
                service.record_failed_attempt(&email);
                prop_assert!(service.check_brute_force(&email).is_err(),
                    "Account MUST be locked after 5 consecutive failures post-reset");
                Ok(())
            })?;
        }

        /// **Validates: Requirements 1.50, 1.51, 1.52**
        ///
        /// Property 10c: For any sequence of attempts, the lockout state is consistent.
        /// The counter only cares about CONSECUTIVE failures — any success resets it.
        #[test]
        fn prop_lockout_state_machine_consistency(
            email in email_strategy(),
            attempts in attempt_sequence_strategy()
        ) {
            with_runtime(|| {
                let service = brute_force_service();
                let mut consecutive_failures: u32 = 0;

                for is_failure in &attempts {
                    if *is_failure {
                        service.record_failed_attempt(&email);
                        consecutive_failures += 1;
                    } else {
                        // Successful login resets counter
                        service.reset_failed_attempts(&email);
                        consecutive_failures = 0;
                    }
                }

                let result = service.check_brute_force(&email);
                if consecutive_failures >= 5 {
                    prop_assert!(result.is_err(),
                        "Account MUST be locked with {} consecutive failures", consecutive_failures);
                } else {
                    prop_assert!(result.is_ok(),
                        "Account should NOT be locked with only {} consecutive failures", consecutive_failures);
                }
                Ok(())
            })?;
        }

        /// **Validates: Requirements 1.50, 1.51, 1.52**
        ///
        /// Property 10d: During lockout, check_brute_force consistently returns error.
        /// Multiple checks during lockout all return AccountLocked.
        #[test]
        fn prop_locked_account_remains_locked(
            email in email_strategy(),
            extra_checks in 1u32..10
        ) {
            with_runtime(|| {
                let service = brute_force_service();

                // Lock the account
                for _ in 0..5 {
                    service.record_failed_attempt(&email);
                }

                // All subsequent checks should fail
                for _ in 0..extra_checks {
                    let result = service.check_brute_force(&email);
                    prop_assert!(result.is_err(),
                        "Locked account must remain locked during lockout period");
                }
                Ok(())
            })?;
        }

        /// **Validates: Requirements 1.50, 1.51, 1.52**
        ///
        /// Property 10e: Different emails have independent lockout states.
        #[test]
        fn prop_independent_email_lockout(
            email1 in email_strategy(),
            email2 in email_strategy()
        ) {
            with_runtime(|| {
                prop_assume!(email1 != email2);
                let service = brute_force_service();

                // Lock email1
                for _ in 0..5 {
                    service.record_failed_attempt(&email1);
                }
                prop_assert!(service.check_brute_force(&email1).is_err(),
                    "email1 must be locked");

                // email2 should still be accessible
                prop_assert!(service.check_brute_force(&email2).is_ok(),
                    "email2 must NOT be affected by email1's lockout");
                Ok(())
            })?;
        }
    }
}
