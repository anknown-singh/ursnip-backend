use chrono::Utc;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Algorithm, Validation};

use crate::errors::AppError;
use crate::middleware::auth_extractor::AccessTokenClaims;
use crate::models::common::Role;

/// Access token TTL for regular users: 15 minutes.
const USER_TTL_SECONDS: i64 = 15 * 60;

/// Access token TTL for admin users: 5 minutes.
const ADMIN_TTL_SECONDS: i64 = 5 * 60;

/// Encode an access token JWT using HS256.
///
/// Sets the `exp` claim based on the user's role:
/// - `Role::User` → 15 minutes from now
/// - `Role::Admin` → 5 minutes from now
///
/// The `exp` field on the input claims is overridden.
pub fn encode_access_token(mut claims: AccessTokenClaims, secret: &str) -> String {
    let ttl = match claims.role {
        Role::User => USER_TTL_SECONDS,
        Role::Admin => ADMIN_TTL_SECONDS,
    };

    claims.exp = Utc::now().timestamp() + ttl;

    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .expect("JWT encoding should not fail with valid claims")
}

/// Decode and validate an access token JWT.
///
/// Verifies the HS256 signature and checks that the token has not expired.
/// Returns `AppError::Unauthorized` on any failure (invalid signature, expired, malformed).
pub fn decode_access_token(token: &str, secret: &str) -> Result<AccessTokenClaims, AppError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;

    let token_data = decode::<AccessTokenClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .map_err(|_| AppError::Unauthorized)?;

    Ok(token_data.claims)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::common::{ClientType, Tier};
    use uuid::Uuid;

    const TEST_SECRET: &str = "test-jwt-secret-for-unit-tests";

    fn user_claims() -> AccessTokenClaims {
        AccessTokenClaims {
            sub: Uuid::new_v4(),
            client_type: ClientType::Native,
            role: Role::User,
            permissions: vec!["snippets:read".to_string()],
            subscription_tier: Tier::Free,
            status: "active".to_string(),
            must_reset_password: false,
            exp: 0,
        }
    }

    fn admin_claims() -> AccessTokenClaims {
        AccessTokenClaims {
            sub: Uuid::new_v4(),
            client_type: ClientType::Web,
            role: Role::Admin,
            permissions: vec!["admin:all".to_string()],
            subscription_tier: Tier::Pro,
            status: "active".to_string(),
            must_reset_password: false,
            exp: 0,
        }
    }

    #[test]
    fn encode_user_token_sets_15_min_ttl() {
        let claims = user_claims();
        let before = Utc::now().timestamp();
        let token = encode_access_token(claims, TEST_SECRET);
        let after = Utc::now().timestamp();

        let decoded = decode_access_token(&token, TEST_SECRET).unwrap();
        // exp should be ~15 minutes from now
        assert!(decoded.exp >= before + USER_TTL_SECONDS);
        assert!(decoded.exp <= after + USER_TTL_SECONDS);
    }

    #[test]
    fn encode_admin_token_sets_5_min_ttl() {
        let claims = admin_claims();
        let before = Utc::now().timestamp();
        let token = encode_access_token(claims, TEST_SECRET);
        let after = Utc::now().timestamp();

        let decoded = decode_access_token(&token, TEST_SECRET).unwrap();
        // exp should be ~5 minutes from now
        assert!(decoded.exp >= before + ADMIN_TTL_SECONDS);
        assert!(decoded.exp <= after + ADMIN_TTL_SECONDS);
    }

    #[test]
    fn decode_preserves_all_claims() {
        let claims = user_claims();
        let original_sub = claims.sub;
        let token = encode_access_token(claims, TEST_SECRET);

        let decoded = decode_access_token(&token, TEST_SECRET).unwrap();
        assert_eq!(decoded.sub, original_sub);
        assert_eq!(decoded.client_type, ClientType::Native);
        assert_eq!(decoded.role, Role::User);
        assert_eq!(decoded.permissions, vec!["snippets:read".to_string()]);
        assert_eq!(decoded.subscription_tier, Tier::Free);
        assert_eq!(decoded.status, "active");
        assert!(!decoded.must_reset_password);
    }

    #[test]
    fn decode_with_wrong_secret_returns_unauthorized() {
        let claims = user_claims();
        let token = encode_access_token(claims, TEST_SECRET);

        let result = decode_access_token(&token, "wrong-secret");
        assert!(result.is_err());
    }

    #[test]
    fn decode_expired_token_returns_unauthorized() {
        // Manually create an already-expired token
        let mut claims = user_claims();
        claims.exp = Utc::now().timestamp() - 3600; // expired 1 hour ago

        let token = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(TEST_SECRET.as_bytes()),
        )
        .unwrap();

        let result = decode_access_token(&token, TEST_SECRET);
        assert!(result.is_err());
    }

    #[test]
    fn decode_malformed_token_returns_unauthorized() {
        let result = decode_access_token("not.a.valid.token", TEST_SECRET);
        assert!(result.is_err());
    }
}
