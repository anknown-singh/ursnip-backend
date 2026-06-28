use std::sync::Arc;

use chrono::{Duration, Utc};
use reqwest::Client;
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::jwt::encode_access_token;
use crate::auth::service::{
    client_type_to_str, default_user_permissions, generate_random_code, generate_refresh_token,
    hash_token, AuthResponse, UserInfo,
};
use crate::config::AppConfig;
use crate::errors::AppError;
use crate::middleware::auth_extractor::AccessTokenClaims;
use crate::models::common::{ClientType, Role, Tier};

// ─── Supported OAuth Providers ──────────────────────────────────────────────────

/// Supported OAuth provider identifiers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OAuthProvider {
    Google,
    GitHub,
}

impl OAuthProvider {
    /// Parse provider from path segment string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "google" => Some(Self::Google),
            "github" => Some(Self::GitHub),
            _ => None,
        }
    }

    /// Return the database string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::GitHub => "github",
        }
    }
}

// ─── OAuth Provider Response DTOs ───────────────────────────────────────────────

/// Google token endpoint response.
#[derive(Debug, Deserialize)]
struct GoogleTokenResponse {
    access_token: String,
}

/// Google userinfo endpoint response.
#[derive(Debug, Deserialize)]
struct GoogleUserInfo {
    #[serde(rename = "sub")]
    id: String,
    email: Option<String>,
    email_verified: Option<bool>,
}

/// GitHub token endpoint response.
#[derive(Debug, Deserialize)]
struct GitHubTokenResponse {
    access_token: String,
}

/// GitHub user API response.
#[derive(Debug, Deserialize)]
struct GitHubUser {
    id: i64,
}

/// GitHub email entry from the emails API.
#[derive(Debug, Deserialize)]
struct GitHubEmail {
    email: String,
    verified: bool,
    primary: bool,
}

// ─── Internal row types ─────────────────────────────────────────────────────────

/// Row returned when looking up an existing user by email.
#[derive(Debug, sqlx::FromRow)]
struct ExistingUserRow {
    pub id: Uuid,
    pub email: String,
    pub role: Role,
    pub referral_code: String,
}

// ─── OAuth Service ──────────────────────────────────────────────────────────────

/// OAuth service handling authorization URL generation and callback processing.
pub struct OAuthService {
    pool: PgPool,
    config: Arc<AppConfig>,
    http_client: Client,
}

impl OAuthService {
    /// Create a new OAuthService instance.
    pub fn new(pool: PgPool, config: Arc<AppConfig>) -> Self {
        let http_client = Client::new();
        Self {
            pool,
            config,
            http_client,
        }
    }

    // ─── Authorize ──────────────────────────────────────────────────────────────

    /// Build the OAuth authorization URL for the given provider and client type.
    ///
    /// - `client_type = Native` → redirect_uri uses deep-link (`ursnip://oauth/callback`)
    /// - `client_type = Web` → redirect_uri uses the configured web callback URL
    ///
    /// Returns the full authorization URL as a string (the handler sends the redirect).
    pub fn oauth_authorize(
        &self,
        provider: OAuthProvider,
        client_type: &ClientType,
    ) -> String {
        let redirect_uri = self.build_redirect_uri(provider, client_type);

        match provider {
            OAuthProvider::Google => {
                format!(
                    "https://accounts.google.com/o/oauth2/v2/auth?\
                     client_id={}&\
                     redirect_uri={}&\
                     response_type=code&\
                     scope=openid%20email%20profile&\
                     access_type=offline",
                    urlencoded(&self.config.google_client_id),
                    urlencoded(&redirect_uri),
                )
            }
            OAuthProvider::GitHub => {
                format!(
                    "https://github.com/login/oauth/authorize?\
                     client_id={}&\
                     redirect_uri={}&\
                     scope=user:email",
                    urlencoded(&self.config.github_client_id),
                    urlencoded(&redirect_uri),
                )
            }
        }
    }

    // ─── Callback ───────────────────────────────────────────────────────────────

    /// Process the OAuth callback: exchange code for token, retrieve verified email,
    /// upsert user, link OAuth identity, and issue a token pair.
    ///
    /// Handles the following error cases:
    /// - Provider returned an `error` parameter → 401 OAUTH_AUTHORIZATION_DENIED
    /// - Provider does not return a verified email → 422 EMAIL_VERIFICATION_REQUIRED
    /// - Account linking conflict → 409 ACCOUNT_LINKING_CONFLICT
    pub async fn oauth_callback(
        &self,
        provider: OAuthProvider,
        code: &str,
        client_type: &ClientType,
        error_param: Option<&str>,
        authenticated_user_id: Option<Uuid>,
    ) -> Result<AuthResponse, AppError> {
        // Handle provider error parameter (Req 1.20)
        if error_param.is_some() {
            return Err(AppError::OAuthAuthorizationDenied);
        }

        // Exchange code for access token and retrieve user info
        let (external_id, verified_email) = match provider {
            OAuthProvider::Google => self.exchange_google(code, client_type).await?,
            OAuthProvider::GitHub => self.exchange_github(code, client_type).await?,
        };

        // Must have a verified email (Req 1.23)
        let email = verified_email.ok_or(AppError::EmailVerificationRequired)?;

        // Upsert user and link OAuth identity within a transaction
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AppError::InternalError)?;

        // Check if this OAuth identity is already linked
        let existing_oauth_user_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT user_id FROM oauth_accounts WHERE provider = $1 AND external_id = $2",
        )
        .bind(provider.as_str())
        .bind(&external_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| AppError::InternalError)?;

        let user_id: Uuid;
        let user_email: String;
        let user_role: Role;
        let user_referral_code: String;

        if let Some(linked_user_id) = existing_oauth_user_id {
            // OAuth identity already linked — check for conflict (Req 1.24)
            if let Some(auth_uid) = authenticated_user_id {
                if auth_uid != linked_user_id {
                    return Err(AppError::AccountLinkingConflict);
                }
            }

            // Fetch user info
            let user_row = sqlx::query_as::<_, ExistingUserRow>(
                "SELECT id, email, role, referral_code FROM users WHERE id = $1 AND deleted_at IS NULL",
            )
            .bind(linked_user_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| AppError::InternalError)?;

            user_id = user_row.id;
            user_email = user_row.email;
            user_role = user_row.role;
            user_referral_code = user_row.referral_code;
        } else {
            // OAuth identity not yet linked — check if email matches an existing account
            let existing_user = sqlx::query_as::<_, ExistingUserRow>(
                "SELECT id, email, role, referral_code FROM users WHERE email = $1 AND deleted_at IS NULL",
            )
            .bind(&email)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| AppError::InternalError)?;

            if let Some(existing) = existing_user {
                // Auto-merge: link OAuth identity to existing account (Req 1.21, 1.22)
                // Check for conflict if user is authenticated with a different account
                if let Some(auth_uid) = authenticated_user_id {
                    if auth_uid != existing.id {
                        return Err(AppError::AccountLinkingConflict);
                    }
                }

                // Link OAuth identity to existing user
                sqlx::query(
                    "INSERT INTO oauth_accounts (user_id, provider, external_id) VALUES ($1, $2, $3)",
                )
                .bind(existing.id)
                .bind(provider.as_str())
                .bind(&external_id)
                .execute(&mut *tx)
                .await
                .map_err(|_| AppError::InternalError)?;

                user_id = existing.id;
                user_email = existing.email;
                user_role = existing.role;
                user_referral_code = existing.referral_code;
            } else {
                // New user — create account (same as register flow)
                let referral_code = self.generate_unique_referral_code_tx(&mut tx).await?;

                let new_user_id: Uuid = sqlx::query_scalar(
                    r#"
                    INSERT INTO users (email, password_hash, role, status, referral_code)
                    VALUES ($1, '', 'user', 'active', $2)
                    RETURNING id
                    "#,
                )
                .bind(&email)
                .bind(&referral_code)
                .fetch_one(&mut *tx)
                .await
                .map_err(|_| AppError::InternalError)?;

                // Link OAuth identity
                sqlx::query(
                    "INSERT INTO oauth_accounts (user_id, provider, external_id) VALUES ($1, $2, $3)",
                )
                .bind(new_user_id)
                .bind(provider.as_str())
                .bind(&external_id)
                .execute(&mut *tx)
                .await
                .map_err(|_| AppError::InternalError)?;

                // Create individual workspace
                let workspace_name = format!("{}'s Workspace", email);
                let workspace_id: Uuid = sqlx::query_scalar(
                    r#"
                    INSERT INTO workspaces (type, owner_id, name)
                    VALUES ('individual', $1, $2)
                    RETURNING id
                    "#,
                )
                .bind(new_user_id)
                .bind(&workspace_name)
                .fetch_one(&mut *tx)
                .await
                .map_err(|_| AppError::InternalError)?;

                // Add workspace member with owner role
                sqlx::query(
                    r#"
                    INSERT INTO workspace_members (workspace_id, user_id, role)
                    VALUES ($1, $2, 'owner')
                    "#,
                )
                .bind(workspace_id)
                .bind(new_user_id)
                .execute(&mut *tx)
                .await
                .map_err(|_| AppError::InternalError)?;

                // Create free subscription
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

                user_id = new_user_id;
                user_email = email;
                user_role = Role::User;
                user_referral_code = referral_code;
            }
        }

        // Generate refresh token
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
        .bind(client_type_to_str(client_type))
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
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AppError::InternalError)?;

        let tier = match subscription_tier.as_deref() {
            Some("pro") => Tier::Pro,
            Some("teams") => Tier::Teams,
            _ => Tier::Free,
        };

        // Generate access token JWT
        let claims = AccessTokenClaims {
            sub: user_id,
            client_type: client_type.clone(),
            role: user_role.clone(),
            permissions: default_user_permissions(),
            subscription_tier: tier,
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
                email: user_email,
                role: user_role,
                referral_code: user_referral_code,
            },
        })
    }

    // ─── Google Exchange ────────────────────────────────────────────────────────

    /// Exchange a Google authorization code for an access token and retrieve user info.
    /// Returns (external_id, Option<verified_email>).
    async fn exchange_google(
        &self,
        code: &str,
        client_type: &ClientType,
    ) -> Result<(String, Option<String>), AppError> {
        let redirect_uri = self.build_redirect_uri(OAuthProvider::Google, client_type);

        // Exchange code for token
        let token_resp = self
            .http_client
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("code", code),
                ("client_id", &self.config.google_client_id),
                ("client_secret", &self.config.google_client_secret),
                ("redirect_uri", &redirect_uri),
                ("grant_type", "authorization_code"),
            ])
            .send()
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "Google token exchange failed");
                AppError::InternalError
            })?;

        if !token_resp.status().is_success() {
            tracing::error!(
                status = %token_resp.status(),
                "Google token endpoint returned non-success status"
            );
            return Err(AppError::InternalError);
        }

        let token_data: GoogleTokenResponse = token_resp.json().await.map_err(|e| {
            tracing::error!(error = %e, "Failed to parse Google token response");
            AppError::InternalError
        })?;

        // Fetch user info
        let userinfo_resp = self
            .http_client
            .get("https://www.googleapis.com/oauth2/v3/userinfo")
            .bearer_auth(&token_data.access_token)
            .send()
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "Google userinfo request failed");
                AppError::InternalError
            })?;

        if !userinfo_resp.status().is_success() {
            tracing::error!(
                status = %userinfo_resp.status(),
                "Google userinfo endpoint returned non-success status"
            );
            return Err(AppError::InternalError);
        }

        let userinfo: GoogleUserInfo = userinfo_resp.json().await.map_err(|e| {
            tracing::error!(error = %e, "Failed to parse Google userinfo response");
            AppError::InternalError
        })?;

        // Only return email if it's verified
        let verified_email = match (userinfo.email, userinfo.email_verified) {
            (Some(email), Some(true)) => Some(email),
            _ => None,
        };

        Ok((userinfo.id, verified_email))
    }

    // ─── GitHub Exchange ────────────────────────────────────────────────────────

    /// Exchange a GitHub authorization code for an access token and retrieve user info.
    /// Returns (external_id, Option<verified_email>).
    async fn exchange_github(
        &self,
        code: &str,
        client_type: &ClientType,
    ) -> Result<(String, Option<String>), AppError> {
        let redirect_uri = self.build_redirect_uri(OAuthProvider::GitHub, client_type);

        // Exchange code for token
        let token_resp = self
            .http_client
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .form(&[
                ("code", code),
                ("client_id", &self.config.github_client_id),
                ("client_secret", &self.config.github_client_secret),
                ("redirect_uri", &redirect_uri),
            ])
            .send()
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "GitHub token exchange failed");
                AppError::InternalError
            })?;

        if !token_resp.status().is_success() {
            tracing::error!(
                status = %token_resp.status(),
                "GitHub token endpoint returned non-success status"
            );
            return Err(AppError::InternalError);
        }

        let token_data: GitHubTokenResponse = token_resp.json().await.map_err(|e| {
            tracing::error!(error = %e, "Failed to parse GitHub token response");
            AppError::InternalError
        })?;

        // Fetch user ID
        let user_resp = self
            .http_client
            .get("https://api.github.com/user")
            .bearer_auth(&token_data.access_token)
            .header("User-Agent", "ursnip-backend")
            .send()
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "GitHub user API request failed");
                AppError::InternalError
            })?;

        if !user_resp.status().is_success() {
            tracing::error!(
                status = %user_resp.status(),
                "GitHub user API returned non-success status"
            );
            return Err(AppError::InternalError);
        }

        let github_user: GitHubUser = user_resp.json().await.map_err(|e| {
            tracing::error!(error = %e, "Failed to parse GitHub user response");
            AppError::InternalError
        })?;

        // Fetch verified primary email from emails API
        let emails_resp = self
            .http_client
            .get("https://api.github.com/user/emails")
            .bearer_auth(&token_data.access_token)
            .header("User-Agent", "ursnip-backend")
            .send()
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "GitHub emails API request failed");
                AppError::InternalError
            })?;

        if !emails_resp.status().is_success() {
            tracing::error!(
                status = %emails_resp.status(),
                "GitHub emails API returned non-success status"
            );
            return Err(AppError::InternalError);
        }

        let emails: Vec<GitHubEmail> = emails_resp.json().await.map_err(|e| {
            tracing::error!(error = %e, "Failed to parse GitHub emails response");
            AppError::InternalError
        })?;

        // Find the primary verified email
        let verified_email = emails
            .into_iter()
            .find(|e| e.verified && e.primary)
            .map(|e| e.email);

        let external_id = github_user.id.to_string();

        Ok((external_id, verified_email))
    }

    // ─── Helpers ────────────────────────────────────────────────────────────────

    /// Build the redirect URI based on client type.
    /// - Native → `ursnip://oauth/callback`
    /// - Web → `{oauth_redirect_base_url}/auth/oauth/{provider}/callback`
    fn build_redirect_uri(&self, provider: OAuthProvider, client_type: &ClientType) -> String {
        match client_type {
            ClientType::Native => "ursnip://oauth/callback".to_string(),
            ClientType::Web => {
                format!(
                    "{}/auth/oauth/{}/callback",
                    self.config.oauth_redirect_base_url,
                    provider.as_str()
                )
            }
        }
    }

    /// Generate a unique referral code within a transaction.
    async fn generate_unique_referral_code_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<String, AppError> {
        for _ in 0..10 {
            let code = generate_random_code(8);
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM users WHERE referral_code = $1)",
            )
            .bind(&code)
            .fetch_one(&mut **tx)
            .await
            .map_err(|_| AppError::InternalError)?;

            if !exists {
                return Ok(code);
            }
        }

        Err(AppError::InternalError)
    }
}

// ─── URL Encoding Helper ────────────────────────────────────────────────────────

/// Minimal percent-encoding for URL query parameter values.
fn urlencoded(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            _ => format!("%{:02X}", c as u8),
        })
        .collect()
}
