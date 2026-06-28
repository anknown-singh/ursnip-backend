# ursnip-backend

Cloud backend for the Ursnip desktop application. A Rust/Axum monolithic HTTP server providing authentication, real-time snippet synchronization, AI-powered expansion, subscription billing, team workspaces, and an admin management API.

## Architecture

```
┌──────────────┐     ┌──────────────┐
│ Native Client│     │  Web Client  │
│(Tauri Desktop)│    │  (Browser)   │
└──────┬───────┘     └──────┬───────┘
       │  REST + WS          │  REST only
       └──────────┬──────────┘
          ┌───────▼────────┐
          │  Axum Backend  │
          │  (Single Port) │
          └───┬───┬───┬────┘
              │   │   │
   ┌──────────┘   │   └──────────┐
   │              │               │
┌──▼────┐  ┌─────▼──────┐  ┌─────▼──────────┐
│Postgres│  │AI Provider │  │Billing Provider│
└────────┘  └────────────┘  └────────────────┘
```

## Services

| Service | Description |
|---------|-------------|
| **Auth** | Registration, login, OAuth (Google/GitHub), JWT issuance, token rotation, password reset, brute-force protection, session management |
| **Sync** | Snippet/folder CRUD, workspace-scoped versioning, batch operations, WebSocket real-time push, delta polling |
| **AI** | Proxy to AI provider with tier-based quotas (50/free, 1000/pro) and concurrency control |
| **Subscription** | Tier management, checkout with invoice computation, billing webhooks, grace periods, coupon/referral system |
| **Workspace/Teams** | Workspace creation, team invitations, seat management (max 3) |
| **Admin** | User/workspace management, subscription overrides, feature flags, audit logs, analytics |
| **Email** | Provider abstraction (SMTP/API), templated emails, async dispatch with retry |
| **Scheduler** | Recurring background tasks (delta purge, token cleanup, grace period checks) |

## Quick Start

### Prerequisites

- Rust 1.75+ (install via [rustup](https://rustup.rs))
- PostgreSQL 14+
- Docker (optional, for test database)

### Setup

```bash
# Clone and enter the project
cd ursnip-backend

# Copy environment config
cp .env.example .env
# Edit .env with your credentials

# Start PostgreSQL (or use Docker)
docker run -d --name ursnip-pg \
  -e POSTGRES_USER=ursnip -e POSTGRES_PASSWORD=ursnip -e POSTGRES_DB=ursnip \
  -p 5432:5432 postgres:16-alpine

# Build and run
cargo run
```

The server starts on `http://localhost:8080`. Check health at `GET /health`.

### Running Tests

```bash
# Unit tests + property tests (no database needed)
cargo test

# Full suite including integration tests (requires Docker)
./scripts/test-full.sh
```

## API Overview

### Public Endpoints (No Auth)

| Method | Path | Description |
|--------|------|-------------|
| GET | `/health` | Liveness check |
| GET | `/ready` | Readiness check |
| POST | `/auth/register` | Register with email+password |
| POST | `/auth/login` | Login |
| POST | `/auth/refresh` | Rotate refresh token |
| POST | `/auth/forgot-password` | Request password reset |
| POST | `/auth/reset-password` | Reset password with token |
| GET | `/auth/oauth/{provider}/authorize` | OAuth redirect |
| GET | `/auth/oauth/{provider}/callback` | OAuth callback |
| GET | `/auth/verify-email-change` | Verify email change token |
| POST | `/webhooks/billing` | Billing webhook (signature verified) |

### Auth Required — Native + Web

| Method | Path | Description |
|--------|------|-------------|
| POST | `/auth/logout` | Invalidate refresh token |
| PATCH | `/auth/profile` | Update profile |
| POST | `/auth/change-email` | Initiate email change |
| POST | `/auth/change-password` | Change password |
| DELETE | `/auth/account` | Soft-delete account |
| GET | `/auth/sessions` | List active sessions |
| DELETE | `/auth/sessions/{id}` | Revoke session |

### Sync (Native Only)

| Method | Path | Description |
|--------|------|-------------|
| POST | `/sync/snippets` | Create snippet |
| PATCH | `/sync/snippets/{id}` | Update snippet |
| DELETE | `/sync/snippets/{id}` | Soft-delete snippet |
| POST | `/sync/snippets/batch` | Batch operations (max 100) |
| POST | `/sync/folders` | Create folder |
| PATCH | `/sync/folders/{id}` | Update folder |
| DELETE | `/sync/folders/{id}` | Soft-delete folder |
| GET | `/sync/snapshot` | Full workspace snapshot |
| GET | `/sync/deltas` | Delta polling |
| GET | `/sync/ws` | WebSocket upgrade |

### AI (Native Only)

| Method | Path | Description |
|--------|------|-------------|
| POST | `/ai/expand` | AI snippet expansion |

### Subscriptions (Web Only)

| Method | Path | Description |
|--------|------|-------------|
| POST | `/subscriptions/upgrade` | Initiate upgrade |
| POST | `/subscriptions/checkout` | Compute invoice + checkout URL |
| GET | `/subscriptions/current` | Current subscription details |

### Teams (Web Only)

| Method | Path | Description |
|--------|------|-------------|
| POST | `/teams` | Create team workspace |
| POST | `/teams/{id}/invites` | Generate invite link |
| POST | `/teams/{id}/join` | Join via invite code |
| DELETE | `/teams/{id}/members/{user_id}` | Remove member |
| GET | `/teams/{id}` | Workspace details |
| GET | `/teams/{id}/members` | List members |

### Admin (Web Only, role=admin)

38 endpoints covering users, workspaces, subscriptions, coupons, discounts, tax rates, feature flags, audit logs, and analytics. See `docs/API.md` for full details.

## Environment Variables

See `.env.example` for the complete list. Key required variables:

| Variable | Description |
|----------|-------------|
| `DATABASE_URL` | PostgreSQL connection string |
| `JWT_SECRET` | Secret for signing JWTs |
| `GOOGLE_CLIENT_ID/SECRET` | Google OAuth credentials |
| `GITHUB_CLIENT_ID/SECRET` | GitHub OAuth credentials |
| `AI_PROVIDER_URL/KEY` | AI expansion service |
| `BILLING_WEBHOOK_SECRET` | Billing webhook signature key |
| `EMAIL_PROVIDER` | `smtp` or `api` |
| `SEED_ADMIN_EMAIL/PASSWORD` | Initial super-admin account |

## Project Structure

```
src/
├── main.rs              # Entry point, startup sequence
├── config.rs            # Environment config loading
├── errors.rs            # Unified error types
├── router.rs            # Route definitions + middleware layering
├── auth/                # Authentication service
├── sync/                # Snippet sync + WebSocket
├── ai/                  # AI expansion proxy
├── admin/               # Admin management
├── subscription/        # Billing + subscriptions
├── workspace/           # Teams + workspaces
├── email/               # Email provider abstraction
├── scheduler/           # Background task scheduler
├── middleware/           # 11-layer middleware stack
├── db/                  # Database pool setup
└── models/              # Shared types
migrations/              # SQL migrations (forward-only)
tests/                   # Integration + property tests
```

## Middleware Stack (Applied in Order)

1. Trace ID (UUID per request)
2. Security headers (CSP, HSTS, X-Frame-Options, etc.)
3. CORS (configurable origins)
4. Body size limit (1 MB default, 10 MB for sync)
5. Panic recovery (catch-panic → 500)
6. IP rate limit (100 req/min)
7. Auth extraction (JWT → claims)
8. Client type guard (native/web enforcement)
9. Subscription context injection
10. User rate limit (500 req/min)
11. Admin guard (role=admin check)

## Testing

| Category | Count | Requires DB |
|----------|-------|-------------|
| Unit tests | 117 | No |
| Property tests (proptest) | 78 | No |
| In-memory integration | 40 | No |
| WebSocket registry tests | 3 | No |
| DB integration tests | 62 | Yes |
| **Total** | **300** | |

## License

Proprietary. All rights reserved.
