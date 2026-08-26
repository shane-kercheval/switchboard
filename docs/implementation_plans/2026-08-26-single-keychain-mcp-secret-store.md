# One-prompt Keychain storage for MCP credentials

**Status:** in progress (Milestone 2 implemented; review pending) · **Created:** 2026-08-26

Switchboard currently stores every MCP prompt-provider credential in a separate
macOS Keychain item. Bearer credentials use the provider name as the item
account; OAuth envelopes use `oauth:<provider-name>`. Startup synchronizes all
configured providers concurrently, and OAuth deliberately reloads credentials
several times while it checks local state, initializes its client, probes the
token, and authenticates MCP requests. After an ad-hoc-signed build changes,
macOS no longer recognizes the rebuilt executable as the application approved
on those per-provider items. A user with two providers can therefore approve
multiple items after one rebuild even when they always choose **Always Allow**.

This plan changes the release credential backend to keep the existing logical
key/value `SecretStore` contract while persisting the whole map in one versioned
Keychain item and caching that map for the process lifetime. Once legacy entries
have migrated, one Keychain authorization covers every provider and repeated
logical OAuth loads are served from the cache. Credential mutations may still
cause the locked `keyring` backend to reread the physical item internally, but
after **Always Allow** those reads must not present another dialog. With the
project's current ad-hoc signing, the expected behavior is **one authorization
after a rebuild and none after an ordinary restart**. Stable Developer ID signing
is separately owned release work; it is not part of this plan.

The first post-upgrade run is an explicit exception: it may request access to
each old per-provider item while migrating it. Migration must finish without
losing a bearer, OAuth registration, access token, or rotating refresh token,
and must track any legacy item it could not yet delete so the duplicate is not
silently forgotten.

## Scope

In scope:

- The release `SecretStore` implementation in `crates/app`.
- One aggregate macOS Keychain item for all Switchboard-owned MCP prompt-provider
  secrets.
- A process-lifetime cache that explicitly loads the aggregate item once and
  prevents repeated logical reads from producing additional authorization
  dialogs.
- Lazy, crash-safe migration of existing per-provider Keychain items.
- Serialization of aggregate writes across concurrent provider refreshes.
- Secret-safe errors, tests, documentation, and manual macOS verification.

Out of scope:

- Developer ID enrollment, certificates, notarization, CI release signing, or
  changes to `signingIdentity`. Those remain in
  `2026-05-30-macos-release-distribution.md` and are owned separately.
- Harness-owned MCP credentials. Claude Code, Codex, Gemini, and Antigravity own
  their own MCP registries and authentication; this plan only covers the MCP
  prompt providers managed in Switchboard Settings.
- A new user preference, credential-management UI, encryption scheme, encrypted
  sidecar file, or manual migration command.
- Changes to bearer/OAuth wire behavior, provider configuration, prompt cache
  timing, or the `SecretStore` trait used by `crates/prompts`.
- Aggregating the debug `FileSecretStore`. Debug builds already avoid Keychain
  prompts and its current map file is adequate.

## Required reading before implementing

The implementing agent must read these sources before changing code. The Apple
documents are required because the user-visible behavior is an OS ACL decision,
not merely a count of Rust method calls.

- `AGENTS.md`.
- `docs/system-design.md` §6, especially the ownership boundary between
  non-secret provider config and app-owned secrets.
- `docs/implementation_plans/2026-06-01-prompt-mcp-support.md`, especially the
  `SecretStore` seam and the never-log-credentials invariant.
- `docs/implementation_plans/2026-08-08-mcp-oauth.md`, especially the credential
  envelope, blocking-pool bridge, per-provider transaction lock, rotating refresh
  token, removal, and sign-out decisions. This plan changes the physical backend,
  not those logical contracts.
- `crates/app/src/secret_store.rs` and the release wiring in
  `crates/app/src/lib.rs`.
- `crates/prompts/src/{secret,oauth,service}.rs`, including every `SecretStore`
  call and the existing provider credential lifecycle locks.
- The pinned `keyring` macOS backend and its `security-framework` implementation
  in the local Cargo registry. Confirm that the locked versions still use the
  file-based Keychain generic-password APIs before relying on the ACL behavior
  below.
- Apple, **Access Control Lists**:
  https://developer.apple.com/documentation/security/access-control-lists
- Apple, **SecAccessCreate**, especially the default safe/restricted ACL entries
  and their operation-specific authorization sets:
  https://developer.apple.com/documentation/security/secaccesscreate%28_%3A_%3A_%3A_%29
- Apple, **Allow apps to access your keychain**:
  https://support.apple.com/guide/mac-help/allow-apps-to-access-your-keychain-kychn002/mac
- Apple Technical Note TN3137, **On Mac keychain APIs and implementations**:
  https://developer.apple.com/documentation/technotes/tn3137-on-mac-keychains
- Apple, **Understanding the Code Signature**, particularly designated
  requirements and Keychain access-control tracking:
  https://developer.apple.com/library/archive/documentation/Security/Conceptual/CodeSigningGuide/AboutCS/AboutCS.html

## Decisions and conventions established here

Later milestones must reuse these decisions rather than introducing parallel
storage or migration paths.

### Keep the logical keyed store; aggregate only the physical backend

`crates/prompts` continues to call `SecretStore::{get,set,delete}` with the same
keys and semantics. Bearer remains `<provider-name>` and OAuth remains
`oauth:<provider-name>`. The prompt crate must not learn that release builds put
those logical entries inside one physical record. This preserves the existing
Tauri-free platform seam and keeps OAuth lifecycle logic unchanged.

The aggregate Keychain account must be an internal constant containing `:`, so
it cannot collide with a valid provider name (provider names reject `:`), and it
must not use the `oauth:` prefix. Continue using the existing `switchboard`
service namespace. Do not rename the service in this work: a service rename
would turn migration into a cross-namespace search problem without helping the
one-prompt goal.

### Use one versioned JSON record, not an encrypted sidecar

The aggregate item stores a versioned JSON document containing:

- the logical secret map;
- the logical keys whose legacy per-provider item has been checked; and
- legacy keys whose aggregate copy is authoritative but whose old item still
  needs deletion.

The exact Rust type and field names follow local conventions, but these three
pieces of state are contractual. The migration metadata is not optional: without
it, an absent credential would re-probe its old Keychain item forever, and a
failed cleanup could leave a secret duplicate with no durable reminder.

Use an explicit integer format version starting at `1`. A malformed document or
unknown version is a store-unavailable error. Never reinterpret it as an empty
map and never overwrite it during recovery; doing so would silently destroy
every provider credential at once. Error and tracing text may identify the
operation and format problem but must never include serialized record bytes,
secret values, authorization codes, access tokens, or refresh tokens.

An encrypted file protected by one Keychain-held master key was considered and
rejected for this scope. It would also achieve one explicit Keychain load, but adds a
cryptographic format, nonce/key lifecycle, another atomic durable artifact, and
two-file recovery states. There is no observed requirement that the simpler
single Keychain record cannot satisfy. Do not introduce that design unless a
real Keychain item-size failure is demonstrated and the scope is revisited.

Do not add a speculative provider-count or serialized-size policy. If the
backend reports its existing `TooLong`/size-limit failure, preserve the previous
durable record and return the existing secret-store error. This is an honest,
recoverable failure; inventing an unverified limit would not improve it.

### One cached snapshot, one mutation lock, and per-key migration gates are load-bearing

All clones of the production store share one process-lifetime immutable snapshot
of the last completed durable aggregate record. A cached `get` clones that
snapshot under a brief read guard and does not acquire the mutation or migration
locks, so one provider waiting on a Keychain dialog cannot stall a sibling whose
credential is already cached. Readers may see the immediately preceding durable
snapshot while a mutation is in flight, matching the debug file store's existing
old-or-new read contract; they must never see a partially mutated candidate.

The first operation with no snapshot takes the global mutation lock, rechecks,
then loads and parses the aggregate item exactly once before publishing it. Every
other initializer waits for that single flight. This initial wait across
providers is inherent: no provider can use the shared record before the one
aggregate authorization settles. A missing aggregate item initializes an empty
migration-aware record, not a permanently negative cache that prevents legacy
lookup. Cache only a successful load (including a confirmed missing item); a
denied, unavailable, corrupt, or unknown-version read leaves initialization
retryable so one transient Keychain failure does not poison the store until the
app quits.

A global synchronous mutation mutex serializes aggregate candidate construction
and persistence across all logical keys. This serialization is deliberate:
after aggregation, two OAuth providers refreshing concurrently would otherwise
both derive from the same map and the last writer would erase the other
provider's rotated refresh token. Hold the mutation lock across the aggregate
Keychain write, but do not hold the cached snapshot's write guard during that
blocking call; publish the new immutable snapshot only after persistence
succeeds. Cached readers therefore continue seeing the preceding durable state
while a write is blocked.

Legacy reads and deletes use a separate synchronous gate per logical key. The
gate prevents two callers for the same key—including the synchronous Settings
display path—from presenting duplicate migration dialogs, while different
providers remain isolated. Never hold the global mutation lock across a legacy
Keychain read or delete. After legacy I/O, acquire the mutation lock, recheck the
latest snapshot, and merge only if another caller has not already completed or
superseded that key's migration.

The existing per-provider transaction locks in `crates/prompts` remain necessary
for provider lifecycle races; the new locks protect different physical-store
boundaries. The store never acquires a prompt-layer lock. Within the store, the
order is per-key gate before mutation lock, and no path acquires a per-key gate
while holding the mutation lock. This prevents a cycle while preserving sibling
isolation. Existing async provider paths already move `SecretStore` calls to the
blocking pool; the synchronous Settings display read remains the known exception.
These rationales must survive in the production store's module or type docs.

Every mutation follows **persist candidate, then publish snapshot**. If
serialization or Keychain persistence fails, readers continue seeing the last
known durable snapshot. Do not publish first and attempt rollback: a rollback
cannot prove what the Keychain accepted after an ambiguous platform error.

### Migration is lazy, durable, and aggregate-first

Do not parse provider configuration a second time in `crates/app`, enumerate the
whole Keychain, or widen `SecretStore` with a migration-only method. The keyed
interface already supplies the exact legacy key at the point it is needed.
Migration is therefore lazy per logical key and runs through that key's migration
gate. Startup sync naturally touches every configured bearer or OAuth credential,
so configured providers migrate on the first post-upgrade launch without a new
orchestrator.

For a first `get(key)` whose aggregate record has neither a secret nor a
"legacy checked" marker:

1. Acquire the key's migration gate and recheck the cached snapshot; a caller
   that raced ahead may already have settled it.
2. Without holding the global mutation lock, read the old
   `(service = switchboard, account = key)` item.
3. Acquire the mutation lock and recheck the latest snapshot before deriving a
   candidate. If another mutation made an aggregate value authoritative while
   the legacy read was blocked, do not overwrite it with the stale legacy value.
4. If the legacy item was absent, persist a candidate marking the key checked,
   publish that snapshot, and return `None`.
5. If it existed, persist a candidate containing the secret, the checked marker,
   and a pending-delete marker. Only after that persistence succeeds may the
   snapshot expose the aggregate value.
6. Release the mutation lock, then delete the legacy item while retaining only
   the key's migration gate. `NoEntry` is success. On success, reacquire the
   mutation lock, recheck, and persist a candidate without the pending marker.
   If cleanup or the marker-clearing write fails, retain the durable pending
   marker, return the aggregate credential, and emit a secret-free warning; a
   later operation or launch retries cleanup.

Aggregate-first ordering is load-bearing: deleting first can lose the only copy
on a crash or failed aggregate write. Returning the secured aggregate value when
legacy cleanup fails keeps providers usable while the durable marker prevents
the duplicate from being forgotten.

`set(key, value)` takes the key's migration gate, then makes the aggregate value
authoritative under the mutation lock with the same checked and pending-delete
bookkeeping. It releases the mutation lock before attempting legacy cleanup. A
cleanup failure does not turn a successfully persisted new credential into a
failed OAuth token save; it leaves the pending marker for retry and logs safely.
This matters for a rotating refresh token: reporting a failed save can provoke a
retry using a token the authorization server has already invalidated.

`delete(key)` takes the key's migration gate, removes the aggregate value under
the mutation lock, and durably records that the legacy item must also be absent.
It releases the mutation lock before deleting the old item. Unlike migration
cleanup after `get`/`set`, failure to delete the legacy item must surface as
`SecretStoreError`: provider removal's existing contract is that it must not
claim success while credential material remains. Retain enough durable pending
state that retrying the same deletion is idempotent and can finish cleanup after
a restart.

Do not drain pending deletions while holding the initialization or mutation lock.
Retry a pending deletion when an operation touches its affected key, under that
key's migration gate and outside the mutation lock; reacquire the mutation lock
only to persist marker removal. Cleanup of one key must not delay another
provider's cached read or legacy migration. Warn and retain the marker on failure.
A direct `delete` of the affected key still surfaces its cleanup failure as
described above.

The first migration launch can prompt once per legacy item and may prompt for a
cleanup operation. This one-time cost is unavoidable without weakening the old
items' ACLs. Record it in user-facing documentation and manual verification; do
not misrepresent the migration launch as already satisfying the one-prompt
steady-state guarantee.

---

## Milestone 1 — Prove the one-item macOS ACL behavior

This is intentionally a small, disposable proof because it validates an OS
behavior on which the chosen storage design depends; do not manufacture product
architecture in this milestone.

### Goal & Outcome

Verify on macOS that one **Always Allow** approval for a rebuilt ad-hoc app's
single generic-password item covers the exact read and update operations the
aggregate store will perform.

- A rebuilt ad-hoc executable reads one existing disposable Keychain item,
  receives one authorization dialog, and is approved with **Always Allow**.
- Repeated reads and a password-data update by that same executable complete
  without another authorization dialog.
- The proof touches no real Switchboard credential and leaves no disposable
  Keychain item behind.

### Implementation Outline

Use the locked `keyring`/`security-framework` path, not the `security` CLI, with
an isolated test service/account that cannot collide with production. The proof
must exercise the same `get_password` followed by `set_password` behavior as the
planned raw backend. Build/run once to create the disposable item, change and
rebuild the ad-hoc executable so its designated requirement changes, then run
the read/read/update/read sequence and count the OS authorization dialogs.

Apple documents Keychain ACLs as operation-specific. The aggregate design
assumes the approval needed to decrypt/read the record does not lead to a second
password request when OAuth later updates the item data. Do not infer that from
API success or from an unlocked Keychain; observe the actual dialog behavior.

If macOS asks a second time for the update operation, stop after recording the
result. Do not proceed with Milestones 2–3 and do not silently introduce an
encrypted sidecar. That result would invalidate the promised one-prompt outcome
and requires a scope decision between the previously considered encrypted-file
design and waiting for stable signing.

### Definition of Done

- The read/read/update/read sequence is recorded in the implementation notes or
  commit/PR description, including macOS version, architecture, and confirmation
  that exactly one dialog appeared after the rebuild.
- The disposable item is deleted after the observation.
- No production Switchboard service/account or real credential is read,
  modified, printed, or deleted.
- If the result is not exactly one prompt, implementation stops with the plan
  explicitly reported as blocked by the failed premise.

### Verification record — 2026-08-26

- Verified on macOS 26.6.2 (build 25G83), Apple silicon (`arm64`), using the
  locked `keyring` 3.6.3 and `security-framework` 3.7.0 dependency path.
- The seed probe was linker-signed ad hoc with designated requirement CDHash
  `e922bd0f6d0155a6b38610dd88ab3819b2f62ff8`. After a source change and
  rebuild, the probe was linker-signed ad hoc with designated requirement
  CDHash `39bc49b2693429531d5bcddfbb7e2e78efceaf7d`.
- The rebuilt probe's first `get_password` presented one authorization dialog.
  After entering the Keychain password and choosing **Always Allow**, its second
  `get_password`, `set_password`, and final `get_password` completed without
  another dialog. This validates the one-item ACL premise, including the locked
  macOS backend's internal lookup during `set_password`.
- The probe used a dedicated non-production service/account, accessed no real
  Switchboard credential, printed no secret, and successfully deleted the
  disposable item after the observation.

---

## Milestone 2 — Aggregate record and process cache

### Goal & Outcome

Build and prove the new production storage primitive without changing the
release wiring yet.

- Multiple logical bearer and OAuth keys round-trip through one physical secure
  record while callers continue using the existing `SecretStore` interface.
- Successful cache initialization performs one explicit aggregate-record load;
  repeated OAuth-style reads and reads of sibling providers use the cached
  snapshot. Credential writes may cause the locked macOS backend to reread the
  item internally, but must not cause another authorization dialog once the item
  has been approved for that build.
- Concurrent updates to different providers cannot lose either value.
- A failed write leaves both the durable record and the visible cache at the
  previous successful state.
- Corrupt or future-version data is reported without being overwritten and no
  secret reaches an error or log message.

### Implementation Outline

Introduce the aggregate-record implementation in `crates/app/src/secret_store.rs`
behind an app-private raw credential backend. Production raw operations use the
existing `keyring` crate; tests use an in-memory/failure-injectable backend. This
small seam is justified by behavior the global keyring mock cannot prove: the
current mock creates a fresh empty credential per `Entry`, while this work must
assert explicit raw-operation counts, persistence failures, blocked-operation
isolation, and concurrent read-modify-write behavior. Keep the seam private to
the app secret-store module; it is not a new workspace abstraction or
prompt-crate API.

Implement the versioned aggregate document, cached snapshot, and mutation lock
according to the conventions above. The raw backend's explicit aggregate read
must distinguish `NoEntry` from every other failure and map errors through the
existing credential-safe taxonomy. Do not claim that
`keyring::Entry::set_password` is a write-only operation: on the locked macOS
backend it first looks up the item, and a failed lookup can fall through to an
add attempt. Treat that API as an opaque create-or-update operation; any returned
failure leaves the published snapshot unchanged. Keep the current safeguard
that avoids formatting `keyring::Error::BadEncoding`, because it can contain
stored bytes.

At this milestone the legacy methods may exist in the raw backend for the next
milestone, but the production `build_secret_store` wiring must remain on the old
implementation. This makes the component independently testable before any real
credential reads change.

The production type/module docs must retain the non-obvious rationale for:

- one physical item versus logical keyed entries;
- cached immutable snapshots and the global mutation lock despite existing
  prompt-layer provider locks;
- persist-before-cache ordering; and
- treating corrupt/unknown aggregate formats as non-destructive errors.

### Definition of Done

- Unit tests with the fake raw backend prove:
  - absent aggregate item initializes an empty logical store;
  - bearer and OAuth-shaped logical keys round-trip independently;
  - multiple `get` calls across multiple keys cause exactly one explicit
    aggregate-load backend call in one process;
  - concurrent first operations share one successful initialization, while a
    failed initialization is retried rather than cached;
  - clones share the same cached snapshot and mutation lock;
  - a blocked aggregate write does not block a cached sibling read, which sees
    the preceding durable snapshot until the write succeeds;
  - concurrent writes to distinct keys preserve every value;
  - updating one key preserves all siblings;
  - deleting an absent logical key is idempotent;
  - raw write failure leaves the previous cached and durable record visible;
  - the concrete record round-trips through JSON encoding; because its strings,
    maps, and sets are infallibly serializable by `serde_json`, do not add a codec
    abstraction solely to inject an unreachable serialization failure. Retain a
    secret-safe encode-error mapping for API correctness;
  - a corrupt JSON record and an unknown format version return errors and cause
    no write;
  - explicit raw reads map `NoEntry` to absence and keep denial/unavailability
    distinct, while a failed raw create-or-update never publishes its candidate;
  - raw backend error text and serialized error output contain none of the test
    secrets.
- Existing `FileSecretStore` tests remain unchanged and green; debug behavior is
  not coupled to the new aggregate format.
- Existing keyring absence/error-mapping coverage remains, adapted only as needed
  to test the raw production backend rather than aggregate semantics through a
  non-persistent global mock.
- Run the repository's normal test and lint targets appropriate to the touched
  Rust code, using the exact Makefile flags per `AGENTS.md`.

---

## Milestone 3 — Legacy migration and release cutover

### Goal & Outcome

Migrate existing installations safely and make the aggregate store the release
backend.

- A release build with no prior credentials reads and writes only the aggregate
  Keychain item.
- Existing bearer and OAuth credentials migrate automatically when their
  configured providers are first synchronized or used.
- A crash or failure at every migration boundary leaves at least one readable
  durable copy; it never silently replaces all credentials with an empty map.
- Legacy duplicates are deleted or durably tracked for retry.
- Provider removal still surfaces a failure when any copy of that provider's
  credential cannot be removed.
- A provider blocked on legacy-item authorization cannot block cached reads or
  legacy migration for a different provider.
- After migration settles, any number of configured MCP prompt providers cause
  one explicit aggregate load per app process and, after **Always Allow**, at
  most one authorization dialog after an ad-hoc rebuild. Internal reads made by
  credential mutations are not part of a false single-data-read guarantee.

### Implementation Outline

Add the lazy migration state machine described in the shared conventions to the
aggregate store. The raw backend addresses two kinds of item in the same existing
service namespace: the single internal aggregate account and a legacy logical-key
account. Keep those operations explicit so no aggregate operation can
accidentally target a provider item or vice versa.

Migration and ordinary writes reuse the same mutation lock and
candidate-persistence path from Milestone 2. Add the shared per-logical-key gates
defined in the conventions above; do not add a config-driven bulk migration path
or hold the global mutation lock across legacy reads or deletes. Double-check
the latest snapshot after acquiring each gate or lock so work that blocked on
Keychain I/O cannot overwrite a newer aggregate value. Explicit legacy reads
must distinguish `NoEntry` from denial or store unavailability. Retry pending
cleanup without withholding a valid aggregate secret. Preserve the stricter
error behavior for an explicit logical delete.

Once migration behavior is proven, switch only the non-debug
`build_secret_store` branch to the aggregate implementation. Keep the debug file
branch exactly as it is. No frontend or Tauri command changes should be needed:
the existing `PromptService` and MCP settings flows continue through
`SecretStore`.

Remove the old per-item production `SecretStore` implementation after its raw
legacy read/delete operations have been absorbed into the new backend. Do not
keep two selectable release stores. The old on-disk Keychain items remain the
migration input, not a supported alternative architecture.

Update documentation in the same milestone:

- `docs/system-design.md` §6: logical secrets are still keyed by provider, but
  release persistence is one cached, versioned Keychain record.
- `README.md` near MCP authentication: the first launch after this upgrade may
  ask once per existing provider while migrating; after migration, an
  ad-hoc-built app asks at most once after a rebuild and not after a restart.
  Do not promise approvals surviving rebuilds until stable signing lands.
- `docs/implementation_plans/2026-05-30-macos-release-distribution.md`: add the
  Keychain consequence to the signing rationale—stable signing is what lets an
  **Always Allow** ACL continue recognizing updated builds. Do not implement the
  signing milestones here.

The code must retain comments/docstrings for the per-key-gate/global-mutation-lock
boundary, aggregate-first migration, pending-delete durability, the different
error semantics of migration cleanup versus explicit delete, and the
rotating-refresh-token reason a successful aggregate write is not reported as
failed merely because legacy cleanup is pending. In particular, record why the
global mutation lock is never held across legacy Keychain I/O: a provider
awaiting an authorization dialog must not stall its siblings.

### Definition of Done

- Migration tests with the fake raw backend cover:
  - a legacy bearer moves into the aggregate record and the old item is deleted;
  - a legacy OAuth envelope migrates byte-for-byte as an opaque logical value;
  - two legacy providers both survive migration into one map;
  - an absent legacy item is checked once and is not probed again on later
    operations or a simulated process restart;
  - aggregate persistence failure leaves the legacy item untouched and returns
    an error;
  - a crash-equivalent state after aggregate persistence but before legacy
    deletion serves the aggregate value and retries deletion;
  - legacy deletion failure leaves a durable pending marker, does not hide the
    aggregate value, and succeeds idempotently on a later retry;
  - failure to persist removal of a completed pending marker is harmless and
    repairs on the next retry;
  - `set` during migration preserves the new value even when old-item cleanup
    must retry;
  - `delete` removes the aggregate value, attempts legacy cleanup, and surfaces
    an affected-key cleanup failure until no copy remains;
  - concurrent first reads of the same legacy key perform one legacy read and
    do not present duplicate migration work;
  - different legacy keys can be read concurrently, and a blocked legacy read
    for one key neither blocks a cached sibling read nor prevents another key's
    legacy read from starting;
  - concurrent migrations of two legacy keys merge under the mutation lock and
    produce one aggregate record containing both;
  - a blocked or failed pending-cleanup retry for one key does not block another
    key's cached read or legacy migration;
  - no migration error or trace text contains legacy or aggregate secret bytes.
- Existing prompt-service behavior tests remain green, especially:
  - concurrent OAuth refreshes do not lose a rotated refresh token;
  - sign-out preserves OAuth registration but clears tokens;
  - provider removal attempts both bearer and OAuth logical deletes and surfaces
    failures;
  - store-unavailable status remains distinct from a missing credential.
- A focused app-level test proves the release constructor injects the aggregate
  store while the debug constructor still injects the file store. Use compile-
  configuration-appropriate helpers rather than touching the real Keychain.
- Manual macOS acceptance on an ad-hoc release build:
  1. Seed two legacy provider entries using the pre-change build and verify both
     providers work.
  2. Install the changed build. Complete the one-time migration prompts and
     verify both providers still list/render prompts, including an OAuth provider
     across an app restart.
  3. Verify Keychain Access contains the aggregate Switchboard item and no legacy
     item for either migrated logical key.
  4. Quit and reopen the unchanged app: no Keychain authorization prompt.
  5. Rebuild/redeploy the ad-hoc app: one authorization prompt total even though
     both providers synchronize; after choosing **Always Allow**, the same launch
     produces no further prompt.
  6. Exercise an OAuth refresh or re-sign-in, restart, and verify the newly
     persisted credentials are retained and produce no additional authorization
     dialogs.
- Update the three documentation locations listed above with the precise
  migration and signing limitations.
- Run `make check` in the foreground and wait for completion. If the real
  Keychain acceptance steps cannot be performed in the implementation
  environment, report them explicitly as unverified; unit tests are not a
  substitute for the macOS ACL dialog.

## Final expected behavior and known limitation

After a successful migration:

- ordinary restart of an already-approved unchanged app: **zero** prompts;
- first access by a newly rebuilt ad-hoc app: **one** prompt total, independent
  of provider count, followed by cached reads for that process;
- first post-upgrade migration: potentially more than one prompt because each
  legacy item has its own ACL;
- after stable Developer ID signing is separately delivered and approved with
  **Always Allow**: the Keychain ACL can recognize updated signed builds, so the
  rebuild prompt should no longer recur solely because code changed.

This plan intentionally does not claim that aggregation replaces signing. It
fixes prompt multiplication; stable signing fixes application identity across
builds.
