# ursnip-backend API Documentation

Base URL: `http://localhost:8080` (configurable via `PORT` env var)

All endpoints return JSON. Authenticated endpoints require `Authorization: Bearer <access_token>`.

## Error Response Format

All errors follow a consistent structure:

```json
{
  "trace_id": "550e8400-e29b-41d4-a716-446655440000",
  "error": {
    "code": "SCREAMING_SNAKE_CASE",
    "message": "Human-readable description"
  }
}
```

Common HTTP status codes:
- `401` — Authentication required or invalid credentials
- `403` — Forbidden (wrong client type, suspended, etc.)
- `404` — Resource not found
- `409` — Conflict (duplicate resource)
- `413` — Request body too large
- `422` — Validation error
- `429` — Rate limited (includes `Retry-After` header)
- `500` — Internal server error

---

## Health

### GET /health

Liveness check. Returns database reachability status.

**Auth:** None

**Response 200:**
```json
{
  "status": "ok",
  "db": "ok"
}
```

**Response 503:**
```json
{
  "status": "degraded",
  "db": "error"
}
```

### GET /ready

Readiness check. Verifies all subsystems are initialized.

**Auth:** None

**Response 200:**
```json
{
  "status": "ready",
  "checks": {
    "migrations": "ok",
    "pool": "ok",
    "scheduler": "ok",
    "email": "ok"
  }
}
```

---

## Authentication

### POST /auth/register

Register a new user account.

**Auth:** None  
**Client Type:** native, web

**Request:**
```json
{
  "email": "user@example.com",
  "password": "minimum8chars",
  "client_type": "native",
  "referral_code": "ABC12345",
  "first_name": "John",
  "last_name": "Doe"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| email | string | yes | User email address |
| password | string | yes | Minimum 8 characters |
| client_type | string | yes | `native` or `web` |
| referral_code | string | no | Referral code from another user |
| first_name | string | no | User's first name |
| last_name | string | no | User's last name |

**Response 201:**
```json
{
  "access_token": "eyJ...",
  "refresh_token": "opaque-token-string",
  "user": {
    "id": "uuid",
    "email": "user@example.com",
    "role": "user",
    "referral_code": "XYZ98765"
  }
}
```

**Errors:**
- `409 EMAIL_ALREADY_REGISTERED`
- `422 PASSWORD_TOO_SHORT` (< 8 chars)

### POST /auth/login

Authenticate with email and password.

**Auth:** None  
**Client Type:** native, web

**Request:**
```json
{
  "email": "user@example.com",
  "password": "userpassword",
  "client_type": "native"
}
```

**Response 200:** Same as register response.

**Errors:**
- `401 INVALID_CREDENTIALS` (minimum 100ms response time)
- `429 ACCOUNT_LOCKED` (after 5 failures, 15-min lockout; includes `Retry-After`)

### POST /auth/refresh

Rotate refresh token and get a new access/refresh pair.

**Auth:** None (uses refresh token in body)

**Request:**
```json
{
  "refresh_token": "current-refresh-token",
  "client_type": "native"
}
```

**Response 200:**
```json
{
  "access_token": "eyJ...",
  "refresh_token": "new-opaque-token"
}
```

**Errors:**
- `401 INVALID_REFRESH_TOKEN` (expired/revoked)
- `401 TOKEN_REUSE_DETECTED` (all tokens revoked — possible theft)

### POST /auth/logout

Invalidate the current refresh token.

**Auth:** Required

**Request:**
```json
{
  "refresh_token": "token-to-revoke"
}
```

**Response:** `204 No Content`

### GET /auth/oauth/{provider}/authorize

Redirect to OAuth provider. Provider is `google` or `github`.

**Auth:** None  
**Query Params:** `client=native` or `client=web`

**Response:** `302 Redirect` to provider authorization URL.

### GET /auth/oauth/{provider}/callback

OAuth callback handler. Exchanges code for tokens.

**Auth:** None  
**Query Params:** `code`, `state`, `error` (optional)

**Response 200:** Same as register/login response (new or existing user).

**Errors:**
- `401 OAUTH_AUTHORIZATION_DENIED` (error param present)
- `409 ACCOUNT_LINKING_CONFLICT`
- `422 EMAIL_VERIFICATION_REQUIRED`

### POST /auth/forgot-password

Request a password reset email. Always returns 200 (no email enumeration).

**Auth:** None

**Request:**
```json
{
  "email": "user@example.com"
}
```

**Response:** `200 OK` (always, regardless of email existence)

**Rate Limit:** 3 requests/hour per email.

### POST /auth/reset-password

Reset password using a valid token.

**Auth:** None

**Request:**
```json
{
  "token": "reset-token-from-email",
  "password": "newpassword123"
}
```

**Response:** `200 OK`

**Errors:**
- `422 INVALID_RESET_TOKEN` (expired, used, or invalid)
- `422 PASSWORD_TOO_SHORT`

### PATCH /auth/profile

Update user profile fields.

**Auth:** Required

**Request (all fields optional):**
```json
{
  "first_name": "John",
  "last_name": "Doe",
  "profile_picture_url": "https://...",
  "timezone": "America/New_York",
  "language": "en",
  "country_code": "US",
  "phone": "+1234567890"
}
```

**Response 200:** Updated user profile object.

### POST /auth/change-email

Initiate email change. Sends verification to new address.

**Auth:** Required

**Request:**
```json
{
  "new_email": "newemail@example.com"
}
```

**Response:** `200 OK`

### GET /auth/verify-email-change

Verify email change token (from email link).

**Auth:** None  
**Query Params:** `token=...`

**Response:** `200 OK`

### POST /auth/change-password

Change password (requires current password).

**Auth:** Required

**Request:**
```json
{
  "current_password": "oldpass",
  "new_password": "newpass123"
}
```

**Response:** `200 OK` (all refresh tokens revoked)

**Errors:**
- `401 INVALID_CURRENT_PASSWORD`
- `422 PASSWORD_TOO_SHORT`

### DELETE /auth/account

Soft-delete the authenticated user's account. Recoverable for 30 days via login.

**Auth:** Required

**Response:** `204 No Content`

**Errors:**
- `422 TRANSFER_OWNERSHIP_REQUIRED` (owns team workspaces)

### GET /auth/sessions

List active sessions for the authenticated user.

**Auth:** Required

**Response 200:**
```json
[
  {
    "session_id": "uuid",
    "client_type": "native",
    "created_at": "2024-01-15T10:00:00Z",
    "last_used_at": "2024-01-15T12:30:00Z"
  }
]
```

### DELETE /auth/sessions/{session_id}

Revoke a specific session.

**Auth:** Required

**Response:** `204 No Content`

---

## Sync Service

All sync endpoints require `client_type = native`.

### POST /sync/snippets

Create a new snippet.

**Auth:** Required (native only)

**Request:**
```json
{
  "workspace_id": "uuid",
  "trigger": "hello",
  "content": "Hello, world!",
  "snippet_type": "text",
  "folder_id": "uuid (optional)"
}
```

**Response 201:**
```json
{
  "id": "uuid",
  "workspace_id": "uuid",
  "trigger": "hello",
  "content": "Hello, world!",
  "snippet_type": "text",
  "version": 1,
  "created_at": "2024-01-15T10:00:00Z",
  "updated_at": "2024-01-15T10:00:00Z"
}
```

**Errors:**
- `409 TRIGGER_ALREADY_EXISTS` (duplicate in same workspace)
- `422 SNIPPET_LIMIT_REACHED` (free tier: 10 max)
- `422 SNIPPET_CONTENT_TOO_LONG` (free tier: 2000 chars max)

### PATCH /sync/snippets/{id}

Update a snippet.

**Auth:** Required (native only)

**Request:**
```json
{
  "trigger": "new-trigger",
  "content": "Updated content",
  "snippet_type": "text"
}
```

**Response 200:** Updated snippet object with incremented version.

### DELETE /sync/snippets/{id}

Soft-delete a snippet.

**Auth:** Required (native only)

**Response:** `204 No Content`

### POST /sync/snippets/batch

Execute multiple snippet operations atomically.

**Auth:** Required (native only)

**Request:**
```json
{
  "workspace_id": "uuid",
  "operations": [
    {
      "type": "create_snippet",
      "workspace_id": "uuid",
      "trigger": "greet",
      "content": "Hi!",
      "snippet_type": "text"
    },
    {
      "type": "update_snippet",
      "id": "uuid",
      "content": "Updated"
    },
    {
      "type": "delete_snippet",
      "id": "uuid"
    }
  ]
}
```

**Response 200:**
```json
{
  "results": [...],
  "workspace_version": 5
}
```

**Errors:**
- `422 BATCH_SIZE_EXCEEDED` (max 100 items)
- Any validation error rolls back the entire batch.

### POST /sync/folders

Create a folder.

**Auth:** Required (native only)

**Request:**
```json
{
  "workspace_id": "uuid",
  "name": "My Folder"
}
```

**Response 201:** Folder object with version.

**Errors:**
- `422 FOLDER_LIMIT_REACHED` (free tier: 3 max)

### PATCH /sync/folders/{id}

Update a folder.

**Auth:** Required (native only)

**Request:**
```json
{
  "name": "Renamed Folder"
}
```

**Response 200:** Updated folder object.

### DELETE /sync/folders/{id}

Soft-delete a folder.

**Auth:** Required (native only)

**Response:** `204 No Content`

### GET /sync/snapshot

Full workspace snapshot (all active snippets + folders).

**Auth:** Required (native only)  
**Query Params:** `workspace_id=uuid`

**Response 200:**
```json
{
  "workspace_id": "uuid",
  "version": 42,
  "snippets": [...],
  "folders": [...]
}
```

### GET /sync/deltas

Delta polling for incremental sync.

**Auth:** Required (native only)  
**Query Params:** `workspace_id=uuid`, `since_version=N`, `limit=500` (optional, max 1000)

**Response 200:**
```json
{
  "deltas": [
    {
      "id": "uuid",
      "entity_type": "snippet",
      "entity_id": "uuid",
      "operation": "create",
      "payload": {...},
      "version": 43
    }
  ],
  "has_more": false,
  "next_since_version": 43
}
```

**Errors:**
- `409 SNAPSHOT_REQUIRED` (since_version older than 30-day retention)
- `422 INVALID_SINCE_VERSION`

### GET /sync/ws

WebSocket upgrade for real-time push.

**Auth:** Token in `Authorization` header or `token` query param  
**Query Params:** `workspace_id`, `last_known_version` (optional, for catch-up)

**Protocol:** JSON envelope with types: `delta`, `snapshot_required`, `ack`, `error`, `ping`, `pong`

**Heartbeat:** Server sends `ping` every 30s. Close after 2 missed pongs.

---

## AI Service

### POST /ai/expand

Expand a snippet using the AI provider.

**Auth:** Required (native only)

**Request:**
```json
{
  "trigger": "greeting",
  "system_prompt": "Generate a professional greeting",
  "context": "Optional additional context"
}
```

**Validation:**
- `trigger`: required, max 500 chars
- `system_prompt`: required, max 10 KB
- `context`: optional, max 50 KB

**Response 200:**
```json
{
  "expanded_content": "Generated text..."
}
```

**Errors:**
- `429 AI_QUOTA_EXCEEDED` (free: 50/day, pro/teams: 1000/day)
- `429 AI_SERVICE_BUSY` (queue full)
- `502 AI_PROVIDER_UNAVAILABLE` (upstream error/timeout)

---

## Subscriptions

All subscription endpoints require `client_type = web`.

### POST /subscriptions/upgrade

Initiate upgrade from free to pro tier.

**Auth:** Required (web only)

**Request:**
```json
{
  "workspace_id": "uuid"
}
```

**Response:** `200 OK`

**Errors:**
- `422 ALREADY_UPGRADED`

### POST /subscriptions/checkout

Compute invoice and get checkout URL.

**Auth:** Required (web only)

**Request:**
```json
{
  "workspace_id": "uuid",
  "tier": "pro",
  "billing_cycle_months": 12,
  "coupon_code": "SAVE20",
  "country_code": "US",
  "success_url": "https://app.example.com/billing/success",
  "cancel_url": "https://app.example.com/billing/cancel"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| workspace_id | UUID | Yes | Workspace to upgrade |
| tier | string | Yes | Target tier (`pro` or `teams`) |
| billing_cycle_months | integer | Yes | Billing period (minimum 12) |
| coupon_code | string | No | Coupon code for discount (cannot combine with discount_id) |
| discount_id | UUID | No | Direct discount ID (cannot combine with coupon_code) |
| country_code | string | No | ISO country code for tax calculation |
| success_url | string | No | URL to redirect after successful payment. Falls back to `BILLING_SUCCESS_URL` env var if not provided. |
| cancel_url | string | No | URL to redirect if user cancels payment. Falls back to `BILLING_CANCEL_URL` env var if not provided. |

**Response 200:**
```json
{
  "checkout_url": "https://checkout.provider.com/session/...",
  "invoice": {
    "base_price": "99.00",
    "discount_amount": "19.80",
    "discount_type": "percentage",
    "subtotal_after_discount": "79.20",
    "tax_rate": "0.18",
    "tax_name": "Sales Tax",
    "tax_amount": "14.26",
    "total_amount": "93.46",
    "billing_cycle_months": 12
  }
}
```

**Errors:**
- `422 MINIMUM_BILLING_CYCLE_NOT_MET` (< 12 months)
- `422 MULTIPLE_DISCOUNTS_NOT_ALLOWED`
- Various coupon errors (see Coupon Validation below)

### GET /subscriptions/current

Get current subscription details.

**Auth:** Required (web only)

**Response 200:**
```json
{
  "id": "uuid",
  "workspace_id": "uuid",
  "tier": "pro",
  "status": "active",
  "period_start": "2024-01-15T00:00:00Z",
  "period_end": "2025-01-15T00:00:00Z"
}
```

---

## Teams

All teams endpoints require `client_type = web`.

### POST /teams

Create a team workspace.

**Auth:** Required (web only)

**Request:**
```json
{
  "name": "My Team"
}
```

**Response 201:** Workspace object.

### POST /teams/{workspace_id}/invites

Generate a team invite link.

**Auth:** Required (web only, must be owner)

**Request:**
```json
{
  "max_uses": 5,
  "expires_in_hours": 72
}
```

**Response 201:**
```json
{
  "invite_code": "abc123xyz",
  "expires_at": "2024-01-18T10:00:00Z"
}
```

### POST /teams/{workspace_id}/join

Join a team using an invite code.

**Auth:** Required (web only)

**Request:**
```json
{
  "invite_code": "abc123xyz"
}
```

**Response:** `200 OK`

**Errors:**
- `422 SEAT_LIMIT_REACHED` (max 3 members)
- `422 ALREADY_A_MEMBER`
- `422 INVITE_USAGE_LIMIT_REACHED`

### DELETE /teams/{workspace_id}/members/{user_id}

Remove a team member.

**Auth:** Required (web only, must be owner)

**Response:** `204 No Content`

**Errors:**
- `422 CANNOT_REMOVE_OWNER`

---

## Admin

All admin endpoints require `client_type = web` and `role = admin`. Every action writes an audit log.

### Users

| Method | Path | Description |
|--------|------|-------------|
| GET | `/admin/users` | List users (paginated, filterable) |
| GET | `/admin/users/{id}` | User detail |
| POST | `/admin/users/{id}/suspend` | Suspend user |
| POST | `/admin/users/{id}/unsuspend` | Unsuspend user |
| POST | `/admin/users/{id}/force-password-reset` | Force password reset |
| DELETE | `/admin/users/{id}` | Soft-delete user |

### Workspaces

| Method | Path | Description |
|--------|------|-------------|
| GET | `/admin/workspaces` | List workspaces |
| GET | `/admin/workspaces/{id}` | Workspace detail |
| POST | `/admin/workspaces/{id}/deactivate` | Deactivate workspace |
| DELETE | `/admin/workspaces/{id}?confirm=true` | Hard-delete workspace |

### Subscriptions

| Method | Path | Description |
|--------|------|-------------|
| GET | `/admin/subscriptions` | List subscriptions |
| GET | `/admin/subscriptions/{id}` | Subscription detail |
| POST | `/admin/subscriptions/{id}/extend` | Extend period |
| POST | `/admin/subscriptions/{id}/cancel` | Force-cancel |
| PATCH | `/admin/subscriptions/{id}/tier` | Override tier |

### Discounts & Coupons

| Method | Path | Description |
|--------|------|-------------|
| GET | `/admin/discounts` | List discounts |
| POST | `/admin/discounts` | Create discount |
| PATCH | `/admin/discounts/{id}` | Update (deactivate only) |
| GET | `/admin/coupons` | List coupons |
| GET | `/admin/coupons/{id}` | Coupon detail |
| POST | `/admin/coupons` | Create platform coupon |
| PATCH | `/admin/coupons/{id}` | Update coupon |
| GET | `/admin/referrals` | Referral statistics |

### Feature Flags

| Method | Path | Description |
|--------|------|-------------|
| GET | `/admin/feature-flags` | List flags |
| POST | `/admin/feature-flags` | Create flag (kebab-case, max 100 chars) |
| PUT | `/admin/feature-flags/{name}` | Update flag |
| DELETE | `/admin/feature-flags/{name}` | Delete flag |

### Other

| Method | Path | Description |
|--------|------|-------------|
| GET | `/admin/tax-rates` | List tax rates |
| POST | `/admin/tax-rates` | Create tax rate |
| PATCH | `/admin/tax-rates/{country_code}` | Update tax rate |
| GET | `/admin/audit-logs` | List audit logs |
| GET | `/admin/audit-logs/{id}` | Audit log detail |
| GET | `/admin/billing-events` | List billing events |
| GET | `/admin/admins` | List admin accounts |
| DELETE | `/admin/admins/{id}` | Demote admin to user |
| POST | `/admin/invites` | Send admin invite |
| GET | `/admin/stats/overview` | Dashboard analytics |
| GET | `/admin/stats/referrals` | Referral analytics |

---

## Webhooks

### POST /webhooks/billing

Receives billing provider webhook events. No JWT — verified via signature.

**Headers:** `X-Webhook-Signature: <hmac-sha256>`

**Supported Events:**
- `subscription.activated`
- `subscription.renewed`
- `subscription.past_due` (sets 7-day grace period)
- `subscription.cancelled`
- `subscription.reactivated`

Idempotent: duplicate `event_id` values are ignored.

---

## Rate Limits

| Scope | Limit | Window |
|-------|-------|--------|
| IP (global) | 100 requests | 1 minute |
| User (authenticated) | 500 requests | 1 minute |
| Admin | 300 requests | 1 minute |
| Sync mutations | 60 requests | 1 minute |
| Sync reads | 120 requests | 1 minute |
| Forgot password | 3 requests | 1 hour |

Exceeded limits return `429` with `Retry-After` header (seconds).

---

## JWT Claims

```json
{
  "sub": "user-uuid",
  "client_type": "native",
  "role": "user",
  "permissions": ["..."],
  "subscription_tier": "free",
  "exp": 1700000000
}
```

**TTL:** 15 minutes (user), 5 minutes (admin)  
**Refresh Token TTL:** 30 days  
**Max Sessions:** 5 per user (6th evicts oldest)
