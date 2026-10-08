//! Rate-limiter port for authentication and contribution endpoints. Login,
//! registration, password reset and verification resend
//! so authentication never ships without brute-force/account-enumeration
//! protection. Credential-sensitive checks have an explicit failure policy.

use async_trait::async_trait;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum RateLimitError {
    #[error("rate limiter unavailable")]
    Unavailable,
    #[error("rate limiter error: {0}")]
    Unexpected(String),
}

/// Port: enforce a sliding-window limit on a key.
///
/// `Ok(true)` = allowed; `Ok(false)` = over the limit (`limit` hits within
/// `window`).
#[async_trait]
pub trait RateLimiter: Send + Sync {
    async fn check(&self, key: &str, limit: u32, window: Duration) -> Result<bool, RateLimitError>;

    /// Credential-sensitive admission. Implementations with a configurable
    /// degradation policy override this to fail closed; simple test/in-memory
    /// limiters share the normal counter behavior.
    async fn check_sensitive(
        &self,
        key: &str,
        limit: u32,
        window: Duration,
    ) -> Result<bool, RateLimitError> {
        self.check(key, limit, window).await
    }
}
