//! The bearer token a client must present.
//!
//! **Why a token on a server only the Mac can reach.** Any web page the user
//! visits can make the browser send requests to `127.0.0.1`. The `Host` and
//! `Origin` checks refuse those; the token is a second layer in case those
//! checks ever have a bug, and it keeps out other accounts on the same Mac,
//! which can reach a loopback port but cannot read this user's secret store
//! or the clients' configuration files. Both MCP Inspector (CVE-2025-49596)
//! and Claude Code's IDE extensions (CVE-2025-52882) trusted localhost and
//! were reachable from web pages.
//!
//! The token is 32 random bytes from the OS generator, base64url without
//! padding, kept in the app's secret store (the Keychain in a release build,
//! the development file store in a debug build) under one key. Regenerating
//! it is how the user cuts every client off: each client holds its own copy
//! in its configuration.

use base64::Engine as _;
use subtle::ConstantTimeEq as _;
use switchboard_prompts::SecretStore;

use crate::error::AppError;

/// The secret store key. Provider names cannot contain `:`, and this does not
/// use the `oauth:` prefix, so it cannot collide with a prompt provider's
/// credential.
const KEY: &str = "agent-access:token";

/// A fresh token.
pub fn generate() -> Result<String, AppError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|e| AppError::Internal(format!("could not generate a token: {e}")))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// The stored token, generating and storing one if there is none yet.
pub fn load_or_create(store: &dyn SecretStore) -> Result<String, AppError> {
    if let Some(token) = store.get(KEY).map_err(|e| store_error(&e))? {
        return Ok(token);
    }
    regenerate(store)
}

/// The stored token, if there is one.
pub fn load(store: &dyn SecretStore) -> Result<Option<String>, AppError> {
    store.get(KEY).map_err(|e| store_error(&e))
}

/// Replace the stored token with a fresh one and return it.
pub fn regenerate(store: &dyn SecretStore) -> Result<String, AppError> {
    let token = generate()?;
    store.set(KEY, &token).map_err(|e| store_error(&e))?;
    Ok(token)
}

/// Whether an `Authorization` header value carries `token`, compared in
/// constant time so the comparison's duration says nothing about how much of
/// a guess was right. The scheme name is case-insensitive, as HTTP defines it.
#[must_use]
pub fn authorizes(header: Option<&[u8]>, token: &str) -> bool {
    const SCHEME: &[u8] = b"bearer ";
    let Some(header) = header else {
        return false;
    };
    if header.len() < SCHEME.len() || !header[..SCHEME.len()].eq_ignore_ascii_case(SCHEME) {
        return false;
    }
    let presented = &header[SCHEME.len()..];
    // `ct_eq` on slices of different lengths is false without comparing, and
    // the length of a fixed-size token is no secret.
    presented.ct_eq(token.as_bytes()).into()
}

fn store_error(error: &switchboard_prompts::SecretStoreError) -> AppError {
    AppError::Internal(format!("the secret store could not be used: {error}"))
}

#[cfg(test)]
mod tests {
    use switchboard_prompts::InMemorySecretStore;

    use super::*;

    #[test]
    fn a_token_is_43_url_safe_characters_and_never_repeats() {
        let a = generate().unwrap();
        let b = generate().unwrap();
        assert_eq!(a.len(), 43, "32 bytes, base64url without padding");
        assert!(
            a.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        );
        assert_ne!(a, b);
    }

    #[test]
    fn the_token_is_created_once_and_kept_until_regenerated() {
        let store = InMemorySecretStore::new();
        assert_eq!(load(&store).unwrap(), None);
        let first = load_or_create(&store).unwrap();
        assert_eq!(load_or_create(&store).unwrap(), first);
        let second = regenerate(&store).unwrap();
        assert_ne!(second, first);
        assert_eq!(load(&store).unwrap(), Some(second));
    }

    #[test]
    fn only_the_exact_bearer_header_authorizes() {
        let token = "abc123";
        assert!(authorizes(Some(b"Bearer abc123"), token));
        assert!(!authorizes(None, token));
        assert!(!authorizes(Some(b"Bearer abc12"), token));
        assert!(!authorizes(Some(b"Bearer abc1234"), token));
        assert!(authorizes(Some(b"bearer abc123"), token));
        assert!(!authorizes(Some(b"abc123"), token));
        assert!(!authorizes(Some(b"Bearer "), token));
    }
}
