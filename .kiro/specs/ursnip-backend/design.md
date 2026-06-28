# Design Document: ursnip-backend

## High-Level Design

### System Context

```
┌──────────────┐     ┌──────────────┐
│ Native Client│     │  Web Client  │
│(Tauri Desktop│     │  (Browser)   │
│  /Mobile)    │     │              │
└──────┬───────┘     └──────┬───────┘
       │  REST + WS          │  REST only
       │  (sync, ai)         │  (auth, checkout, teams, admin)
       └──────────┬──────────┘
                  │
          ┌───────▼────────┐
          │  Axum Backend  │
          │  (Single Port) │
          └───┬───┬───┬────┘
              │   │   │
   ┌──────────┘   │   └──────────────┐
   │              │                   │
┌──▼───┐   ┌─────▼──────┐   ┌───────▼────────┐
│Postgres│  │ AI Provider│   │Billing Provider│
│ (sqlx) │  │ (HTTP API) │   │  (Webhooks)    │
└────────┘  └────────────┘   └────────────────┘
                              ┌────────────────┐
                              │OAuth Providers │
                              │(Google/GitHub) │
                              └────────────────┘
                              ┌────────────────┐
                              │ Email Provider │
                              │ (SMTP or API)  │
                              └────────────────┘
```

### Component Responsibilities

| Component | Responsibility |
|-----------|---------------|
| **Auth Service** | Registration, login, OAuth (Google/GitHub), JWT issuance (access+refresh), token refresh/rotation, password reset, email change, profile management, account deletion, admin invites, brute-force protection, session management, security audit logging |
| **Sync Service** | Snippet/folder CRUD with workspace-scoped versioning, batch operations, delta generation, delta polling with pagination, snapshot generation, WebSocket push, heartbeat, reconnection catch-up, trigger uniqueness enforcement |
| **AI Service** | Proxy requests to AI Provider, enforce tier-based quotas (50/free, 1000/pro+teams), input validation, concurrency limiting, queue management |
| **Admin Service** | User suspend/unsuspend/force-reset/delete, workspace management, subscription overrides, coupon/discount/tax CRUD, feature flag management, audit log viewer, dashboard analytics, admin invite management |
| **Subscription Service** | Tier management (free/pro/teams), checkout flow with invoice computation, billing webhook processing, grace period enforcement, coupon validation, referral rewards, tax calculation, soft-lock on downgrade |
| **Workspace/Teams Service** | Workspace creation (individual at registration, team on demand), team invitations, member join/remove, seat limit enforcement, ownership rules |
| **Email Service** | Provider abstraction (SMTP/API), template rendering (HTML+plaintext), async dispatch, retry with exponential backoff |
| **Scheduler Service** | Recurring background tasks (delta purge, soft-delete cleanup, token cleanup, grace period check, payment deadline check, account hard-delete), idempotent execution, retry on transient failure |
| **Middleware Stack** | Trace ID, security headers, CORS, body size limit, panic recovery, IP rate limit, auth extraction, client type guard, subscription context injection, per-user rate limit, admin guard |
| **Config** | Environment variable loading, validation of required vars, defaults for optional vars, startup termination on missing secrets |
| **Migrations** | Forward-only sqlx migrations, compile-time query checking, seed data (super-admin + default feature flags) |


### Data Models (22 Tables)

All tables use UUID primary keys (except `feature_flags` which uses `name` as PK and `tax_rates` which uses `country_code` as PK). Timestamps are `TIMESTAMPTZ` (UTC).

```sql
-- 1. users
CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email TEXT NOT NULL,
    password_hash TEXT,
    first_name TEXT,
    last_name TEXT,
    profile_picture_url TEXT,
    timezone TEXT,
    language TEXT,
    country_code CHAR(2),
    phone TEXT,
    role TEXT NOT NULL DEFAULT 'user' CHECK (role IN ('user', 'admin')),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'suspended')),
    referral_code TEXT NOT NULL,
    must_reset_password BOOLEAN NOT NULL DEFAULT FALSE,
    deleted_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX idx_users_email ON users (email);
CREATE UNIQUE INDEX idx_users_referral_code ON users (referral_code);

-- 2. refresh_tokens
CREATE TABLE refresh_tokens (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL,
    client_type TEXT NOT NULL CHECK (client_type IN ('native', 'web')),
    expires_at TIMESTAMPTZ NOT NULL,
    revoked BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX idx_refresh_tokens_hash ON refresh_tokens (token_hash);
CREATE INDEX idx_refresh_tokens_user ON refresh_tokens (user_id);

-- 3. oauth_accounts
CREATE TABLE oauth_accounts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider TEXT NOT NULL CHECK (provider IN ('google', 'github')),
    external_id TEXT NOT NULL
);
CREATE UNIQUE INDEX idx_oauth_provider_external ON oauth_accounts (provider, external_id);

-- 4. password_reset_tokens
CREATE TABLE password_reset_tokens (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    used BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_password_reset_hash ON password_reset_tokens (token_hash);
CREATE INDEX idx_password_reset_user ON password_reset_tokens (user_id);

-- 5. email_change_requests
CREATE TABLE email_change_requests (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    new_email TEXT NOT NULL,
    token_hash TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    used BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 6. admin_invites
CREATE TABLE admin_invites (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email TEXT NOT NULL,
    token_hash TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    used BOOLEAN NOT NULL DEFAULT FALSE,
    created_by UUID NOT NULL REFERENCES users(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 7. audit_logs
CREATE TABLE audit_logs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    admin_id UUID,
    user_id UUID,
    action TEXT NOT NULL,
    ip_address TEXT,
    user_agent TEXT,
    client_type TEXT,
    target_resource TEXT,
    target_id TEXT,
    result TEXT CHECK (result IN ('success', 'failure')),
    metadata JSONB,
    trace_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_audit_logs_admin_date ON audit_logs (admin_id, created_at);
CREATE INDEX idx_audit_logs_target ON audit_logs (target_resource, target_id);

-- 8. workspaces
CREATE TABLE workspaces (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    type TEXT NOT NULL CHECK (type IN ('individual', 'team')),
    owner_id UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    name TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_workspaces_owner ON workspaces (owner_id);

-- 9. workspace_members
CREATE TABLE workspace_members (
    workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    role TEXT NOT NULL CHECK (role IN ('owner', 'member')),
    joined_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (workspace_id, user_id)
);
CREATE UNIQUE INDEX idx_workspace_members_unique ON workspace_members (workspace_id, user_id);
CREATE INDEX idx_workspace_members_user ON workspace_members (user_id);

-- 10. snippets
CREATE TABLE snippets (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    created_by UUID NOT NULL REFERENCES users(id),
    trigger TEXT NOT NULL,
    content TEXT NOT NULL,
    snippet_type TEXT NOT NULL,
    version BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at TIMESTAMPTZ
);
CREATE INDEX idx_snippets_workspace ON snippets (workspace_id);
CREATE UNIQUE INDEX idx_snippets_workspace_trigger
    ON snippets (workspace_id, trigger) WHERE deleted_at IS NULL;

-- 11. folders
CREATE TABLE folders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    created_by UUID NOT NULL REFERENCES users(id),
    version BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at TIMESTAMPTZ
);
CREATE INDEX idx_folders_workspace ON folders (workspace_id);

-- 12. snippet_folders
CREATE TABLE snippet_folders (
    snippet_id UUID NOT NULL REFERENCES snippets(id) ON DELETE CASCADE,
    folder_id UUID NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    PRIMARY KEY (snippet_id, folder_id)
);
CREATE UNIQUE INDEX idx_snippet_folders_unique ON snippet_folders (snippet_id, folder_id);

-- 13. sync_deltas
CREATE TABLE sync_deltas (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    entity_type TEXT NOT NULL CHECK (entity_type IN ('snippet', 'folder')),
    entity_id UUID NOT NULL,
    operation TEXT NOT NULL CHECK (operation IN ('create', 'update', 'delete')),
    payload JSONB NOT NULL,
    version BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_sync_deltas_workspace_version ON sync_deltas (workspace_id, version);

-- 14. subscriptions
CREATE TABLE subscriptions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    tier TEXT NOT NULL CHECK (tier IN ('free', 'pro', 'teams')),
    status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'past_due', 'cancelled', 'pending_payment', 'deactivated')),
    period_start TIMESTAMPTZ,
    period_end TIMESTAMPTZ,
    grace_period_end TIMESTAMPTZ,
    payment_deadline TIMESTAMPTZ,
    cancelled_at TIMESTAMPTZ,
    external_subscription_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX idx_subscriptions_workspace ON subscriptions (workspace_id);

-- 15. team_invites
CREATE TABLE team_invites (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    invite_code TEXT NOT NULL,
    max_uses INTEGER NOT NULL DEFAULT 1,
    times_used INTEGER NOT NULL DEFAULT 0,
    expires_at TIMESTAMPTZ NOT NULL,
    created_by UUID NOT NULL REFERENCES users(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX idx_team_invites_code ON team_invites (invite_code);

-- 16. discounts
CREATE TABLE discounts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    type TEXT NOT NULL CHECK (type IN ('percentage', 'flat')),
    value NUMERIC NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 17. coupon_codes
CREATE TABLE coupon_codes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code TEXT NOT NULL,
    type TEXT NOT NULL CHECK (type IN ('platform', 'referral')),
    discount_id UUID NOT NULL REFERENCES discounts(id),
    owner_id UUID REFERENCES users(id),
    max_uses INTEGER,
    times_used INTEGER NOT NULL DEFAULT 0,
    valid_from TIMESTAMPTZ NOT NULL DEFAULT now(),
    valid_until TIMESTAMPTZ,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX idx_coupon_codes_code ON coupon_codes (LOWER(code));
CREATE INDEX idx_coupon_codes_owner ON coupon_codes (owner_id);

-- 18. referrals
CREATE TABLE referrals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    referrer_id UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    referred_user_id UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'converted')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_referrals_referrer ON referrals (referrer_id);
CREATE INDEX idx_referrals_referred ON referrals (referred_user_id);
CREATE UNIQUE INDEX idx_referrals_pair ON referrals (referrer_id, referred_user_id);

-- 19. referral_credits
CREATE TABLE referral_credits (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id),
    months INTEGER NOT NULL DEFAULT 1,
    redeemed BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 20. tax_rates
CREATE TABLE tax_rates (
    country_code CHAR(2) PRIMARY KEY,
    rate NUMERIC NOT NULL,
    tax_name TEXT NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 21. billing_events
CREATE TABLE billing_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    external_event_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    workspace_id UUID REFERENCES workspaces(id),
    payload JSONB NOT NULL,
    processed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX idx_billing_events_external ON billing_events (external_event_id);

-- 22. feature_flags
CREATE TABLE feature_flags (
    name TEXT PRIMARY KEY,
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    description TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

### Complete API Surface

All endpoints return JSON. Auth-required endpoints expect `Authorization: Bearer <access_token>`. Client type restrictions noted per endpoint.

#### Auth Service (`/auth/*`) — Accepts: `native` + `web`

| Method | Path | Client | Auth | Description |
|--------|------|--------|------|-------------|
| POST | `/auth/register` | native, web | No | Register with email+password |
| POST | `/auth/login` | native, web | No | Login with email+password |
| POST | `/auth/refresh` | native, web | No (uses refresh token) | Rotate refresh token, get new pair |
| POST | `/auth/logout` | native, web | Yes | Invalidate refresh token |
| GET | `/auth/oauth/{provider}/authorize` | native, web | No | Redirect to OAuth provider |
| GET | `/auth/oauth/{provider}/callback` | native, web | No | OAuth callback handler |
| POST | `/auth/forgot-password` | native, web | No | Request password reset email |
| POST | `/auth/reset-password` | native, web | No | Reset password with token |
| PATCH | `/auth/profile` | native, web | Yes | Update profile fields |
| POST | `/auth/change-email` | native, web | Yes | Initiate email change |
| GET | `/auth/verify-email-change` | native, web | No | Verify email change token |
| POST | `/auth/change-password` | native, web | Yes | Change password |
| DELETE | `/auth/account` | native, web | Yes | Soft-delete account |
| GET | `/auth/sessions` | native, web | Yes | List active sessions |
| DELETE | `/auth/sessions/{session_id}` | native, web | Yes | Revoke specific session |

#### Sync Service (`/sync/*`) — Accepts: `native` only

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| POST | `/sync/snippets` | Yes | Create snippet |
| PATCH | `/sync/snippets/{id}` | Yes | Update snippet |
| DELETE | `/sync/snippets/{id}` | Yes | Soft-delete snippet |
| POST | `/sync/snippets/batch` | Yes | Batch snippet operations (max 100) |
| POST | `/sync/folders` | Yes | Create folder |
| PATCH | `/sync/folders/{id}` | Yes | Update folder |
| DELETE | `/sync/folders/{id}` | Yes | Soft-delete folder |
| GET | `/sync/snapshot` | Yes | Full workspace snapshot |
| GET | `/sync/deltas` | Yes | Delta polling with pagination |
| GET | `/sync/ws` | Yes | WebSocket upgrade for real-time push |

#### AI Service (`/ai/*`) — Accepts: `native` only

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| POST | `/ai/expand` | Yes | Expand snippet via AI Provider |

#### Subscription Service (`/subscriptions/*`) — Accepts: `web` only

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| POST | `/subscriptions/upgrade` | Yes | Initiate free→pro upgrade |
| POST | `/subscriptions/checkout` | Yes | Compute invoice and get checkout URL |
| GET | `/subscriptions/current` | Yes | Get current subscription details |

#### Teams Service (`/teams/*`) — Accepts: `web` only

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| POST | `/teams` | Yes | Create team workspace |
| POST | `/teams/{workspace_id}/invites` | Yes | Generate invite link |
| POST | `/teams/{workspace_id}/join` | Yes | Join via invite code |
| DELETE | `/teams/{workspace_id}/members/{user_id}` | Yes | Remove team member |
| GET | `/teams/{workspace_id}` | Yes | Get team workspace details |
| GET | `/teams/{workspace_id}/members` | Yes | List team members |

#### Admin Service (`/admin/*`) — Accepts: `web` only, requires `role = admin`

| Method | Path | Description |
|--------|------|-------------|
| GET | `/admin/users` | List users (paginated, filterable) |
| GET | `/admin/users/{user_id}` | User detail with subscriptions/referrals |
| POST | `/admin/users/{user_id}/suspend` | Suspend user |
| POST | `/admin/users/{user_id}/unsuspend` | Unsuspend user |
| POST | `/admin/users/{user_id}/force-password-reset` | Force password reset |
| DELETE | `/admin/users/{user_id}` | Admin-initiated soft-delete |
| GET | `/admin/workspaces` | List workspaces (paginated, filterable) |
| GET | `/admin/workspaces/{workspace_id}` | Workspace detail |
| POST | `/admin/workspaces/{workspace_id}/deactivate` | Deactivate workspace |
| DELETE | `/admin/workspaces/{workspace_id}` | Hard-delete workspace (requires confirm=true) |
| GET | `/admin/discounts` | List discounts |
| POST | `/admin/discounts` | Create discount |
| PATCH | `/admin/discounts/{id}` | Update discount |
| GET | `/admin/coupons` | List coupons (paginated, filterable) |
| GET | `/admin/coupons/{id}` | Coupon detail |
| POST | `/admin/coupons` | Create platform coupon |
| PATCH | `/admin/coupons/{id}` | Update coupon |
| GET | `/admin/referrals` | Referral statistics |
| GET | `/admin/subscriptions` | List subscriptions (paginated, filterable) |
| GET | `/admin/subscriptions/{workspace_id}` | Subscription detail with billing history |
| POST | `/admin/subscriptions/{workspace_id}/extend` | Extend subscription period |
| POST | `/admin/subscriptions/{workspace_id}/cancel` | Force-cancel subscription |
| PATCH | `/admin/subscriptions/{workspace_id}/tier` | Override subscription tier |
| GET | `/admin/billing-events` | List billing webhook events |
| GET | `/admin/tax-rates` | List tax rates |
| POST | `/admin/tax-rates` | Create tax rate |
| PATCH | `/admin/tax-rates/{country_code}` | Update tax rate |
| GET | `/admin/audit-logs` | List audit logs (paginated, filterable) |
| GET | `/admin/audit-logs/{id}` | Audit log detail |
| GET | `/admin/feature-flags` | List feature flags |
| POST | `/admin/feature-flags` | Create feature flag |
| PUT | `/admin/feature-flags/{flag_name}` | Update feature flag |
| DELETE | `/admin/feature-flags/{flag_name}` | Delete feature flag |
| GET | `/admin/admins` | List admin accounts |
| DELETE | `/admin/admins/{admin_id}` | Demote admin to user |
| POST | `/admin/invites` | Send admin invite |
| GET | `/admin/stats/overview` | Dashboard analytics |
| GET | `/admin/stats/referrals` | Referral analytics |

#### Webhooks (`/webhooks/*`) — No JWT, signature verification

| Method | Path | Description |
|--------|------|-------------|
| POST | `/webhooks/billing` | Billing provider webhook receiver |

#### Health (`/health`, `/ready`) — No authentication

| Method | Path | Description |
|--------|------|-------------|
| GET | `/health` | Liveness check (DB reachability) |
| GET | `/ready` | Readiness check (all systems initialized) |

---

## Low-Level Design

### Module Structure

```
src/
├── main.rs                    # Entry point, startup sequence
├── config.rs                  # AppConfig, env var loading
├── errors.rs                  # AppError enum, error response formatting
├── router.rs                  # Route definitions, middleware layering
├── auth/
│   ├── mod.rs
│   ├── handlers.rs            # HTTP handlers (register, login, refresh, etc.)
│   ├── service.rs             # AuthService impl
│   ├── jwt.rs                 # JWT encode/decode, claims structs
│   ├── oauth.rs               # OAuth flow (Google, GitHub)
│   ├── password.rs            # Argon2id hashing, verification
│   └── models.rs              # Request/response DTOs
├── sync/
│   ├── mod.rs
│   ├── handlers.rs            # REST handlers (CRUD, batch, snapshot, deltas)
│   ├── service.rs             # SyncService impl (workspace-scoped versioning)
│   ├── websocket.rs           # WS upgrade, session management, push
│   ├── session_registry.rs    # Workspace-scoped WS session tracking
│   └── models.rs              # SyncDelta, WsMessage, DTOs
├── ai/
│   ├── mod.rs
│   ├── handlers.rs            # POST /ai/expand handler
│   ├── service.rs             # AiService impl (proxy, quota, queue)
│   └── models.rs              # Request/response DTOs
├── admin/
│   ├── mod.rs
│   ├── handlers.rs            # Admin REST handlers (users, workspaces, etc.)
│   ├── service.rs             # AdminService impl
│   └── models.rs              # Admin DTOs, pagination
├── subscription/
│   ├── mod.rs
│   ├── handlers.rs            # Checkout, upgrade, webhook handlers
│   ├── service.rs             # SubscriptionService impl
│   ├── invoice.rs             # Invoice calculation (discount + tax)
│   ├── webhook.rs             # Billing webhook signature verification
│   └── models.rs              # Tier, Status, Invoice DTOs
├── workspace/
│   ├── mod.rs
│   ├── handlers.rs            # Teams endpoints
│   ├── service.rs             # WorkspaceService, TeamService
│   └── models.rs              # Workspace, Member DTOs
├── email/
│   ├── mod.rs
│   ├── service.rs             # EmailService (provider abstraction)
│   ├── smtp.rs                # SMTP provider impl
│   ├── api_provider.rs        # HTTP API provider impl
│   └── templates.rs           # Email template rendering
├── scheduler/
│   ├── mod.rs
│   └── service.rs             # SchedulerService (task registry, recurring)
├── middleware/
│   ├── mod.rs
│   ├── trace_id.rs            # UUID v4 trace ID injection
│   ├── security_headers.rs    # X-Content-Type-Options, CSP, etc.
│   ├── cors.rs                # CORS handling
│   ├── body_limit.rs          # Request body size limit
│   ├── panic_recovery.rs      # Catch-panic layer
│   ├── rate_limit.rs          # Sliding window (IP + user + admin + sync)
│   ├── auth_extractor.rs      # JWT extraction, claims validation
│   ├── client_type_guard.rs   # Client type enforcement per route
│   ├── subscription_context.rs # Inject tier/status into request extensions
│   └── admin_guard.rs         # role=admin check
├── db/
│   ├── mod.rs
│   └── pool.rs                # PgPool initialization, connection config
└── models/
    └── common.rs              # Shared types (Uuid, DateTime, Pagination)
```

### Key Data Structures

```rust
// ─── config.rs ───

/// All runtime configuration loaded from environment variables.
pub struct AppConfig {
    // Required (no defaults, panic on absence)
    pub database_url: String,
    pub jwt_secret: String,
    pub google_client_id: String,
    pub google_client_secret: String,
    pub github_client_id: String,
    pub github_client_secret: String,
    pub oauth_redirect_base_url: String,
    pub ai_provider_url: String,
    pub ai_provider_key: String,
    pub billing_webhook_secret: String,
    pub email_from_address: String,
    pub seed_admin_email: String,
    pub seed_admin_password: String,
    pub email_provider: EmailProviderType, // "smtp" or "api"

    // Email SMTP (required when email_provider = smtp)
    pub email_smtp_host: Option<String>,
    pub email_smtp_port: Option<u16>,
    pub email_smtp_user: Option<String>,
    pub email_smtp_password: Option<String>,

    // Email API (required when email_provider = api)
    pub email_api_key: Option<String>,
    pub email_api_url: Option<String>,

    // Optional with defaults
    pub email_from_name: String,              // default: "Ursnip"
    pub port: u16,                            // default: 8080
    pub log_level: String,                    // default: "info"
    pub database_max_connections: u32,        // default: 20
    pub database_min_connections: u32,        // default: 5
    pub database_connect_timeout_secs: u64,   // default: 5
    pub database_idle_timeout_secs: u64,      // default: 300
    pub database_statement_timeout_secs: u64, // default: 30
    pub cors_allowed_origins: Vec<String>,    // default: empty (reject all)
    pub trusted_proxy_cidrs: Vec<String>,     // default: empty
    pub ws_max_connections: usize,            // default: 10_000
    pub ai_max_concurrent_requests: usize,    // default: 50
    pub shutdown_timeout_secs: u64,           // default: 30
}

#[derive(Clone, Debug)]
pub enum EmailProviderType {
    Smtp,
    Api,
}

// ─── errors.rs ───

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

#[derive(Debug, Serialize)]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

/// Standard error response body
#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: ErrorBody,
    pub trace_id: Uuid,
}

#[derive(Serialize)]
pub struct ErrorBody {
    pub code: String,       // SCREAMING_SNAKE_CASE
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Vec<FieldError>>,
}

// ─── auth/jwt.rs ───

/// Access token claims embedded in every JWT.
#[derive(Debug, Serialize, Deserialize)]
pub struct AccessTokenClaims {
    pub sub: Uuid,                    // user_id
    pub client_type: ClientType,      // "native" or "web"
    pub role: Role,                   // "user" or "admin"
    pub permissions: Vec<String>,     // granular permissions
    pub subscription_tier: Tier,      // "free", "pro", "teams"
    pub exp: i64,                     // expiry unix timestamp
    pub iat: i64,                     // issued at
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ClientType {
    Native,
    Web,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Admin,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Free,
    Pro,
    Teams,
}

// ─── middleware/subscription_context.rs ───

/// Injected into request extensions for downstream handlers.
#[derive(Debug, Clone)]
pub struct SubscriptionContext {
    pub workspace_id: Uuid,
    pub tier: Tier,
    pub status: SubscriptionStatus,
    pub period_end: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SubscriptionStatus {
    Active,
    PastDue,
    Cancelled,
    PendingPayment,
    Deactivated,
}

// ─── sync/models.rs ───

/// A single sync delta record.
#[derive(Debug, Serialize, Deserialize)]
pub struct SyncDelta {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub entity_type: EntityType,  // "snippet" or "folder"
    pub entity_id: Uuid,
    pub operation: Operation,     // "create", "update", "delete"
    pub payload: serde_json::Value,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum EntityType {
    Snippet,
    Folder,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    Create,
    Update,
    Delete,
}

/// WebSocket message envelope (all WS messages use this shape).
#[derive(Debug, Serialize, Deserialize)]
pub struct WsMessage {
    #[serde(rename = "type")]
    pub msg_type: WsMessageType,
    pub workspace_id: Option<String>,
    pub version: Option<i64>,
    pub timestamp: String,         // ISO 8601
    pub payload: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WsMessageType {
    Delta,
    SnapshotRequired,
    Ack,
    Error,
    Ping,
    Pong,
}

/// Batch operation request item.
#[derive(Debug, Deserialize)]
pub struct BatchItem {
    pub operation: Operation,
    pub snippet: BatchSnippetPayload,
    pub client_request_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct BatchSnippetPayload {
    pub id: Option<Uuid>,         // required for update/delete
    pub trigger: Option<String>,
    pub content: Option<String>,
    pub snippet_type: Option<String>,
    pub folder_id: Option<Uuid>,
}

// ─── subscription/invoice.rs ───

/// Structured invoice returned during checkout.
#[derive(Debug, Serialize)]
pub struct Invoice {
    pub base_price: Decimal,
    pub discount_amount: Decimal,
    pub discount_type: Option<String>,  // "percentage" or "flat"
    pub subtotal_after_discount: Decimal,
    pub tax_rate: Decimal,
    pub tax_amount: Decimal,
    pub total_amount: Decimal,
    pub currency: String,
    pub billing_cycle_months: u32,
}
```

### Service Function Signatures

#### AuthService

```rust
impl AuthService {
    pub async fn register(&self, email: &str, password: &str, client_type: ClientType, referral_code: Option<&str>) -> Result<AuthTokenPair, AppError>;
    pub async fn login(&self, email: &str, password: &str, client_type: ClientType, ip: &str, user_agent: &str) -> Result<LoginResponse, AppError>;
    pub async fn refresh_token(&self, refresh_token: &str) -> Result<AuthTokenPair, AppError>;
    pub async fn logout(&self, user_id: Uuid, refresh_token_id: Uuid) -> Result<(), AppError>;
    pub async fn oauth_authorize(&self, provider: &str, client: ClientType) -> Result<String, AppError>;
    pub async fn oauth_callback(&self, provider: &str, code: &str, client_type: ClientType) -> Result<AuthTokenPair, AppError>;
    pub async fn forgot_password(&self, email: &str, ip: &str) -> Result<(), AppError>;
    pub async fn reset_password(&self, token: &str, new_password: &str) -> Result<(), AppError>;
    pub async fn change_password(&self, user_id: Uuid, current_password: &str, new_password: &str, ip: &str) -> Result<(), AppError>;
    pub async fn update_profile(&self, user_id: Uuid, updates: ProfileUpdate) -> Result<UserProfile, AppError>;
    pub async fn initiate_email_change(&self, user_id: Uuid, new_email: &str) -> Result<(), AppError>;
    pub async fn verify_email_change(&self, token: &str) -> Result<(), AppError>;
    pub async fn delete_account(&self, user_id: Uuid) -> Result<(), AppError>;
    pub async fn list_sessions(&self, user_id: Uuid) -> Result<Vec<SessionInfo>, AppError>;
    pub async fn revoke_session(&self, user_id: Uuid, session_id: Uuid) -> Result<(), AppError>;
    pub async fn create_admin_invite(&self, admin_id: Uuid, email: &str) -> Result<(), AppError>;
    pub async fn register_via_invite(&self, token: &str, email: &str, password: &str) -> Result<AuthTokenPair, AppError>;
    fn check_brute_force(&self, email: &str) -> Result<(), AppError>;
    fn record_failed_attempt(&self, email: &str);
    fn reset_failed_attempts(&self, email: &str);
    fn enforce_session_limit(&self, user_id: Uuid) -> Result<(), AppError>;
}
```

#### SyncService (workspace-scoped)

```rust
impl SyncService {
    pub async fn create_snippet(&self, workspace_id: Uuid, user_id: Uuid, payload: CreateSnippetRequest) -> Result<Snippet, AppError>;
    pub async fn update_snippet(&self, workspace_id: Uuid, snippet_id: Uuid, payload: UpdateSnippetRequest) -> Result<Snippet, AppError>;
    pub async fn delete_snippet(&self, workspace_id: Uuid, snippet_id: Uuid) -> Result<(), AppError>;
    pub async fn batch_operations(&self, workspace_id: Uuid, user_id: Uuid, items: Vec<BatchItem>) -> Result<Vec<BatchAck>, AppError>;
    pub async fn create_folder(&self, workspace_id: Uuid, user_id: Uuid, payload: CreateFolderRequest) -> Result<Folder, AppError>;
    pub async fn update_folder(&self, workspace_id: Uuid, folder_id: Uuid, payload: UpdateFolderRequest) -> Result<Folder, AppError>;
    pub async fn delete_folder(&self, workspace_id: Uuid, folder_id: Uuid) -> Result<(), AppError>;
    pub async fn get_snapshot(&self, workspace_id: Uuid) -> Result<WorkspaceSnapshot, AppError>;
    pub async fn get_deltas(&self, workspace_id: Uuid, since_version: i64, limit: Option<i32>) -> Result<DeltaPage, AppError>;
    async fn assign_next_version(&self, workspace_id: Uuid, tx: &mut Transaction<'_, Postgres>) -> Result<i64, AppError>;
    async fn record_delta(&self, delta: &SyncDelta, tx: &mut Transaction<'_, Postgres>) -> Result<(), AppError>;
    async fn push_to_workspace(&self, workspace_id: Uuid, delta: &SyncDelta, exclude_user: Option<Uuid>);
}
```

#### WorkspaceService

```rust
impl WorkspaceService {
    pub async fn create_individual_workspace(&self, user_id: Uuid) -> Result<Workspace, AppError>;
    pub async fn create_team_workspace(&self, user_id: Uuid, name: &str) -> Result<Workspace, AppError>;
    pub async fn get_workspace(&self, workspace_id: Uuid) -> Result<Workspace, AppError>;
    pub async fn list_user_workspaces(&self, user_id: Uuid) -> Result<Vec<Workspace>, AppError>;
    pub async fn verify_membership(&self, workspace_id: Uuid, user_id: Uuid) -> Result<WorkspaceMember, AppError>;
}
```

#### TeamService

```rust
impl TeamService {
    pub async fn create_invite(&self, workspace_id: Uuid, owner_id: Uuid, max_uses: Option<i32>, expires_at: Option<DateTime<Utc>>) -> Result<TeamInvite, AppError>;
    pub async fn join_via_invite(&self, workspace_id: Uuid, user_id: Uuid, invite_code: &str) -> Result<(), AppError>;
    pub async fn remove_member(&self, workspace_id: Uuid, owner_id: Uuid, target_user_id: Uuid) -> Result<(), AppError>;
    pub async fn list_members(&self, workspace_id: Uuid) -> Result<Vec<WorkspaceMember>, AppError>;
}
```

#### AdminService (expanded)

```rust
impl AdminService {
    // User management
    pub async fn list_users(&self, params: PaginationParams, filters: UserFilters) -> Result<PaginatedResponse<AdminUserSummary>, AppError>;
    pub async fn get_user(&self, user_id: Uuid) -> Result<AdminUserDetail, AppError>;
    pub async fn suspend_user(&self, admin_id: Uuid, user_id: Uuid) -> Result<AdminUserSummary, AppError>;
    pub async fn unsuspend_user(&self, admin_id: Uuid, user_id: Uuid) -> Result<AdminUserSummary, AppError>;
    pub async fn force_password_reset(&self, admin_id: Uuid, user_id: Uuid) -> Result<(), AppError>;
    pub async fn delete_user(&self, admin_id: Uuid, user_id: Uuid) -> Result<(), AppError>;

    // Workspace management
    pub async fn list_workspaces(&self, params: PaginationParams, filters: WorkspaceFilters) -> Result<PaginatedResponse<AdminWorkspaceSummary>, AppError>;
    pub async fn get_workspace(&self, workspace_id: Uuid) -> Result<AdminWorkspaceDetail, AppError>;
    pub async fn deactivate_workspace(&self, admin_id: Uuid, workspace_id: Uuid) -> Result<(), AppError>;
    pub async fn delete_workspace(&self, admin_id: Uuid, workspace_id: Uuid, confirm: bool) -> Result<(), AppError>;

    // Discount/Coupon management
    pub async fn list_discounts(&self) -> Result<Vec<Discount>, AppError>;
    pub async fn create_discount(&self, admin_id: Uuid, payload: CreateDiscountRequest) -> Result<Discount, AppError>;
    pub async fn update_discount(&self, admin_id: Uuid, discount_id: Uuid, payload: UpdateDiscountRequest) -> Result<Discount, AppError>;
    pub async fn list_coupons(&self, params: PaginationParams, filters: CouponFilters) -> Result<PaginatedResponse<CouponCode>, AppError>;
    pub async fn get_coupon(&self, coupon_id: Uuid) -> Result<CouponCode, AppError>;
    pub async fn create_coupon(&self, admin_id: Uuid, payload: CreateCouponRequest) -> Result<CouponCode, AppError>;
    pub async fn update_coupon(&self, admin_id: Uuid, coupon_id: Uuid, payload: UpdateCouponRequest) -> Result<CouponCode, AppError>;
    pub async fn get_referral_stats(&self) -> Result<ReferralStats, AppError>;

    // Subscription management
    pub async fn list_subscriptions(&self, params: PaginationParams, filters: SubscriptionFilters) -> Result<PaginatedResponse<SubscriptionSummary>, AppError>;
    pub async fn get_subscription(&self, workspace_id: Uuid) -> Result<SubscriptionDetail, AppError>;
    pub async fn extend_subscription(&self, admin_id: Uuid, workspace_id: Uuid, duration: ExtendDuration) -> Result<Subscription, AppError>;
    pub async fn cancel_subscription(&self, admin_id: Uuid, workspace_id: Uuid) -> Result<(), AppError>;
    pub async fn override_tier(&self, admin_id: Uuid, workspace_id: Uuid, tier: Tier) -> Result<Subscription, AppError>;
    pub async fn list_billing_events(&self, params: PaginationParams, filters: BillingEventFilters) -> Result<PaginatedResponse<BillingEvent>, AppError>;

    // Tax rates
    pub async fn list_tax_rates(&self) -> Result<Vec<TaxRate>, AppError>;
    pub async fn create_tax_rate(&self, admin_id: Uuid, payload: CreateTaxRateRequest) -> Result<TaxRate, AppError>;
    pub async fn update_tax_rate(&self, admin_id: Uuid, country_code: &str, payload: UpdateTaxRateRequest) -> Result<TaxRate, AppError>;

    // Audit logs
    pub async fn list_audit_logs(&self, params: PaginationParams, filters: AuditLogFilters) -> Result<PaginatedResponse<AuditLog>, AppError>;
    pub async fn get_audit_log(&self, id: Uuid) -> Result<AuditLog, AppError>;

    // Feature flags
    pub async fn list_feature_flags(&self) -> Result<Vec<FeatureFlag>, AppError>;
    pub async fn create_feature_flag(&self, admin_id: Uuid, payload: CreateFeatureFlagRequest) -> Result<FeatureFlag, AppError>;
    pub async fn update_feature_flag(&self, admin_id: Uuid, flag_name: &str, payload: UpdateFeatureFlagRequest) -> Result<FeatureFlag, AppError>;
    pub async fn delete_feature_flag(&self, admin_id: Uuid, flag_name: &str) -> Result<(), AppError>;

    // Admin management
    pub async fn list_admins(&self) -> Result<Vec<AdminUserSummary>, AppError>;
    pub async fn demote_admin(&self, admin_id: Uuid, target_admin_id: Uuid) -> Result<(), AppError>;

    // Stats
    pub async fn get_overview_stats(&self) -> Result<OverviewStats, AppError>;
    pub async fn get_referral_analytics(&self) -> Result<ReferralStats, AppError>;
}
```

#### SubscriptionService (with checkout/referral/coupon)

```rust
impl SubscriptionService {
    pub async fn create_free_subscription(&self, workspace_id: Uuid) -> Result<Subscription, AppError>;
    pub async fn initiate_upgrade(&self, user_id: Uuid, workspace_id: Uuid) -> Result<(), AppError>;
    pub async fn checkout(&self, user_id: Uuid, workspace_id: Uuid, tier: Tier, coupon_code: Option<&str>) -> Result<CheckoutResponse, AppError>;
    pub async fn compute_invoice(&self, user_id: Uuid, workspace_id: Uuid, tier: Tier, coupon_code: Option<&str>) -> Result<Invoice, AppError>;
    pub async fn get_current(&self, user_id: Uuid, workspace_id: Uuid) -> Result<Subscription, AppError>;
    pub async fn process_webhook(&self, event: BillingWebhookEvent) -> Result<(), AppError>;
    pub async fn validate_coupon(&self, code: &str) -> Result<CouponValidation, AppError>;
    pub async fn apply_referral_reward(&self, referrer_id: Uuid, referred_user_id: Uuid) -> Result<(), AppError>;
    pub async fn check_grace_periods(&self) -> Result<u32, AppError>;
    pub async fn check_payment_deadlines(&self) -> Result<u32, AppError>;
    pub async fn enforce_tier_limits(&self, workspace_id: Uuid, tier: Tier, operation: &str) -> Result<(), AppError>;
    fn calculate_tax(&self, country_code: Option<&str>, subtotal: Decimal) -> Result<(Decimal, Decimal), AppError>;
    fn apply_discount(&self, base_price: Decimal, discount: &Discount) -> Decimal;
}
```

#### AiService

```rust
impl AiService {
    pub async fn expand(&self, user_id: Uuid, trigger: &str, system_prompt: &str, context: Option<&str>, tier: Tier, subscription_status: SubscriptionStatus) -> Result<String, AppError>;
    async fn check_quota(&self, user_id: Uuid, tier: Tier, status: SubscriptionStatus) -> Result<(), AppError>;
    async fn call_provider(&self, trigger: &str, system_prompt: &str, context: Option<&str>) -> Result<String, AppError>;
}
```

#### EmailService

```rust
impl EmailService {
    pub async fn send_password_reset(&self, to: &str, token: &str) -> Result<(), AppError>;
    pub async fn send_email_change_verification(&self, to: &str, token: &str) -> Result<(), AppError>;
    pub async fn send_email_change_notification(&self, to: &str) -> Result<(), AppError>;
    pub async fn send_admin_invite(&self, to: &str, token: &str) -> Result<(), AppError>;
    pub async fn send_team_invite(&self, to: &str, workspace_name: &str, invite_link: &str) -> Result<(), AppError>;
    async fn send(&self, to: &str, subject: &str, html: &str, text: &str) -> Result<(), AppError>;
    async fn send_with_retry(&self, to: &str, subject: &str, html: &str, text: &str) -> Result<(), AppError>;
}
```

#### SchedulerService

```rust
impl SchedulerService {
    pub fn new(pool: PgPool, email: Arc<EmailService>, subscription: Arc<SubscriptionService>) -> Self;
    pub async fn start(&self) -> JoinHandle<()>;
    pub async fn shutdown(&self);
    async fn run_task(&self, task: ScheduledTask) -> Result<(), AppError>;
    async fn delta_purge(&self) -> Result<u64, AppError>;
    async fn soft_delete_cleanup(&self) -> Result<u64, AppError>;
    async fn expired_token_cleanup(&self) -> Result<u64, AppError>;
    async fn grace_period_check(&self) -> Result<u32, AppError>;
    async fn payment_deadline_check(&self) -> Result<u32, AppError>;
    async fn account_hard_delete_check(&self) -> Result<u32, AppError>;
}
```

### Middleware Stack

Middleware is applied in strict order. Each layer short-circuits on failure per Req 7.28–7.34.

```
Request
  │
  ▼
┌─────────────────────────┐
│ 1. TraceId              │  Generate UUID v4, attach to extensions + response header
├─────────────────────────┤
│ 2. SecurityHeaders      │  X-Content-Type-Options, X-Frame-Options, CSP, HSTS, etc.
├─────────────────────────┤
│ 3. CORS                 │  Validate origin against CORS_ALLOWED_ORIGINS, handle preflight
├─────────────────────────┤
│ 4. BodySizeLimit        │  1 MB default, 10 MB for /sync/* routes
├─────────────────────────┤
│ 5. PanicRecovery        │  tower catch-panic, log stack trace, return 500
├─────────────────────────┤
│ 6. IPRateLimit          │  100 req/min per resolved client IP (sliding window)
├─────────────────────────┤
│ 7. Auth                 │  Extract & validate JWT, inject claims into extensions
│                         │  (skip for public routes: /health, /ready, /auth/*, /webhooks/*)
├─────────────────────────┤
│ 8. ClientTypeGuard      │  Enforce client_type claim matches endpoint restriction
├─────────────────────────┤
│ 9. SubscriptionContext  │  Load tier/status/period_end into extensions (skip for admin)
├─────────────────────────┤
│10. UserRateLimit        │  500 req/min per user_id (sliding window)
│                         │  + 60 mutations/min, 120 reads/min for /sync/*
│                         │  + 300 req/min for /admin/*
├─────────────────────────┤
│11. AdminGuard           │  For /admin/* routes: verify role=admin
├─────────────────────────┤
│     Handler             │  Business logic execution
└─────────────────────────┘
```

### WebSocket Session Management

```rust
/// Workspace-scoped registry tracking all active WS connections.
pub struct SessionRegistry {
    /// Map: workspace_id → Vec<(user_id, sender)>
    workspaces: DashMap<Uuid, Vec<WsSession>>,
    /// Map: user_id → Vec<(workspace_id, sender)> for per-user limit enforcement
    user_connections: DashMap<Uuid, Vec<UserConnection>>,
    /// Global connection counter for server-wide limit
    total_connections: AtomicUsize,
}

pub struct WsSession {
    pub user_id: Uuid,
    pub sender: mpsc::UnboundedSender<WsMessage>,
    pub connected_at: Instant,
    pub last_activity: AtomicInstant,
}

impl SessionRegistry {
    /// Register a new WS connection. Enforces:
    /// - Per-user limit (5 connections) → closes oldest if exceeded
    /// - Server-wide limit (WS_MAX_CONNECTIONS) → rejects with 503
    pub fn register(&self, workspace_id: Uuid, user_id: Uuid, sender: mpsc::UnboundedSender<WsMessage>) -> Result<(), AppError>;

    /// Remove a connection when it closes.
    pub fn unregister(&self, workspace_id: Uuid, user_id: Uuid, session_id: Uuid);

    /// Broadcast a delta to all members of a workspace except the originator.
    pub async fn broadcast_to_workspace(&self, workspace_id: Uuid, delta: &WsMessage, exclude_user: Option<Uuid>);

    /// Close all connections for a user (used on logout, suspend, password reset).
    pub async fn close_user_sessions(&self, user_id: Uuid, close_code: u16);

    /// Close all connections for workspace members (used on admin force-cancel).
    pub async fn close_workspace_sessions(&self, workspace_id: Uuid, close_code: u16);
}
```

**Heartbeat**: Server sends `ping` every 30s. Client must respond `pong` within 10s. After 2 missed pongs, connection is closed.

**Idle timeout**: Connections with no application messages (excluding ping/pong) for 5 minutes are closed.

**Reconnection catch-up**: On WS handshake, client sends `workspace_id` + `last_known_version`. Server sends missed deltas if within retention window, or a `snapshot_required` message otherwise.

**Token expiry**: An established WS connection remains open regardless of the original access token's expiration. Only explicit revocation (logout, password change, suspend, lockout) closes it via `close_user_sessions`.

### Background Scheduler Design

```rust
pub struct SchedulerService {
    pool: PgPool,
    tasks: Vec<ScheduledTask>,
    shutdown_signal: watch::Receiver<bool>,
}

pub struct ScheduledTask {
    pub name: &'static str,
    pub interval: Duration,
    pub handler: Box<dyn Fn() -> Pin<Box<dyn Future<Output = Result<(), AppError>> + Send>> + Send + Sync>,
}

/// Task registry with intervals (per Req 7.10):
/// ┌────────────────────────────────┬────────────┐
/// │ Task                           │ Interval   │
/// ├────────────────────────────────┼────────────┤
/// │ delta_purge                    │ 1 hour     │
/// │ soft_delete_cleanup            │ 6 hours    │
/// │ expired_token_cleanup          │ 1 hour     │
/// │ grace_period_check             │ 1 hour     │
/// │ payment_deadline_check         │ 1 hour     │
/// │ account_hard_delete_check      │ 24 hours   │
/// └────────────────────────────────┴────────────┘
```

**Retry mechanism**: On transient failure (DB timeout, network error), retry up to 3 times with exponential backoff: 1s → 5s → 30s. Non-transient failures (validation, not-found) are logged immediately without retry.

**Idempotency**: All tasks use timestamp-based filtering (e.g., `WHERE deleted_at < now() - interval '30 days'`) so repeated execution within the same interval produces the same result.

**Shutdown**: On `SIGTERM`/`SIGINT`, the scheduler stops spawning new task runs and waits for in-progress tasks to complete (up to `SHUTDOWN_TIMEOUT_SECS`).

### Email Service Design

```rust
/// Provider-agnostic email service with configurable backend.
pub struct EmailService {
    provider: Box<dyn EmailProvider + Send + Sync>,
    from_address: String,
    from_name: String,
    templates: TemplateEngine,
}

#[async_trait]
pub trait EmailProvider: Send + Sync {
    async fn send(&self, to: &str, subject: &str, html: &str, text: &str) -> Result<(), EmailError>;
}

pub struct SmtpProvider { /* lettre-based SMTP client */ }
pub struct ApiProvider { /* reqwest-based HTTP client */ }
```

**Template system**: Compile-time templates (using `askama` or similar) rendering both HTML and plaintext versions. Templates: `password_reset`, `email_change_verify`, `email_change_notification`, `admin_invite`, `team_invite`.

**Async dispatch**: All email sends are spawned via `tokio::spawn` so the HTTP handler returns immediately without waiting for delivery.

**Retry**: Exponential backoff (1s → 5s → 30s, 3 attempts total). On final failure, log at ERROR level with full context and discard.

### Rate Limiter Implementation

```rust
/// Sliding-window rate limiter using an in-memory data structure.
/// Separate limiters for different scopes.
pub struct RateLimiter {
    /// Per-IP: 100 req/min (unauthenticated endpoints)
    ip_limiter: DashMap<IpAddr, SlidingWindow>,
    /// Per-user: 500 req/min (all authenticated endpoints)
    user_limiter: DashMap<Uuid, SlidingWindow>,
    /// Per-admin: 300 req/min (admin endpoints)
    admin_limiter: DashMap<Uuid, SlidingWindow>,
    /// Per-user sync mutations: 60 req/min
    sync_mutation_limiter: DashMap<Uuid, SlidingWindow>,
    /// Per-user sync reads: 120 req/min
    sync_read_limiter: DashMap<Uuid, SlidingWindow>,
    /// Per-email forgot-password: 3 req/hour
    forgot_password_limiter: DashMap<String, SlidingWindow>,
}

pub struct SlidingWindow {
    window_ms: u64,
    max_requests: u32,
    timestamps: VecDeque<Instant>,
}

impl SlidingWindow {
    /// Returns Ok(()) if under limit, Err(retry_after_secs) if exceeded.
    pub fn check_and_record(&mut self) -> Result<(), u64>;
}
```

**Client IP resolution** (per Req 7.50–7.53): If request IP is within `TRUSTED_PROXY_CIDRS`, extract rightmost untrusted IP from `X-Forwarded-For`. Otherwise, use TCP peer address.

### Version Assignment (per-workspace)

Each workspace maintains an independent version counter. Version assignment uses `SELECT ... FOR UPDATE` row-level locking on the workspace's current max version to ensure strict monotonicity under concurrent writes:

```rust
async fn assign_next_version(
    workspace_id: Uuid,
    tx: &mut Transaction<'_, Postgres>,
) -> Result<i64, AppError> {
    // SELECT MAX(version) FROM sync_deltas WHERE workspace_id = $1 FOR UPDATE
    // If no deltas exist, start at 1
    // Return max + 1
    let row = sqlx::query_scalar!(
        "SELECT COALESCE(MAX(version), 0) + 1 FROM sync_deltas WHERE workspace_id = $1",
        workspace_id
    )
    .fetch_one(tx.as_mut())
    .await?;
    Ok(row.unwrap_or(1))
}
```

For batch operations, each item in the batch receives a sequential version (v, v+1, v+2, ...) within the same transaction.

### Startup Sequence

```
1. Load AppConfig from environment variables
   ├── Validate all required vars present
   ├── Validate email provider credentials match provider type
   └── Terminate with non-zero exit code if any required var missing

2. Initialize structured JSON logger (tracing-subscriber)
   └── Set level from LOG_LEVEL env var

3. Create PgPool with configured limits
   ├── max_connections, min_connections, connect_timeout, idle_timeout
   ├── Set statement_timeout on each connection
   └── Terminate if pool cannot establish initial connection

4. Run pending sqlx migrations
   ├── Log each migration applied
   └── Terminate with error context if any migration fails

5. Execute seed data (idempotent)
   ├── Upsert super-admin from SEED_ADMIN_EMAIL/PASSWORD
   └── Upsert default feature flags

6. Initialize services
   ├── AuthService (pool, jwt_secret, oauth configs)
   ├── SyncService (pool, session_registry)
   ├── AiService (provider_url, provider_key, semaphore)
   ├── SubscriptionService (pool, billing config)
   ├── WorkspaceService (pool)
   ├── TeamService (pool)
   ├── AdminService (pool)
   └── EmailService (provider, templates)

7. Start background scheduler
   └── Spawn all recurring tasks on their intervals

8. Initialize email service warmup
   └── Verify SMTP connection or API reachability

9. Build Axum router
   ├── Mount all route groups with middleware layers
   ├── Apply global middleware (trace_id, security_headers, cors, body_limit, panic_recovery)
   └── Apply per-group middleware (auth, client_type, subscription_context, admin_guard)

10. Bind to 0.0.0.0:{PORT}
    └── Log "server listening on port {PORT}"

11. Set readiness flag
    └── /ready now returns 200
```

### Graceful Shutdown (per Req 7.66–7.72)

```
SIGTERM or SIGINT received
  │
  ▼
1. Stop accepting new TCP connections (listener close)
2. Stop accepting new WebSocket upgrades
3. Send WS Close frame (code 1001 "Going Away") to all active WS sessions
4. Signal scheduler to stop spawning new tasks
5. Wait for in-flight HTTP requests (up to SHUTDOWN_TIMEOUT_SECS)
6. Wait for in-progress background tasks (up to SHUTDOWN_TIMEOUT_SECS)
7. If timeout exceeded → force-terminate remaining requests and tasks
8. Close database connection pool
9. Log "shutdown complete"
10. Exit with code 0
```

---

## Correctness Properties

*A property is a characteristic or behavior that should hold true across all valid executions of a system — essentially, a formal statement about what the system should do. Properties serve as the bridge between human-readable specifications and machine-verifiable correctness guarantees.*

### Property 1: Client type endpoint enforcement

*For any* endpoint and any valid JWT with a given `client_type` claim, the backend SHALL allow the request if and only if the `client_type` is in the allowed set for that endpoint; otherwise it SHALL return 403 `CLIENT_TYPE_NOT_ALLOWED`.

**Validates: Requirements 1.5, 1.6**

### Property 2: Workspace-scoped version monotonicity

*For any* workspace and any sequence of sync mutations applied to that workspace, the assigned workspace versions SHALL form a strictly monotonically increasing integer sequence with no gaps.

**Validates: Requirements 2.4**

### Property 3: Trigger uniqueness per workspace

*For any* workspace, there SHALL NOT exist two active snippets (where `deleted_at IS NULL`) with the same `trigger` value within that workspace. Two different workspaces MAY independently use identical trigger values.

**Validates: Requirements 2.32, 2.33**

### Property 4: Batch operation atomicity

*For any* batch of snippet operations submitted in a single request, either all operations succeed and are persisted with sequential workspace versions, or all operations fail with no changes applied to the database.

**Validates: Requirements 2.34, 2.37**

### Property 5: Delta retention window enforcement

*For any* delta polling request where `since_version` refers to a version older than the 30-day retention window, the backend SHALL return 409 `SNAPSHOT_REQUIRED`. *For any* request within the retention window, the backend SHALL return the requested deltas.

**Validates: Requirements 2.16, 2.45, 2.46, 2.47**

### Property 6: WebSocket connections survive token expiry

*For any* established WebSocket session, the connection SHALL remain open and functional regardless of the original access token's expiration time. Only explicit session revocation (logout, password change, suspend, lockout) SHALL close an active WebSocket connection.

**Validates: Requirements 2.29, 2.30**

### Property 7: WebSocket push workspace scoping

*For any* sync mutation affecting a workspace, delta push messages SHALL be delivered to all connected members of that workspace and SHALL NOT be delivered to users who are not members of the workspace where the mutation occurred.

**Validates: Requirements 2.23, 2.24, 2.25**

### Property 8: Token refresh rotation and reuse detection

*For any* valid refresh token, using it for refresh SHALL invalidate the old token and issue a new pair. *For any* previously invalidated refresh token presented for refresh, the backend SHALL revoke ALL tokens for that user and return 401 `TOKEN_REUSE_DETECTED`.

**Validates: Requirements 1.14, 1.15, 1.53**

### Property 9: Concurrent session limit

*For any* user, the number of active (non-revoked, non-expired) refresh tokens SHALL never exceed 5. When a 6th login occurs, the oldest refresh token SHALL be revoked.

**Validates: Requirements 1.47**

### Property 10: Brute-force lockout state machine

*For any* email address, after 5 consecutive failed login attempts the account SHALL be locked for 15 minutes (returning 429 `ACCOUNT_LOCKED`). A successful login SHALL reset the counter to zero.

**Validates: Requirements 1.50, 1.51, 1.52**

### Property 11: Admin cannot act on self

*For any* admin performing a suspend, delete, or demote action where the target ID equals their own user ID, the backend SHALL return 422 with the appropriate `CANNOT_ACT_ON_SELF` or `CANNOT_DEMOTE_SELF` error code.

**Validates: Requirements 4.7, 4.14, 4.57**

### Property 12: Admin cannot act on other admins

*For any* admin attempting to suspend or delete a user with `role = admin`, the backend SHALL return 422 `CANNOT_ACT_ON_ADMIN`.

**Validates: Requirements 4.8, 4.15**

### Property 13: Per-tier feature limits enforcement

*For any* write operation on a workspace with `free` tier subscription, the backend SHALL enforce: max 10 snippets, max 3 folders, max 2000 chars per snippet content. *For any* workspace with `pro` or `teams` tier, these limits SHALL NOT be enforced.

**Validates: Requirements 5.21, 5.23**

### Property 14: Soft-lock on downgrade

*For any* user whose subscription expires or is cancelled and whose content exceeds free tier limits, write operations that would exceed free limits SHALL return 422 `CONTENT_SOFT_LOCKED`. Existing content SHALL remain readable.

**Validates: Requirements 5.24, 5.25**

### Property 15: Grace period transitions

*For any* subscription in `past_due` status, the backend SHALL continue granting paid-tier access for exactly 7 calendar days. After the grace period expires without renewal, the status SHALL transition to `cancelled` and free-tier limits SHALL be enforced.

**Validates: Requirements 5.30, 5.31**

### Property 16: Referral reward idempotency

*For any* (referrer_id, referred_user_id) pair, the referral reward (1 month extension) SHALL be applied at most once. Duplicate conversion attempts SHALL be idempotent — no error returned, no additional reward applied.

**Validates: Requirements 5.48**

### Property 17: Billing webhook idempotency

*For any* billing webhook event with a given `external_event_id`, processing it multiple times SHALL produce the same database state as processing it once. Duplicate events SHALL return HTTP 200 without re-processing.

**Validates: Requirements 5.57**

### Property 18: Team workspace seat limit

*For any* team workspace, the total number of members (including the owner) SHALL never exceed 3. Any join attempt when the workspace is at capacity SHALL return 422 `SEAT_LIMIT_REACHED`.

**Validates: Requirements 5.17**

### Property 19: Invoice calculation correctness

*For any* base price, discount (percentage or flat), and tax rate, the invoice total SHALL equal `(base_price - discount_amount) × (1 + tax_rate)` where `discount_amount` is clamped to `[0, base_price]` and the final total is rounded to 2 decimal places.

**Validates: Requirements 5.33, 5.51, 5.53**

### Property 20: Coupon validation completeness

*For any* coupon code submitted at checkout, the backend SHALL validate all conditions (exists, active, valid_from ≤ now, valid_until is null or > now, times_used < max_uses) and reject with the specific error code corresponding to the first failing condition. The `times_used` increment SHALL occur within the same transaction as subscription creation.

**Validates: Requirements 5.37, 5.38, 5.39**

### Property 21: Audit log immutability

*For any* audit log record, the backend SHALL NOT expose update or delete operations. Audit log records SHALL be retained indefinitely with no automated purge.

**Validates: Requirements 4.50, 4.51**

### Property 22: Error response format consistency

*For any* API error response from the backend, the response body SHALL contain an `error` object with `code` (SCREAMING_SNAKE_CASE string) and `message` (string), plus a top-level `trace_id` (UUID v4). Validation errors SHALL additionally include a `details` array with per-field errors.

**Validates: Requirements 7.25, 7.26, 7.27**

### Property 23: Rate limiting sliding window

*For any* IP address exceeding 100 requests per minute on unauthenticated endpoints, or any user exceeding 500 requests per minute on authenticated endpoints, the backend SHALL return 429 with a valid `Retry-After` header.

**Validates: Requirements 7.83, 7.84**

### Property 24: Client IP resolution

*For any* request arriving from an IP within `TRUSTED_PROXY_CIDRS`, the resolved client IP SHALL be the rightmost untrusted IP from the `X-Forwarded-For` header. *For any* request from an IP outside all trusted CIDRs, the resolved client IP SHALL be the TCP peer address.

**Validates: Requirements 7.50, 7.51, 7.52, 7.53**

### Property 25: WebSocket per-user connection limit

*For any* user, the number of concurrent WebSocket connections SHALL never exceed 5. When a 6th connection is established, the oldest existing connection SHALL be closed with code 1008.

**Validates: Requirements 7.42, 7.43**

### Property 26: Partial unique index allows trigger reuse after deletion

*For any* workspace where a snippet with trigger T has been soft-deleted (`deleted_at IS NOT NULL`), creating a new snippet with the same trigger T in the same workspace SHALL succeed.

**Validates: Requirements 6.13, 2.32**

### Property 27: AI tier-based quota enforcement

*For any* user with `free` tier, AI expansion requests SHALL be limited to 50 per 24-hour rolling window. *For any* user with `pro` or `teams` tier (including during grace period), the limit SHALL be 1000 per 24-hour rolling window. Exceeding the limit SHALL return 429 `AI_QUOTA_EXCEEDED`.

**Validates: Requirements 3.4, 3.5, 3.6, 3.7**

### Property 28: Password reset token single-use

*For any* password reset token, it SHALL be usable exactly once. After use, presenting the same token SHALL return 422 `INVALID_RESET_TOKEN`. Generating a new reset token SHALL invalidate all previous tokens for that user.

**Validates: Requirements 1.25, 1.28, 1.29**
