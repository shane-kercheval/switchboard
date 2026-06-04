//! The OS-keychain-backed [`SecretStore`] — the production credential store the
//! app injects into `PromptService`. It lives here, not in `crates/prompts`, so
//! the pure prompts crate stays platform-agnostic; `keyring` is cross-platform
//! (macOS Keychain / Windows Credential Manager / Linux Secret Service), so this
//! single implementation serves every target.

use switchboard_prompts::{SecretStore, SecretStoreError};

/// A plaintext, file-backed secret store — **debug builds only**.
///
/// It exists purely to avoid the macOS Keychain access prompts an *unsigned* dev
/// binary triggers on **every** read (the OS can't recognize a build whose
/// signature changes each compile, so it re-asks for each access). Tokens here
/// live as JSON in the dev config dir — gitignored runtime data on the
/// developer's own machine — so plaintext is an acceptable dev tradeoff. Release
/// builds use [`KeyringSecretStore`] (the real OS keychain); see
/// `build_prompt_service`.
#[cfg(debug_assertions)]
pub struct FileSecretStore {
    path: std::path::PathBuf,
}

#[cfg(debug_assertions)]
impl FileSecretStore {
    pub fn new(path: std::path::PathBuf) -> Self {
        Self { path }
    }

    fn read_map(&self) -> Result<std::collections::BTreeMap<String, String>, SecretStoreError> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| SecretStoreError::Backend(format!("corrupt dev secrets file: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(std::collections::BTreeMap::new())
            }
            Err(e) => Err(SecretStoreError::Backend(e.to_string())),
        }
    }

    fn write_map(
        &self,
        map: &std::collections::BTreeMap<String, String>,
    ) -> Result<(), SecretStoreError> {
        let bytes =
            serde_json::to_vec_pretty(map).map_err(|e| SecretStoreError::Backend(e.to_string()))?;
        std::fs::write(&self.path, bytes).map_err(|e| SecretStoreError::Backend(e.to_string()))?;
        // Best-effort owner-only perms so the dev token isn't world-readable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

#[cfg(debug_assertions)]
impl SecretStore for FileSecretStore {
    fn get(&self, key: &str) -> Result<Option<String>, SecretStoreError> {
        Ok(self.read_map()?.get(key).cloned())
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SecretStoreError> {
        let mut map = self.read_map()?;
        map.insert(key.to_owned(), value.to_owned());
        self.write_map(&map)
    }

    fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
        let mut map = self.read_map()?;
        map.remove(key);
        self.write_map(&map)
    }
}

/// Stores each provider's bearer under `(service, provider-name)` in the OS
/// keychain. `service` namespaces the entries. Used by **release** builds; debug
/// builds use [`FileSecretStore`] (so it's unused by the debug lib, though still
/// exercised by the tests below).
#[cfg_attr(debug_assertions, allow(dead_code))]
pub struct KeyringSecretStore {
    service: String,
}

#[cfg_attr(debug_assertions, allow(dead_code))]
impl KeyringSecretStore {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, key: &str) -> Result<keyring::Entry, SecretStoreError> {
        keyring::Entry::new(&self.service, key).map_err(map_error)
    }
}

impl SecretStore for KeyringSecretStore {
    fn get(&self, key: &str) -> Result<Option<String>, SecretStoreError> {
        match self.entry(key)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            // Absence is the normal "no credential stored" case, not an error.
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_error(e)),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SecretStoreError> {
        self.entry(key)?.set_password(value).map_err(map_error)
    }

    fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
        match self.entry(key)?.delete_credential() {
            // Deleting an absent credential is a successful no-op (idempotent).
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(map_error(e)),
        }
    }
}

/// Map a keyring error to a generic, **secret-free** message. Deliberately does
/// *not* use `keyring::Error`'s `Display` for `BadEncoding`, which embeds the raw
/// stored bytes — credential material must never reach an error string.
///
/// Takes the error by value so it can be used directly as a `map_err` callback.
#[cfg_attr(debug_assertions, allow(dead_code))]
#[allow(clippy::needless_pass_by_value)]
fn map_error(error: keyring::Error) -> SecretStoreError {
    let message = match error {
        keyring::Error::NoEntry => "no credential stored",
        keyring::Error::NoStorageAccess(_) => "secret store is unavailable",
        keyring::Error::PlatformFailure(_) => "secret store platform failure",
        keyring::Error::BadEncoding(_) => "stored credential has invalid encoding",
        keyring::Error::TooLong(_, _) => "credential exceeds the store's size limit",
        keyring::Error::Invalid(_, _) => "invalid credential parameters",
        keyring::Error::Ambiguous(_) => "ambiguous credential (multiple matches)",
        _ => "secret store error",
    };
    SecretStoreError::Backend(message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Once;

    static MOCK: Once = Once::new();

    /// Route keyring through its in-process mock so tests never touch the real OS
    /// keychain. The mock yields a fresh (empty) credential per `Entry`, so it
    /// exercises the absence/`NoEntry` mappings we own — the happy set→get
    /// round-trip is keyring's own contract (it doesn't persist across entries).
    fn use_mock_store() {
        MOCK.call_once(|| {
            keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        });
    }

    #[test]
    fn absent_credential_reads_as_none_and_delete_is_idempotent() {
        use_mock_store();
        let store = KeyringSecretStore::new("switchboard-test");
        assert_eq!(store.get("never-set").unwrap(), None);
        store.delete("never-set").unwrap();
    }

    #[test]
    fn file_store_round_trips() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = FileSecretStore::new(dir.path().join("mcp-secrets.json"));
        assert_eq!(store.get("team").unwrap(), None);
        store.set("team", "tok").unwrap();
        assert_eq!(store.get("team").unwrap().as_deref(), Some("tok"));
        store.delete("team").unwrap();
        assert_eq!(store.get("team").unwrap(), None);
        store.delete("team").unwrap(); // idempotent
    }
}
