//! A keyed secret store for provider bearer tokens. The store is the abstraction
//! `PromptService` resolves MCP bearers through; the concrete backend is injected
//! by `crates/app` (same "app owns side effects, pure crate takes a dependency"
//! pattern as config-path resolution).
//!
//! This milestone ships the trait + an in-memory implementation (the only
//! consumer here is tests, and the not-yet-populated app). The OS-keychain-backed
//! implementation lands with the Settings flow that populates it.

use std::collections::HashMap;
use std::sync::Mutex;

/// Errors from a secret-store backend. Never embeds the secret value or key in a
/// way that could leak a token; the message is backend-diagnostic only.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SecretStoreError {
    #[error("secret store backend error: {0}")]
    Backend(String),
}

/// Store, fetch, and delete a bearer token by provider key. Object-safe so
/// `PromptService` can hold `Arc<dyn SecretStore>`.
pub trait SecretStore: Send + Sync {
    /// The bearer for `key`. `Ok(None)` means *no credential stored* (normal — a
    /// provider configured without one); `Err` means the secure store itself
    /// could not be read (e.g. a headless Linux host with no Secret Service).
    /// Callers distinguish the two: the first is "token missing", the second is
    /// "store unavailable".
    fn get(&self, key: &str) -> Result<Option<String>, SecretStoreError>;
    fn set(&self, key: &str, value: &str) -> Result<(), SecretStoreError>;
    fn delete(&self, key: &str) -> Result<(), SecretStoreError>;
}

/// In-memory secret store — process-lifetime only. Used by tests and as the
/// app's placeholder store until the keychain backend is wired up.
#[derive(Debug, Default)]
pub struct InMemorySecretStore {
    secrets: Mutex<HashMap<String, String>>,
}

impl InMemorySecretStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for InMemorySecretStore {
    fn get(&self, key: &str) -> Result<Option<String>, SecretStoreError> {
        Ok(lock(&self.secrets).get(key).cloned())
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SecretStoreError> {
        lock(&self.secrets).insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
        lock(&self.secrets).remove(key);
        Ok(())
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_round_trips() {
        let store = InMemorySecretStore::new();
        assert_eq!(store.get("p").unwrap(), None);
        store.set("p", "tok").unwrap();
        assert_eq!(store.get("p").unwrap().as_deref(), Some("tok"));
        store.delete("p").unwrap();
        assert_eq!(store.get("p").unwrap(), None);
    }
}
