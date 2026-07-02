//! HTTP handlers for all auth endpoints.
//!
//! Each handler extracts the request body/params, delegates to the appropriate
//! service method, and formats the HTTP response.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, Query},
    http::StatusCode,
    response::{IntoResponse, Redirect},
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::oauth::{OAuthProvider, OAuthService};
use crate::auth::service::{
    AuthResponse, AuthService, ChangeEmailRequest, ChangePasswordRequest,
    CreateAdminInviteRequest, ForgotPasswordRequest, LoginRequest, LogoutRequest, ProfileResponse,
    RefreshRequest, RegisterRequest, RegisterViaInviteRequest, ResetPasswordRequest, SessionInfo,
    UpdateProfileRequest, VerifyEmailChangeRequest,
};
use crate::config::AppConfig;
use crate::errors::AppError;
use crate::middleware::auth_extractor::AccessTokenClaims;
use crate::models::common::ClientType;

// ─── OAuth Query Parameter DTOs ─────────────────────────────────────────────────

/// Query parameters for the OAuth authorize endpoint.
#[derive(Debug, Deserialize)]
pub struct OAuthAuthorizeParams {
    /// Client type: "native" or "web".
    pub client: String,
}

/// Query parameters for the OAuth callback endpoint.
#[derive(Debug, Deserialize)]
pub struct OAuthCallbackParams {
    /// Authorization code from the provider.
    pub code: Option<String>,
    /// Error returned by the provider (e.g., user denied access).
    pub error: Option<String>,
    /// State parameter (for CSRF protection / client type encoding).
    pub state: Option<String>,
}

// ─── Public Auth Handlers ───────────────────────────────────────────────────────

/// POST /auth/register
///
/// Register a new user account and return a token pair.
pub async fn register_handler(
    Extension(auth_service): Extension<Arc<AuthService>>,
    Json(body): Json<RegisterRequest>,
) -> Result<impl IntoResponse, AppError> {
    let response = auth_service.register(body).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// POST /auth/login
///
/// Authenticate a user and return a token pair.
pub async fn login_handler(
    Extension(auth_service): Extension<Arc<AuthService>>,
    Json(body): Json<LoginRequest>,
) -> Result<Json<AuthResponse>, AppError> {
    let response = auth_service.login(body).await?;
    Ok(Json(response))
}

/// POST /auth/refresh
///
/// Refresh an access/refresh token pair using token rotation.
pub async fn refresh_handler(
    Extension(auth_service): Extension<Arc<AuthService>>,
    Json(body): Json<RefreshRequest>,
) -> Result<Json<AuthResponse>, AppError> {
    let response = auth_service.refresh_token(body).await?;
    Ok(Json(response))
}

/// POST /auth/logout
///
/// Invalidate a refresh token (logout).
pub async fn logout_handler(
    Extension(auth_service): Extension<Arc<AuthService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Json(body): Json<LogoutRequest>,
) -> Result<StatusCode, AppError> {
    let _ = claims; // Ensures user is authenticated
    auth_service.logout(body).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// POST /auth/forgot-password
///
/// Initiate a password reset flow. Always returns 200 to prevent email enumeration.
pub async fn forgot_password_handler(
    Extension(auth_service): Extension<Arc<AuthService>>,
    Json(body): Json<ForgotPasswordRequest>,
) -> Result<StatusCode, AppError> {
    auth_service.forgot_password(body).await?;
    Ok(StatusCode::OK)
}

/// POST /auth/reset-password
///
/// Reset a user's password using a valid reset token.
pub async fn reset_password_handler(
    Extension(auth_service): Extension<Arc<AuthService>>,
    Json(body): Json<ResetPasswordRequest>,
) -> Result<StatusCode, AppError> {
    auth_service.reset_password(body).await?;
    Ok(StatusCode::OK)
}

// ─── Authenticated Auth Handlers ────────────────────────────────────────────────

/// PATCH /auth/profile
///
/// Update the authenticated user's profile fields.
pub async fn update_profile_handler(
    Extension(auth_service): Extension<Arc<AuthService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Json(body): Json<UpdateProfileRequest>,
) -> Result<Json<ProfileResponse>, AppError> {
    let response = auth_service.update_profile(claims.sub, body).await?;
    Ok(Json(response))
}

/// GET /auth/profile
///
/// Return the authenticated user's full profile.
pub async fn get_profile_handler(
    Extension(auth_service): Extension<Arc<AuthService>>,
    Extension(claims): Extension<AccessTokenClaims>,
) -> Result<Json<ProfileResponse>, AppError> {
    let response = auth_service.get_profile(claims.sub).await?;
    Ok(Json(response))
}

/// POST /auth/change-email
///
/// Initiate an email change by sending a verification link to the new address.
pub async fn change_email_handler(
    Extension(auth_service): Extension<Arc<AuthService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Json(body): Json<ChangeEmailRequest>,
) -> Result<StatusCode, AppError> {
    auth_service.initiate_email_change(claims.sub, body).await?;
    Ok(StatusCode::OK)
}

/// GET /auth/verify-email-change?token=...
///
/// Verify an email change token and update the user's email.
pub async fn verify_email_change_handler(
    Extension(auth_service): Extension<Arc<AuthService>>,
    Query(params): Query<VerifyEmailChangeRequest>,
) -> Result<StatusCode, AppError> {
    auth_service.verify_email_change(params).await?;
    Ok(StatusCode::OK)
}

/// POST /auth/change-password
///
/// Change the authenticated user's password.
pub async fn change_password_handler(
    Extension(auth_service): Extension<Arc<AuthService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Json(body): Json<ChangePasswordRequest>,
) -> Result<StatusCode, AppError> {
    auth_service.change_password(claims.sub, body).await?;
    Ok(StatusCode::OK)
}

/// DELETE /auth/account
///
/// Soft-delete the authenticated user's account.
pub async fn delete_account_handler(
    Extension(auth_service): Extension<Arc<AuthService>>,
    Extension(claims): Extension<AccessTokenClaims>,
) -> Result<StatusCode, AppError> {
    auth_service.delete_account(claims.sub).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ─── Session Management Handlers ────────────────────────────────────────────────

/// GET /auth/sessions
///
/// List all active sessions for the authenticated user.
pub async fn list_sessions_handler(
    Extension(auth_service): Extension<Arc<AuthService>>,
    Extension(claims): Extension<AccessTokenClaims>,
) -> Result<Json<Vec<SessionInfo>>, AppError> {
    let sessions = auth_service.list_sessions(claims.sub).await?;
    Ok(Json(sessions))
}

/// DELETE /auth/sessions/:session_id
///
/// Revoke a specific session for the authenticated user.
pub async fn revoke_session_handler(
    Extension(auth_service): Extension<Arc<AuthService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Path(session_id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    auth_service.revoke_session(claims.sub, session_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ─── OAuth Handlers ─────────────────────────────────────────────────────────────

/// GET /auth/oauth/:provider/authorize?client=native|web
///
/// Redirect the user to the OAuth provider's authorization page.
pub async fn oauth_authorize_handler(
    Extension(oauth_service): Extension<Arc<OAuthService>>,
    Path(provider): Path<String>,
    Query(params): Query<OAuthAuthorizeParams>,
) -> Result<Redirect, AppError> {
    let provider = OAuthProvider::from_str(&provider).ok_or(AppError::InvalidRequestBody)?;

    let client_type = parse_client_type(&params.client)?;

    let url = oauth_service.oauth_authorize(provider, &client_type);
    Ok(Redirect::temporary(&url))
}

/// GET /auth/oauth/:provider/callback?code=...&error=...&state=...
///
/// Handle the OAuth callback from the provider. Exchanges the authorization code
/// for tokens, upserts the user, and redirects to the frontend callback page
/// with tokens in URL params (for web clients) or returns JSON (for native clients).
pub async fn oauth_callback_handler(
    Extension(oauth_service): Extension<Arc<OAuthService>>,
    Extension(config): Extension<Arc<AppConfig>>,
    Path(provider): Path<String>,
    Query(params): Query<OAuthCallbackParams>,
) -> Result<impl IntoResponse, AppError> {
    tracing::info!(
        provider = %provider,
        has_code = params.code.is_some(),
        has_error = params.error.is_some(),
        "OAuth callback received"
    );

    let provider = OAuthProvider::from_str(&provider).ok_or(AppError::InvalidRequestBody)?;

    // Determine client type from state parameter or default to web
    let client_type = match params.state.as_deref() {
        Some("native") => ClientType::Native,
        _ => ClientType::Web,
    };

    let code = params.code.as_deref().unwrap_or("");
    let frontend_url = &config.frontend_url;

    // Handle error from provider — redirect to frontend with error param
    if let Some(ref error) = params.error {
        if client_type == ClientType::Web {
            let redirect_url = format!(
                "{}/callback?error={}",
                frontend_url,
                urlencoding::encode(error)
            );
            return Ok(Redirect::temporary(&redirect_url).into_response());
        }
    }

    let response = oauth_service
        .oauth_callback(
            provider,
            code,
            &client_type,
            params.error.as_deref(),
            None, // No authenticated user for public callback
        )
        .await;

    match (response, &client_type) {
        (Ok(auth_response), ClientType::Web) => {
            // Redirect to frontend callback page with tokens in URL params
            tracing::info!(email = %auth_response.user.email, "OAuth success, redirecting to frontend");
            let user_json = serde_json::to_string(&auth_response.user)
                .unwrap_or_default();
            let redirect_url = format!(
                "{}/callback?access_token={}&refresh_token={}&user={}",
                frontend_url,
                urlencoding::encode(&auth_response.access_token),
                urlencoding::encode(&auth_response.refresh_token),
                urlencoding::encode(&user_json),
            );
            Ok(Redirect::temporary(&redirect_url).into_response())
        }
        (Ok(auth_response), ClientType::Native) => {
            // Native clients get JSON response
            Ok(Json(auth_response).into_response())
        }
        (Err(ref e), ClientType::Web) => {
            // Redirect to frontend with error
            tracing::error!(error = ?e, "OAuth callback failed, redirecting with error");
            let error_code = match &e {
                AppError::OAuthAuthorizationDenied => "OAUTH_AUTHORIZATION_DENIED",
                AppError::AccountLinkingConflict => "ACCOUNT_LINKING_CONFLICT",
                _ => "OAUTH_ERROR",
            };
            let redirect_url = format!(
                "{}/callback?error={}",
                frontend_url,
                urlencoding::encode(error_code)
            );
            Ok(Redirect::temporary(&redirect_url).into_response())
        }
        (Err(e), _) => Err(e),
    }
}

// ─── Admin Invite Handlers ──────────────────────────────────────────────────────

/// POST /admin/invites
///
/// Create an admin invite (admin-only).
pub async fn create_admin_invite_handler(
    Extension(auth_service): Extension<Arc<AuthService>>,
    Extension(claims): Extension<AccessTokenClaims>,
    Json(body): Json<CreateAdminInviteRequest>,
) -> Result<impl IntoResponse, AppError> {
    let response = auth_service
        .create_admin_invite(body.email, claims.sub)
        .await?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// POST /auth/register-invite
///
/// Register a new admin user via a valid invite token.
pub async fn register_via_invite_handler(
    Extension(auth_service): Extension<Arc<AuthService>>,
    Json(body): Json<RegisterViaInviteRequest>,
) -> Result<impl IntoResponse, AppError> {
    let response = auth_service.register_via_invite(body).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

// ─── Helpers ────────────────────────────────────────────────────────────────────

/// Parse a client type string ("native" or "web") into a `ClientType` enum.
fn parse_client_type(s: &str) -> Result<ClientType, AppError> {
    match s.to_lowercase().as_str() {
        "native" => Ok(ClientType::Native),
        "web" => Ok(ClientType::Web),
        _ => Err(AppError::InvalidRequestBody),
    }
}
