use axum::{
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use uuid::Uuid;

/// Unified application error type. Each variant maps to an HTTP status + error code.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    // Auth errors (401)
    #[error("Invalid credentials")]
    InvalidCredentials,
    #[error("Unauthorized")]
    Unauthorized,
    #[error("Invalid refresh token")]
    InvalidRefreshToken,
    #[error("Token reuse detected")]
    TokenReuseDetected,
    #[error("OAuth authorization denied")]
    OAuthAuthorizationDenied,
    #[error("Invalid current password")]
    InvalidCurrentPassword,
    #[error("Invalid webhook signature")]
    InvalidWebhookSignature,

    // Forbidden (403)
    #[error("Forbidden")]
    Forbidden,
    #[error("Client type not allowed")]
    ClientTypeNotAllowed,
    #[error("Account suspended")]
    AccountSuspended,
    #[error("Password reset required")]
    PasswordResetRequired,
    #[error("Not a workspace member")]
    NotAWorkspaceMember,

    // Payment required (402)
    #[error("Subscription required")]
    SubscriptionRequired,

    // Not found (404)
    #[error("User not found")]
    UserNotFound,
    #[error("Workspace not found")]
    WorkspaceNotFound,
    #[error("Subscription not found")]
    SubscriptionNotFound,
    #[error("Coupon not found")]
    CouponNotFound,
    #[error("Audit log not found")]
    AuditLogNotFound,
    #[error("Feature flag not found")]
    FeatureFlagNotFound,

    // Conflict (409)
    #[error("Email already registered")]
    EmailAlreadyRegistered,
    #[error("Trigger already exists")]
    TriggerAlreadyExists,
    #[error("Snapshot required")]
    SnapshotRequired,
    #[error("Account linking conflict")]
    AccountLinkingConflict,
    #[error("Coupon code already exists")]
    CouponCodeAlreadyExists,
    #[error("Tax rate already exists")]
    TaxRateAlreadyExists,
    #[error("Feature flag already exists")]
    FeatureFlagAlreadyExists,

    // Unprocessable (422)
    #[error("Validation error")]
    ValidationError { details: Vec<FieldError> },
    #[error("Password too short")]
    PasswordTooShort,
    #[error("Invalid reset token")]
    InvalidResetToken,
    #[error("Email verification required")]
    EmailVerificationRequired,
    #[error("Invite expired")]
    InviteExpired,
    #[error("Transfer ownership required")]
    TransferOwnershipRequired,
    #[error("Already upgraded")]
    AlreadyUpgraded,
    #[error("Seat limit reached")]
    SeatLimitReached,
    #[error("Already a member")]
    AlreadyAMember,
    #[error("Cannot remove owner")]
    CannotRemoveOwner,
    #[error("Invite usage limit reached")]
    InviteUsageLimitReached,
    #[error("Snippet limit reached")]
    SnippetLimitReached,
    #[error("Folder limit reached")]
    FolderLimitReached,
    #[error("Snippet content too long")]
    SnippetContentTooLong,
    #[error("Content soft locked")]
    ContentSoftLocked,
    #[error("Minimum billing cycle not met")]
    MinimumBillingCycleNotMet,
    #[error("Discount not found")]
    DiscountNotFound,
    #[error("Multiple discounts not allowed")]
    MultipleDiscountsNotAllowed,
    #[error("Coupon inactive")]
    CouponInactive,
    #[error("Coupon not yet valid")]
    CouponNotYetValid,
    #[error("Coupon expired")]
    CouponExpired,
    #[error("Coupon usage limit reached")]
    CouponUsageLimitReached,
    #[error("Referral code not found")]
    ReferralCodeNotFound,
    #[error("Self referral not allowed")]
    SelfReferralNotAllowed,
    #[error("Referral already used")]
    ReferralAlreadyUsed,
    #[error("Cannot act on self")]
    CannotActOnSelf,
    #[error("Cannot act on admin")]
    CannotActOnAdmin,
    #[error("Cannot demote self")]
    CannotDemoteSelf,
    #[error("Last admin cannot be removed")]
    LastAdminCannotBeRemoved,
    #[error("Max pending invites reached")]
    MaxPendingInvitesReached,
    #[error("Confirmation required")]
    ConfirmationRequired,
    #[error("Cannot delete individual workspace")]
    CannotDeleteIndividualWorkspace,
    #[error("Invalid tier")]
    InvalidTier,
    #[error("Invalid flag name")]
    InvalidFlagName,
    #[error("Batch size exceeded")]
    BatchSizeExceeded,
    #[error("Snippet content too large")]
    SnippetContentTooLarge,
    #[error("Invalid since version")]
    InvalidSinceVersion,
    #[error("Trigger too long")]
    TriggerTooLong,
    #[error("System prompt too long")]
    SystemPromptTooLong,
    #[error("Context too long")]
    ContextTooLong,
    #[error("Invalid request body")]
    InvalidRequestBody,

    // Rate limiting (429)
    #[error("Account locked")]
    AccountLocked { retry_after_secs: u64 },
    #[error("Rate limit exceeded")]
    RateLimitExceeded { retry_after_secs: u64 },
    #[error("AI quota exceeded")]
    AiQuotaExceeded,
    #[error("AI service busy")]
    AiServiceBusy,

    // Request too large (413)
    #[error("Request body too large")]
    RequestBodyTooLarge,

    // Server errors (5xx)
    #[error("AI provider unavailable")]
    AiProviderUnavailable,
    #[error("AI provider invalid response")]
    AiProviderInvalidResponse,
    #[error("Database timeout")]
    DatabaseTimeout,
    #[error("Service unavailable")]
    ServiceUnavailable,
    #[error("Internal error")]
    InternalError,

    // Malformed (400)
    #[error("Malformed request body")]
    MalformedRequestBody,
}

/// Field-level validation error detail.
#[derive(Debug, Clone, Serialize)]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

/// Standard error response body returned to clients.
#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: ErrorBody,
    pub trace_id: Uuid,
}

/// Inner error object within the response.
#[derive(Serialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Vec<FieldError>>,
}

impl AppError {
    /// Returns the HTTP status code for this error variant.
    fn status_code(&self) -> StatusCode {
        match self {
            // 400
            Self::MalformedRequestBody => StatusCode::BAD_REQUEST,

            // 401
            Self::InvalidCredentials
            | Self::Unauthorized
            | Self::InvalidRefreshToken
            | Self::TokenReuseDetected
            | Self::OAuthAuthorizationDenied
            | Self::InvalidCurrentPassword
            | Self::InvalidWebhookSignature => StatusCode::UNAUTHORIZED,

            // 402
            Self::SubscriptionRequired => StatusCode::PAYMENT_REQUIRED,

            // 403
            Self::Forbidden
            | Self::ClientTypeNotAllowed
            | Self::AccountSuspended
            | Self::PasswordResetRequired
            | Self::NotAWorkspaceMember => StatusCode::FORBIDDEN,

            // 404
            Self::UserNotFound
            | Self::WorkspaceNotFound
            | Self::SubscriptionNotFound
            | Self::CouponNotFound
            | Self::AuditLogNotFound
            | Self::FeatureFlagNotFound => StatusCode::NOT_FOUND,

            // 409
            Self::EmailAlreadyRegistered
            | Self::TriggerAlreadyExists
            | Self::SnapshotRequired
            | Self::AccountLinkingConflict
            | Self::CouponCodeAlreadyExists
            | Self::TaxRateAlreadyExists
            | Self::FeatureFlagAlreadyExists => StatusCode::CONFLICT,

            // 413
            Self::RequestBodyTooLarge => StatusCode::PAYLOAD_TOO_LARGE,

            // 422
            Self::ValidationError { .. }
            | Self::PasswordTooShort
            | Self::InvalidResetToken
            | Self::EmailVerificationRequired
            | Self::InviteExpired
            | Self::TransferOwnershipRequired
            | Self::AlreadyUpgraded
            | Self::SeatLimitReached
            | Self::AlreadyAMember
            | Self::CannotRemoveOwner
            | Self::InviteUsageLimitReached
            | Self::SnippetLimitReached
            | Self::FolderLimitReached
            | Self::SnippetContentTooLong
            | Self::ContentSoftLocked
            | Self::MinimumBillingCycleNotMet
            | Self::DiscountNotFound
            | Self::MultipleDiscountsNotAllowed
            | Self::CouponInactive
            | Self::CouponNotYetValid
            | Self::CouponExpired
            | Self::CouponUsageLimitReached
            | Self::ReferralCodeNotFound
            | Self::SelfReferralNotAllowed
            | Self::ReferralAlreadyUsed
            | Self::CannotActOnSelf
            | Self::CannotActOnAdmin
            | Self::CannotDemoteSelf
            | Self::LastAdminCannotBeRemoved
            | Self::MaxPendingInvitesReached
            | Self::ConfirmationRequired
            | Self::CannotDeleteIndividualWorkspace
            | Self::InvalidTier
            | Self::InvalidFlagName
            | Self::BatchSizeExceeded
            | Self::SnippetContentTooLarge
            | Self::InvalidSinceVersion
            | Self::TriggerTooLong
            | Self::SystemPromptTooLong
            | Self::ContextTooLong
            | Self::InvalidRequestBody => StatusCode::UNPROCESSABLE_ENTITY,

            // 429
            Self::AccountLocked { .. }
            | Self::RateLimitExceeded { .. }
            | Self::AiQuotaExceeded
            | Self::AiServiceBusy => StatusCode::TOO_MANY_REQUESTS,

            // 5xx
            Self::AiProviderUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::AiProviderInvalidResponse | Self::InternalError => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            Self::DatabaseTimeout => StatusCode::GATEWAY_TIMEOUT,
            Self::ServiceUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    /// Returns the SCREAMING_SNAKE_CASE error code string for this variant.
    fn error_code(&self) -> &'static str {
        match self {
            // 400
            Self::MalformedRequestBody => "MALFORMED_REQUEST_BODY",

            // 401
            Self::InvalidCredentials => "INVALID_CREDENTIALS",
            Self::Unauthorized => "UNAUTHORIZED",
            Self::InvalidRefreshToken => "INVALID_REFRESH_TOKEN",
            Self::TokenReuseDetected => "TOKEN_REUSE_DETECTED",
            Self::OAuthAuthorizationDenied => "OAUTH_AUTHORIZATION_DENIED",
            Self::InvalidCurrentPassword => "INVALID_CURRENT_PASSWORD",
            Self::InvalidWebhookSignature => "INVALID_WEBHOOK_SIGNATURE",

            // 403
            Self::Forbidden => "FORBIDDEN",
            Self::ClientTypeNotAllowed => "CLIENT_TYPE_NOT_ALLOWED",
            Self::AccountSuspended => "ACCOUNT_SUSPENDED",
            Self::PasswordResetRequired => "PASSWORD_RESET_REQUIRED",
            Self::NotAWorkspaceMember => "NOT_A_WORKSPACE_MEMBER",

            // 402
            Self::SubscriptionRequired => "SUBSCRIPTION_REQUIRED",

            // 404
            Self::UserNotFound => "USER_NOT_FOUND",
            Self::WorkspaceNotFound => "WORKSPACE_NOT_FOUND",
            Self::SubscriptionNotFound => "SUBSCRIPTION_NOT_FOUND",
            Self::CouponNotFound => "COUPON_NOT_FOUND",
            Self::AuditLogNotFound => "AUDIT_LOG_NOT_FOUND",
            Self::FeatureFlagNotFound => "FEATURE_FLAG_NOT_FOUND",

            // 409
            Self::EmailAlreadyRegistered => "EMAIL_ALREADY_REGISTERED",
            Self::TriggerAlreadyExists => "TRIGGER_ALREADY_EXISTS",
            Self::SnapshotRequired => "SNAPSHOT_REQUIRED",
            Self::AccountLinkingConflict => "ACCOUNT_LINKING_CONFLICT",
            Self::CouponCodeAlreadyExists => "COUPON_CODE_ALREADY_EXISTS",
            Self::TaxRateAlreadyExists => "TAX_RATE_ALREADY_EXISTS",
            Self::FeatureFlagAlreadyExists => "FEATURE_FLAG_ALREADY_EXISTS",

            // 422
            Self::ValidationError { .. } => "VALIDATION_ERROR",
            Self::PasswordTooShort => "PASSWORD_TOO_SHORT",
            Self::InvalidResetToken => "INVALID_RESET_TOKEN",
            Self::EmailVerificationRequired => "EMAIL_VERIFICATION_REQUIRED",
            Self::InviteExpired => "INVITE_EXPIRED",
            Self::TransferOwnershipRequired => "TRANSFER_OWNERSHIP_REQUIRED",
            Self::AlreadyUpgraded => "ALREADY_UPGRADED",
            Self::SeatLimitReached => "SEAT_LIMIT_REACHED",
            Self::AlreadyAMember => "ALREADY_A_MEMBER",
            Self::CannotRemoveOwner => "CANNOT_REMOVE_OWNER",
            Self::InviteUsageLimitReached => "INVITE_USAGE_LIMIT_REACHED",
            Self::SnippetLimitReached => "SNIPPET_LIMIT_REACHED",
            Self::FolderLimitReached => "FOLDER_LIMIT_REACHED",
            Self::SnippetContentTooLong => "SNIPPET_CONTENT_TOO_LONG",
            Self::ContentSoftLocked => "CONTENT_SOFT_LOCKED",
            Self::MinimumBillingCycleNotMet => "MINIMUM_BILLING_CYCLE_NOT_MET",
            Self::DiscountNotFound => "DISCOUNT_NOT_FOUND",
            Self::MultipleDiscountsNotAllowed => "MULTIPLE_DISCOUNTS_NOT_ALLOWED",
            Self::CouponInactive => "COUPON_INACTIVE",
            Self::CouponNotYetValid => "COUPON_NOT_YET_VALID",
            Self::CouponExpired => "COUPON_EXPIRED",
            Self::CouponUsageLimitReached => "COUPON_USAGE_LIMIT_REACHED",
            Self::ReferralCodeNotFound => "REFERRAL_CODE_NOT_FOUND",
            Self::SelfReferralNotAllowed => "SELF_REFERRAL_NOT_ALLOWED",
            Self::ReferralAlreadyUsed => "REFERRAL_ALREADY_USED",
            Self::CannotActOnSelf => "CANNOT_ACT_ON_SELF",
            Self::CannotActOnAdmin => "CANNOT_ACT_ON_ADMIN",
            Self::CannotDemoteSelf => "CANNOT_DEMOTE_SELF",
            Self::LastAdminCannotBeRemoved => "LAST_ADMIN_CANNOT_BE_REMOVED",
            Self::MaxPendingInvitesReached => "MAX_PENDING_INVITES_REACHED",
            Self::ConfirmationRequired => "CONFIRMATION_REQUIRED",
            Self::CannotDeleteIndividualWorkspace => "CANNOT_DELETE_INDIVIDUAL_WORKSPACE",
            Self::InvalidTier => "INVALID_TIER",
            Self::InvalidFlagName => "INVALID_FLAG_NAME",
            Self::BatchSizeExceeded => "BATCH_SIZE_EXCEEDED",
            Self::SnippetContentTooLarge => "SNIPPET_CONTENT_TOO_LARGE",
            Self::InvalidSinceVersion => "INVALID_SINCE_VERSION",
            Self::TriggerTooLong => "TRIGGER_TOO_LONG",
            Self::SystemPromptTooLong => "SYSTEM_PROMPT_TOO_LONG",
            Self::ContextTooLong => "CONTEXT_TOO_LONG",
            Self::InvalidRequestBody => "INVALID_REQUEST_BODY",

            // 429
            Self::AccountLocked { .. } => "ACCOUNT_LOCKED",
            Self::RateLimitExceeded { .. } => "RATE_LIMIT_EXCEEDED",
            Self::AiQuotaExceeded => "AI_QUOTA_EXCEEDED",
            Self::AiServiceBusy => "AI_SERVICE_BUSY",

            // 413
            Self::RequestBodyTooLarge => "REQUEST_BODY_TOO_LARGE",

            // 5xx
            Self::AiProviderUnavailable => "AI_PROVIDER_UNAVAILABLE",
            Self::AiProviderInvalidResponse => "AI_PROVIDER_INVALID_RESPONSE",
            Self::DatabaseTimeout => "DATABASE_TIMEOUT",
            Self::ServiceUnavailable => "SERVICE_UNAVAILABLE",
            Self::InternalError => "INTERNAL_ERROR",
        }
    }

    /// Returns the retry-after value in seconds, if applicable.
    fn retry_after(&self) -> Option<u64> {
        match self {
            Self::AccountLocked { retry_after_secs } => Some(*retry_after_secs),
            Self::RateLimitExceeded { retry_after_secs } => Some(*retry_after_secs),
            _ => None,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let code = self.error_code();
        let message = self.to_string();
        let retry_after = self.retry_after();

        // Extract details for ValidationError
        let details = match &self {
            Self::ValidationError { details } => Some(details.clone()),
            _ => None,
        };

        // Generate a trace_id (will be replaced by middleware-injected value once trace_id middleware is implemented)
        // TODO: Accept trace_id from request extensions via an extractor wrapper
        let trace_id = Uuid::new_v4();

        // Log the error at the appropriate level
        match status.as_u16() {
            500..=599 => {
                tracing::error!(
                    trace_id = %trace_id,
                    error_code = code,
                    status = status.as_u16(),
                    "Server error: {message}"
                );
            }
            429 => {
                tracing::warn!(
                    trace_id = %trace_id,
                    error_code = code,
                    status = status.as_u16(),
                    "Rate limited: {message}"
                );
            }
            _ => {
                tracing::debug!(
                    trace_id = %trace_id,
                    error_code = code,
                    status = status.as_u16(),
                    "Client error: {message}"
                );
            }
        }

        let body = ErrorResponse {
            error: ErrorBody {
                code: code.to_string(),
                message,
                details,
            },
            trace_id,
        };

        let mut response = (status, Json(body)).into_response();

        // Set Retry-After header for rate-limited responses
        if let Some(secs) = retry_after {
            response.headers_mut().insert(
                header::RETRY_AFTER,
                secs.to_string().parse().unwrap(),
            );
        }

        response
    }
}
