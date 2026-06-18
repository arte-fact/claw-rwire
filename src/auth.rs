//! Shared-secret gate for the single operator.
//!
//! The token comes from `CLAW_AUTH_TOKEN`; if unset we mint a random one for the
//! run and print it once, so the server is never silently open on the LAN. Login
//! state lives in per-connection `Auth` memory state, which rwire caches by session
//! cookie — so a successful login survives reloads and extra tabs, resetting only
//! on server restart or session-idle eviction.

use std::sync::OnceLock;

use crate::id::random_hex;

static TOKEN: OnceLock<String> = OnceLock::new();

const TOKEN_ENV: &str = "CLAW_AUTH_TOKEN";

pub enum TokenSource {
    Configured,
    Generated(String),
}

/// Resolves the gate token once. Errors only if no token is configured and the
/// system RNG can't be read to mint one.
pub fn init() -> std::io::Result<TokenSource> {
    if let Ok(configured) = std::env::var(TOKEN_ENV) {
        let _ = TOKEN.set(configured);
        return Ok(TokenSource::Configured);
    }
    let minted = random_hex(16)?;
    let _ = TOKEN.set(minted.clone());
    Ok(TokenSource::Generated(minted))
}

pub fn verify(candidate: &str) -> bool {
    let token = TOKEN.get().expect("auth::init must run before verify");
    constant_time_eq(candidate.as_bytes(), token.as_bytes())
}

/// Length-aware constant-time compare: equal-length inputs take the same time
/// regardless of where they differ (the length itself is not a secret here).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_matches_only_identical_inputs() {
        assert!(constant_time_eq(b"hunter2", b"hunter2"));
        assert!(!constant_time_eq(b"hunter2", b"hunter3"));
        assert!(!constant_time_eq(b"short", b"longer"));
        assert!(constant_time_eq(b"", b""));
    }
}
