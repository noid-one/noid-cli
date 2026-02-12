use anyhow::{bail, Result};
use rand::Rng;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;
use subtle::ConstantTimeEq;

const TOKEN_PREFIX: &str = "noid_tok_";
const TOKEN_BYTES: usize = 32; // 64 hex chars

/// Generate a new authentication token.
pub fn generate_token() -> String {
    let mut rng = rand::thread_rng();
    let mut bytes = [0u8; TOKEN_BYTES];
    rng.fill(&mut bytes);
    format!("{}{}", TOKEN_PREFIX, hex_encode(&bytes))
}

/// Hash a token for storage (SHA-256).
pub fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex_encode(&hasher.finalize())
}

/// Verify a token against a stored hash using constant-time comparison.
pub fn verify_token(stored_hash: &str, token: &str) -> bool {
    let candidate_hash = hash_token(token);
    let a = candidate_hash.as_bytes();
    let b = stored_hash.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    a.ct_eq(b).into()
}

/// Validate that a string looks like a valid noid token.
pub fn validate_token_format(token: &str) -> Result<()> {
    if !token.starts_with(TOKEN_PREFIX) {
        bail!("token must start with '{TOKEN_PREFIX}'");
    }
    let hex_part = &token[TOKEN_PREFIX.len()..];
    if hex_part.len() != TOKEN_BYTES * 2 {
        bail!(
            "token hex part must be {} characters, got {}",
            TOKEN_BYTES * 2,
            hex_part.len()
        );
    }
    if !hex_part.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("token contains non-hex characters");
    }
    Ok(())
}

/// Extract the prefix of a token for rate-limiting key (first 16 chars after prefix).
pub fn token_rate_key(token: &str) -> String {
    let after_prefix = token.get(TOKEN_PREFIX.len()..).unwrap_or("");
    after_prefix.chars().take(16).collect()
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// --- Rate limiter ---

const MAX_FAILURES: u32 = 10;
const WINDOW_SECS: u64 = 60;

struct RateEntry {
    failures: u32,
    window_start: Instant,
}

pub struct RateLimiter {
    entries: Mutex<HashMap<String, RateEntry>>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Check if a key is rate-limited. Returns Err if blocked.
    pub fn check(&self, key: &str) -> Result<()> {
        let mut map = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = map.get(key) {
            if entry.window_start.elapsed().as_secs() > WINDOW_SECS {
                map.remove(key);
                return Ok(());
            }
            if entry.failures >= MAX_FAILURES {
                bail!("too many authentication failures, try again later");
            }
        }
        Ok(())
    }

    /// Record an authentication failure for a key.
    pub fn record_failure(&self, key: &str) {
        let mut map = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = map.entry(key.to_string()).or_insert(RateEntry {
            failures: 0,
            window_start: Instant::now(),
        });
        if entry.window_start.elapsed().as_secs() > WINDOW_SECS {
            entry.failures = 0;
            entry.window_start = Instant::now();
        }
        entry.failures += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_token_format() {
        let token = generate_token();
        assert!(token.starts_with("noid_tok_"));
        assert_eq!(token.len(), TOKEN_PREFIX.len() + TOKEN_BYTES * 2);
        validate_token_format(&token).unwrap();
    }

    #[test]
    fn generate_token_uniqueness() {
        let t1 = generate_token();
        let t2 = generate_token();
        assert_ne!(t1, t2);
    }

    #[test]
    fn hash_and_verify() {
        let token = generate_token();
        let hash = hash_token(&token);
        assert!(verify_token(&hash, &token));
        assert!(!verify_token(&hash, "noid_tok_0000000000000000000000000000000000000000000000000000000000000000"));
    }

    #[test]
    fn verify_wrong_token() {
        let token = generate_token();
        let hash = hash_token(&token);
        let other = generate_token();
        assert!(!verify_token(&hash, &other));
    }

    #[test]
    fn validate_token_format_ok() {
        let token = generate_token();
        validate_token_format(&token).unwrap();
    }

    #[test]
    fn validate_token_format_bad_prefix() {
        assert!(validate_token_format("bad_prefix_abcd").is_err());
    }

    #[test]
    fn validate_token_format_short() {
        assert!(validate_token_format("noid_tok_abc").is_err());
    }

    #[test]
    fn token_rate_key_extracts_prefix() {
        let key = token_rate_key("noid_tok_abcdef1234567890rest");
        assert_eq!(key, "abcdef1234567890");
    }

    #[test]
    fn rate_limiter_allows_initial() {
        let rl = RateLimiter::new();
        assert!(rl.check("testkey").is_ok());
    }

    #[test]
    fn rate_limiter_blocks_after_max() {
        let rl = RateLimiter::new();
        for _ in 0..MAX_FAILURES {
            rl.record_failure("testkey");
        }
        assert!(rl.check("testkey").is_err());
    }
}
