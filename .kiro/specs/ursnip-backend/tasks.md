# Implementation Plan: ursnip-backend

## Overview

A greenfield Rust/Axum monolithic backend with 22 database tables, 76 API endpoints across 7 services, WebSocket real-time sync, background scheduler, and an 11-layer middleware stack. Tasks follow the startup sequence dependency order: scaffolding → database → errors → middleware → services → integration.

## Tasks

- [x] 1. Project scaffolding and configuration
  - [x] 1.1 Initialize Cargo workspace and core dependencies
    - Create `Cargo.toml` with dependencies: axum, tokio, sqlx (postgres, runtime-tokio, tls-rustls), serde, serde_json, uuid, chrono, jsonwebtoken, argon2, tower, tower-http, tracing, tracing-subscriber, thiserror, dashmap, reqwest, lettre, askama, rust_decimal, rand, sha2, hex, dotenvy
    - Create `src/main.rs` with placeholder main function
    - Create `.env.example` with all required and optional environment variables
    - _Requirements: 7.15, 6.1_

  - [x] 1.2 Implement `config.rs` — environment variable loading and validation
    - Implement `AppConfig` struct with all fields from the design (required and optional with defaults)
    - Implement `AppConfig::from_env()` that loads from environment, validates required vars, validates email provider credentials match provider type, and terminates with descriptive error if any required var is missing
    - Implement `EmailProviderType` enum parsing
    - _Requirements: 7.1, 7.2, 7.3, 7.4, 7.11, 7.15_

  - [x] 1.3 Implement structured JSON logging
    - Configure `tracing-subscriber` with JSON formatting, reading `LOG_LEVEL` from `AppConfig`
    - Set up span context propagation for trace IDs
    - _Requirements: 7.18_

- [x] 2. Database layer
  - [x] 2.1 Implement `db/pool.rs` — PgPool initialization
    - Create `init_pool(config: &AppConfig) -> PgPool` that configures max_connections, min_connections, connect_timeout, idle_timeout, and statement_timeout
    - Terminate process with non-zero exit code if initial connection fails
    - _Requirements: 6.14, 6.15, 7.38, 7.40_

  - [x] 2.2 Create sqlx migrations for all 22 tables
    - Write `migrations/0001_initial.sql` with all CREATE TABLE statements, indexes, unique constraints (including partial indexes for soft-delete tables), and foreign key relationships as specified in the design
    - Ensure correct ON DELETE CASCADE/RESTRICT rules per Requirements 6.8, 6.9, 6.10
    - _Requirements: 6.1, 6.6, 6.7, 6.8, 6.9, 6.10, 6.11, 6.12, 6.13_

  - [x] 2.3 Create seed migration for super-admin and default feature flags
    - Write `migrations/0002_seed.sql` with idempotent upsert of super-admin user (from env vars) and default feature flags (all `enabled = false`)
    - Use `INSERT ... ON CONFLICT DO NOTHING` for idempotency
    - _Requirements: 6.16, 6.17, 1.40_

  - [x] 2.4 Define shared model types in `models/common.rs`
    - Implement `Pagination` struct (page, per_page with defaults and max), `PaginatedResponse<T>` generic wrapper
    - Define shared enums: `ClientType`, `Role`, `Tier`, `SubscriptionStatus`
    - Implement `sqlx::FromRow` derivations for database model structs used across services
    - _Requirements: 4.4, 4.16_

- [x] 3. Error handling framework
  - [x] 3.1 Implement `errors.rs` — AppError enum and error response formatting
    - Define all `AppError` variants as specified in the design (auth, forbidden, payment required, not found, conflict, unprocessable, rate limiting, request too large, server errors, malformed)
    - Implement `FieldError` and `ErrorResponse`/`ErrorBody` structs
    - Implement `IntoResponse` for `AppError` mapping each variant to correct HTTP status + SCREAMING_SNAKE_CASE error code + trace_id
    - _Requirements: 7.25, 7.26, 7.27_

- [x] 4. Middleware stack
  - [x] 4.1 Implement `middleware/trace_id.rs` — trace ID generation and propagation
    - Generate UUID v4 on each request, store in request extensions, add `X-Trace-Id` response header
    - _Requirements: 7.18, 7.27_

  - [x] 4.2 Implement `middleware/security_headers.rs`
    - Add X-Content-Type-Options: nosniff, X-Frame-Options: DENY, Referrer-Policy: strict-origin-when-cross-origin, X-XSS-Protection: 0, Strict-Transport-Security, Content-Security-Policy: default-src 'none'
    - _Requirements: 7.47, 7.48, 7.49_

  - [x] 4.3 Implement `middleware/cors.rs` — CORS handling
    - Read allowed origins from `AppConfig`, handle preflight OPTIONS with 204, set Access-Control-Allow-Origin/Methods/Headers/Credentials headers
    - Reject cross-origin requests when CORS_ALLOWED_ORIGINS is empty
    - _Requirements: 7.15, 7.16, 7.17, 7.18, 7.19, 7.20, 7.21_

  - [x] 4.4 Implement `middleware/body_limit.rs` — request body size enforcement
    - Default 1 MB limit, override to 10 MB for `/sync/*` routes
    - Return 413 REQUEST_BODY_TOO_LARGE on violation
    - _Requirements: 7.22, 7.23, 7.24_

  - [x] 4.5 Implement `middleware/panic_recovery.rs` — catch-panic layer
    - Wrap handlers with tower catch-panic, log stack trace at ERROR with trace_id, return 500 INTERNAL_ERROR
    - _Requirements: 7.35, 7.36, 7.37_

  - [x] 4.6 Implement `middleware/rate_limit.rs` — sliding window rate limiter
    - Implement `SlidingWindow` struct with `check_and_record()` method
    - Implement `RateLimiter` with DashMap-based limiters: ip_limiter (100/min), user_limiter (500/min), admin_limiter (300/min), sync_mutation_limiter (60/min), sync_read_limiter (120/min), forgot_password_limiter (3/hour)
    - Implement client IP resolution: extract rightmost untrusted IP from X-Forwarded-For when request IP is in TRUSTED_PROXY_CIDRS, otherwise use TCP peer address
    - Return 429 with Retry-After header on limit exceeded
    - _Requirements: 7.50, 7.51, 7.52, 7.53, 7.83, 7.84, 2.42, 4.2, 1.27_

  - [x] 4.7 Implement `middleware/auth_extractor.rs` — JWT extraction and validation
    - Extract JWT from Authorization Bearer header, decode and validate signature/expiry using `jsonwebtoken`, inject `AccessTokenClaims` into request extensions
    - Skip for public routes: /health, /ready, /auth/register, /auth/login, /auth/refresh, /auth/forgot-password, /auth/reset-password, /auth/oauth/*, /auth/verify-email-change, /webhooks/*
    - Check `status = suspended` → return 403 ACCOUNT_SUSPENDED
    - Check `must_reset_password = true` → return 403 PASSWORD_RESET_REQUIRED
    - _Requirements: 1.2, 1.31, 4.9, 4.12, 7.31_

  - [x] 4.8 Implement `middleware/client_type_guard.rs` — client type enforcement
    - Enforce per-endpoint client_type restrictions: /sync/* native only, /subscriptions/* web only, /teams/* web only, /admin/* web only, /ai/* native only
    - Return 403 CLIENT_TYPE_NOT_ALLOWED on mismatch
    - _Requirements: 1.5, 1.6_

  - [x] 4.9 Implement `middleware/subscription_context.rs` — tier/status injection
    - Load workspace subscription tier, status, period_end into request extensions for downstream handlers
    - Skip for admin routes and public routes
    - _Requirements: 5.59_

  - [x] 4.10 Implement `middleware/admin_guard.rs` — admin role check
    - For `/admin/*` routes verify `role = admin` from claims
    - Return 403 FORBIDDEN if not admin
    - _Requirements: 4.1_

  - [x] 4.11 Implement `router.rs` — route definitions with middleware layering
    - Define all route groups (/auth, /sync, /ai, /admin, /subscriptions, /teams, /webhooks, /health, /ready)
    - Apply global middleware in strict order: trace_id → security_headers → cors → body_limit → panic_recovery → ip_rate_limit
    - Apply per-group middleware: auth → client_type_guard → subscription_context → user_rate_limit → admin_guard
    - Wire shared AppState (pool, services, config) into router
    - _Requirements: 7.28, 7.29, 7.30, 7.31, 7.32, 7.33, 7.34_

- [x] 5. Checkpoint - Ensure project compiles and middleware stack is wired
  - Ensure all tests pass, ask the user if questions arise.

- [x] 6. Auth service — core flows
  - [x] 6.1 Implement `auth/password.rs` — Argon2id hashing and verification
    - Implement `hash_password(password: &str) -> Result<String>` using argon2 crate
    - Implement `verify_password(password: &str, hash: &str) -> Result<bool>` with constant-time comparison
    - _Requirements: 1.7, 1.11_

  - [x] 6.2 Implement `auth/jwt.rs` — JWT encode/decode
    - Implement `encode_access_token(claims: AccessTokenClaims, secret: &str) -> String`
    - Implement `decode_access_token(token: &str, secret: &str) -> Result<AccessTokenClaims, AppError>`
    - Implement TTL logic: 15 min for role=user, 5 min for role=admin
    - _Requirements: 1.2, 1.3, 1.44_

  - [x] 6.3 Implement `auth/service.rs` — registration flow
    - Implement `register()`: validate email uniqueness, enforce min 8 char password, hash with Argon2id, persist user, generate referral code, create individual workspace + free subscription, issue token pair
    - Handle referral_code: validate exists, not self-referral, record in referrals table
    - Block admin creation via register endpoint
    - _Requirements: 1.7, 1.8, 1.9, 1.10, 5.1, 5.6, 5.41, 5.42, 5.43, 5.44_

  - [x] 6.4 Implement `auth/service.rs` — login flow with brute-force protection
    - Implement `login()`: verify Argon2id hash, enforce 100ms minimum response time, handle soft-deleted account reactivation, issue token pair with role in response
    - Implement `check_brute_force()`: track failed attempts per email, lock after 5 failures for 15 min, return 429 ACCOUNT_LOCKED with Retry-After
    - Implement `record_failed_attempt()` and `reset_failed_attempts()`
    - Implement `enforce_session_limit()`: revoke oldest token when 6th session created
    - _Requirements: 1.11, 1.12, 1.13, 1.47, 1.50, 1.51, 1.52_

  - [x] 6.5 Implement `auth/service.rs` — token refresh with rotation and reuse detection
    - Implement `refresh_token()`: validate token, invalidate old, issue new pair
    - Detect reuse of invalidated token → revoke ALL user tokens, return 401 TOKEN_REUSE_DETECTED, log security event
    - _Requirements: 1.14, 1.15, 1.45, 1.53_

  - [x] 6.6 Implement `auth/service.rs` — logout
    - Implement `logout()`: invalidate the refresh token associated with the session, return 204
    - _Requirements: 1.16_

  - [x] 6.7 Implement `auth/service.rs` — forgot password and reset password
    - Implement `forgot_password()`: generate crypto-secure token, store SHA-256 hash with 30-min TTL, invalidate previous tokens, trigger email send, return 200 regardless of email existence
    - Implement `reset_password()`: validate token (not expired, not used, exists), hash new password, update user, mark token used, revoke ALL refresh tokens
    - _Requirements: 1.25, 1.26, 1.27, 1.28, 1.29_

  - [x] 6.8 Implement `auth/service.rs` — profile management, email change, password change
    - Implement `update_profile()`: partial update of allowed fields (first_name, last_name, profile_picture_url, timezone, language, country_code, phone)
    - Implement `initiate_email_change()`: generate token (24h TTL), store in email_change_requests, send verification email
    - Implement `verify_email_change()`: validate token, update email, notify old address, mark token used
    - Implement `change_password()`: verify current password, hash new (min 8 chars), update, revoke all refresh tokens
    - _Requirements: 1.30, 1.31, 1.32, 1.33, 1.34, 1.35_

  - [x] 6.9 Implement `auth/service.rs` — account deletion and session management
    - Implement `delete_account()`: check no owned team workspaces (return 422 TRANSFER_OWNERSHIP_REQUIRED), set deleted_at
    - Implement `list_sessions()`: return active refresh tokens with session_id, client_type, created_at, last_used_at
    - Implement `revoke_session()`: revoke specific refresh token
    - _Requirements: 1.36, 1.37, 1.38, 1.48, 1.49_

  - [x] 6.10 Implement `auth/oauth.rs` — OAuth flow (Google and GitHub)
    - Implement `oauth_authorize()`: build OAuth URL with correct redirect_uri based on client_type (native → deep link, web → web callback)
    - Implement `oauth_callback()`: exchange code for token, retrieve verified email, upsert user (auto-merge if email matches existing account), link OAuth identity, issue token pair
    - Handle missing verified email → 422 EMAIL_VERIFICATION_REQUIRED
    - Handle account linking conflict → 409 ACCOUNT_LINKING_CONFLICT
    - Handle provider error parameter → 401 OAUTH_AUTHORIZATION_DENIED
    - _Requirements: 1.17, 1.18, 1.19, 1.20, 1.21, 1.22, 1.23, 1.24_

  - [x] 6.11 Implement `auth/service.rs` — admin invite flow
    - Implement `create_admin_invite()`: enforce max 5 pending invites, generate token (24h TTL), store hashed in admin_invites, send invite email
    - Implement `register_via_invite()`: validate token (not expired, not used), create user with role=admin, mark invite used
    - _Requirements: 1.41, 1.42, 1.43, 4.59_

  - [x] 6.12 Implement `auth/handlers.rs` — wire all auth HTTP handlers
    - Create Axum handlers for all 15 auth endpoints (register, login, refresh, logout, oauth authorize/callback, forgot-password, reset-password, profile, change-email, verify-email-change, change-password, delete account, sessions, revoke session)
    - Parse request bodies, extract claims from extensions, call service methods, format responses
    - _Requirements: 1.1–1.53_

  - [x] 6.13 Write property tests for auth service
    - **Property 8: Token refresh rotation and reuse detection**
    - **Property 9: Concurrent session limit**
    - **Property 10: Brute-force lockout state machine**
    - **Property 28: Password reset token single-use**
    - **Validates: Requirements 1.14, 1.15, 1.47, 1.50, 1.51, 1.52, 1.53, 1.25, 1.28, 1.29**

- [x] 7. Workspace and Teams service
  - [x] 7.1 Implement `workspace/service.rs` — WorkspaceService
    - Implement `create_individual_workspace()`: create workspace with type=individual, add owner membership
    - Implement `create_team_workspace()`: create workspace with type=team, add owner membership, create teams subscription with status=pending_payment and 7-day payment_deadline
    - Implement `get_workspace()`, `list_user_workspaces()`, `verify_membership()`
    - _Requirements: 5.1, 5.2, 5.3, 5.4, 5.9, 5.10, 5.12_

  - [x] 7.2 Implement `workspace/service.rs` — TeamService
    - Implement `create_invite()`: generate unique invite_code, store in team_invites with max_uses and expires_at
    - Implement `join_via_invite()`: validate code belongs to workspace, times_used < max_uses, not expired, seat limit (max 3), not already member; add membership, increment times_used
    - Implement `remove_member()`: verify requester is owner, target is not owner (422 CANNOT_REMOVE_OWNER), remove from workspace_members
    - Implement `list_members()`
    - _Requirements: 5.13, 5.14, 5.15, 5.16, 5.17, 5.18, 5.19, 5.20_

  - [x] 7.3 Implement `workspace/handlers.rs` — Teams HTTP handlers
    - Wire POST /teams, POST /teams/{workspace_id}/invites, POST /teams/{workspace_id}/join, DELETE /teams/{workspace_id}/members/{user_id}, GET /teams/{workspace_id}, GET /teams/{workspace_id}/members
    - _Requirements: 5.9–5.20_

  - [x] 7.4 Write property test for team workspace seat limit
    - **Property 18: Team workspace seat limit**
    - **Validates: Requirements 5.17**

- [x] 8. Checkpoint - Ensure auth and workspace services compile and pass tests
  - Ensure all tests pass, ask the user if questions arise.

- [x] 9. Sync service — CRUD, versioning, and WebSocket
  - [x] 9.1 Implement `sync/service.rs` — workspace-scoped versioning and snippet CRUD
    - Implement `assign_next_version()`: SELECT MAX(version) FOR UPDATE within transaction, return max+1
    - Implement `create_snippet()`: validate payload, enforce tier limits (free: 10 snippets, 2000 chars), enforce trigger uniqueness per workspace, assign version, persist, record delta
    - Implement `update_snippet()`: validate, assign version, update, record delta
    - Implement `delete_snippet()`: set deleted_at, assign version, record delta
    - _Requirements: 2.1, 2.4, 2.5, 2.6, 2.7, 2.32, 2.33, 2.38, 2.39, 2.40, 5.21_

  - [x] 9.2 Implement `sync/service.rs` — folder CRUD and batch operations
    - Implement `create_folder()`: validate, enforce tier limits (free: 3 folders), assign version, persist, record delta
    - Implement `update_folder()`, `delete_folder()`
    - Implement `batch_operations()`: transactional execution of up to 100 items, sequential version assignment per item, reject entire batch on any validation failure with per-item errors
    - _Requirements: 2.2, 2.8, 2.9, 2.10, 2.34, 2.35, 2.36, 2.37, 5.21_

  - [x] 9.3 Implement `sync/service.rs` — snapshot and delta polling
    - Implement `get_snapshot()`: return all active folders + snippets + latest workspace version
    - Implement `get_deltas()`: return deltas where version > since_version, ordered ascending, with pagination (limit default 500, max 1000), has_more, next_since_version
    - Handle missing/invalid since_version → 422 INVALID_SINCE_VERSION
    - Handle since_version older than 30-day retention → 409 SNAPSHOT_REQUIRED
    - _Requirements: 2.11, 2.12, 2.13, 2.14, 2.15, 2.16_

  - [x] 9.4 Implement `sync/session_registry.rs` — WebSocket session management
    - Implement `SessionRegistry` with DashMap: workspace_id → Vec<WsSession>, user_id → Vec<UserConnection>, AtomicUsize total counter
    - Implement `register()`: enforce per-user limit (5 connections, close oldest if exceeded), server-wide limit (WS_MAX_CONNECTIONS, reject with 503)
    - Implement `unregister()`, `broadcast_to_workspace()` (exclude originator), `close_user_sessions()`, `close_workspace_sessions()`
    - _Requirements: 2.23, 2.24, 2.25, 7.42, 7.43, 7.44, 7.45_

  - [x] 9.5 Implement `sync/websocket.rs` — WebSocket upgrade, protocol, and heartbeat
    - Implement WS upgrade handler: validate access token (from header or query param), accept connection, associate with user workspaces
    - Implement reconnection catch-up: read workspace_id + last_known_version from query params, send missed deltas or snapshot_required message
    - Implement JSON envelope protocol: delta, snapshot_required, ack, error, ping, pong message types
    - Implement heartbeat: server ping every 30s, close after 2 missed pongs (10s timeout each)
    - Implement idle timeout: close after 5 min with no application messages
    - Connection survives token expiry; only explicit revocation (logout, suspend, password reset, lockout) closes via close_user_sessions with code 1008
    - _Requirements: 2.17, 2.18, 2.19, 2.20, 2.21, 2.22, 2.23, 2.26, 2.27, 2.28, 2.29, 2.30, 2.31, 7.46_

  - [x] 9.6 Implement `sync/handlers.rs` — REST sync handlers
    - Wire POST /sync/snippets, PATCH /sync/snippets/{id}, DELETE /sync/snippets/{id}, POST /sync/snippets/batch, POST /sync/folders, PATCH /sync/folders/{id}, DELETE /sync/folders/{id}, GET /sync/snapshot, GET /sync/deltas, GET /sync/ws
    - After each mutation, call `push_to_workspace()` to broadcast delta via WebSocket
    - _Requirements: 2.1–2.47_

  - [x] 9.7 Write property tests for sync service
    - **Property 2: Workspace-scoped version monotonicity**
    - **Property 3: Trigger uniqueness per workspace**
    - **Property 4: Batch operation atomicity**
    - **Property 5: Delta retention window enforcement**
    - **Property 26: Partial unique index allows trigger reuse after deletion**
    - **Validates: Requirements 2.4, 2.32, 2.34, 2.37, 2.16, 2.45, 6.13**

- [x] 10. Subscription service — tiers, checkout, webhooks
  - [x] 10.1 Implement `subscription/service.rs` — tier management and limit enforcement
    - Implement `create_free_subscription()`: create subscription with tier=free, status=active
    - Implement `initiate_upgrade()`: validate current tier is free, transition to pending checkout
    - Implement `enforce_tier_limits()`: for free tier enforce 10 snippets, 3 folders, 2000 char content; for pro/teams no limits
    - Implement soft-lock logic: when expired/cancelled and content exceeds free limits, return 422 CONTENT_SOFT_LOCKED on writes
    - _Requirements: 5.5, 5.6, 5.7, 5.21, 5.22, 5.23, 5.24, 5.25_

  - [x] 10.2 Implement `subscription/invoice.rs` — invoice calculation
    - Implement `compute_invoice()`: base_price, apply discount (percentage or flat, clamped to [0, base_price]), calculate tax from user's country_code, round to 2 decimal places
    - Implement `apply_discount()` and `calculate_tax()`
    - Enforce minimum 12-month billing cycle
    - Return structured Invoice object
    - _Requirements: 5.26, 5.27, 5.32, 5.33, 5.51, 5.52, 5.53_

  - [x] 10.3 Implement `subscription/service.rs` — coupon validation and checkout
    - Implement `validate_coupon()`: case-insensitive lookup, check active, valid_from, valid_until, max_uses vs times_used; return specific error codes for each failure
    - Implement `checkout()`: validate request, validate coupon if present, enforce no stacking (only one discount source), compute invoice, initiate billing provider session, return checkout URL
    - Increment times_used atomically within same transaction as subscription creation
    - _Requirements: 5.28, 5.34, 5.35, 5.36, 5.37, 5.38, 5.39, 5.40_

  - [x] 10.4 Implement `subscription/service.rs` — referral system
    - Implement referral code generation at registration (unique alphanumeric, create coupon_codes record with type=referral, 20% discount)
    - Implement `apply_referral_reward()`: on first paid subscription by referred user, mark referral as converted, add 1 month to referrer's period_end (or store credit if no active sub)
    - Enforce idempotency: unique constraint on (referrer_id, referred_user_id), no error on duplicate
    - _Requirements: 5.41, 5.42, 5.43, 5.44, 5.45, 5.46, 5.47, 5.48, 5.49_

  - [x] 10.5 Implement `subscription/webhook.rs` — billing webhook processing
    - Implement signature verification against BILLING_WEBHOOK_SECRET
    - Implement `process_webhook()`: parse event type, handle subscription.activated/renewed/past_due/cancelled/reactivated transitions
    - Implement idempotency via billing_events table (external_event_id dedup)
    - Set grace_period_end on past_due (7 days from event)
    - Respond within 5 seconds; defer heavy processing to async task
    - _Requirements: 5.55, 5.56, 5.57, 5.58_

  - [x] 10.6 Implement `subscription/service.rs` — grace period and payment deadline checks
    - Implement `check_grace_periods()`: find past_due subscriptions where grace_period_end has passed, transition to cancelled, enforce free limits with soft-lock
    - Implement `check_payment_deadlines()`: find team workspaces with pending_payment past 7-day deadline, set to deactivated
    - _Requirements: 5.10, 5.11, 5.30, 5.31_

  - [x] 10.7 Implement `subscription/handlers.rs` — subscription HTTP handlers
    - Wire POST /subscriptions/upgrade, POST /subscriptions/checkout, GET /subscriptions/current, POST /webhooks/billing
    - _Requirements: 5.5–5.60_

  - [x] 10.8 Write property tests for subscription service
    - **Property 13: Per-tier feature limits enforcement**
    - **Property 14: Soft-lock on downgrade**
    - **Property 15: Grace period transitions**
    - **Property 16: Referral reward idempotency**
    - **Property 17: Billing webhook idempotency**
    - **Property 19: Invoice calculation correctness**
    - **Property 20: Coupon validation completeness**
    - **Validates: Requirements 5.21, 5.23, 5.24, 5.25, 5.30, 5.31, 5.48, 5.57, 5.33, 5.51, 5.53, 5.37, 5.38, 5.39**

- [x] 11. Checkpoint - Ensure sync and subscription services compile and pass tests
  - Ensure all tests pass, ask the user if questions arise.

- [x] 12. AI service
  - [x] 12.1 Implement `ai/service.rs` — AI expansion with quota and concurrency
    - Implement `expand()`: validate inputs (trigger required, max 500 chars; system_prompt required, max 10KB; context optional, max 50KB), check quota, acquire semaphore, call provider, return result
    - Implement `check_quota()`: sliding 24h window per user_id; free=50, pro/teams=1000; past_due during grace period uses paid limit; cancelled/expired uses free limit
    - Implement `call_provider()`: HTTP POST to AI_PROVIDER_URL with 10s timeout; return 502 AI_PROVIDER_UNAVAILABLE on error/timeout, 502 AI_PROVIDER_INVALID_RESPONSE on bad response
    - Implement concurrency control: tokio Semaphore (AI_MAX_CONCURRENT_REQUESTS default 50), queue up to 100 with 5s timeout, return 429 AI_SERVICE_BUSY when full
    - _Requirements: 3.1–3.16, 7.63, 7.64, 7.65_

  - [x] 12.2 Implement `ai/handlers.rs` — AI HTTP handler
    - Wire POST /ai/expand handler, extract claims, validate client_type=native, call AiService::expand()
    - _Requirements: 3.1, 3.12, 3.13_

  - [x] 12.3 Write property test for AI quota enforcement
    - **Property 27: AI tier-based quota enforcement**
    - **Validates: Requirements 3.4, 3.5, 3.6, 3.7**

- [x] 13. Admin service
  - [x] 13.1 Implement `admin/service.rs` — user management
    - Implement `list_users()`: paginated with filters (search, role, subscription_tier, status)
    - Implement `get_user()`: full user detail with subscriptions, workspaces, referrals
    - Implement `suspend_user()`: set status=suspended, revoke all refresh tokens, close all WebSocket connections (code 1008); block self-action and action on other admins
    - Implement `unsuspend_user()`: set status=active
    - Implement `force_password_reset()`: set must_reset_password=true
    - Implement `delete_user()`: soft-delete with 30-day retention; block self-action and action on admins
    - _Requirements: 4.4–4.15_

  - [x] 13.2 Implement `admin/service.rs` — workspace management
    - Implement `list_workspaces()`: paginated with filters (type, subscription_status)
    - Implement `get_workspace()`: full detail with members, snippet count, folder count, subscription
    - Implement `deactivate_workspace()`: set subscription status=deactivated, deny member access
    - Implement `delete_workspace()`: require confirm=true, hard-delete workspace and all associated data; block deletion of individual workspaces without deleting user
    - _Requirements: 4.16–4.21_

  - [x] 13.3 Implement `admin/service.rs` — discount and coupon management
    - Implement `list_discounts()`, `create_discount()`, `update_discount()` (no delete, deactivate only)
    - Implement `list_coupons()` (paginated, filterable by type/active), `get_coupon()`, `create_coupon()` (type=platform, enforce code uniqueness case-insensitive), `update_coupon()`
    - Implement `get_referral_stats()`: total_referrals, converted_referrals, top_referrers (top 10), conversion_rate
    - _Requirements: 4.22–4.33_

  - [x] 13.4 Implement `admin/service.rs` — subscription and billing oversight
    - Implement `list_subscriptions()` (paginated, filterable by tier/status/workspace_id/expiry range)
    - Implement `get_subscription()`: full detail with billing event history
    - Implement `extend_subscription()`: add months/days to period_end, audit log
    - Implement `cancel_subscription()`: immediate cancellation (no grace period), soft-lock, close WS connections for workspace members
    - Implement `override_tier()`: update tier without billing provider interaction; validate tier values
    - Implement `list_billing_events()`: paginated with filters
    - _Requirements: 4.34–4.41_

  - [x] 13.5 Implement `admin/service.rs` — tax rates, audit logs, feature flags, admin management, stats
    - Tax rates: `list_tax_rates()`, `create_tax_rate()` (conflict on existing country_code), `update_tax_rate()` (no delete, deactivate only)
    - Audit logs: `list_audit_logs()` (paginated, filterable), `get_audit_log()` (immutable, no update/delete)
    - Feature flags: `list_feature_flags()`, `create_feature_flag()` (validate kebab-case max 100 chars, enforce uniqueness), `update_feature_flag()`, `delete_feature_flag()`
    - Admin management: `list_admins()`, `demote_admin()` (block self-demote, block last admin removal)
    - Stats: `get_overview_stats()`, `get_referral_analytics()` (computed on-demand)
    - _Requirements: 4.42–4.67_

  - [x] 13.6 Implement `admin/handlers.rs` — wire all admin HTTP handlers
    - Wire all 38 admin endpoints: users (6), workspaces (4), discounts (3), coupons (4), referrals (1), subscriptions (6), billing-events (1), tax-rates (3), audit-logs (2), feature-flags (4), admins (2), invites (1), stats (2)
    - Write audit log on every admin action (including reads)
    - _Requirements: 4.1–4.67_

  - [x] 13.7 Write property tests for admin service
    - **Property 11: Admin cannot act on self**
    - **Property 12: Admin cannot act on other admins**
    - **Property 21: Audit log immutability**
    - **Validates: Requirements 4.7, 4.8, 4.14, 4.15, 4.50, 4.51, 4.57**

- [x] 14. Email service
  - [x] 14.1 Implement `email/service.rs` — provider abstraction and async dispatch
    - Define `EmailProvider` trait with async `send()` method
    - Implement `EmailService` with provider selection based on AppConfig::email_provider
    - Implement `send_with_retry()`: exponential backoff (1s → 5s → 30s, 3 attempts), log at ERROR on final failure
    - All sends dispatched via `tokio::spawn` (async, non-blocking to HTTP handler)
    - _Requirements: 7.1, 7.6, 7.7, 7.8_

  - [x] 14.2 Implement `email/smtp.rs` and `email/api_provider.rs` — provider implementations
    - SMTP: implement using `lettre` crate with TLS, read host/port/user/password from config
    - API: implement using `reqwest` with API key/URL from config
    - _Requirements: 7.2, 7.3_

  - [x] 14.3 Implement `email/templates.rs` — HTML + plaintext email templates
    - Implement templates using `askama`: password_reset, email_change_verify, email_change_notification, admin_invite, team_invite
    - Each template renders both HTML and plaintext variants
    - _Requirements: 7.5_

- [x] 15. Background scheduler
  - [x] 15.1 Implement `scheduler/service.rs` — task scheduler with recurring tasks
    - Implement `SchedulerService` with tokio interval-based task execution
    - Register all 6 recurring tasks: delta_purge (1h), soft_delete_cleanup (6h), expired_token_cleanup (1h), grace_period_check (1h), payment_deadline_check (1h), account_hard_delete_check (24h)
    - Implement retry on transient failure: 3 attempts with exponential backoff (1s → 5s → 30s)
    - Implement idempotency: all tasks use timestamp-based filtering
    - Implement graceful shutdown: stop spawning new tasks, wait for in-progress tasks up to SHUTDOWN_TIMEOUT_SECS
    - _Requirements: 7.9, 7.10, 7.11, 7.12, 7.13, 7.14_

- [x] 16. Main entry point and startup wiring
  - [x] 16.1 Implement `main.rs` — full startup sequence
    - Load AppConfig from environment
    - Initialize structured JSON logger
    - Create PgPool, run pending migrations, execute seed data
    - Initialize all services (AuthService, SyncService, AiService, SubscriptionService, WorkspaceService, TeamService, AdminService, EmailService)
    - Start background scheduler
    - Verify email service connectivity (warmup)
    - Build Axum router with all routes and middleware
    - Bind to 0.0.0.0:{PORT}, set readiness flag
    - Implement graceful shutdown handler (SIGTERM/SIGINT): stop listener, close WS sessions with 1001, stop scheduler, drain in-flight requests, close pool, exit 0
    - _Requirements: 7.55, 7.56, 7.57, 7.58, 7.66–7.72_

  - [x] 16.2 Implement health and readiness endpoints
    - GET /health: verify Postgres reachability, return {status: "ok", db: "ok"} or {status: "degraded", db: "error"} with 503
    - GET /ready: check migrations complete, pool established, scheduler started, email initialized; return 200 or 503 with per-check status
    - _Requirements: 7.55, 7.56, 7.57, 7.58_

- [x] 17. Checkpoint - Full application compiles and starts successfully
  - Ensure all tests pass, ask the user if questions arise.

- [x] 18. Integration testing
  - [x] 18.1 Write integration tests for auth flows
    - Test register → login → refresh → logout flow
    - Test OAuth mock flow
    - Test brute-force lockout and unlock
    - Test password reset full flow
    - Test account deletion and reactivation within 30-day window
    - Test session limit enforcement
    - _Requirements: 1.7–1.53_

  - [x] 18.2 Write integration tests for sync service
    - Test snippet CRUD with version assignment
    - Test folder CRUD with version assignment
    - Test batch operations (success and rollback on failure)
    - Test delta polling with pagination
    - Test snapshot retrieval
    - Test trigger uniqueness enforcement and reuse after soft-delete
    - Test WebSocket connection, heartbeat, and delta push
    - _Requirements: 2.1–2.47_

  - [x] 18.3 Write integration tests for subscription and billing
    - Test free → pro upgrade checkout flow with invoice calculation
    - Test coupon validation (all error cases)
    - Test referral flow (register with code → convert on first paid sub → reward applied)
    - Test billing webhook processing (all event types + idempotency)
    - Test grace period transition (past_due → cancelled after 7 days)
    - Test soft-lock enforcement on downgrade
    - _Requirements: 5.5–5.60_

  - [x] 18.4 Write integration tests for admin service
    - Test user suspend/unsuspend/force-reset/delete flows
    - Test workspace deactivation and hard-delete (with confirm)
    - Test coupon/discount CRUD
    - Test subscription extend/cancel/tier-override
    - Test feature flag CRUD
    - Test admin demote (self-demote blocked, last admin blocked)
    - _Requirements: 4.1–4.67_

  - [x] 18.5 Write integration tests for middleware stack
    - Test client type enforcement across all endpoint groups
    - Test rate limiting (IP and user-level)
    - Test CORS preflight handling
    - Test security headers present in responses
    - Test body size limit enforcement
    - Test error response format consistency (trace_id, error code, details)
    - **Property 1: Client type endpoint enforcement**
    - **Property 22: Error response format consistency**
    - **Property 23: Rate limiting sliding window**
    - **Property 24: Client IP resolution**
    - **Validates: Requirements 1.5, 1.6, 7.22–7.27, 7.47–7.53, 7.83, 7.84**

- [x] 19. Final checkpoint - All tests pass, application starts and serves requests
  - Ensure all tests pass, ask the user if questions arise.

## Notes

- Tasks marked with `*` are optional and can be skipped for faster MVP
- Each task references specific requirements for traceability
- Checkpoints ensure incremental validation
- Property tests validate universal correctness properties from the design document
- The implementation language is Rust with Axum framework
- All database queries should use `sqlx::query!` macro for compile-time verification
- Integration tests should use a test database with migrations applied fresh per test suite

## Task Dependency Graph

```json
{
  "waves": [
    { "id": 0, "tasks": ["1.1"] },
    { "id": 1, "tasks": ["1.2", "1.3"] },
    { "id": 2, "tasks": ["2.1", "2.2", "2.4"] },
    { "id": 3, "tasks": ["2.3", "3.1"] },
    { "id": 4, "tasks": ["4.1", "4.2", "4.3", "4.4", "4.5"] },
    { "id": 5, "tasks": ["4.6", "4.7", "4.8", "4.9", "4.10"] },
    { "id": 6, "tasks": ["4.11"] },
    { "id": 7, "tasks": ["6.1", "6.2"] },
    { "id": 8, "tasks": ["6.3", "6.4", "6.5", "6.6"] },
    { "id": 9, "tasks": ["6.7", "6.8", "6.9", "6.10", "6.11"] },
    { "id": 10, "tasks": ["6.12", "7.1"] },
    { "id": 11, "tasks": ["6.13", "7.2"] },
    { "id": 12, "tasks": ["7.3", "7.4"] },
    { "id": 13, "tasks": ["9.1"] },
    { "id": 14, "tasks": ["9.2", "9.3", "9.4"] },
    { "id": 15, "tasks": ["9.5", "9.6"] },
    { "id": 16, "tasks": ["9.7", "10.1"] },
    { "id": 17, "tasks": ["10.2", "10.3"] },
    { "id": 18, "tasks": ["10.4", "10.5", "10.6"] },
    { "id": 19, "tasks": ["10.7", "10.8"] },
    { "id": 20, "tasks": ["12.1", "13.1"] },
    { "id": 21, "tasks": ["12.2", "12.3", "13.2", "13.3"] },
    { "id": 22, "tasks": ["13.4", "13.5"] },
    { "id": 23, "tasks": ["13.6", "13.7"] },
    { "id": 24, "tasks": ["14.1"] },
    { "id": 25, "tasks": ["14.2", "14.3"] },
    { "id": 26, "tasks": ["15.1"] },
    { "id": 27, "tasks": ["16.1", "16.2"] },
    { "id": 28, "tasks": ["18.1", "18.2", "18.3", "18.4"] },
    { "id": 29, "tasks": ["18.5"] }
  ]
}
```
