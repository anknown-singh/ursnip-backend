use argon2::{
    Argon2,
    PasswordHash,
    PasswordHasher,
    PasswordVerifier,
    password_hash::SaltString,
};
use rand::rngs::OsRng;

use crate::AppError;

/// Hash a plaintext password using Argon2id with a random salt.
///
/// Returns the PHC-formatted hash string on success.
pub fn hash_password(password: &str) -> Result<String, AppError> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default(); // Argon2id with default params

    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|_| AppError::InternalError)?;

    Ok(hash.to_string())
}

/// Verify a plaintext password against a PHC-formatted Argon2id hash.
///
/// Uses constant-time comparison internally (provided by the argon2 crate).
/// Returns `true` if the password matches, `false` otherwise.
pub fn verify_password(password: &str, hash: &str) -> Result<bool, AppError> {
    let parsed_hash = PasswordHash::new(hash).map_err(|_| AppError::InternalError)?;

    let argon2 = Argon2::default();

    match argon2.verify_password(password.as_bytes(), &parsed_hash) {
        Ok(()) => Ok(true),
        Err(argon2::password_hash::Error::Password) => Ok(false),
        Err(_) => Err(AppError::InternalError),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_verify_correct_password() {
        let password = "secure_password_123!";
        let hash = hash_password(password).unwrap();

        // Hash should be a PHC string starting with $argon2id$
        assert!(hash.starts_with("$argon2id$"));

        // Verification should succeed with the correct password
        assert!(verify_password(password, &hash).unwrap());
    }

    #[test]
    fn verify_wrong_password_returns_false() {
        let password = "correct_password";
        let hash = hash_password(password).unwrap();

        // Wrong password should return Ok(false), not an error
        assert!(!verify_password("wrong_password", &hash).unwrap());
    }

    #[test]
    fn hash_produces_unique_salts() {
        let password = "same_password";
        let hash1 = hash_password(password).unwrap();
        let hash2 = hash_password(password).unwrap();

        // Two hashes of the same password should differ (different salts)
        assert_ne!(hash1, hash2);

        // But both should verify correctly
        assert!(verify_password(password, &hash1).unwrap());
        assert!(verify_password(password, &hash2).unwrap());
    }

    #[test]
    fn verify_invalid_hash_returns_error() {
        let result = verify_password("password", "not_a_valid_hash");
        assert!(result.is_err());
    }

    #[test]
    fn hash_empty_password_succeeds() {
        // Empty passwords should still hash successfully (validation is separate)
        let hash = hash_password("").unwrap();
        assert!(verify_password("", &hash).unwrap());
        assert!(!verify_password("non_empty", &hash).unwrap());
    }
}
