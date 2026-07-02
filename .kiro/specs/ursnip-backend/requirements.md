# Requirements Document

## Introduction

The `ursnip-backend` is a greenfield Rust/Axum monolithic HTTP server that acts as the cloud counterpart to the Keystone desktop application. It provides seven primary capabilities: authentication (email+password and OAuth), snippet synchronization (WebSocket push and REST polling fallback), AI-powered snippet expansion, an admin management API, a comprehensive subscription and billing service (covering seat management, per-tier feature limits, billing cycles, discounts, coupon codes, a referral system, country-based tax calculation, and billing webhook ingestion), database migration management, and cross-cutting platform concerns such as rate limiting, distributed tracing, health checks, and environment-based configuration.

The desktop client already implements a Sync Manager with a WebSocket path and a polling fallback (`GET /sync/deltas?since_version=N`), an AI client calling `POST /ai/expand`, and an Auth Manager that stores JWTs. This backend fulfils the server-side contract those components expect.

---

## Glossary

- **Backend**: The `ursnip-backend` Rust/Axum HTTP server described in this document.
- **Client**: The Keystone desktop application (Tauri/Rust) or the web application, identified by a Client Type claim in the JWT.
- **User**: An authenticated human end-user of the Keystone desktop application or web application.
- **Admin**: A human operator with the `admin` role claim in a JWT, authorized to manage users, subscriptions, and feature flags via the Admin API. Admin accounts cannot self-register and are created only via admin invites or deployment seeding.
- **JWT**: A JSON Web Token containing `sub` (user_id), `client_type`, `role`, `permissions`, `subscription_tier`, and `exp` claims, signed with a secret known only to the Backend.
- **Access Token**: A short-lived JWT (15 minutes for regular users, 5 minutes for admins) used to authenticate API requests.
- **Refresh Token**: A long-lived opaque token (30 days) stored in the `refresh_tokens` table, used to issue new Access Tokens.
- **Client Type**: One of `native` or `web`, identifying the application from which a request originates. Included in JWT claims to enforce endpoint restrictions.
- **Password Reset Token**: A cryptographically secure, single-use, time-limited token (30-minute TTL) stored hashed (SHA-256) in the `password_reset_tokens` table, used to authorize a password reset.
- **Account Soft-Delete**: A reversible deletion where `deleted_at` is set on the `users` record. The account remains recoverable for 30 days before permanent hard-deletion.
- **Admin Invite**: A time-limited (24h), single-use invitation sent by an existing admin to create a new admin account. Stored in an `admin_invites` table.
- **Session**: A refresh token record representing an active login on a specific device. Users may have up to 5 concurrent sessions.
- **Audit Log**: An immutable record of admin actions and security events stored in the `audit_logs` table with fields: `id`, `admin_id`, `user_id`, `action`, `ip_address`, `user_agent`, `client_type`, `target_resource`, `target_id`, `result`, `metadata`, `trace_id`, `created_at`.
- **Snippet**: A workspace-owned record comprising a trigger string, content, type, and workspace version integer, stored in the `snippets` Postgres table. Attributed to the creating user via `created_by`.
- **Delta**: A single sync change (snippet upsert, snippet soft-delete, folder upsert, or folder soft-delete) within a workspace, carrying a workspace version number.
- **Version**: See Workspace Version.
- **Workspace Version**: A per-workspace monotonically increasing integer assigned to each sync mutation within that workspace; used to order and filter deltas.
- **Snapshot**: A full point-in-time state of a workspace including all active folders and snippets, returned when delta history is insufficient for incremental sync.
- **Delta Retention**: The 30-day period during which sync delta records are kept before being permanently purged.
- **Batch Operation**: A single API request containing multiple snippet mutations (up to 100) executed transactionally within one workspace version sequence.
- **Workspace**: An organisational container for snippets and members. Every account belongs to at least one workspace. Stored in the `workspaces` table with `id`, `type`, `owner_id`, `name`, `created_at`.
- **Individual Workspace**: A workspace of type `individual`, automatically created at registration and tied to the user's personal subscription (`free` or `pro`). Each user has exactly one.
- **Team Workspace**: A workspace of type `team`, created explicitly by a user who becomes the owner. Has its own `teams` tier subscription. Supports up to 3 seats (including the owner). Ownership is non-transferable.
- **Workspace Member**: A record in the `workspace_members` table (`workspace_id`, `user_id`, `role`, `joined_at`) associating a user with a workspace. `role` is either `owner` or `member`.
- **Invite Link**: A unique, time-limited URL containing an `invite_code` that allows a user to join a Team Workspace. Stored in the `team_invites` table with `id`, `workspace_id`, `invite_code`, `max_uses`, `times_used`, `expires_at`, `created_by`, `created_at`.
- **Subscription Tier**: One of `free`, `pro`, or `teams`. `free` and `pro` apply to Individual Workspaces. `teams` applies to Team Workspaces.
- **Seat**: An authorization slot within a Team Workspace allowing one User to access team features. A Team Workspace has a maximum of 3 seats (including the owner).
- **Billing Cycle**: The subscription payment period, minimum 12 months.
- **Discount**: A price reduction applied to a subscription, either `percentage` (a ratio of the base price) or `flat` (a fixed currency amount).
- **Coupon Code**: A redeemable alphanumeric code stored in the `coupon_codes` table with a `type` field of either `platform` (manually created by Admin) or `referral` (auto-generated per account at registration). Maps to a Discount. Only one code can be applied per purchase.
- **Referral Code**: A coupon code of type `referral`, auto-generated for every User at registration, shareable to refer new users. Pre-filled during referred user registration and persists through to the first subscription checkout.
- **Referrer**: The User whose referral code was used during a new User's registration.
- **Tax Rate**: A country-specific tax percentage applied to the discounted subscription subtotal.
- **Invoice**: A structured pricing breakdown returned at checkout including base price, discount, tax, and total.
- **Grace Period**: A 7-day window after subscription status transitions to `past_due`, during which the user retains paid-tier access before automatic cancellation.
- **snippet_folders**: A junction table allowing many-to-many assignment of snippets to folders.
- **WebSocket Session**: A persistent, authenticated WebSocket connection over which the Backend pushes snippet deltas to the Client in real time.
- **Billing Provider**: An external service (e.g., Paddle, LemonSqueezy) that sends webhook events to update subscription status.
- **AI Provider**: An external upstream HTTP service that performs text expansion given a trigger, system prompt, and optional context.
- **Argon2**: The password-hashing algorithm used by the Auth Service.
- **OAuth Provider**: Google or GitHub, accessed via OAuth 2.0 authorization-code flow.
- **Admin API**: The set of REST endpoints restricted to `role = admin` JWT claims.
- **Feature Flag**: A named boolean stored in the `feature_flags` table, toggled by Admins and evaluated by the Backend at request time.
- **sqlx**: The Rust async Postgres driver and migration tool used by the Backend.
- **Rate Limiter**: The per-IP and per-user sliding-window counter enforced by the Backend middleware.

---

## Requirements

### Requirement 1: Authentication Service

**User Story:** As a User, I want to register, log in, refresh my session, reset my password, manage my profile, and link OAuth accounts from either the native desktop app or the web app, so that I can securely access my account with role-based authorization and multi-client support.

#### Acceptance Criteria

**Multi-Client Authentication and JWT Structure**

1. THE Backend SHALL support two Client Types: `native` (installed desktop/mobile app with sync capabilities) and `web` (browser-based app for landing pages, auth, checkout, account management, and admin panel).

2. THE Backend SHALL issue Access Tokens containing the following claims: `sub` (user_id), `client_type` (`native` or `web`), `role` (`user` or `admin`), `permissions` (array), `subscription_tier`, and `exp`.

3. THE Backend SHALL issue Access Tokens with a TTL of 15 minutes for Users with `role = user` and a TTL of 5 minutes for Users with `role = admin`.

4. THE Backend SHALL issue Refresh Tokens with a TTL of 30 days, stored in the `refresh_tokens` table with associated `user_id`, `client_type`, `created_at`, and `expires_at`.

5. THE Backend SHALL enforce endpoint restrictions based on `client_type` claim: `/sync/*` endpoints accept `native` only; `/profile/*` endpoints accept `native` and `web`; `/checkout/*`, `/subscriptions/*`, and `/teams/*` endpoints accept `web` only; `/admin/*` endpoints accept `web` only (with additional `role = admin` check); `/auth/*` endpoints accept `native` and `web`; `/ai/expand` accepts `native` only; `/health` requires no authentication; `/webhooks/*` requires signature verification with no JWT.

6. IF a request is received with a valid JWT but the `client_type` claim does not match the endpoint restriction, THEN THE Backend SHALL return HTTP 403 with error code `CLIENT_TYPE_NOT_ALLOWED`.

**Registration**

7. WHEN a `POST /auth/register` request is received with a valid `email` and `password`, THE Backend SHALL hash the password with Argon2id and persist a new user record in the `users` table with `role = user`, returning an Access Token and Refresh Token pair with HTTP 201.

7a. THE Backend SHALL accept optional `first_name` and `last_name` fields in the `POST /auth/register` request body and persist them on the user record if provided.

7b. THE Backend SHALL include the user's Individual Workspace `workspace_id` in the authentication response (register and login) so that clients can use it for subscription and sync operations without a separate lookup.

8. IF a `POST /auth/register` request arrives with an `email` that already exists in the `users` table, THEN THE Backend SHALL return HTTP 409 with a machine-readable error code `EMAIL_ALREADY_REGISTERED`.

9. IF a `POST /auth/register` request arrives with a `password` shorter than 8 characters, THEN THE Backend SHALL return HTTP 422 with error code `PASSWORD_TOO_SHORT` without creating any record.

10. THE Backend SHALL NOT allow admin account creation via `POST /auth/register`; admin accounts are created exclusively via admin invites or deployment seeding.

**Login**

11. WHEN a `POST /auth/login` request is received with a valid `email` and correct `password`, THE Backend SHALL verify the Argon2id hash, include the User's `role` in the response body so the client can route to the appropriate UI, and return a new Access Token and Refresh Token pair with HTTP 200.

12. IF a `POST /auth/login` request is received with an unrecognized `email` or an incorrect `password`, THEN THE Backend SHALL return HTTP 401 with error code `INVALID_CREDENTIALS`, taking no fewer than 100 ms to respond (to resist timing attacks).

13. WHEN a `POST /auth/login` request is received for a soft-deleted account (where `deleted_at` is set and within the 30-day retention window), THE Backend SHALL reactivate the account by unsetting `deleted_at` and return a new Access Token and Refresh Token pair with HTTP 200.

**Token Refresh and Logout**

14. WHEN a `POST /auth/refresh` request is received with a valid, unexpired Refresh Token, THE Backend SHALL invalidate the presented Refresh Token, issue a new Access Token and Refresh Token pair, and return them with HTTP 200.

15. IF a `POST /auth/refresh` request is received with an expired or previously invalidated Refresh Token, THEN THE Backend SHALL return HTTP 401 with error code `INVALID_REFRESH_TOKEN`.

16. WHEN a `POST /auth/logout` request is received with a valid Access Token, THE Backend SHALL invalidate the associated Refresh Token in the `refresh_tokens` table and return HTTP 204.

**OAuth Multi-Client Flow**

17. WHEN a `GET /auth/oauth/{provider}/authorize` request is received where `{provider}` is `google` or `github` and `client` query parameter is `native`, THE Backend SHALL redirect to the OAuth Provider's authorization URL with a `redirect_uri` set to a deep-link URI (e.g., `ursnip://oauth/callback`).

18. WHEN a `GET /auth/oauth/{provider}/authorize` request is received where `{provider}` is `google` or `github` and `client` query parameter is `web`, THE Backend SHALL redirect to the OAuth Provider's authorization URL with a `redirect_uri` set to the web callback URL (e.g., `https://app.example.com/auth/callback`).

19. WHEN the OAuth Provider redirects to `GET /auth/oauth/{provider}/callback` with a valid authorization code, THE Backend SHALL exchange the code for an OAuth access token, retrieve the user's verified email from the OAuth Provider, upsert a user record in the `users` table, and return a Backend Access Token and Refresh Token pair with the `client_type` claim matching the originating client.

20. IF the OAuth Provider redirects to `GET /auth/oauth/{provider}/callback` with an `error` parameter, THEN THE Backend SHALL return HTTP 401 with error code `OAUTH_AUTHORIZATION_DENIED`.

**OAuth Account Linking**

21. WHEN an OAuth Provider returns a verified email that matches an existing email+password account in the `users` table, THE Backend SHALL link the OAuth identity to the existing account (auto-merge) without creating a duplicate user record.

22. WHEN multiple OAuth Providers return the same verified email, THE Backend SHALL link all OAuth identities to the same user account.

23. IF an OAuth Provider does NOT return a verified email, THEN THE Backend SHALL return HTTP 422 with error code `EMAIL_VERIFICATION_REQUIRED`.

24. IF an OAuth identity's verified email matches a different existing user account and the requesting user is already authenticated with a separate account, THEN THE Backend SHALL return HTTP 409 with error code `ACCOUNT_LINKING_CONFLICT` and require the user to authenticate with the existing account method before linking.

**Forgot Password and Password Reset**

25. WHEN a `POST /auth/forgot-password` request is received with a valid `email`, THE Backend SHALL generate a cryptographically secure random token, store the token hashed with SHA-256 in the `password_reset_tokens` table with a 30-minute TTL, invalidate any previously issued reset tokens for that user, and send a reset link email to the address.

26. IF a `POST /auth/forgot-password` request is received with an email that does not exist in the `users` table, THEN THE Backend SHALL return HTTP 200 (identical to the success response) to prevent email enumeration.

27. THE Backend SHALL enforce a rate limit of 3 `POST /auth/forgot-password` requests per hour per email address; requests exceeding this limit SHALL receive HTTP 429 with a `Retry-After` header.

28. WHEN a `POST /auth/reset-password` request is received with a valid, unexpired, single-use token and a new `password` (minimum 8 characters), THE Backend SHALL hash the new password with Argon2id, update the user's password in the `users` table, mark the reset token as used, revoke ALL Refresh Tokens for that user in the `refresh_tokens` table, and return HTTP 200.

29. IF a `POST /auth/reset-password` request is received with an expired, already-used, or invalid token, THEN THE Backend SHALL return HTTP 422 with error code `INVALID_RESET_TOKEN`.

**Profile Management**

30. WHEN an authenticated User sends `PATCH /auth/profile` with any combination of the updatable fields (`first_name`, `last_name`, `profile_picture_url`, `timezone`, `language`, `country_code`, `phone`), THE Backend SHALL update the specified fields on the user record and return the updated profile with HTTP 200.

31. WHEN an authenticated User sends `POST /auth/change-email` with a `new_email`, THE Backend SHALL generate a verification token (TTL: 24 hours), store it in a `email_change_requests` table, and send a verification email to the new address containing a verification link.

32. WHEN a `GET /auth/verify-email-change?token=...` request is received with a valid, unexpired verification token, THE Backend SHALL update the user's email to the new email, send a notification to the previous email address informing of the change, mark the token as used, and return HTTP 200.

33. IF an email change verification token expires (after 24 hours) without being used, THEN THE Backend SHALL silently discard the change request with no notification to the user.

34. WHEN an authenticated User sends `POST /auth/change-password` with `current_password` and `new_password` (minimum 8 characters), THE Backend SHALL verify the current password, hash the new password with Argon2id, update the user record, revoke ALL Refresh Tokens for that user, and return HTTP 200.

35. IF a `POST /auth/change-password` request contains an incorrect `current_password`, THEN THE Backend SHALL return HTTP 401 with error code `INVALID_CURRENT_PASSWORD`.

**Account Deletion**

36. WHEN an authenticated User sends `DELETE /auth/account`, THE Backend SHALL soft-delete the account by setting `deleted_at` to the current UTC timestamp and return HTTP 204.

37. IF a User who owns one or more Team Workspaces sends `DELETE /auth/account`, THEN THE Backend SHALL return HTTP 422 with error code `TRANSFER_OWNERSHIP_REQUIRED` indicating the user must delete those Team Workspaces before deleting the account.

38. WHILE an account is soft-deleted and within the 30-day retention period, THE Backend SHALL retain all user data and allow reactivation upon successful login (per criterion 13).

39. WHEN 30 days have elapsed since an account's `deleted_at` timestamp without reactivation, THE Backend SHALL permanently hard-delete all user data associated with that account.

**Admin Account Creation**

40. WHEN the Backend is deployed for the first time, THE Backend SHALL seed an initial super-admin account via a migration or environment-based seed script.

41. WHEN an authenticated Admin sends `POST /admin/invites` with an `email`, THE Backend SHALL generate a time-limited (24-hour), single-use invite token, store it in the `admin_invites` table, and send an invite link to the specified email address, returning HTTP 201.

42. WHEN a user registers via a valid admin invite link, THE Backend SHALL create the account with `role = admin` and mark the invite token as used.

43. IF an admin invite token is expired (older than 24 hours) or already used, THEN THE Backend SHALL return HTTP 422 with error code `INVITE_EXPIRED`.

**Admin Session Security**

44. WHILE a User has `role = admin`, THE Backend SHALL issue Access Tokens with a TTL of 5 minutes (instead of 15 minutes for regular users).

45. THE Backend SHALL enforce Refresh Token rotation for admin sessions; every token refresh SHALL invalidate the previous Refresh Token and issue a new one.

46. WHEN an Admin performs any action via the Admin API, THE Backend SHALL write an Audit Log record containing `admin_id`, `action`, `target_resource`, `target_id`, `metadata`, and `created_at` to the `audit_logs` table.

**Concurrent Session Management**

47. THE Backend SHALL enforce a maximum of 5 active Refresh Tokens (Sessions) per user; when a 6th login occurs, THE Backend SHALL revoke the oldest Refresh Token.

48. WHEN an authenticated User sends `GET /auth/sessions`, THE Backend SHALL return a list of active Sessions for that user including `session_id`, `client_type`, `created_at`, and `last_used_at`.

49. WHEN an authenticated User sends `DELETE /auth/sessions/{session_id}`, THE Backend SHALL revoke the specified Refresh Token and return HTTP 204.

**Brute-Force Protection**

50. WHEN 5 consecutive failed login attempts are recorded for a given email address, THE Backend SHALL lock the account for 15 minutes.

51. WHILE an account is locked due to brute-force protection, THE Backend SHALL return HTTP 429 with error code `ACCOUNT_LOCKED` and a `Retry-After` header indicating remaining lockout time on any login attempt for that email.

52. WHEN a successful login occurs, THE Backend SHALL reset the failed attempt counter for that email to zero.

**Refresh Token Rotation and Reuse Detection**

53. IF a previously invalidated Refresh Token is presented to `POST /auth/refresh`, THEN THE Backend SHALL treat the event as token theft, revoke ALL Refresh Tokens for that user, return HTTP 401 with error code `TOKEN_REUSE_DETECTED`, and log a security event with IP, user_agent, and timestamp.

---

### Requirement 2: Snippet Synchronization Service

**User Story:** As a Client, I want to receive snippet and folder changes in real time over WebSocket and fall back to REST polling, so that the local workspace cache stays consistent with the server-side canonical store across all workspace members.

#### Acceptance Criteria

**Workspace-Aware Data Model**

1. THE Backend SHALL store each snippet in the `snippets` Postgres table with columns: `id` (UUID), `workspace_id` (FK to `workspaces`), `created_by` (FK to `users`), `trigger` (text), `content` (text), `snippet_type` (text), `version` (bigint), `created_at` (UTC timestamp), `updated_at` (UTC timestamp), `deleted_at` (nullable UTC timestamp).

2. THE Backend SHALL store each folder in the `folders` Postgres table with columns: `id` (UUID), `workspace_id` (FK to `workspaces`), `name` (text), `created_by` (FK to `users`), `version` (bigint), `created_at` (UTC timestamp), `updated_at` (UTC timestamp), `deleted_at` (nullable UTC timestamp).

3. THE Backend SHALL scope every snippet and folder to exactly one workspace; Users access snippets and folders exclusively through their workspace membership.

4. THE Backend SHALL maintain a Workspace Version as an independent per-workspace monotonically increasing integer sequence; each sync mutation within a workspace SHALL be assigned the next value in that workspace's sequence.

**Snippet CRUD and Version Tracking**

5. WHEN a Client sends a `POST /sync/snippets` request with fields `trigger`, `content`, `snippet_type`, `workspace_id`, and optional `folder_id`, THE Backend SHALL validate the payload, assign the next Workspace Version, persist the snippet in the `snippets` table with `created_by` set to the authenticated user, record the change in the `sync_deltas` table, and return the created snippet with HTTP 201.

6. WHEN a Client sends a `PATCH /sync/snippets/{id}` request with updatable fields, THE Backend SHALL validate the payload, assign the next Workspace Version, update the snippet record, record the change in the `sync_deltas` table, and return the updated snippet with HTTP 200.

7. WHEN a Client sends a `DELETE /sync/snippets/{id}` request for a snippet within a workspace the Client is a member of, THE Backend SHALL set `deleted_at` to the current UTC timestamp, assign the next Workspace Version, record a soft-delete delta in the `sync_deltas` table, and return HTTP 204.

**Folder CRUD and Version Tracking**

8. WHEN a Client sends a `POST /sync/folders` request with fields `name` and `workspace_id`, THE Backend SHALL validate the payload, assign the next Workspace Version, persist the folder in the `folders` table with `created_by` set to the authenticated user, record the change in the `sync_deltas` table, and return the created folder with HTTP 201.

9. WHEN a Client sends a `PATCH /sync/folders/{id}` request with updatable fields (e.g., `name`), THE Backend SHALL validate the payload, assign the next Workspace Version, update the folder record, record the change in the `sync_deltas` table, and return the updated folder with HTTP 200.

10. WHEN a Client sends a `DELETE /sync/folders/{id}` request for a folder within a workspace the Client is a member of, THE Backend SHALL set `deleted_at` to the current UTC timestamp, assign the next Workspace Version, record a soft-delete delta in the `sync_deltas` table, and return HTTP 204.

**Initial Snapshot Synchronization**

11. WHEN an authenticated Client sends `GET /sync/snapshot?workspace_id=<id>`, THE Backend SHALL return a JSON response containing: workspace metadata, all active folders (where `deleted_at IS NULL`), all active snippets (where `deleted_at IS NULL`), and the latest Workspace Version number, with HTTP 200.

12. IF a Client requests a snapshot for a workspace they are not a member of, THEN THE Backend SHALL return HTTP 403 with error code `NOT_A_WORKSPACE_MEMBER`.

**Delta Polling with Pagination**

13. WHEN a `GET /sync/deltas?workspace_id=<id>&since_version=N` request is received from an authenticated Client, THE Backend SHALL return all `sync_deltas` for that workspace where `version > N`, ordered by `version` ascending, with HTTP 200.

14. WHEN the `GET /sync/deltas` request includes an optional `limit` query parameter, THE Backend SHALL cap the returned deltas at the specified limit (default 500, maximum 1000) and include `has_more` (boolean) and `next_since_version` (integer) fields in the response to support pagination.

15. IF the `since_version` query parameter is absent or non-numeric on a `GET /sync/deltas` request, THEN THE Backend SHALL return HTTP 422 with error code `INVALID_SINCE_VERSION`.

16. IF the requested `since_version` is older than the Delta Retention window (30 days), THEN THE Backend SHALL return HTTP 409 with error code `SNAPSHOT_REQUIRED` indicating the client must perform a full snapshot sync.

**WebSocket Connection and Authentication**

17. WHEN an authenticated Client establishes a WebSocket connection to `GET /sync/ws`, THE Backend SHALL verify the Access Token supplied in the `Authorization` header or `token` query parameter, accept the connection with HTTP 101, and associate the session with the authenticated `user_id` and their workspace memberships.

18. IF a Client presents an expired or invalid Access Token when connecting to `GET /sync/ws`, THEN THE Backend SHALL reject the WebSocket handshake with HTTP 401 and reason `UNAUTHORIZED`.

19. WHEN a Client supplies `workspace_id` and `last_known_version` during the WebSocket handshake (as query parameters or in an initial message), THE Backend SHALL perform catch-up: if deltas since `last_known_version` are available, send all missed deltas immediately; if deltas are not available (older than Delta Retention), send a `snapshot_required` message.

**WebSocket Message Protocol**

20. THE Backend SHALL use a consistent JSON envelope for all WebSocket messages with fields: `type` (string), `workspace_id` (string), `version` (integer), `timestamp` (ISO 8601 string), and `payload` (object).

21. THE Backend SHALL support the following WebSocket message types: `delta` (server pushes a snippet or folder change to clients), `snapshot_required` (server instructs client to perform full snapshot), `ack` (server acknowledges a client mutation with `client_request_id` and assigned `version`), `error` (server reports a validation or processing error for a client request), `ping` (server heartbeat), and `pong` (client heartbeat response).

22. WHEN a Client submits a mutation via the WebSocket connection, THE Backend SHALL respond with an `ack` message containing `client_request_id` (matching the client-provided identifier) and the assigned `version` number.

**WebSocket Real-Time Push**

23. WHILE a WebSocket Session is open and associated with a workspace, THE Backend SHALL push a `delta` message to the Client within 500 ms of any snippet or folder mutation affecting that workspace.

24. WHILE a Team Workspace has multiple connected members, THE Backend SHALL push delta messages to ALL connected members of that workspace when any member commits a mutation.

25. THE Backend SHALL NOT push delta messages to users who are not members of the workspace where the mutation occurred.

**WebSocket Heartbeat**

26. WHILE a WebSocket Session is open, THE Backend SHALL send a `ping` message every 30 seconds.

27. IF a Client fails to respond with a `pong` message within 10 seconds of a `ping`, THE Backend SHALL increment a missed-pong counter for that connection.

28. WHEN a Client misses 2 consecutive `pong` responses, THE Backend SHALL close the WebSocket connection.

**WebSocket Authentication Lifecycle**

29. WHILE a WebSocket connection is established, THE Backend SHALL maintain the connection regardless of the original Access Token's expiry; token expiry alone SHALL NOT terminate an active WebSocket Session.

30. WHEN the server explicitly revokes a user's session (due to logout, password reset, account suspension, or brute-force lockout), THE Backend SHALL immediately close ALL associated WebSocket connections for that user with WebSocket close code 1008 (Policy Violation).

31. WHEN a Client reconnects after a disconnection, THE Backend SHALL require a valid (non-expired) Access Token for the new handshake.

**Trigger Uniqueness**

32. THE Backend SHALL enforce a unique constraint on the combination of `workspace_id` and `trigger` within the `snippets` table; two different workspaces may use identical triggers.

33. IF a snippet creation or update would result in a duplicate `trigger` within the same workspace, THEN THE Backend SHALL return HTTP 409 with error code `TRIGGER_ALREADY_EXISTS`.

**Batch Operations**

34. WHEN a Client sends a `POST /sync/snippets/batch` request with an array of snippet operations (create, update, or soft-delete), THE Backend SHALL execute all operations transactionally: either all succeed or all fail with no partial application.

35. THE Backend SHALL accept a maximum of 100 items per batch request; a request exceeding 100 items SHALL receive HTTP 422 with error code `BATCH_SIZE_EXCEEDED`.

36. WHEN processing a batch, THE Backend SHALL assign a sequential Workspace Version to each operation within the batch (one increment per operation) and generate one set of deltas pushed via WebSocket.

37. IF any individual item within a batch fails validation, THEN THE Backend SHALL reject the entire batch with HTTP 422 and include per-item error details in the response body.

**Conflict Resolution**

38. THE Backend SHALL act as the authoritative source for version assignment; Clients SHALL NOT assign or send version numbers for concurrency control.

39. THE Backend SHALL apply a Last Write Wins conflict resolution policy: mutations are accepted in server-arrival order and each is assigned the next sequential Workspace Version regardless of client-side state.

**Payload Limits**

40. THE Backend SHALL enforce a maximum single snippet content size of 1 MB; a request with content exceeding this limit SHALL receive HTTP 422 with error code `SNIPPET_CONTENT_TOO_LARGE`.

41. THE Backend SHALL enforce a maximum request body size of 10 MB for all sync endpoints; requests exceeding this limit SHALL receive HTTP 413 with error code `REQUEST_BODY_TOO_LARGE`.

42. THE Backend SHALL enforce sync-specific rate limits separate from the global rate limits defined in Requirement 7: 60 mutation requests per minute per user and 120 read requests per minute per user; requests exceeding these limits SHALL receive HTTP 429 with a `Retry-After` header.

**Soft-Delete Lifecycle**

43. WHEN a snippet or folder is soft-deleted, THE Backend SHALL set `deleted_at` to the current UTC timestamp, generate a delta, and push the delta to all connected workspace members.

44. WHEN 30 days have elapsed since a record's `deleted_at` timestamp, THE Backend SHALL permanently hard-delete the record and its associated delta history via a background cleanup task.

**Delta Retention and Purge**

45. THE Backend SHALL retain sync delta records in the `sync_deltas` table for 30 days from creation.

46. WHEN a delta record is older than 30 days, THE Backend SHALL permanently purge the record via a background cleanup task.

47. IF a Client requests deltas older than the retention window, THEN THE Backend SHALL return HTTP 409 with error code `SNAPSHOT_REQUIRED` (per criterion 16).

---

### Requirement 3: AI Expansion Service (currently on hold)

**User Story:** As a Client, I want to expand a snippet trigger via the backend AI service, so that the desktop app can produce AI-generated text without embedding AI credentials on the device.

#### Acceptance Criteria

**Core Expansion Flow**

1. WHEN an authenticated `POST /ai/expand` request is received with a JSON body containing `trigger` (string) and `system_prompt` (string), THE Backend SHALL forward the request to the AI Provider and return `{ "expanded_text": "<result>" }` with HTTP 200.

2. WHERE the request body contains an optional `context` field (string), THE Backend SHALL include the `context` value in the upstream AI Provider request.

3. THE Backend SHALL wait for the AI Provider to return the complete response before sending the result to the Client (no streaming); partial results SHALL NOT be returned.

**Tier-Based Quotas**

4. WHILE the requesting User's subscription tier is `free`, THE Backend SHALL enforce a limit of 50 AI expansion requests per 24-hour rolling window per `user_id`, returning HTTP 429 with error code `AI_QUOTA_EXCEEDED` when the limit is reached.

5. WHILE the requesting User's subscription tier is `pro` or `teams`, THE Backend SHALL permit up to 1 000 AI expansion requests per 24-hour rolling window per `user_id`.

6. WHILE a User's subscription status is `past_due` (within the 7-day grace period), THE Backend SHALL continue enforcing the paid-tier quota limits (1 000 requests) until the grace period expires.

7. IF a User's subscription status is `cancelled` or the grace period has expired, THEN THE Backend SHALL enforce `free` tier limits (50 requests per 24 hours) regardless of the previous tier.

**Input Validation and Size Limits**

8. IF the `trigger` or `system_prompt` fields are absent or empty strings, THEN THE Backend SHALL return HTTP 422 with error code `INVALID_REQUEST_BODY`.

9. IF the `trigger` field exceeds 500 characters, THEN THE Backend SHALL return HTTP 422 with error code `TRIGGER_TOO_LONG`.

10. IF the `system_prompt` field exceeds 10 KB (10 240 bytes), THEN THE Backend SHALL return HTTP 422 with error code `SYSTEM_PROMPT_TOO_LONG`.

11. IF the `context` field exceeds 50 KB (51 200 bytes), THEN THE Backend SHALL return HTTP 422 with error code `CONTEXT_TOO_LONG`.

**Authentication and Authorization**

12. IF a `POST /ai/expand` request is received without a valid Access Token, THEN THE Backend SHALL return HTTP 401 with error code `UNAUTHORIZED` without forwarding anything to the AI Provider.

13. IF a `POST /ai/expand` request is received with a `client_type` other than `native`, THEN THE Backend SHALL return HTTP 403 with error code `CLIENT_TYPE_NOT_ALLOWED`.

**Error Handling**

14. IF the AI Provider returns an error response or is unreachable within 10 seconds, THEN THE Backend SHALL return HTTP 502 with error code `AI_PROVIDER_UNAVAILABLE`.

15. IF the AI Provider returns an empty or malformed response, THEN THE Backend SHALL return HTTP 502 with error code `AI_PROVIDER_INVALID_RESPONSE`.

16. THE Backend SHALL NOT cache AI expansion responses; every request SHALL result in a fresh call to the AI Provider.

---

### Requirement 4: Admin API

**User Story:** As an Admin, I want a comprehensive set of REST endpoints to manage users, workspaces, subscriptions, coupons, discounts, tax rates, feature flags, audit logs, and platform analytics, so that I can operate and oversee the entire platform without direct database access.

#### Acceptance Criteria

**Access Control**

1. THE Backend SHALL restrict all `/admin/*` endpoints to requests carrying a JWT with both `role = admin` AND `client_type = web`; requests missing either claim SHALL receive HTTP 403 with error code `FORBIDDEN`.

2. THE Backend SHALL apply a per-admin rate limit of 300 requests per minute on all `/admin/*` endpoints; requests exceeding this limit SHALL receive HTTP 429 with a `Retry-After` header.

3. WHEN an Admin performs any action via the Admin API (including read operations), THE Backend SHALL write an Audit Log record containing `admin_id`, `action`, `target_resource`, `target_id`, `metadata`, and `created_at` to the `audit_logs` table.

**User Management — Listing and Detail**

4. WHEN an authenticated Admin sends `GET /admin/users` with optional query parameters `page` (integer, default 1), `per_page` (integer, default 50, max 200), `search` (string), `role` (string filter), `subscription_tier` (string filter), and `status` (string filter), THE Backend SHALL return a paginated list of user records including `id`, `email`, `role`, `status`, `created_at`, and current subscription tier.

5. WHEN an authenticated Admin sends `GET /admin/users/{user_id}`, THE Backend SHALL return the full user record including profile details, subscription history, workspace memberships, and referral statistics, or HTTP 404 with error code `USER_NOT_FOUND` if the `user_id` does not exist.

**User Management — Suspend and Unsuspend**

6. WHEN an authenticated Admin sends `POST /admin/users/{user_id}/suspend`, THE Backend SHALL set `status = suspended` on the user record, revoke all Refresh Tokens for that user in the `refresh_tokens` table, close all active WebSocket connections for that user with close code 1008, and return HTTP 200 with the updated user record.

7. IF an Admin attempts to suspend their own account, THEN THE Backend SHALL return HTTP 422 with error code `CANNOT_ACT_ON_SELF`.

8. IF an Admin attempts to suspend another user with `role = admin`, THEN THE Backend SHALL return HTTP 422 with error code `CANNOT_ACT_ON_ADMIN`.

9. WHILE a User's `status` is `suspended`, THE Backend SHALL return HTTP 403 with error code `ACCOUNT_SUSPENDED` on any login attempt by that User.

10. WHEN an authenticated Admin sends `POST /admin/users/{user_id}/unsuspend`, THE Backend SHALL set `status = active` on the user record and return HTTP 200 with the updated user record; the User may then log in normally.

**User Management — Force Password Reset**

11. WHEN an authenticated Admin sends `POST /admin/users/{user_id}/force-password-reset`, THE Backend SHALL set a `must_reset_password` flag on the user record and return HTTP 200.

12. WHEN a User with `must_reset_password = true` attempts to log in with valid credentials, THE Backend SHALL return HTTP 403 with error code `PASSWORD_RESET_REQUIRED` and include a single-use password reset token in the response body; the User SHALL NOT receive Access or Refresh Tokens until the password is reset.

**User Management — Admin-Initiated Deletion**

13. WHEN an authenticated Admin sends `DELETE /admin/users/{user_id}`, THE Backend SHALL perform a soft-delete by setting `deleted_at` to the current UTC timestamp on the user record (following the same 30-day retention policy as user-initiated deletion) and return HTTP 204.

14. IF an Admin attempts to delete their own account via `DELETE /admin/users/{user_id}`, THEN THE Backend SHALL return HTTP 422 with error code `CANNOT_ACT_ON_SELF`.

15. IF an Admin attempts to delete another user with `role = admin`, THEN THE Backend SHALL return HTTP 422 with error code `CANNOT_ACT_ON_ADMIN`.

**Workspace Management**

16. WHEN an authenticated Admin sends `GET /admin/workspaces` with optional query parameters `page`, `per_page`, `type` (filter by `individual` or `team`), and `subscription_status` (string filter), THE Backend SHALL return a paginated list of workspace records.

17. WHEN an authenticated Admin sends `GET /admin/workspaces/{workspace_id}`, THE Backend SHALL return the full workspace record including members, snippet count, folder count, subscription details, and `created_at`, or HTTP 404 with error code `WORKSPACE_NOT_FOUND` if the `workspace_id` does not exist.

18. WHEN an authenticated Admin sends `POST /admin/workspaces/{workspace_id}/deactivate`, THE Backend SHALL set the workspace subscription status to `deactivated`, deny member access to that workspace, and return HTTP 200; workspace data SHALL NOT be deleted.

19. WHEN an authenticated Admin sends `DELETE /admin/workspaces/{workspace_id}` with query parameter `confirm=true`, THE Backend SHALL permanently hard-delete the workspace record and all associated data (snippets, folders, sync deltas, membership records) and return HTTP 204.

20. IF a `DELETE /admin/workspaces/{workspace_id}` request is missing the `confirm=true` query parameter, THEN THE Backend SHALL return HTTP 422 with error code `CONFIRMATION_REQUIRED`.

21. IF an Admin attempts to delete an Individual Workspace without deleting the owning user, THEN THE Backend SHALL return HTTP 422 with error code `CANNOT_DELETE_INDIVIDUAL_WORKSPACE` indicating the user must be deleted instead.

**Coupon and Discount Management — Discounts**

22. WHEN an authenticated Admin sends `GET /admin/discounts`, THE Backend SHALL return a list of all discount records including `id`, `type`, `value`, and `active` status.

23. WHEN an authenticated Admin sends `POST /admin/discounts` with fields `type` (`percentage` or `flat`), `value` (decimal), and `active` (boolean), THE Backend SHALL create a new discount record and return it with HTTP 201.

24. WHEN an authenticated Admin sends `PATCH /admin/discounts/{id}` with updatable fields (`value`, `active`), THE Backend SHALL update the discount record and return the updated record with HTTP 200.

25. WHEN a discount is set to `active = false` and that discount is linked to active coupon codes, THE Backend SHALL allow the deactivation; linked coupons SHALL stop applying the discount at their next validation attempt.

26. THE Backend SHALL NOT provide delete endpoints for discounts; discounts can only be deactivated (set `active = false`) to preserve history.

**Coupon and Discount Management — Coupons**

27. WHEN an authenticated Admin sends `GET /admin/coupons` with optional query parameters `page`, `per_page`, `type` (filter by `platform` or `referral`), and `active` (boolean filter), THE Backend SHALL return a paginated list of coupon code records.

28. WHEN an authenticated Admin sends `POST /admin/coupons` with fields `code` (string), `discount_id` (FK), `max_uses` (nullable integer), `valid_from` (UTC timestamp), and `valid_until` (nullable UTC timestamp), THE Backend SHALL create a new platform coupon code record with `type = platform` and `active = true`, and return it with HTTP 201.

29. THE Backend SHALL enforce coupon code uniqueness case-insensitively; if a `POST /admin/coupons` request provides a `code` that already exists (case-insensitive match), THE Backend SHALL return HTTP 409 with error code `COUPON_CODE_ALREADY_EXISTS`.

30. WHEN an authenticated Admin sends `PATCH /admin/coupons/{id}` with updatable fields (`active`, `max_uses`, `valid_until`), THE Backend SHALL update the coupon record and return the updated record with HTTP 200.

31. WHEN an authenticated Admin sends `GET /admin/coupons/{id}`, THE Backend SHALL return the full coupon detail including usage count (`times_used`), or HTTP 404 with error code `COUPON_NOT_FOUND` if the `id` does not exist.

32. THE Backend SHALL NOT provide delete endpoints for coupons; coupons can only be deactivated (set `active = false`) to preserve history.

**Coupon and Discount Management — Referral Statistics**

33. WHEN an authenticated Admin sends `GET /admin/referrals`, THE Backend SHALL return referral statistics including `total_referrals`, `converted_referrals`, `top_referrers` (top 10 by conversion count), and `conversion_rate`.

**Subscription and Billing Oversight**

34. WHEN an authenticated Admin sends `GET /admin/subscriptions` with optional query parameters `page`, `per_page`, `tier` (string filter), `status` (string filter), `workspace_id` (UUID filter), and `expiry_from`/`expiry_to` (date range filter), THE Backend SHALL return a paginated list of subscription records.

35. WHEN an authenticated Admin sends `GET /admin/subscriptions/{workspace_id}`, THE Backend SHALL return the full subscription detail including billing event history for that workspace, or HTTP 404 with error code `SUBSCRIPTION_NOT_FOUND` if no subscription exists for the `workspace_id`.

36. WHEN an authenticated Admin sends `POST /admin/subscriptions/{workspace_id}/extend` with either `{ "months": N }` or `{ "days": N }` (where N is a positive integer), THE Backend SHALL add the specified duration to the subscription's `period_end`, audit-log the action with the extension reason, and return the updated subscription with HTTP 200; no payment is involved.

37. WHEN an authenticated Admin sends `POST /admin/subscriptions/{workspace_id}/cancel`, THE Backend SHALL immediately set subscription `status` to `cancelled`, set `cancelled_at` to the current UTC timestamp, trigger soft-lock on workspace content, close all WebSocket connections for workspace members, and return HTTP 200; no grace period applies to admin-initiated cancellations.

38. IF an Admin force-cancels a Team Workspace subscription, THE Backend SHALL deny access to all team members immediately.

39. WHEN an authenticated Admin sends `PATCH /admin/subscriptions/{workspace_id}/tier` with `{ "tier": "free"|"pro"|"teams" }`, THE Backend SHALL update the subscription tier immediately, audit-log the override, and return the updated subscription with HTTP 200; this change SHALL NOT trigger the Billing Provider.

40. IF a `PATCH /admin/subscriptions/{workspace_id}/tier` request specifies a `tier` value outside `free`, `pro`, `teams`, THEN THE Backend SHALL return HTTP 422 with error code `INVALID_TIER`.

41. WHEN an authenticated Admin sends `GET /admin/billing-events` with optional query parameters `page`, `per_page`, `workspace_id` (UUID filter), `event_type` (string filter), and `date_from`/`date_to` (date range filter), THE Backend SHALL return a paginated list of billing webhook event log records.

**Tax Rate Management**

42. WHEN an authenticated Admin sends `GET /admin/tax-rates`, THE Backend SHALL return a list of all tax rate records including `country_code`, `rate`, `tax_name`, and `active` status.

43. WHEN an authenticated Admin sends `POST /admin/tax-rates` with fields `country_code` (ISO 3166-1 alpha-2), `rate` (decimal), `tax_name` (string), and `active` (boolean), THE Backend SHALL create a new tax rate record and return it with HTTP 201.

44. IF a `POST /admin/tax-rates` request specifies a `country_code` that already has an active tax rate, THEN THE Backend SHALL return HTTP 409 with error code `TAX_RATE_ALREADY_EXISTS`.

45. WHEN an authenticated Admin sends `PATCH /admin/tax-rates/{country_code}` with updatable fields (`rate`, `active`), THE Backend SHALL update the tax rate record and return the updated record with HTTP 200.

46. THE Backend SHALL NOT provide delete endpoints for tax rates; tax rates can only be deactivated (set `active = false`).

47. WHEN a tax rate is deactivated, THE Backend SHALL apply a 0% tax rate for that country on all future invoices only; existing invoices remain unaffected.

**Audit Log Viewer**

48. WHEN an authenticated Admin sends `GET /admin/audit-logs` with optional query parameters `page`, `per_page`, `admin_id` (UUID filter), `action` (string filter), `target_resource` (string filter), and `date_from`/`date_to` (date range filter), THE Backend SHALL return a paginated list of audit log entries.

49. WHEN an authenticated Admin sends `GET /admin/audit-logs/{id}`, THE Backend SHALL return the full audit log entry detail, or HTTP 404 with error code `AUDIT_LOG_NOT_FOUND` if the `id` does not exist.

50. THE Backend SHALL NOT provide PUT, PATCH, or DELETE endpoints for audit logs; audit log records are immutable.

51. THE Backend SHALL retain audit log records indefinitely with no automated purge.

**Dashboard and Analytics**

52. WHEN an authenticated Admin sends `GET /admin/stats/overview`, THE Backend SHALL compute and return: `total_users`, `active_users_30d`, `total_workspaces`, `subscriptions_by_tier` (object with counts per tier), `subscriptions_by_status` (object with counts per status), `signups_last_30d`, and `revenue_last_30d` (if trackable from billing events).

53. WHEN an authenticated Admin sends `GET /admin/stats/referrals`, THE Backend SHALL compute and return: `total_referrals`, `converted_referrals`, `top_referrers` (top 10 by conversion count), and `conversion_rate`.

54. THE Backend SHALL compute all dashboard statistics on-demand at request time without pre-aggregation.

**Admin Self-Management**

55. WHEN an authenticated Admin sends `GET /admin/admins`, THE Backend SHALL return a list of all user accounts with `role = admin`.

56. WHEN an authenticated Admin sends `DELETE /admin/admins/{admin_id}`, THE Backend SHALL change the target user's `role` from `admin` to `user`, revoke all Refresh Tokens for that user (forcing re-login with user-level access), and return HTTP 204.

57. IF an Admin attempts to demote themselves via `DELETE /admin/admins/{admin_id}` where `admin_id` matches the requesting Admin, THEN THE Backend SHALL return HTTP 422 with error code `CANNOT_DEMOTE_SELF`.

58. IF an Admin attempts to demote the last remaining Admin account, THEN THE Backend SHALL return HTTP 422 with error code `LAST_ADMIN_CANNOT_BE_REMOVED`.

59. THE Backend SHALL enforce a maximum of 5 pending (unused) admin invites at any time; a `POST /admin/invites` request that would exceed this limit SHALL return HTTP 422 with error code `MAX_PENDING_INVITES_REACHED`.

**Feature Flag Management**

60. WHEN an authenticated Admin sends `GET /admin/feature-flags`, THE Backend SHALL return all feature flags with their names, current boolean values, and descriptions.

61. WHEN an authenticated Admin sends `POST /admin/feature-flags` with fields `name` (string, kebab-case, max 100 characters), `enabled` (boolean), and `description` (string), THE Backend SHALL create a new feature flag record and return it with HTTP 201.

62. THE Backend SHALL enforce feature flag name uniqueness; if a `POST /admin/feature-flags` request provides a `name` that already exists, THE Backend SHALL return HTTP 409 with error code `FEATURE_FLAG_ALREADY_EXISTS`.

63. THE Backend SHALL validate that feature flag names are kebab-case and do not exceed 100 characters; names violating this constraint SHALL receive HTTP 422 with error code `INVALID_FLAG_NAME`.

64. WHEN an authenticated Admin sends `PUT /admin/feature-flags/{flag_name}` with `{ "enabled": true | false }` and optional `description`, THE Backend SHALL update the flag record and return HTTP 200 with the updated flag.

65. IF a `PUT /admin/feature-flags/{flag_name}` request references a `flag_name` that does not exist in the `feature_flags` table, THEN THE Backend SHALL return HTTP 404 with error code `FEATURE_FLAG_NOT_FOUND`.

66. WHEN an authenticated Admin sends `DELETE /admin/feature-flags/{flag_name}`, THE Backend SHALL permanently remove the flag from the `feature_flags` table and return HTTP 204; any feature relying on the deleted flag SHALL default to disabled behavior.

67. IF a `DELETE /admin/feature-flags/{flag_name}` request references a `flag_name` that does not exist, THEN THE Backend SHALL return HTTP 404 with error code `FEATURE_FLAG_NOT_FOUND`.

---

### Requirement 5: Subscription and Billing Service

**User Story:** As the platform, I want to manage workspaces, subscription tiers, team creation and invitations, enforce per-tier feature limits, handle billing cycles with grace periods, apply discounts and coupon codes, process referral rewards, calculate country-based taxes, and receive billing webhook events, so that the correct features and limits are enforced for every user and team workspace.

#### Acceptance Criteria

**Workspace Model**

1. WHEN a new User registers (via email or OAuth), THE Backend SHALL create an Individual Workspace for that User in the `workspaces` table with `type = individual`, `owner_id` set to the new `user_id`, and a default `name`, and insert a corresponding record in `workspace_members` with `role = owner`.

2. THE Backend SHALL store workspaces in a `workspaces` table with columns: `id` (UUID), `type` (`individual` or `team`), `owner_id` (FK to `users`), `name` (text), `created_at` (UTC timestamp).

3. THE Backend SHALL store workspace membership in a `workspace_members` table with columns: `workspace_id` (FK to `workspaces`), `user_id` (FK to `users`), `role` (`owner` or `member`), `joined_at` (UTC timestamp).

4. THE Backend SHALL allow a single User account to belong to multiple workspaces simultaneously: exactly one Individual Workspace and zero or more Team Workspaces.

**Tiers and Subscriptions**

5. THE Backend SHALL support exactly three subscription tiers: `free`, `pro`, and `teams`. The `free` and `pro` tiers apply to Individual Workspaces; the `teams` tier applies to Team Workspaces.

6. WHEN a new User registers, THE Backend SHALL create a subscription record with `tier = free`, `status = active`, and `workspace_id` referencing the User's Individual Workspace.

7. WHEN an authenticated User sends `POST /subscriptions/upgrade`, THE Backend SHALL initiate a tier upgrade from `free` to `pro` for that User's Individual Workspace subscription; if the User's current tier is already `pro` or `teams`, the Backend SHALL return HTTP 422 with error code `ALREADY_UPGRADED`.

8. THE Backend SHALL treat Team Workspaces as independent entities with their own `teams` tier subscription, separate from any Individual Workspace subscription.

**Team Workspace Creation**

9. WHEN an authenticated User sends `POST /teams` with a `name` field, THE Backend SHALL create a new Team Workspace in the `workspaces` table with `type = team` and `owner_id` set to the requesting User, add a `workspace_members` record with `role = owner`, create a `teams` tier subscription with `status = pending_payment`, and return the workspace details with HTTP 201.

10. WHILE a Team Workspace subscription has `status = pending_payment`, THE Backend SHALL allow 7 calendar days for the owner to complete payment via the Billing Provider; the Backend SHALL record `payment_deadline` as 7 days from workspace creation.

11. IF a Team Workspace's `payment_deadline` passes without a successful `subscription.activated` webhook event, THEN THE Backend SHALL set the Team Workspace subscription `status` to `deactivated` and deny all member access to that workspace, without deleting the workspace or its data.

12. THE Backend SHALL NOT allow transfer of Team Workspace ownership; the `owner_id` on a Team Workspace is immutable after creation.

**Team Invitation Flow**

13. WHEN the owner of a Team Workspace sends `POST /teams/{workspace_id}/invites` with optional `max_uses` (integer, default 1) and `expires_at` (UTC timestamp), THE Backend SHALL generate a unique `invite_code`, store it in the `team_invites` table with fields `id`, `workspace_id`, `invite_code`, `max_uses`, `times_used` (default 0), `expires_at`, `created_by`, `created_at`, and return the invite link with HTTP 201.

14. WHEN an authenticated User sends `POST /teams/{workspace_id}/join` with a valid `invite_code`, THE Backend SHALL validate that the code belongs to the specified workspace, that `times_used < max_uses`, that `expires_at` has not passed, that the Team Workspace has fewer than 3 members, and that the User is not already a member; upon success, the Backend SHALL add a `workspace_members` record with `role = member`, increment `times_used`, and return HTTP 200.

15. IF an invite code is expired, THE Backend SHALL return HTTP 422 with error code `INVITE_EXPIRED`.

16. IF an invite code has reached its `max_uses`, THE Backend SHALL return HTTP 422 with error code `INVITE_USAGE_LIMIT_REACHED`.

17. IF a Team Workspace already has 3 members (including the owner), THE Backend SHALL return HTTP 422 with error code `SEAT_LIMIT_REACHED` on any join attempt.

18. IF a User is already a member of the specified Team Workspace, THE Backend SHALL return HTTP 422 with error code `ALREADY_A_MEMBER`.

19. WHEN the owner of a Team Workspace sends `DELETE /teams/{workspace_id}/members/{user_id}`, THE Backend SHALL remove the specified member from the `workspace_members` table and return HTTP 204; the owner SHALL NOT be removable via this endpoint.

20. IF the `{user_id}` in a member removal request is the workspace owner, THE Backend SHALL return HTTP 422 with error code `CANNOT_REMOVE_OWNER`.

**Per-Tier Feature Limits (Free tier only; pro and teams have no such limits)**

21. WHILE a User's Individual Workspace subscription tier is `free`, THE Backend SHALL enforce the following limits at write time:
    - Maximum 10 snippets per user; a `POST /sync/snippets` that would exceed this limit SHALL return HTTP 422 with error code `SNIPPET_LIMIT_REACHED`.
    - Maximum 3 folders per user; a folder creation request that would exceed this limit SHALL return HTTP 422 with error code `FOLDER_LIMIT_REACHED`.
    - Maximum 2 000 characters per snippet content field; a snippet with content exceeding this limit SHALL return HTTP 422 with error code `SNIPPET_CONTENT_TOO_LONG`.

22. THE Backend SHALL allow a snippet to be assigned to multiple folders via a `snippet_folders` junction table (`snippet_id`, `folder_id`), regardless of tier.

23. WHILE a User's active tier is `pro` or `teams`, THE Backend SHALL NOT enforce snippet count, folder count, or content length limits.

**Soft-Lock on Downgrade**

24. WHEN a User's subscription expires or is cancelled and the User's content exceeds `free` tier limits, THE Backend SHALL soft-lock content beyond those limits: existing snippets and folders remain readable and exportable but the User SHALL NOT create new snippets or folders until within free tier limits.

25. WHILE content is soft-locked, THE Backend SHALL return HTTP 422 with error code `CONTENT_SOFT_LOCKED` on any write operation that would exceed free tier limits, and SHALL include a message indicating the content is read-only due to tier downgrade.

**Billing Cycle and Checkout**

26. THE Backend SHALL only accept subscription purchases with a minimum billing cycle of 12 months; any checkout request specifying fewer than 12 months SHALL return HTTP 422 with error code `MINIMUM_BILLING_CYCLE_NOT_MET`.

27. THE Backend SHALL store the subscription period as `period_start` (UTC timestamp) and `period_end` (UTC timestamp, exactly 12 months after `period_start`) in the `subscriptions` table.

28. WHEN an authenticated User sends `POST /subscriptions/checkout` with `tier` (`pro` or `teams`), `workspace_id`, and optional `coupon_code`, THE Backend SHALL validate the request, compute the invoice, initiate a checkout session with the Billing Provider, and return the checkout URL with HTTP 200.

28a. THE Backend SHALL accept optional `success_url` and `cancel_url` fields in the checkout request body and pass them to the Billing Provider session creation so the user is redirected to the appropriate frontend page after payment completion or cancellation.

29. WHEN a billing webhook event signals successful payment renewal, THE Backend SHALL extend `period_end` by 12 months from the current `period_end`, update `status` to `active`, and persist the change atomically.

**Grace Period**

30. WHEN a billing webhook event sets subscription `status` to `past_due`, THE Backend SHALL record `grace_period_end` as 7 calendar days from the event timestamp and continue granting paid-tier access during that window.

31. IF `grace_period_end` passes without a successful renewal webhook, THEN THE Backend SHALL transition `status` from `past_due` to `cancelled` and enforce free tier limits (with soft-locking per criterion 24).

**Discounts — Flat and Percentage**

32. THE Backend SHALL support two discount types stored in a `discounts` table: `percentage` (0.01–1.00 representing 1%–100%) and `flat` (a fixed amount in the subscription's billing currency).

33. WHEN a checkout request includes a `discount_id`, THE Backend SHALL apply the discount to the total subscription amount before tax: for `percentage` discounts subtract `base_price × discount_rate`; for `flat` discounts subtract the flat amount, flooring the result at zero.

34. IF a `discount_id` references a record that does not exist or has `active = false`, THEN THE Backend SHALL return HTTP 422 with error code `DISCOUNT_NOT_FOUND`.

35. THE Backend SHALL NOT stack multiple discount sources on a single subscription purchase; only one of `discount_id`, `coupon_code` (platform type), or `coupon_code` (referral type) SHALL be applied per checkout. If multiple are present, THE Backend SHALL return HTTP 422 with error code `MULTIPLE_DISCOUNTS_NOT_ALLOWED`.

**Coupon Codes (Platform and Referral)**

36. THE Backend SHALL store coupon codes in a `coupon_codes` table with fields: `code` (unique, case-insensitive), `type` (`platform` or `referral`), `discount_id` (FK to `discounts`), `max_uses` (nullable integer), `times_used` (integer, default 0), `valid_from` (UTC timestamp), `valid_until` (nullable UTC timestamp), `active` (boolean), `owner_id` (nullable FK to `users`, set for referral type codes).

37. WHEN a checkout request includes a `coupon_code`, THE Backend SHALL validate the code case-insensitively, check `active = true`, check `valid_from <= now()`, check `valid_until IS NULL OR valid_until > now()`, check `max_uses IS NULL OR times_used < max_uses`, apply the linked discount, increment `times_used` atomically, and return the discounted total.

38. IF a `coupon_code` fails any validation check in criterion 37, THE Backend SHALL return HTTP 422 with a specific error code:
    - Code does not exist: `COUPON_NOT_FOUND`
    - Code inactive: `COUPON_INACTIVE`
    - Code not yet valid: `COUPON_NOT_YET_VALID`
    - Code expired: `COUPON_EXPIRED`
    - Usage limit reached: `COUPON_USAGE_LIMIT_REACHED`

39. THE Backend SHALL increment `times_used` within the same database transaction as the subscription creation; if the transaction rolls back, `times_used` SHALL NOT be incremented.

40. WHEN a `platform` type coupon code is provided at checkout, THE Backend SHALL require the User to have entered it manually; platform codes SHALL NOT be auto-applied.

**Referral System**

41. WHEN a new User registers, THE Backend SHALL generate a unique alphanumeric referral code, create a corresponding `coupon_codes` record with `type = referral`, `owner_id` set to the new User, and a linked discount of 20% for the first checkout.

42. WHEN a new User registers with a valid referral code (a `coupon_code` of `type = referral` belonging to another User), THE Backend SHALL record the referral relationship in a `referrals` table with fields: `id`, `referrer_id`, `referred_user_id`, `status` (`pending`, `converted`), `created_at`, and persist the referral code so it is pre-filled at the referred User's first subscription checkout.

43. IF a referral code supplied during registration does not match any `coupon_code` of `type = referral`, THEN THE Backend SHALL return HTTP 422 with error code `REFERRAL_CODE_NOT_FOUND` and abort registration.

44. IF a User attempts to register using their own referral code, THEN THE Backend SHALL return HTTP 422 with error code `SELF_REFERRAL_NOT_ALLOWED`.

45. WHEN the referred User completes their first paid subscription (status transitions to `active` for a non-free tier) using the referral code, THE Backend SHALL mark the referral `status` as `converted` and add 1 month to the Referrer's current subscription `period_end`.

46. IF the Referrer has no active paid subscription at the time the referral converts, THE Backend SHALL store the 1-month credit in a `referral_credits` table with fields: `id`, `user_id`, `months` (integer), `redeemed` (boolean, default false), `created_at`; the credit SHALL be applied to extend `period_end` when the Referrer next subscribes to a paid tier.

47. IF the referred User cancels before completing a paid subscription, THE Backend SHALL retain the referral record with `status = pending` and SHALL NOT apply any reward.

48. THE Backend SHALL NOT apply the referral reward more than once per `referrer_id` + `referred_user_id` pair; duplicate conversion attempts SHALL be idempotent (no error, no additional reward).

49. THE Backend SHALL validate that a referral code has not already been used by the same `referred_user_id`; if it has, THE Backend SHALL return HTTP 422 with error code `REFERRAL_ALREADY_USED`.

**Taxes**

50. THE Backend SHALL store country-specific tax rates in a `tax_rates` table with fields: `country_code` (ISO 3166-1 alpha-2), `rate` (decimal, e.g., 0.18 for 18%), `tax_name` (e.g., "GST", "VAT"), `active` (boolean).

51. WHEN calculating the final invoice amount, THE Backend SHALL determine the User's country from a `country_code` field on the `users` table and look up the applicable tax rate; the tax amount SHALL be computed as `(base_price - discount_amount) × tax_rate` and added to the discounted subtotal.

52. IF no tax rate row exists for the User's `country_code`, THE Backend SHALL apply a tax rate of 0 and proceed without error.

53. THE Backend SHALL return a structured invoice object in the checkout response containing: `base_price`, `discount_amount`, `discount_type`, `subtotal_after_discount`, `tax_rate`, `tax_amount`, `total_amount`, `currency`, `billing_cycle_months`.

54. THE Backend SHALL NOT apply tax retrospectively to already-issued invoices; tax changes in `tax_rates` SHALL only affect new invoices.

**Webhook and Status Transitions**

55. WHEN a `POST /webhooks/billing` request is received with a valid webhook signature from the configured Billing Provider, THE Backend SHALL parse the event type and:
    - `subscription.activated` → set `status = active`, set `period_start` and `period_end`
    - `subscription.renewed` → extend `period_end` by 12 months
    - `subscription.past_due` → set `status = past_due`, set `grace_period_end` to 7 days from event timestamp
    - `subscription.cancelled` → set `status = cancelled`, set `cancelled_at` to current UTC
    - `subscription.reactivated` → set `status = active`, clear `cancelled_at`, clear `grace_period_end`

56. IF a `POST /webhooks/billing` request arrives with an invalid or missing webhook signature, THEN THE Backend SHALL return HTTP 401 with error code `INVALID_WEBHOOK_SIGNATURE` and make no changes.

57. THE Backend SHALL process billing webhook events idempotently using the `external_event_id` field in a `billing_events` table; duplicate events with the same `external_event_id` SHALL return HTTP 200 without re-processing.

58. WHEN processing a billing webhook, THE Backend SHALL respond to the Billing Provider within 5 seconds to prevent provider retries; processing that exceeds this budget SHALL be deferred to an asynchronous task.

**Request Context Injection**

59. WHILE processing every authenticated request, THE Backend SHALL attach the `user_id`'s current `tier`, `status`, `period_end`, and active `workspace_id` to the request context so that downstream handlers can enforce limits without additional database queries.

60. IF a User's subscription `status` is `cancelled` or `period_end` has passed and the User attempts to access a `pro` or `teams` feature, THEN THE Backend SHALL return HTTP 402 with error code `SUBSCRIPTION_REQUIRED`.

---

### Requirement 6: Database and Migrations

**User Story:** As a developer, I want all schema changes managed via sqlx migrations with a complete, reproducible schema, so that the database is version-controlled, correctly indexed, and ready for production workloads.

#### Acceptance Criteria

**Migration Management**

1. THE Backend SHALL manage all Postgres schema changes through sqlx migration files located in the `migrations/` directory, numbered sequentially (e.g., `0001_initial.sql`, `0002_add_feature_flags.sql`).

2. WHEN the Backend process starts, THE Backend SHALL run all pending sqlx migrations before accepting any HTTP or WebSocket connections.

3. IF a migration fails during startup, THEN THE Backend SHALL log the error with full context (migration file name, SQL error message, and line number) and terminate the process with a non-zero exit code.

4. THE Backend SHALL adopt a forward-only migration policy; rollback migrations (down scripts) SHALL NOT be required. Schema corrections SHALL be applied as new forward migrations.

5. THE Backend SHALL validate at compile time (via `sqlx::query!` macro) that all SQL queries are compatible with the declared schema.

**Table Definitions**

6. THE Backend SHALL define the following tables at minimum:

    - `users` — user accounts (id, email, password_hash, first_name, last_name, profile_picture_url, timezone, language, country_code, phone, role, status, referral_code, must_reset_password, deleted_at, created_at, updated_at)
    - `refresh_tokens` — active sessions (id, user_id, token_hash, client_type, expires_at, revoked, created_at, last_used_at)
    - `oauth_accounts` — linked OAuth identities (id, user_id, provider, external_id)
    - `password_reset_tokens` — password reset flow (id, user_id, token_hash, expires_at, used, created_at)
    - `email_change_requests` — email change verification (id, user_id, new_email, token_hash, expires_at, used, created_at)
    - `admin_invites` — admin invitation tokens (id, email, token_hash, expires_at, used, created_by, created_at)
    - `audit_logs` — immutable admin action log (id, admin_id, action, target_resource, target_id, metadata, created_at)
    - `workspaces` — organisational containers (id, type, owner_id, name, created_at)
    - `workspace_members` — workspace membership (workspace_id, user_id, role, joined_at)
    - `snippets` — snippet records (id, workspace_id, created_by, trigger, content, snippet_type, version, created_at, updated_at, deleted_at)
    - `folders` — folder records (id, workspace_id, name, created_by, version, created_at, updated_at, deleted_at)
    - `snippet_folders` — many-to-many snippet-folder assignment (snippet_id, folder_id)
    - `sync_deltas` — sync change log (id, workspace_id, entity_type, entity_id, operation, payload, version, created_at)
    - `subscriptions` — workspace subscriptions (id, workspace_id, tier, status, period_start, period_end, grace_period_end, payment_deadline, cancelled_at, external_subscription_id, created_at, updated_at)
    - `team_invites` — team join invitations (id, workspace_id, invite_code, max_uses, times_used, expires_at, created_by, created_at)
    - `discounts` — pricing discounts (id, type, value, active, created_at)
    - `coupon_codes` — redeemable codes (id, code, type, discount_id, owner_id, max_uses, times_used, valid_from, valid_until, active, created_at)
    - `referrals` — referral tracking (id, referrer_id, referred_user_id, status, created_at)
    - `referral_credits` — banked referral rewards (id, user_id, months, redeemed, created_at)
    - `tax_rates` — country tax rates (country_code, rate, tax_name, active, created_at, updated_at)
    - `billing_events` — webhook idempotency log (id, external_event_id, event_type, workspace_id, payload, processed_at, created_at)
    - `feature_flags` — feature toggles (name, enabled, description, created_at, updated_at)

**Indexes**

7. THE Backend SHALL create the following database indexes at minimum:

    - `users(email)` — UNIQUE
    - `users(referral_code)` — UNIQUE
    - `refresh_tokens(token_hash)` — UNIQUE
    - `refresh_tokens(user_id)`
    - `oauth_accounts(provider, external_id)` — UNIQUE
    - `password_reset_tokens(token_hash)`
    - `password_reset_tokens(user_id)`
    - `workspaces(owner_id)`
    - `workspace_members(workspace_id, user_id)` — UNIQUE
    - `workspace_members(user_id)`
    - `snippets(workspace_id)`
    - `snippets(workspace_id, trigger)` — UNIQUE (where `deleted_at IS NULL`)
    - `folders(workspace_id)`
    - `snippet_folders(snippet_id, folder_id)` — UNIQUE
    - `sync_deltas(workspace_id, version)`
    - `subscriptions(workspace_id)` — UNIQUE
    - `team_invites(invite_code)` — UNIQUE
    - `coupon_codes(code)` — UNIQUE (case-insensitive via `LOWER(code)`)
    - `coupon_codes(owner_id)`
    - `referrals(referrer_id)`
    - `referrals(referred_user_id)`
    - `referrals(referrer_id, referred_user_id)` — UNIQUE
    - `billing_events(external_event_id)` — UNIQUE
    - `audit_logs(admin_id, created_at)`
    - `audit_logs(target_resource, target_id)`

**Foreign Key Cascade Rules**

8. THE Backend SHALL apply `ON DELETE CASCADE` on foreign keys referencing `users(id)` for the following tables: `refresh_tokens`, `oauth_accounts`, `password_reset_tokens`, `email_change_requests`.

9. THE Backend SHALL apply `ON DELETE RESTRICT` on foreign keys referencing `users(id)` for: `workspaces(owner_id)`, `workspace_members(user_id)`, `subscriptions(workspace_id)`, `referrals(referrer_id)`, `referrals(referred_user_id)`; the application layer SHALL handle user deletion by removing dependencies first.

10. THE Backend SHALL apply `ON DELETE CASCADE` on foreign keys referencing `workspaces(id)` for: `workspace_members`, `snippets`, `folders`, `sync_deltas`, `team_invites`, `subscriptions`.

**Soft-Delete vs Hard-Delete**

11. THE Backend SHALL implement soft-delete (via `deleted_at` column) for the following tables: `users`, `snippets`, `folders`, `workspaces`.

12. THE Backend SHALL implement hard-delete (immediate row removal) for the following tables: `refresh_tokens`, `password_reset_tokens`, `email_change_requests`, `admin_invites`, `sync_deltas` (after retention), `billing_events` (never purged, but no soft-delete needed).

13. THE Backend SHALL ensure that unique constraints on soft-deletable tables use partial indexes (e.g., `UNIQUE(workspace_id, trigger) WHERE deleted_at IS NULL`) to allow re-use of triggers after deletion.

**Connection Pool**

14. THE Backend SHALL configure the sqlx connection pool with the following parameters read from environment variables: `DATABASE_MAX_CONNECTIONS` (default 20), `DATABASE_MIN_CONNECTIONS` (default 5), `DATABASE_CONNECT_TIMEOUT_SECS` (default 5), and `DATABASE_IDLE_TIMEOUT_SECS` (default 300).

15. IF the connection pool cannot establish a connection within the configured timeout during startup, THEN THE Backend SHALL log the error and terminate with a non-zero exit code.

**Seed Data**

16. THE Backend SHALL include a seed migration that inserts: one super-admin user account (email and password read from environment variables `SEED_ADMIN_EMAIL` and `SEED_ADMIN_PASSWORD`), and a default set of feature flags required by the application (all defaulting to `enabled = false`).

17. THE Backend SHALL make the seed migration idempotent; running it against an already-seeded database SHALL NOT create duplicate records or fail.

---

### Requirement 7: Cross-Cutting Concerns

**User Story:** As a platform operator, I want email delivery, background scheduling, CORS, request limits, standardised errors, security hardening, graceful shutdown, and environment-based configuration, so that the Backend is observable, operable, secure, and resilient in production.

#### Acceptance Criteria

**7.1 Email Service**

1. THE Backend SHALL support a configurable email provider selectable via the `EMAIL_PROVIDER` environment variable with values `smtp` or `api`.

2. WHILE `EMAIL_PROVIDER` is set to `smtp`, THE Backend SHALL read `EMAIL_SMTP_HOST`, `EMAIL_SMTP_PORT`, `EMAIL_SMTP_USER`, and `EMAIL_SMTP_PASSWORD` from environment variables to connect to the SMTP server.

3. WHILE `EMAIL_PROVIDER` is set to `api`, THE Backend SHALL read `EMAIL_API_KEY` and `EMAIL_API_URL` from environment variables to connect to the transactional email API.

4. THE Backend SHALL read `EMAIL_FROM_ADDRESS` and `EMAIL_FROM_NAME` from environment variables and use them as the sender identity on all outgoing emails.

5. THE Backend SHALL support both HTML and plain-text content in every outgoing email via a template system.

6. WHEN an email delivery attempt fails, THE Backend SHALL retry with exponential backoff: attempt 1 after 1 second, attempt 2 after 5 seconds, attempt 3 after 30 seconds (3 retries total).

7. IF all 3 retry attempts for an email delivery fail, THEN THE Backend SHALL log the failure with full context (recipient, template, error details) at ERROR level and discard the send without crashing the originating request.

8. WHEN a request handler triggers an email (password reset, email change, admin invite, team invite, notification), THE Backend SHALL dispatch the email asynchronously; the HTTP response SHALL NOT wait for email delivery to complete.

**7.2 Background Task Scheduler**

9. THE Backend SHALL implement an internal task scheduler using tokio for both recurring and one-off delayed tasks, without requiring an external queue or message broker.

10. THE Backend SHALL execute the following recurring tasks at the specified intervals:
    - Delta purge (remove sync deltas older than 30 days): every 1 hour
    - Soft-delete cleanup (hard-delete records past 30-day retention): every 6 hours
    - Expired token and invite cleanup: every 1 hour
    - Grace period check (transition `past_due` → `cancelled` after 7-day window): every 1 hour
    - Payment deadline check (deactivate Team Workspaces past 7-day payment window): every 1 hour
    - Account hard-delete check (permanently remove accounts past 30-day soft-delete window): every 24 hours

11. THE Backend SHALL ensure all scheduled tasks are idempotent; running the same task multiple times within its interval SHALL produce the same result without data corruption.

12. WHEN a scheduled task encounters a transient failure, THE Backend SHALL retry the task up to 3 times with exponential backoff (1 second, 5 seconds, 30 seconds).

13. IF a scheduled task fails after all 3 retry attempts, THEN THE Backend SHALL log the failure with full context (task name, error details, affected records) at ERROR level without crashing the process.

14. WHEN the Backend receives a shutdown signal, THE Backend SHALL allow currently-running scheduled tasks to complete before terminating (up to the configured shutdown timeout).

**7.3 CORS Configuration**

15. THE Backend SHALL read allowed origins from the `CORS_ALLOWED_ORIGINS` environment variable as a comma-separated list of origin URLs.

16. WHILE `CORS_ALLOWED_ORIGINS` is configured, THE Backend SHALL respond to cross-origin requests from listed origins with appropriate `Access-Control-Allow-Origin`, `Access-Control-Allow-Methods`, `Access-Control-Allow-Headers`, and `Access-Control-Allow-Credentials` headers.

17. THE Backend SHALL allow the following methods in CORS responses: GET, POST, PUT, PATCH, DELETE, OPTIONS.

18. THE Backend SHALL allow the following headers in CORS responses: `Authorization`, `Content-Type`, `X-Trace-Id`.

19. THE Backend SHALL set `Access-Control-Allow-Credentials` to `true` in CORS responses to allow cookies and authorization headers.

20. WHEN a preflight (OPTIONS) request is received from an allowed origin, THE Backend SHALL respond with the appropriate `Access-Control-*` headers and HTTP 204 with no body.

21. IF `CORS_ALLOWED_ORIGINS` is not set or is empty, THEN THE Backend SHALL omit all CORS headers from responses, effectively rejecting cross-origin requests.

**7.4 Global Request Size Limits**

22. THE Backend SHALL enforce a default maximum request body size of 1 MB for all endpoints.

23. THE Backend SHALL override the default request body limit to 10 MB for all `/sync/*` endpoints.

24. IF a request body exceeds the applicable size limit, THEN THE Backend SHALL return HTTP 413 with error code `REQUEST_BODY_TOO_LARGE`.

**7.5 Standard Error Response Format**

25. THE Backend SHALL return all API errors using the following JSON structure: an `error` object containing `code` (string, SCREAMING_SNAKE_CASE) and `message` (string, descriptive English sentence), and a top-level `trace_id` field (UUID v4).

26. WHEN a request validation error occurs, THE Backend SHALL include an additional `details` array within the `error` object, where each entry contains `field` (string) and `message` (string describing the field-level error).

27. THE Backend SHALL include the `trace_id` in every error response for correlation with log entries.

**7.6 Request Processing Pipeline**

28. THE Backend SHALL process every request through the following middleware stages in strict order: (1) parse request body, (2) rate limiting check, (3) authentication, (4) client type restriction, (5) authorization/role check, (6) request body validation, (7) business logic execution, (8) response.

29. IF request body parsing fails (malformed JSON), THEN THE Backend SHALL return HTTP 400 with error code `MALFORMED_REQUEST_BODY` without proceeding to subsequent pipeline stages.

30. IF the rate limit is exceeded, THEN THE Backend SHALL return HTTP 429 with a `Retry-After` header without proceeding to authentication or subsequent stages.

31. IF authentication fails (missing or invalid token), THEN THE Backend SHALL return HTTP 401 with error code `UNAUTHORIZED` without proceeding to authorization or subsequent stages.

32. IF the client type restriction is violated, THEN THE Backend SHALL return HTTP 403 with error code `CLIENT_TYPE_NOT_ALLOWED` without proceeding to business logic.

33. IF authorization or role check fails, THEN THE Backend SHALL return HTTP 403 with error code `FORBIDDEN` without proceeding to business logic.

34. IF request body validation fails, THEN THE Backend SHALL return HTTP 422 with error code `VALIDATION_ERROR` and a `details` array of per-field errors.

**7.7 Panic Recovery**

35. THE Backend SHALL include a tower/axum catch-panic layer that prevents handler panics from terminating the server process.

36. WHEN a panic occurs within a request handler, THE Backend SHALL log the full stack trace at ERROR level with the request's `trace_id`, return HTTP 500 with error code `INTERNAL_ERROR` and the `trace_id`, and terminate only the affected request while continuing to serve other requests.

37. THE Backend SHALL treat every logged panic as an actionable bug requiring investigation.

**7.8 Database Statement Timeout**

38. THE Backend SHALL enforce a configurable database statement timeout read from `DATABASE_STATEMENT_TIMEOUT_SECS` (default 30 seconds) on all database queries.

39. IF a database statement exceeds the configured timeout, THEN THE Backend SHALL cancel the query and return HTTP 503 with error code `DATABASE_TIMEOUT`.

40. THE Backend SHALL apply the same timeout value to database transactions.

41. WHEN the database connection pool is exhausted (no connections available within the configured `DATABASE_CONNECT_TIMEOUT_SECS`), THE Backend SHALL immediately return HTTP 503 with error code `SERVICE_UNAVAILABLE` without blocking or hanging the request.

**7.9 WebSocket Resource Limits**

42. THE Backend SHALL enforce a maximum of 5 concurrent WebSocket connections per user across all workspaces.

43. IF a user exceeds 5 concurrent WebSocket connections, THEN THE Backend SHALL close the oldest connection with WebSocket close code 1008.

44. THE Backend SHALL enforce a configurable server-wide maximum WebSocket connection count read from `WS_MAX_CONNECTIONS` (default 10,000).

45. IF the server-wide WebSocket connection limit is reached, THEN THE Backend SHALL reject new WebSocket upgrade requests with HTTP 503.

46. WHEN a WebSocket connection has been idle (no application message sent or received, excluding ping/pong) for 5 minutes, THE Backend SHALL close the connection.

**7.10 Security Headers**

47. THE Backend SHALL include the following headers in every HTTP response: `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, `Referrer-Policy: strict-origin-when-cross-origin`, `X-XSS-Protection: 0`.

48. WHILE the Backend is serving behind a TLS-terminating reverse proxy, THE Backend SHALL include `Strict-Transport-Security: max-age=31536000; includeSubDomains` in every HTTP response.

49. THE Backend SHALL include `Content-Security-Policy: default-src 'none'` in every HTTP response.

**7.11 Reverse Proxy and Client IP**

50. THE Backend SHALL read a trusted proxy list from the `TRUSTED_PROXY_CIDRS` environment variable as a comma-separated list of CIDR ranges.

51. WHEN a request arrives from an IP within a trusted proxy CIDR, THE Backend SHALL extract the client IP from the `X-Forwarded-For` header using the rightmost untrusted IP address.

52. WHEN a request arrives from an IP outside all trusted proxy CIDRs, THE Backend SHALL use the TCP peer address as the client IP.

53. THE Backend SHALL use the resolved client IP for per-IP rate limiting, brute-force tracking, audit logging, and security event logging.

54. THE Backend SHALL NOT terminate TLS; the Backend serves plain HTTP and relies on the reverse proxy for TLS termination.

**7.12 Health and Readiness Endpoints**

55. THE Backend SHALL expose a `GET /health` liveness endpoint returning `{ "status": "ok", "db": "ok" }` with HTTP 200 when healthy, verifying Postgres reachability before responding.

56. WHEN the Postgres connection pool is unreachable at health check time, THE Backend SHALL return `{ "status": "degraded", "db": "error" }` with HTTP 503.

57. THE Backend SHALL expose a `GET /ready` readiness endpoint returning HTTP 200 with `{ "status": "ready" }` only after all of the following are satisfied: database migrations complete, connection pool established, background scheduler started, and email service initialized.

58. WHILE any readiness check has not passed, THE Backend SHALL return HTTP 503 on `GET /ready` with `{ "status": "not_ready", "checks": { "db": "ok"|"pending", "scheduler": "ok"|"pending", "email": "ok"|"pending" } }`.

**7.13 Deferred Task Retry**

59. THE Backend SHALL retry all asynchronous/deferred tasks (webhook processing, email delivery, background cleanup) on transient failures using exponential backoff: attempt 1 after 1 second, attempt 2 after 5 seconds, attempt 3 after 30 seconds.

60. IF a deferred task fails after all 3 retry attempts, THEN THE Backend SHALL log the error with full context and record the failure to the `audit_logs` table or a dedicated error log with sufficient information to manually replay the task.

61. THE Backend SHALL classify the following as transient failures eligible for retry: database connection timeout, email provider timeout, and network errors.

62. THE Backend SHALL NOT retry non-transient failures (validation errors, resource not found) and SHALL log them immediately without re-attempt.

**7.14 AI Concurrent Request Limit**

63. THE Backend SHALL enforce a configurable maximum number of concurrent in-flight requests to the AI Provider, read from `AI_MAX_CONCURRENT_REQUESTS` (default 50).

64. WHEN the AI concurrent request limit is reached, THE Backend SHALL queue additional requests up to a maximum queue depth of 100 with a 5-second queue timeout.

65. IF the AI request queue is full or the 5-second queue timeout is exceeded, THEN THE Backend SHALL return HTTP 429 with error code `AI_SERVICE_BUSY`.

**7.15 Graceful Shutdown**

66. WHEN the Backend process receives `SIGTERM` or `SIGINT`, THE Backend SHALL stop accepting new HTTP connections immediately.

67. WHEN the Backend process receives `SIGTERM` or `SIGINT`, THE Backend SHALL stop accepting new WebSocket upgrade requests.

68. WHEN the Backend process receives `SIGTERM` or `SIGINT`, THE Backend SHALL send WebSocket Close frame with code 1001 (Going Away) to all active WebSocket sessions.

69. WHILE shutting down, THE Backend SHALL allow in-flight HTTP requests up to 30 seconds (configurable via `SHUTDOWN_TIMEOUT_SECS`) to complete.

70. WHILE shutting down, THE Backend SHALL allow currently-running background tasks up to 30 seconds (configurable via `SHUTDOWN_TIMEOUT_SECS`) to finish.

71. IF in-flight requests or background tasks have not completed within the configured shutdown timeout, THEN THE Backend SHALL force-terminate remaining requests and tasks.

72. WHEN all connections and tasks are terminated, THE Backend SHALL close the database connection pool, log "shutdown complete", and exit with code 0.

**7.16 Expanded Audit Logging (Security Events)**

73. THE Backend SHALL log the following authentication security events to the `audit_logs` table: successful login, failed login, account locked, password reset requested, password changed, and token reuse detected.

74. WHEN logging a successful login event, THE Backend SHALL record `user_id`, `ip_address`, `user_agent`, and `client_type`.

75. WHEN logging a failed login event, THE Backend SHALL record `email`, `ip_address`, and `user_agent` (with `user_id` set to NULL).

76. WHEN logging an account locked event, THE Backend SHALL record `email` and `ip_address`.

77. WHEN logging a password reset requested event, THE Backend SHALL record `user_id` and `ip_address`.

78. WHEN logging a password changed event, THE Backend SHALL record `user_id` and `ip_address`.

79. WHEN logging a token reuse detected event, THE Backend SHALL record `user_id`, `ip_address`, and `user_agent`.

80. THE Backend SHALL store each security audit record with the following fields: `id`, `user_id` (nullable), `action`, `ip_address`, `user_agent`, `client_type`, `target_resource`, `target_id`, `result` (`success` or `failure`), `trace_id`, `created_at`.

81. THE Backend SHALL treat security audit log records as append-only and immutable; the records SHALL never be purged.

82. THE Backend SHALL write authentication security events directly from the auth service without routing through admin middleware.

**7.17 Rate Limiting**

83. THE Backend SHALL apply a per-IP rate limit of 100 requests per minute on all unauthenticated endpoints, returning HTTP 429 with a `Retry-After` header when the limit is exceeded.

84. THE Backend SHALL apply a per-user rate limit of 500 requests per minute on all authenticated endpoints, returning HTTP 429 with a `Retry-After` header when the limit is exceeded.

**7.18 Trace ID Generation**

85. WHEN a request is received, THE Backend SHALL generate a unique `trace_id` (UUID v4), propagate it through the request context, include it in all log entries for that request, and return it in the `X-Trace-Id` response header.

**7.19 Structured Logging**

86. THE Backend SHALL emit structured JSON log lines to stdout with the following minimum fields per entry: `timestamp`, `level`, `trace_id`, `method`, `path`, `status_code`, `duration_ms`.

**7.20 Environment Variable Configuration**

87. THE Backend SHALL read all runtime configuration exclusively from environment variables, with no defaults for secrets.

88. THE Backend SHALL require the following environment variables at startup (process terminates with non-zero exit code if absent): `DATABASE_URL`, `JWT_SECRET`, `GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET`, `GITHUB_CLIENT_ID`, `GITHUB_CLIENT_SECRET`, `OAUTH_REDIRECT_BASE_URL`, `AI_PROVIDER_URL`, `AI_PROVIDER_KEY`, `BILLING_WEBHOOK_SECRET`, `EMAIL_FROM_ADDRESS`, `SEED_ADMIN_EMAIL`, `SEED_ADMIN_PASSWORD`, and the email provider credentials (`EMAIL_SMTP_HOST`/`EMAIL_SMTP_PORT`/`EMAIL_SMTP_USER`/`EMAIL_SMTP_PASSWORD` when `EMAIL_PROVIDER=smtp`, or `EMAIL_API_KEY`/`EMAIL_API_URL` when `EMAIL_PROVIDER=api`).

89. IF a required environment variable is absent at startup, THEN THE Backend SHALL log the missing variable name and terminate with a non-zero exit code before binding any port.

90. THE Backend SHALL support the following optional environment variables with defaults: `PORT` (default 8080), `LOG_LEVEL` (default "info"), `DATABASE_MAX_CONNECTIONS` (default 20), `DATABASE_MIN_CONNECTIONS` (default 5), `DATABASE_CONNECT_TIMEOUT_SECS` (default 5), `DATABASE_IDLE_TIMEOUT_SECS` (default 300), `DATABASE_STATEMENT_TIMEOUT_SECS` (default 30), `CORS_ALLOWED_ORIGINS` (default: none/reject), `TRUSTED_PROXY_CIDRS` (default: none), `WS_MAX_CONNECTIONS` (default 10000), `AI_MAX_CONCURRENT_REQUESTS` (default 50), `SHUTDOWN_TIMEOUT_SECS` (default 30).

**7.21 Single Port**

91. THE Backend SHALL serve all HTTP and WebSocket traffic on a single configurable TCP port (default 8080).

---

> **Explicitly Deferred (Not In Scope):**
> - API versioning (e.g., `/v1/` path prefix or `Accept` header versioning)
> - Prometheus/OpenTelemetry metrics endpoint
> - Distributed task queue (Redis, RabbitMQ, etc.) — the internal tokio scheduler is sufficient for current scale
> - Request/response compression (gzip/brotli) — deferred until performance profiling indicates need
