-- Migration: 0002_seed.sql
-- Description: Seed super-admin user and default feature flags
-- Note: The super-admin password_hash is a placeholder. The application must
--       hash SEED_ADMIN_PASSWORD (Argon2id) and update the record on first startup.

-- 1. Super-admin user
-- Uses placeholder values; the app overwrites email/password_hash from
-- SEED_ADMIN_EMAIL and SEED_ADMIN_PASSWORD environment variables at boot.
INSERT INTO users (
    id,
    email,
    password_hash,
    first_name,
    last_name,
    role,
    status,
    referral_code,
    must_reset_password
)
VALUES (
    gen_random_uuid(),
    'admin@ursnip.local',
    'PLACEHOLDER_REQUIRES_APP_INIT',
    'Super',
    'Admin',
    'admin',
    'active',
    gen_random_uuid()::text,
    FALSE
)
ON CONFLICT (email) DO NOTHING;

-- 2. Default feature flags (all disabled)
INSERT INTO feature_flags (name, enabled, description)
VALUES
    ('ai-expansion', FALSE, 'AI-powered snippet expansion'),
    ('team-workspaces', FALSE, 'Team workspace creation'),
    ('oauth-google', FALSE, 'Google OAuth login'),
    ('oauth-github', FALSE, 'GitHub OAuth login'),
    ('billing-webhooks', FALSE, 'Billing webhook processing'),
    ('email-notifications', FALSE, 'Email notification sending')
ON CONFLICT (name) DO NOTHING;
