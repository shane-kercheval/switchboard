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
    /// Serializes the read-map→mutate→write-map in `set`/`delete`. The whole
    /// file is rewritten per mutation, so two concurrent writers would read the
    /// same snapshot and the last one would silently drop the other's key —
    /// the same failure mode `switchboard_core::io`'s `YAML_EDIT_LOCK`
    /// documents for `config.yaml`. OAuth token refreshes write concurrently
    /// during a provider sync, and a dropped write there loses a *rotated*
    /// refresh token, which is unrecoverable. `get` deliberately takes no lock:
    /// writes land via atomic rename, so a reader sees one whole file or the
    /// other, never a torn one.
    write_lock: std::sync::Mutex<()>,
}

#[cfg(debug_assertions)]
impl FileSecretStore {
    pub fn new(path: std::path::PathBuf) -> Self {
        Self {
            path,
            write_lock: std::sync::Mutex::new(()),
        }
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

    /// Write via same-directory temp file + rename so concurrent readers never
    /// see a truncated file. Same idiom as `switchboard_core::io::write_yaml`,
    /// minus its fsync steps — a deliberate tradeoff: this file is dev-only,
    /// regenerable data, so crash durability isn't worth the cost, while
    /// rename-atomicity is what lets `get` run lock-free. The fixed temp path
    /// is safe because `write_lock` serializes all writers.
    fn write_map(
        &self,
        map: &std::collections::BTreeMap<String, String>,
    ) -> Result<(), SecretStoreError> {
        let bytes =
            serde_json::to_vec_pretty(map).map_err(|e| SecretStoreError::Backend(e.to_string()))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, bytes).map_err(|e| SecretStoreError::Backend(e.to_string()))?;
        // Owner-only perms go on the temp file *before* the rename so the
        // secrets are never world-readable, even briefly.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, &self.path).map_err(|e| SecretStoreError::Backend(e.to_string()))
    }
}

#[cfg(debug_assertions)]
impl SecretStore for FileSecretStore {
    fn get(&self, key: &str) -> Result<Option<String>, SecretStoreError> {
        Ok(self.read_map()?.get(key).cloned())
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SecretStoreError> {
        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut map = self.read_map()?;
        map.insert(key.to_owned(), value.to_owned());
        self.write_map(&map)
    }

    fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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

    #[test]
    fn file_store_concurrent_writes_do_not_lose_keys() {
        // The whole-file read-modify-write must be serialized: without the
        // write lock, writers racing from a barrier read the same snapshot and
        // the last rename drops every other writer's key. Distinct keys mirror
        // the real workload — concurrent OAuth refreshes for different
        // providers, where a dropped write loses a rotated refresh token.
        let dir = tempfile::TempDir::new().unwrap();
        let store = std::sync::Arc::new(FileSecretStore::new(dir.path().join("mcp-secrets.json")));
        let writers = 8;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(writers));
        let handles: Vec<_> = (0..writers)
            .map(|i: usize| {
                let store = store.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store
                        .set(&format!("provider-{i}"), &format!("tok-{i}"))
                        .unwrap();
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        for i in 0..writers {
            assert_eq!(
                store.get(&format!("provider-{i}")).unwrap().as_deref(),
                Some(format!("tok-{i}").as_str()),
                "provider-{i}'s write was lost"
            );
        }
    }
}
