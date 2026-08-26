# One-prompt Keychain storage for MCP credentials

**Status:** implemented; manual reauthentication acceptance pending · **Created:** 2026-08-26

Switchboard formerly stored every MCP prompt-provider credential in a separate
macOS Keychain item. Bearer credentials used the provider name as the item
account; OAuth envelopes used `oauth:<provider-name>`. After an ad-hoc-signed
build changed, macOS could ask the user to approve each item again even when the
user always chose **Always Allow**.

Release builds now preserve the keyed `SecretStore` contract while persisting
the complete logical map in one versioned Keychain item. The map is cached for
the process lifetime, so one authorization covers all providers and repeated
OAuth reads do not return to Keychain. The locked `keyring` backend may perform
an internal read during a mutation, but the verified **Always Allow** ACL covers
that operation without another dialog. With ad-hoc signing, the expected result
is one authorization after a code-changing rebuild and none after an ordinary
restart. Stable Developer ID signing remains separate release work.

The cutover from the former per-provider format is intentionally breaking. The
two known users reauthenticate once: OAuth providers sign in again, and bearer
providers are removed and re-added with their tokens. The aggregate store does
not read, copy, or delete old per-provider entries. After new credentials work,
users may delete obsolete entries manually in Keychain Access. This decision
keeps one-time upgrade handling out of the permanent credential architecture.

## Scope

In scope:

- The release `SecretStore` implementation in `crates/app`.
- One aggregate Keychain item for all Switchboard-owned MCP prompt-provider
  credentials.
- A process-lifetime cache that loads the aggregate item once explicitly.
- Serialization of full-record writes across concurrent provider refreshes.
- A documented breaking cutover requiring one-time reauthentication.
- Secret-safe errors, tests, documentation, and manual macOS verification.

Out of scope:

- Automatic migration, compatibility reads, deletion of old Keychain items, a
  migration command, or a credential-recovery UI.
- Developer ID enrollment, notarization, CI signing, or changes to
  `signingIdentity`; those remain in
  `2026-05-30-macos-release-distribution.md`.
- Harness-owned MCP credentials. This plan covers only prompt providers managed
  in Switchboard Settings.
- Changes to bearer/OAuth wire behavior, provider configuration, prompt timing,
  or the `SecretStore` trait in `crates/prompts`.
- Aggregating the debug `FileSecretStore`; its existing map file already avoids
  development Keychain prompts.
- An encrypted sidecar, provider-count limit, or speculative record-size policy.

## Required reading before implementing

The implementing agent must read these sources before changing code:

- `AGENTS.md`.
- `docs/system-design.md` §6.
- `docs/implementation_plans/2026-06-01-prompt-mcp-support.md`, especially the
  `SecretStore` seam and never-log-credentials invariant.
- `docs/implementation_plans/2026-08-08-mcp-oauth.md`, especially the credential
  envelope, blocking-pool bridge, provider lifecycle locks, rotating refresh
  tokens, removal, and sign-out behavior.
- `crates/app/src/secret_store.rs` and release wiring in `crates/app/src/lib.rs`.
- `crates/prompts/src/{secret,oauth,service}.rs`, including every `SecretStore`
  call and existing provider credential lifecycle lock.
- The pinned `keyring` macOS backend and its `security-framework` implementation
  in the local Cargo registry.
- Apple, **Access Control Lists**:
  https://developer.apple.com/documentation/security/access-control-lists
- Apple, **SecAccessCreate**:
  https://developer.apple.com/documentation/security/secaccesscreate%28_%3A_%3A_%3A_%29
- Apple, **Allow apps to access your keychain**:
  https://support.apple.com/guide/mac-help/allow-apps-to-access-your-keychain-kychn002/mac
- Apple Technical Note TN3137, **On Mac keychain APIs and implementations**:
  https://developer.apple.com/documentation/technotes/tn3137-on-mac-keychains
- Apple, **Understanding the Code Signature**:
  https://developer.apple.com/library/archive/documentation/Security/Conceptual/CodeSigningGuide/AboutCS/AboutCS.html

## Shared decisions and conventions

### Keep the logical keyed interface; aggregate only physical persistence

`crates/prompts` continues to call `SecretStore::{get,set,delete}` with the same
keys and semantics. Bearer remains `<provider-name>` and OAuth remains
`oauth:<provider-name>`. The prompt crate must not know that release builds store
those keys in one physical record.

The aggregate account is the internal constant `mcp-secrets:aggregate` in the
existing `switchboard` service namespace. Provider names reject `:`, so this
account cannot collide with a bearer key, and it does not use the `oauth:`
prefix. Renaming the service is not part of this work.

### Keep the record minimal and versioned

The physical item stores a JSON document containing only:

- an integer format version, starting at `1`; and
- a map from logical keys to opaque secret strings.

The store does not include migration markers, cleanup state, or copies of old
item identities. A malformed document or unknown version is a store-unavailable
error. It must never be reinterpreted as empty or overwritten during recovery,
because the next mutation would destroy every credential in the item. Error and
trace text may identify the operation or format problem but must never include
serialized record bytes or credential values.

An encrypted sidecar was considered and rejected. It would add a cryptographic
format, nonce and key lifecycle, a second durable artifact, and cross-file
recovery states without an observed need. If the Keychain later demonstrates an
actual item-size problem, that new evidence requires a separate design decision.

### One immutable snapshot and one mutation lock are load-bearing

All clones share an immutable snapshot of the last completed durable record. A
cached `get` holds only the snapshot's read guard briefly. The first operation
without a snapshot takes the mutation lock, rechecks, loads and parses the
aggregate item once, then publishes it. A confirmed missing item initializes an
empty snapshot. A denied, unavailable, corrupt, or unknown-version read remains
retryable and is not cached as empty.

The mutation mutex serializes full-record read-modify-write operations across
logical keys. Existing prompt-layer locks are per provider and cannot prevent
two different OAuth providers from deriving candidates from the same map. Hold
the mutation lock across the physical Keychain write, but never hold the
snapshot's write guard during that blocking call. Cached readers may therefore
observe the immediately preceding durable snapshot while a write is in flight.

Every mutation follows **persist candidate, then publish snapshot**. If encoding
or Keychain persistence fails, the last durable snapshot remains visible. Do not
publish first and attempt rollback: a platform write error can be ambiguous.

### Make the upgrade break explicit

The aggregate backend addresses only `mcp-secrets:aggregate`. It must never probe
logical keys as physical Keychain accounts. On upgrade, existing provider
configuration remains, but credentials appear missing until the user signs in
or supplies the token again. This is the accepted behavior, not an error state
to hide with compatibility code.

Old entries remain ignored. Documentation tells users they may remove them in
Keychain Access after confirming the new credentials work. Do not add automatic
cleanup: it would reintroduce the OS integration and failure handling that the
breaking cutover deliberately removes.

---

## Milestone 1 — Prove one-item macOS ACL behavior

This was a small disposable proof of the OS behavior on which the design rests.

### Goal & Outcome

- A rebuilt ad-hoc executable reads one existing disposable Keychain item and
  receives one authorization dialog.
- After **Always Allow**, repeated reads and a password-data update complete
  without another dialog.
- No real Switchboard credential is touched, and the disposable item is removed.

### Implementation Outline

Use the locked `keyring`/`security-framework` path with an isolated service and
account. Build once to seed the item, change and rebuild the ad-hoc executable,
then perform read/read/update/read while counting actual OS dialogs. The update
matters because Apple ACL permissions are operation-specific and the locked
backend reads the existing item internally before modifying it.

### Definition of Done

- Record macOS version, architecture, dependency versions, designated
  requirements, operation sequence, and prompt count.
- Delete the disposable item and confirm no production namespace was used.
- Stop the implementation if the result is not exactly one prompt.

### Verification record — 2026-08-26

- Verified on macOS 26.6.2 (build 25G83), Apple silicon (`arm64`), using locked
  `keyring` 3.6.3 and `security-framework` 3.7.0.
- The seed probe was ad-hoc signed with designated requirement CDHash
  `e922bd0f6d0155a6b38610dd88ab3819b2f62ff8`; after a source change and
  rebuild, the probe used CDHash `39bc49b2693429531d5bcddfbb7e2e78efceaf7d`.
- The rebuilt probe's first `get_password` presented one dialog. After the
  Keychain password and **Always Allow**, its second `get_password`,
  `set_password`, and final `get_password` completed without another dialog.
- The probe used a non-production service/account, printed no secret, and
  successfully deleted its disposable item.

---

## Milestone 2 — Aggregate record and process cache

### Goal & Outcome

- Bearer and OAuth logical keys round-trip through one physical secure record.
- One successful explicit aggregate load serves repeated reads for the process.
- Concurrent updates to different providers preserve every value.
- A failed write leaves the durable record and visible cache unchanged.
- Corrupt or future data is reported without overwrite or credential leakage.

### Implementation Outline

Implement the aggregate store in `crates/app/src/secret_store.rs` behind a small,
app-private raw backend with `read` and `write` operations. Production uses
`keyring`; tests use a failure-injectable backend that can prove operation counts,
blocked-write isolation, and concurrent read-modify-write behavior. This seam is
not a new workspace abstraction or prompt-crate API.

The raw read distinguishes `NoEntry` from every other failure. Treat
`keyring::Entry::set_password` as an opaque create-or-update operation: on the
locked macOS backend it performs an internal lookup, and any returned failure
must leave the published snapshot unchanged. Preserve the secret-safe mapping
that never formats `keyring::Error::BadEncoding`, because that error can contain
stored bytes.

Production docs must retain the rationale for one physical item, the immutable
snapshot and global mutation lock, persist-before-publish ordering, the breaking
cutover, and non-destructive handling of corrupt or unknown data.

### Definition of Done

- Unit tests prove:
  - a missing aggregate item initializes an empty logical store;
  - bearer and OAuth-shaped keys round-trip independently;
  - repeated reads across keys perform one explicit aggregate load;
  - concurrent initialization is single-flight and failed initialization retries;
  - clones share the snapshot and mutation lock;
  - a blocked write does not block a cached read, which sees the prior snapshot;
  - concurrent writes preserve every distinct key;
  - logical deletion preserves sibling keys and absent deletion is idempotent;
  - failed set and delete persistence leave cache and durable data unchanged;
  - corrupt JSON and future versions return errors without writes;
  - raw errors distinguish absence from unavailability and never leak secrets.
- Existing debug file-store tests remain green.
- The record shape contains no upgrade-only metadata.
- The normal repository test and lint targets pass with Makefile flags.

---

## Milestone 3 — Breaking release cutover

### Goal & Outcome

- Release builds inject the aggregate store; debug builds retain the file store.
- Existing users reauthenticate once instead of carrying automatic migration.
- Old per-provider items are neither read nor deleted by the app.
- After new credentials are stored, provider count no longer multiplies
  authorization prompts.

### Implementation Outline

Switch the non-debug secret-store factory to `AggregateSecretStore` and remove
the old keyed production store. Do not keep a selectable fallback. Remove all
migration-only raw operations, locks, record fields, direct macOS deletion
dependencies, state-machine tests, and documentation.

Keep provider configuration untouched. OAuth detects an absent credential and
uses the existing sign-in flow. A bearer provider requires removal and re-add so
the user can paste its token through the existing form. No new UI is justified
for the two-user upgrade; README documentation is sufficient.

Update `README.md` and `docs/system-design.md` to state the reauthentication
requirement and manual cleanup option. Keep the release-signing plan's explanation
that stable signing is what lets Keychain recognize later builds.

### Definition of Done

- A focused factory test pins release to the aggregate backend and debug to the
  file backend without touching a real Keychain.
- Searches find no production compatibility reads, cleanup markers, migration
  gates, or direct Keychain deletion adapter.
- The direct `security-framework` dependency added only for cleanup is removed;
  the transitive version required by `keyring` remains locked.
- `make check` and a release build pass.
- Manual macOS acceptance:
  1. Install the changed build over a setup with one OAuth and one bearer provider.
  2. Confirm both initially require credentials rather than silently reading old
     per-provider entries.
  3. Sign in to OAuth again; remove/re-add the bearer provider and paste its token.
  4. Verify both providers list and render prompts.
  5. Quit and reopen the unchanged app; verify zero authorization prompts and
     retained credentials.
  6. Rebuild/redeploy the ad-hoc app; verify one prompt total, then no further
     prompts after choosing **Always Allow**.
  7. Optionally delete obsolete per-provider Switchboard items in Keychain Access
     only after confirming the aggregate credentials work. Keep the
     `mcp-secrets:aggregate` item.
- If manual Keychain acceptance cannot be run in the implementation environment,
  report it as pending; unit tests do not substitute for the OS dialog check.

## Final expected behavior and limitation

- Upgrade from the former keyed store: one-time reauthentication; old entries
  remain ignored until the user removes them manually.
- Ordinary restart of an approved unchanged app: **zero** prompts.
- First access by a newly rebuilt ad-hoc app: **one** prompt total, independent
  of provider count, followed by cached reads for that process.
- After stable Developer ID signing is delivered separately and approved with
  **Always Allow**, code updates should stop invalidating the app identity used
  by the Keychain ACL.

Aggregation fixes prompt multiplication. Stable signing fixes identity across
builds; this plan does not claim otherwise.
