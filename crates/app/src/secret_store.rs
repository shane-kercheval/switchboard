//! The OS-keychain-backed [`SecretStore`] — the production credential store the
//! app injects into `PromptService`. It lives here, not in `crates/prompts`, so
//! the pure prompts crate stays platform-agnostic; `keyring` is cross-platform
//! (macOS Keychain / Windows Credential Manager / Linux Secret Service), so this
//! single implementation serves every target.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};
use switchboard_prompts::{SecretStore, SecretStoreError};

// `:` is invalid in provider names, and this does not use the `oauth:` logical
// prefix, so the physical account cannot collide with a logical credential key.
const AGGREGATE_ACCOUNT: &str = "mcp-secrets:aggregate";
const AGGREGATE_FORMAT_VERSION: u32 = 1;

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

/// The credential operations shared by the keyed and aggregate stores.
///
/// This seam stays app-private: logical secret behavior belongs to
/// [`SecretStore`], while this trait exists so aggregate persistence can be
/// tested without touching the process-global keyring mock or a real Keychain.
/// Implementations must return credential-safe errors that never contain the
/// account's stored value.
trait RawCredentialBackend: Send + Sync {
    fn read(&self, account: &str) -> Result<Option<String>, SecretStoreError>;
    fn write(&self, account: &str, value: &str) -> Result<(), SecretStoreError>;
    fn delete(&self, account: &str) -> Result<(), SecretStoreError>;
}

struct KeyringCredentialBackend {
    service: String,
}

impl KeyringCredentialBackend {
    fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, account: &str) -> Result<keyring::Entry, SecretStoreError> {
        keyring::Entry::new(&self.service, account).map_err(map_error)
    }
}

impl RawCredentialBackend for KeyringCredentialBackend {
    fn read(&self, account: &str) -> Result<Option<String>, SecretStoreError> {
        match self.entry(account)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(map_error(error)),
        }
    }

    fn write(&self, account: &str, value: &str) -> Result<(), SecretStoreError> {
        // On macOS this is a fallible create-or-update operation whose locked
        // implementation first looks up the item. It is not a write-only API.
        self.entry(account)?.set_password(value).map_err(map_error)
    }

    fn delete(&self, account: &str) -> Result<(), SecretStoreError> {
        match self.entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(map_error(error)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AggregateRecord {
    format_version: u32,
    secrets: BTreeMap<String, String>,
    legacy_checked: BTreeSet<String>,
    pending_legacy_deletions: BTreeSet<String>,
}

#[derive(Deserialize)]
struct AggregateRecordHeader {
    format_version: u32,
}

impl AggregateRecord {
    fn empty() -> Self {
        Self {
            format_version: AGGREGATE_FORMAT_VERSION,
            secrets: BTreeMap::new(),
            legacy_checked: BTreeSet::new(),
            pending_legacy_deletions: BTreeSet::new(),
        }
    }
}

/// Stores all logical MCP credentials in one cached, versioned Keychain item.
///
/// The aggregate physical item reduces post-rebuild authorization to one item
/// without changing the keyed [`SecretStore`] contract. All clones share an
/// immutable snapshot of the last completed durable record. Cached reads only
/// hold the snapshot's read guard briefly, so they continue seeing the previous
/// durable state while another thread is blocked in a Keychain write.
///
/// The mutation mutex is distinct from the prompt layer's per-provider locks:
/// it serializes full-record read-modify-write operations across providers so
/// concurrent OAuth refreshes cannot overwrite one another. A candidate is
/// persisted before it replaces the snapshot. Publishing first and attempting
/// rollback would be unsafe because a platform write error can be ambiguous.
#[derive(Clone)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct AggregateSecretStore {
    inner: Arc<AggregateSecretStoreInner>,
}

struct AggregateSecretStoreInner {
    raw: Arc<dyn RawCredentialBackend>,
    snapshot: RwLock<Option<Arc<AggregateRecord>>>,
    mutation_lock: Mutex<()>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl AggregateSecretStore {
    // Release selection still intentionally uses the keyed store while this
    // independently tested backend is not yet the configured production path.
    #[cfg_attr(test, allow(dead_code))]
    pub fn new(service: impl Into<String>) -> Self {
        Self::with_backend(Arc::new(KeyringCredentialBackend::new(service)))
    }

    fn with_backend(raw: Arc<dyn RawCredentialBackend>) -> Self {
        Self {
            inner: Arc::new(AggregateSecretStoreInner {
                raw,
                snapshot: RwLock::new(None),
                mutation_lock: Mutex::new(()),
            }),
        }
    }

    fn cached_snapshot(&self) -> Option<Arc<AggregateRecord>> {
        read_lock(&self.inner.snapshot).clone()
    }

    fn initialized_snapshot(&self) -> Result<Arc<AggregateRecord>, SecretStoreError> {
        if let Some(snapshot) = self.cached_snapshot() {
            return Ok(snapshot);
        }

        let _mutation = mutex_lock(&self.inner.mutation_lock);
        self.initialized_snapshot_locked()
    }

    /// Load the aggregate record while the caller holds `mutation_lock`.
    fn initialized_snapshot_locked(&self) -> Result<Arc<AggregateRecord>, SecretStoreError> {
        if let Some(snapshot) = self.cached_snapshot() {
            return Ok(snapshot);
        }

        let record = match self.inner.raw.read(AGGREGATE_ACCOUNT)? {
            Some(encoded) => decode_record(&encoded)?,
            None => AggregateRecord::empty(),
        };
        Ok(self.publish(record))
    }

    /// Persist and publish a candidate while the caller holds `mutation_lock`.
    fn persist_candidate(&self, candidate: AggregateRecord) -> Result<(), SecretStoreError> {
        let encoded = encode_record(&candidate)?;
        self.inner.raw.write(AGGREGATE_ACCOUNT, &encoded)?;
        self.publish(candidate);
        Ok(())
    }

    fn publish(&self, record: AggregateRecord) -> Arc<AggregateRecord> {
        let snapshot = Arc::new(record);
        *write_lock(&self.inner.snapshot) = Some(snapshot.clone());
        snapshot
    }
}

impl SecretStore for AggregateSecretStore {
    fn get(&self, key: &str) -> Result<Option<String>, SecretStoreError> {
        Ok(self.initialized_snapshot()?.secrets.get(key).cloned())
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SecretStoreError> {
        let _mutation = mutex_lock(&self.inner.mutation_lock);
        let current = self.initialized_snapshot_locked()?;
        let mut candidate = current.as_ref().clone();
        candidate.secrets.insert(key.to_owned(), value.to_owned());
        self.persist_candidate(candidate)
    }

    fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
        let _mutation = mutex_lock(&self.inner.mutation_lock);
        let current = self.initialized_snapshot_locked()?;
        let mut candidate = current.as_ref().clone();
        if candidate.secrets.remove(key).is_none() {
            return Ok(());
        }
        self.persist_candidate(candidate)
    }
}

/// Decode only the current aggregate format, never unreadable data as empty.
/// Falling back to an empty record would let the next mutation overwrite every
/// provider credential in the physical item.
fn decode_record(encoded: &str) -> Result<AggregateRecord, SecretStoreError> {
    let header: AggregateRecordHeader = serde_json::from_str(encoded).map_err(|_| {
        SecretStoreError::Backend("aggregate credential record is corrupt".to_owned())
    })?;
    if header.format_version != AGGREGATE_FORMAT_VERSION {
        return Err(SecretStoreError::Backend(format!(
            "unsupported aggregate credential format version {}",
            header.format_version
        )));
    }
    serde_json::from_str(encoded)
        .map_err(|_| SecretStoreError::Backend("aggregate credential record is corrupt".to_owned()))
}

fn encode_record(record: &AggregateRecord) -> Result<String, SecretStoreError> {
    serde_json::to_string(record).map_err(|_| {
        SecretStoreError::Backend("could not encode aggregate credential record".to_owned())
    })
}

fn mutex_lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn read_lock<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write_lock<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Stores each logical provider credential under `(service, key)` in the OS
/// keychain. `service` namespaces the entries. Used by **release** builds; debug
/// builds use [`FileSecretStore`] (so it's unused by the debug lib, though still
/// exercised by the tests below).
#[cfg_attr(debug_assertions, allow(dead_code))]
pub struct KeyringSecretStore {
    backend: KeyringCredentialBackend,
}

#[cfg_attr(debug_assertions, allow(dead_code))]
impl KeyringSecretStore {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            backend: KeyringCredentialBackend::new(service),
        }
    }
}

impl SecretStore for KeyringSecretStore {
    fn get(&self, key: &str) -> Result<Option<String>, SecretStoreError> {
        self.backend.read(key)
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SecretStoreError> {
        self.backend.write(key, value)
    }

    fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
        self.backend.delete(key)
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
    use std::sync::{Barrier, Condvar, Once, mpsc};
    use std::time::Duration;

    static MOCK: Once = Once::new();

    #[derive(Default)]
    struct FakeRawState {
        value: Option<String>,
        reads: usize,
        writes: usize,
        deletes: usize,
        fail_next_read: bool,
        fail_next_write: bool,
    }

    #[derive(Default)]
    struct FakeRawCredentialBackend {
        state: Mutex<FakeRawState>,
        next_write_block: Mutex<Option<Arc<WriteBlock>>>,
    }

    impl FakeRawCredentialBackend {
        fn seed(&self, value: String) {
            mutex_lock(&self.state).value = Some(value);
        }

        fn fail_next_read(&self) {
            mutex_lock(&self.state).fail_next_read = true;
        }

        fn fail_next_write(&self) {
            mutex_lock(&self.state).fail_next_write = true;
        }

        fn block_next_write(&self) -> Arc<WriteBlock> {
            let block = Arc::new(WriteBlock::default());
            *mutex_lock(&self.next_write_block) = Some(block.clone());
            block
        }

        fn counts(&self) -> (usize, usize, usize) {
            let state = mutex_lock(&self.state);
            (state.reads, state.writes, state.deletes)
        }

        fn durable_record(&self) -> AggregateRecord {
            let encoded = mutex_lock(&self.state)
                .value
                .clone()
                .expect("aggregate record should be persisted");
            decode_record(&encoded).unwrap()
        }
    }

    impl RawCredentialBackend for FakeRawCredentialBackend {
        fn read(&self, account: &str) -> Result<Option<String>, SecretStoreError> {
            assert_eq!(account, AGGREGATE_ACCOUNT);
            let mut state = mutex_lock(&self.state);
            state.reads += 1;
            if std::mem::take(&mut state.fail_next_read) {
                return Err(SecretStoreError::Backend(
                    "fake credential store is unavailable".to_owned(),
                ));
            }
            Ok(state.value.clone())
        }

        fn write(&self, account: &str, value: &str) -> Result<(), SecretStoreError> {
            assert_eq!(account, AGGREGATE_ACCOUNT);
            {
                let mut state = mutex_lock(&self.state);
                state.writes += 1;
                if std::mem::take(&mut state.fail_next_write) {
                    return Err(SecretStoreError::Backend(
                        "fake credential write failed".to_owned(),
                    ));
                }
            }

            if let Some(block) = mutex_lock(&self.next_write_block).take() {
                block.wait_for_release();
            }

            mutex_lock(&self.state).value = Some(value.to_owned());
            Ok(())
        }

        fn delete(&self, account: &str) -> Result<(), SecretStoreError> {
            assert_eq!(account, AGGREGATE_ACCOUNT);
            let mut state = mutex_lock(&self.state);
            state.deletes += 1;
            state.value = None;
            Ok(())
        }
    }

    #[derive(Default)]
    struct WriteBlock {
        entered: (Mutex<bool>, Condvar),
        released: (Mutex<bool>, Condvar),
    }

    impl WriteBlock {
        fn wait_for_release(&self) {
            let (entered_lock, entered_changed) = &self.entered;
            *mutex_lock(entered_lock) = true;
            entered_changed.notify_all();

            let (released_lock, released_changed) = &self.released;
            let mut released = mutex_lock(released_lock);
            while !*released {
                released = released_changed
                    .wait(released)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }

        fn wait_until_entered(&self) {
            let (entered_lock, entered_changed) = &self.entered;
            let mut entered = mutex_lock(entered_lock);
            while !*entered {
                entered = entered_changed
                    .wait(entered)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }

        fn release(&self) {
            let (released_lock, released_changed) = &self.released;
            *mutex_lock(released_lock) = true;
            released_changed.notify_all();
        }
    }

    fn aggregate_store() -> (AggregateSecretStore, Arc<FakeRawCredentialBackend>) {
        let raw = Arc::new(FakeRawCredentialBackend::default());
        let store = AggregateSecretStore::with_backend(raw.clone());
        (store, raw)
    }

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
    fn keyring_errors_distinguish_absence_from_unavailability_without_leaking_details() {
        use_mock_store();
        let backend = KeyringCredentialBackend::new("switchboard-error-test");
        assert_eq!(backend.read("missing").unwrap(), None);

        let unavailable = map_error(keyring::Error::NoStorageAccess(Box::new(
            std::io::Error::other("private platform detail"),
        )));
        let bad_encoding = map_error(keyring::Error::BadEncoding(
            b"credential-bytes-must-not-leak".to_vec(),
        ));
        let rendered = format!("{unavailable:?} {unavailable} {bad_encoding:?} {bad_encoding}");
        assert!(rendered.contains("secret store is unavailable"));
        assert!(rendered.contains("stored credential has invalid encoding"));
        assert!(!rendered.contains("private platform detail"));
        assert!(!rendered.contains("credential-bytes-must-not-leak"));
    }

    #[test]
    fn aggregate_store_caches_a_confirmed_absent_record() {
        let (store, raw) = aggregate_store();

        assert_eq!(store.get("bearer").unwrap(), None);
        assert_eq!(store.get("oauth:provider").unwrap(), None);
        assert_eq!(raw.counts(), (1, 0, 0));
    }

    #[test]
    fn aggregate_store_round_trips_independent_logical_keys() {
        let (store, raw) = aggregate_store();

        store.set("bearer", "bearer-token").unwrap();
        store.set("oauth:provider", "oauth-envelope-json").unwrap();
        store.set("bearer", "rotated-bearer-token").unwrap();

        assert_eq!(
            store.get("bearer").unwrap().as_deref(),
            Some("rotated-bearer-token")
        );
        assert_eq!(
            store.get("oauth:provider").unwrap().as_deref(),
            Some("oauth-envelope-json")
        );
        let durable = raw.durable_record();
        assert_eq!(durable.secrets.len(), 2);
        assert_eq!(
            durable.secrets.get("oauth:provider").map(String::as_str),
            Some("oauth-envelope-json")
        );
        assert_eq!(raw.counts(), (1, 3, 0));
    }

    #[test]
    fn persisted_aggregate_record_loads_in_a_new_store_instance() {
        let (store, raw) = aggregate_store();
        store.set("bearer", "bearer-token").unwrap();
        store.set("oauth:provider", "oauth-envelope-json").unwrap();

        let restarted_store = AggregateSecretStore::with_backend(raw.clone());

        assert_eq!(
            restarted_store.get("bearer").unwrap().as_deref(),
            Some("bearer-token")
        );
        assert_eq!(
            restarted_store.get("oauth:provider").unwrap().as_deref(),
            Some("oauth-envelope-json")
        );
        assert_eq!(raw.counts(), (2, 2, 0));
    }

    #[test]
    fn aggregate_store_deletes_one_key_without_disturbing_its_sibling() {
        let (store, raw) = aggregate_store();
        store.set("provider-a", "secret-a").unwrap();
        store.set("provider-b", "secret-b").unwrap();

        store.delete("provider-a").unwrap();

        assert_eq!(store.get("provider-a").unwrap(), None);
        assert_eq!(
            store.get("provider-b").unwrap().as_deref(),
            Some("secret-b")
        );
        let durable = raw.durable_record();
        assert_eq!(durable.secrets.len(), 1);
        assert_eq!(
            durable.secrets.get("provider-b").map(String::as_str),
            Some("secret-b")
        );
    }

    #[test]
    fn concurrent_initialization_is_shared_across_clones() {
        let (store, raw) = aggregate_store();
        let workers = 8;
        let barrier = Arc::new(Barrier::new(workers));
        let handles: Vec<_> = (0..workers)
            .map(|index: usize| {
                let store = store.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    assert_eq!(store.get(&format!("missing-{index}")).unwrap(), None);
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(raw.counts(), (1, 0, 0));
    }

    #[test]
    fn failed_initialization_is_retried() {
        let (store, raw) = aggregate_store();
        raw.fail_next_read();

        assert!(store.get("provider").is_err());
        assert_eq!(store.get("provider").unwrap(), None);
        assert_eq!(raw.counts(), (2, 0, 0));
    }

    #[test]
    fn concurrent_aggregate_writes_preserve_every_key() {
        let (store, raw) = aggregate_store();
        let workers = 8;
        let barrier = Arc::new(Barrier::new(workers));
        let handles: Vec<_> = (0..workers)
            .map(|index: usize| {
                let store = store.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store
                        .set(&format!("provider-{index}"), &format!("secret-{index}"))
                        .unwrap();
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }
        let durable = raw.durable_record();
        assert_eq!(durable.secrets.len(), workers);
        for index in 0..workers {
            assert_eq!(
                durable
                    .secrets
                    .get(&format!("provider-{index}"))
                    .map(String::as_str),
                Some(format!("secret-{index}").as_str())
            );
        }
        assert_eq!(raw.counts(), (1, workers, 0));
    }

    #[test]
    fn deleting_an_absent_logical_key_does_not_rewrite_the_record() {
        let (store, raw) = aggregate_store();

        store.delete("missing").unwrap();
        store.delete("missing").unwrap();

        assert_eq!(raw.counts(), (1, 0, 0));
    }

    #[test]
    fn failed_write_keeps_the_previous_cache_and_durable_record() {
        let (store, raw) = aggregate_store();
        store.set("provider", "credential-before-write").unwrap();
        raw.fail_next_write();

        let error = store
            .set("provider", "credential-from-failed-write")
            .unwrap_err();

        assert_eq!(
            store.get("provider").unwrap().as_deref(),
            Some("credential-before-write")
        );
        assert_eq!(
            raw.durable_record()
                .secrets
                .get("provider")
                .map(String::as_str),
            Some("credential-before-write")
        );
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains("credential-before-write"));
        assert!(!rendered.contains("credential-from-failed-write"));
    }

    #[test]
    fn failed_delete_keeps_the_previous_cache_and_durable_record() {
        let (store, raw) = aggregate_store();
        store.set("provider", "credential-before-delete").unwrap();
        raw.fail_next_write();

        let error = store.delete("provider").unwrap_err();

        assert_eq!(
            store.get("provider").unwrap().as_deref(),
            Some("credential-before-delete")
        );
        assert_eq!(
            raw.durable_record()
                .secrets
                .get("provider")
                .map(String::as_str),
            Some("credential-before-delete")
        );
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains("credential-before-delete"));
    }

    #[test]
    fn cached_reads_complete_while_an_aggregate_write_is_blocked() {
        let (store, raw) = aggregate_store();
        store.set("provider-a", "old-a").unwrap();
        store.set("provider-b", "stable-b").unwrap();
        let write_block = raw.block_next_write();

        let writer_store = store.clone();
        let writer = std::thread::spawn(move || {
            writer_store.set("provider-a", "new-a").unwrap();
        });
        write_block.wait_until_entered();

        let reader_store = store.clone();
        let (result_tx, result_rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            result_tx
                .send((
                    reader_store.get("provider-a").unwrap(),
                    reader_store.get("provider-b").unwrap(),
                ))
                .unwrap();
        });
        let result = result_rx.recv_timeout(Duration::from_secs(5));

        write_block.release();
        writer.join().unwrap();
        reader.join().unwrap();

        assert_eq!(
            result.expect("cached reads should not wait for the mutation lock"),
            (Some("old-a".to_owned()), Some("stable-b".to_owned()))
        );
        assert_eq!(store.get("provider-a").unwrap().as_deref(), Some("new-a"));
    }

    #[test]
    fn corrupt_and_future_records_are_not_overwritten_or_leaked() {
        let corrupt_secret = "credential-inside-corrupt-record";
        let (corrupt_store, corrupt_raw) = aggregate_store();
        corrupt_raw.seed(format!(
            r#"{{"format_version":1,"secrets":{{"provider":"{corrupt_secret}"}} BROKEN}}"#
        ));

        let corrupt_error = corrupt_store.get("provider").unwrap_err();
        let corrupt_rendered = format!("{corrupt_error:?} {corrupt_error}");
        assert!(corrupt_rendered.contains("aggregate credential record is corrupt"));
        assert!(!corrupt_rendered.contains(corrupt_secret));
        assert_eq!(corrupt_raw.counts(), (1, 0, 0));

        let future_secret = "credential-inside-future-record";
        let (future_store, future_raw) = aggregate_store();
        future_raw.seed(format!(
            r#"{{"format_version":2,"secrets":{{"provider":"{future_secret}"}},"legacy_checked":[],"pending_legacy_deletions":[],"future_metadata":{{"new":true}}}}"#
        ));

        let future_error = future_store.get("provider").unwrap_err();
        let future_rendered = format!("{future_error:?} {future_error}");
        assert!(future_rendered.contains("unsupported aggregate credential format version 2"));
        assert!(!future_rendered.contains(future_secret));
        assert_eq!(future_raw.counts(), (1, 0, 0));
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
