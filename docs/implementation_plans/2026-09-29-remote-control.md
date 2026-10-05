# Remote control: an iPhone app for Switchboard

**Status:** proposed · **Revision:** 4 · **Created:** 2026-09-29 · **Revised:** 2026-10-04

An iPhone app that lists Switchboard's projects, shows a live transcript, and lets the user
continue work already in progress — send a message, cancel a turn. The Mac stays the only
thing that runs agents; the phone is a remote screen for it. A small relay server carries
encrypted frames between the two, so nothing on the Mac listens for inbound connections and
the same code path serves the same Wi-Fi and the other side of the world.

Transport alternatives and the reasons for the relay are in
[research/remote-transport-evaluation.md](../research/remote-transport-evaluation.md). This
document is the component inventory — what gets written, where, and against which interfaces
— followed by the milestones. The iOS app lives in this repository at `SwitchboardMobile/`,
beside `crates/`, so the protocol fixtures and the shared Rust cryptography are in one tree.

## Changelog

- **Revision 4 (2026-10-04)** — second review, verdict SIMPLIFY (one critical, five major).
  - Finding 1 (critical): a live turn reaches the phone from one source only. The live
    snapshot is taken before the disk read, and disk copies of live turns, and of turns that
    start after the snapshot, are dropped on the Mac (§5.7).
  - Finding 3: forwarded events carry a per-project `seq` and a tail load carries
    `through_seq`; the phone drops anything at or below it. One number per project rather than
    one per turn. Replaces the unspecified "de-duplicated" (§4, §5.6, §7.4).
  - Finding 2: `ConversationCache` has its own key — the journal plus every agent's session
    file — instead of `project_session_fingerprints_impl`, which returns nothing for Codex and
    Antigravity and ignores the journal (§5.7).
  - Finding 4: `list_agents` uses the existing `list_project_agents_readonly_impl`, so it works
    for unopened projects in M3. Busy state comes from the dispatcher's existing queries and is
    carried on `list_projects` and `list_agents`; the `project_status` request is removed. The
    send handler's order is stated (§4, §5.5, §5.7).
  - Finding 5: `origin: SendOrigin` on `WorkPayload::Send` replaces the `emit_user_message`
    flag, and `user_message` is always emitted. **Reverses** revision 2's "add an
    `emit_user_message` parameter": with compose, phone, and workflow sends all emitting, no
    caller would pass `false` (§5.8).
  - Finding 6: the Mac sends its full `paired_devices` set on every registration and change,
    replacing `pair_added` / `pair_revoked`, so a relay restart loses nothing. The relay stamps
    `from` (§4, §5.1–5.3, §6).
  - Finding 7: hidden-window behaviour becomes a done-criterion in M2 and M4 (§5.9, §9).
  - Minors: `cancel_send` is removed from the first version — **reverses** revision 2's
    recipient retention in `SendLedger`, because no screen calls it and cancelling queued sends
    is already the first follow-up. `project_list_changed` and `harness_not_signed_in` are
    removed. `project_load_timeout` is 10 seconds. `firstHydration` is cleared with its
    siblings. The transport's receiver comes from its constructor. `AGENTS.md` updates are
    listed (§8).
  - Found during triage: `RemoteBackend` lives in `crates/remote`, which cannot name
    `crates/app` types, so its signatures now use protocol types, and it gains
    `journaled_recipients` for the receipt fallback revision 3 described (§1).
- **Revision 3 (2026-10-04)** — revision check of revision 2.
  - Check 1: the frontend load path is already separable from selection
    (`ensureProjectLoaded` has one caller, `activateProject`), so milestone 4's spike is
    dropped. The acknowledgement now waits for the project's *first* hydration, through a
    retained promise rather than a second `hydrateProject` call, which returns `"skipped"` and
    would not wait (§5.8).
  - Check 2: `send_receipt` carries `project_id`, so the post-restart journal fallback knows
    which journal to read (§4, §5.5).
  - Check 3: `last_seen` leaves `remote_devices.jsonl` and is held in memory by
    `SessionManager` (§5.3, §5.4, §5.10).
  - Check 4: `appendUserTurnImpl`'s `sendId` parameter becomes required; the production
    wrapper `dispatchUserTurn` already requires it (§5.8).
  - Check 5: the Debug build carries an App Transport Security local-networking exception and
    a Local Network usage description for the milestone 2 local relay; Release carries
    neither (§7.2, §9).
- **Revision 2 (2026-10-03)** — external review, 20 findings, all adopted: per-connection
  `Noise_KK` sessions; one Rust crypto crate shared with iOS through UniFFI; Mac-side paging of
  the merged conversation; pairing confirmation code; self-certifying relay ids; pairing
  bootstrap token; live-turn snapshot; idempotent sends; fragmentation; `user_message` reuse
  with a journaled `origin`; `WakeLease` reuse; launch at login; last seen; authentication
  before send; distribution; milestones. Settled all open decisions.
- **Revision 1 (2026-09-29)** — component inventory, no milestones.

## 0. Decisions

| Question | Decision | Effect |
|---|---|---|
| Audience | **The two project developers only** | No multi-user relay controls yet; baseline limits only (§6). |
| Distribution | **TestFlight, internal testing** | Paid Apple Developer Program on the owner's team; both developers are App Store Connect users on that team. Internal builds need no Beta App Review and expire after 90 days. Bundle id and team are placeholders until the account is set up (§8). |
| Who runs the relay | **The developers, for their own use, as a private pilot** | One deployment, `wss://` with TLS from the host, rate and frame-size limits from day one. If anyone else ever needs to deploy their own relay, redo the transport evaluation first — a self-deployed relay has setup friction comparable to Tailscale's. |
| Authentication before send | **Required** | Face ID with passcode fallback before each send, with a short reuse window (§7.4). Viewing and cancelling are not gated. |
| Pairing confirmation | **Required** | A short code derived from the handshake shows on both screens; the user confirms on the Mac (§2, §5.2). |
| Mac notification for sends from the phone | **None** | Phone sends are not registered with the desktop's send-completion tracking. Removes the hardest frontend integration. |
| Queued sends visible across devices | **Follow-up** | A send queued behind a busy agent appears on the other device when its turn starts. Showing and cancelling queued sends cross-device is the first follow-up (§8); until then the phone can stop a running turn but cannot cancel a queued send. |
| Launch at login | **Added** | Settings row backed by Tauri's autostart plugin (§5.9). |
| "Last seen" on the phone | **Added** | Relay-supplied time in the offline banner, with no inferred diagnosis (§6, §7.5). |
| Sends while the Mac is offline | **Refuse, don't queue** | Relay returns `mac_offline`; nothing is stored for later delivery. |
| "Needs approval" notifications | **Dropped** | Agents run with permission prompts disabled; they never ask. |
| Phone recipient defaults | **One-agent projects preselect it; otherwise the user picks, possibly several** | Remembers the last recipient set per project, dropping agents that no longer exist (§7.4). |
| Macs per phone | **One** | Stated as a limit; replacing the paired Mac is a deliberate action. |
| Archived projects on the phone | **Hidden by default** | Matches the desktop. |
| Push notifications | **Deferred** | No APNs entitlement in the first build. |
| iOS app location | **This repository, `SwitchboardMobile/`** | Shared fixtures and crypto crate; one PR can change both sides. |

## 1. Interfaces and their test doubles

Each row is a seam a test needs. Everything else is a plain module with pure functions, tested
directly — abstract where a second implementation or a test needs it, not everywhere.

| Interface | Production | Test double | Where |
|---|---|---|---|
| `Transport` | `RelayTransport` (WebSocket to relay) | in-memory pair | `crates/remote`; Swift `Transport/` |
| `RemoteBackend` | impl over `AppState` | recording mock | `crates/remote` (trait), `crates/app` (impl) |
| `EventEmitter` *(existing)* | `RemoteEmitter` decorating the existing chain | `RecordingEmitter` *(existing)* | `crates/remote`, installed in `crates/app` |
| `KeyStore` | over the existing secret store | in-memory | `crates/remote` (one-method trait), `crates/app` (impl) |
| `DeviceRegistry` | JSONL-backed | in-memory | `crates/remote` |
| `AgentProjectResolver` | over `AppState.agents_by_id` | map | `crates/remote` (trait), `crates/app` (impl) |
| `PowerSource` | IOKit AC-power observer | toggled fake | `crates/app` |
| `RequestBroker` | over `Transport` | stub returning fixtures | Swift |

**Ownership model**, matching `HarnessAdapter`: async traits use `#[async_trait]` and are held as
`Arc<dyn …>`. `EventEmitter::emit` stays synchronous; `RemoteEmitter` enqueues fan-out work onto a
bounded channel so a slow phone can never block dispatch.

```rust
#[async_trait]
pub trait Transport: Send + Sync {
    async fn send(&self, to: &DeviceId, frame: Frame) -> Result<(), TransportError>;
    fn status(&self) -> watch::Receiver<TransportStatus>;
}

#[async_trait]
pub trait RemoteBackend: Send + Sync {
    async fn list_projects(&self) -> Result<Vec<RemoteProject>, BackendError>;
    async fn list_agents(&self, project: ProjectId) -> Result<Vec<RemoteAgent>, BackendError>;
    async fn load_conversation(&self, project: ProjectId, before: Option<Cursor>, limit: usize,
        live: &LiveSnapshot) -> Result<ConversationWindow, BackendError>;
    async fn ensure_project_loaded(&self, project: ProjectId) -> Result<(), BackendError>;
    async fn send_message(&self, project: ProjectId, agent: AgentId, prompt: &str, send_id: SendId)
        -> Result<MessageId, BackendError>;
    async fn cancel_turn(&self, agent: AgentId) -> Result<(), BackendError>;
    async fn journaled_recipients(&self, project: ProjectId, send_id: SendId)
        -> Result<Vec<AgentId>, BackendError>;
}

pub trait KeyStore: Send + Sync {
    fn get_or_create(&self, name: &str, create: &dyn Fn() -> Vec<u8>) -> Result<Vec<u8>, KeyStoreError>;
}
```

`send_message` is one recipient per call; the request handler loops over recipients (one
`send_message_impl` per recipient, shared `send_id`), exactly as the desktop compose bar does.

A transport's constructor returns the `Arc<dyn Transport>` together with its inbound
`mpsc::Receiver<(DeviceId, Frame)>`: a receiver has one owner, so it is handed over once rather
than fetched through `&self`.

`crates/remote` cannot depend on `crates/app`, so the trait speaks in protocol types defined in
`crates/remote` — `RemoteProject`, `RemoteAgent`, `Cursor`, `LiveSnapshot`,
`ConversationWindow` — and the app's implementation maps its own types into them. A
`ConversationWindow`'s items cross as `serde_json::Value`: the same serialization of
`ConversationItem` the desktop receives over IPC.

```swift
protocol Transport {
    func send(_ frame: Frame) async throws
    var inbound: AsyncStream<Frame> { get }
    var status: AsyncStream<TransportStatus> { get }
}
protocol RequestBroker {
    func request<R: Decodable>(_ type: String, _ payload: Encodable) async throws -> R
}
```

## 2. Security

**What's at stake.** A paired phone can run anything on the Mac. Agents run with permission
prompts disabled (Claude with `--dangerously-skip-permissions` and `--add-dir /`, Codex with
`--dangerously-bypass-approvals-and-sandbox`), so a send from the phone has the same reach as a
send from the keyboard. This feature is remote code execution by design; the security work is
making sure only the right phone, held by the right person, can do it.

**Threat model**

| Threat | Defended? | By |
|---|---|---|
| Relay operator reads conversations or prompts | Yes | End-to-end encryption; the relay sees only device ids, sizes, timing |
| Relay operator injects or alters a frame | Yes | Noise transport messages are authenticated; a forged or altered frame fails to open |
| Relay operator replays a frame | Yes | Per-connection keys: a frame from an earlier connection fails to open; within a connection, `snow`'s implicit in-order nonces reject a repeat |
| Relay operator impersonates the Mac or the phone | Yes | Every connection runs `Noise_KK` with both static keys pinned at pairing |
| Someone photographs the QR code and pairs first | Yes | The confirmation code is derived from the handshake hash, so an attacker's session shows a different code; the registry row is written only when the user confirms the match on the Mac |
| An unpaired device probes whether a Mac is online, or sends it frames | Yes | The relay forwards to a Mac only from devices it announced, or from the holder of an open pairing token; the Mac independently refuses unknown devices |
| Someone claims an existing device id on the relay | Yes | Self-certifying ids: `device_id` is a hash of an Ed25519 public key, and registration signs the relay's challenge |
| A revoked phone that is still connected | Yes | Revocation drops the device's live session; its next frame fails to open and future handshakes are refused |
| Stolen unlocked phone | Partly | Device-owner authentication before each send. Viewing and cancelling are not gated. Revocation from the Mac remains the remedy |
| Compromised Mac | No | Out of scope; the Mac is the trusted end |
| Relay denial of service | Partly | Rate and frame-size limits; worst outcome is loss of remote access, not compromise |
| Traffic analysis | No | Inherent to a relay; accepted |

**Where trust is enforced.** Every trust decision is made on the Mac. The Mac completes a
handshake only with a static key in its `DeviceRegistry`, admits pairing candidates only for
the pairing handshake, and drops a revoked device's live session immediately. The relay's pair
scoping is a second line. A compromised relay can interrupt service but cannot make the Mac
execute anything.

**Key material**

| Key | Where it lives | Secret? |
|---|---|---|
| Mac Noise static key (X25519) | `KeyStore` (Keychain in release, dev store in debug) | Yes |
| Mac identity key (Ed25519) | `KeyStore` | Yes |
| Phone Noise static key, phone identity key | iOS Keychain, `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` | Yes |
| Paired phones' public keys (both kinds) | `remote_devices.jsonl` | No |
| Pairing token and pairing PSK | Mac memory only, for the 5-minute window | Yes |
| Per-connection transport keys | Memory only, discarded on disconnect | Yes |

No session key is ever persisted, and no nonce counter needs to be.

**Cryptography** — one implementation, in Rust, shared with iOS (§3):

- Pairing: `Noise_XXpsk2_25519_ChaChaPoly_SHA256`, PSK from the QR.
- Every connection: `Noise_KK_25519_ChaChaPoly_SHA256`, phone-initiated, both statics pinned.
- Relay identity: Ed25519 (`ed25519-dalek`), separate from the Noise static key. The phone's
  identity public key travels *inside* the encrypted pairing handshake, binding the relay
  identity to the Noise identity so the relay cannot swap one for the other.
- SHA-256 throughout so a pure-CryptoKit fallback stays possible if UniFFI ever has to go.

**Off by default.** Nothing connects and no key is generated until the user enables remote
access. Disabling drops the relay connection immediately.

**Disclosure.** README's harness-limitations list gains: *"A paired phone has the same reach as
the Mac. Agents run without approval prompts, so anyone who can unlock a paired phone can run
commands on your Mac. Revoke lost devices in Settings → Remote access."*
`docs/system-design.md` gains a Remote access section carrying this threat model.

**Review boundary.** `crates/remote-crypto` (handshakes, sealer, fragmentation, code
derivation), the pairing confirmation flow, `DeviceRegistry`, and the Swift key storage get a
dedicated security review, separate from the feature review. The repo's `/security-review` skill
is the gate; each milestone names which of these it touches.

## 3. Shared cryptography — `crates/remote-crypto/`

No tokio, no Tauri. `crates/remote` depends on it; iOS consumes it through UniFFI.

**Contents**

- `pairing` — the `XXpsk2` handshake as a state machine over bytes. Output: the peer's Noise
  static key, the peer's Ed25519 identity key, and the handshake hash.
- `confirmation_code(handshake_hash) -> String` — six digits, identical on both ends of one
  handshake and different for any other.
- `session` — the `KK` handshake and the resulting transport: `seal(envelope) -> Vec<Record>`,
  `open(record) -> Option<Envelope>`.
- `fragment` — splits a serialized envelope into records that each fit the Noise limit
  (65,535 bytes on the wire, so 65,519 bytes of plaintext after the tag). Each record's header —
  message id, index, count — is inside the encrypted payload, so it is authenticated. The
  reassembler enforces a total-size limit, a limit on incomplete messages, and discards partial
  state on disconnect. Content is never truncated.
- `identity` — Ed25519 keygen, `device_id = base32(sha256(public_key))[..26]`, challenge
  signing and verification.

**Ordering assumption.** `snow`'s transport requires in-order delivery. One WebSocket per side
through one relay preserves order; the relay must forward a pair's frames in arrival order and
never fan them across workers.

**UniFFI binding.** `make ios-crypto` builds an xcframework for `aarch64-apple-ios` and
`aarch64-apple-ios-sim`; both targets are added to `rust-toolchain.toml`. Swift gets opaque
`PairingHandshake` and `Session` objects plus the free functions. Keychain access stays in
Swift.

**Tests**

- Rust: known-answer vectors for both handshakes; negative vectors (wrong PSK, wrong static
  key, tampered handshake message, tampered transport record, record from a previous
  connection, repeated record); confirmation codes equal for one handshake and different across
  two; fragmentation round trip for a large envelope; missing, duplicate, out-of-range, and
  oversize fragments rejected; signature verification with a wrong key fails.
- Swift: the binding only — a round trip through `Session`, and a Rust error surfacing as a
  Swift `throw`.

## 4. Shared protocol

**Envelope** — `{ id, type, payload }`. `id` is a sender-minted UUID; responses and errors echo
it. `type` is snake_case, mirroring `#[serde(tag = "type", rename_all = "snake_case")]`.

**Relay frame** — what crosses the relay is `Frame { to, from, record }`. A client sends `to`
and `record`; the relay stamps `from` with the sender's registered device id and never trusts a
client-supplied value. Handshake messages and transport records travel the same way.

**Requests (phone → Mac).** None is accepted before the connection's `KK` handshake completes.

| Type | Payload | Response |
|---|---|---|
| `list_projects` | – | per project: id, name, directory, `directory_available`, `archived`, last activity, `status` (`busy` / `idle`) |
| `list_agents` | `project_id` | per agent: id, name, harness, model/effort, `busy` |
| `load_conversation` | `project_id`, `before?`, `limit` | a window of `ConversationItem`s and `next_before?`; a tail load (no `before`) also carries the live snapshot and `through_seq` (§5.7) |
| `send_message` | `project_id`, `agent_ids[]`, `prompt`, `send_id` | per-recipient result: `accepted { message_id }` or `rejected { code }` |
| `send_receipt` | `project_id`, `send_id` | the stored per-recipient results, or `unknown` |
| `cancel_turn` | `agent_id` | – |

**Cursors.** `before` is `(at, id)`: `id` is the `send_id` for a user row and the durable
hydration key for an agent turn. Turns with no durable key fall back to `(at, ordinal within
at)`, documented as stable only within one cached conversation. Turn ids are never cursors:
the parser regenerates them on every read.

**Events (Mac → phone)**, wrapped with `project_id`, `agent_id`, and `seq`, forwarded from an
explicit allowlist: `turn_start`, `user_message` (now carrying `origin`, §5.8), `content_chunk`,
`turn_identity`, `liveness`, `tool_started`, `tool_completed`, `tool_facet_updated`, `turn_end`,
`message_failed`, `message_cancelled`, `agent_idle`, `session_meta`, `context_report`,
`rate_limit_event`.

**Live sync.** `seq` is a per-project counter that `RemoteEmitter` assigns to each forwarded
event in emit order. A tail load's reply carries `through_seq`: every event at or below it is
already reflected in the reply. The phone subscribes before it loads, buffers what arrives,
and on the reply drops every event with `seq ≤ through_seq` and applies the rest. A gap in
`seq` means the phone missed something, and it reloads the tail.

**Control** — `hello` (`protocol_version`), `subscribe` / `unsubscribe` (`project_id`),
`ping` / `pong`, `error` (`code`, `message`, optional `last_seen`). Pairing: `pairing_hello`,
`pairing_confirmed`, `pairing_declined`.

**Error codes** — `mac_offline` (from the relay, with `last_seen` when known), `phone_offline`,
`not_paired`, `pairing_expired`, `directory_missing`, `agent_busy_elsewhere` (the harness
session lock: `AppError::SessionInUse`), `project_locked` (another Switchboard process holds the
project: `AppError::ProjectLocked`), `project_load_timeout`, `protocol_mismatch` (says which
side to update), `unknown_recipient`. A harness that is not signed in is not a request error:
a send is accepted before its turn starts, so the failure arrives later as `message_failed`
with its failure kind.

**Rules**

- No HTTP semantics. Errors travel in the payload.
- `mac_offline` comes from the relay, which enforces refuse-don't-queue without understanding
  anything else.
- Unknown `type` values decode to an `unknown` case on both sides and are ignored.
- **Fixtures are the contract.** One JSON file per message type at
  `crates/remote/tests/fixtures/protocol/`. A Rust test checks encoding against the committed
  files; `make protocol-fixtures` regenerates them deliberately. Swift decodes the same files,
  and Swift-encoded requests are decoded by a Rust test.

## 5. Switchboard (Mac)

### `crates/remote/` — Tauri-free

**5.1 `RelayTransport`.** One tokio task owning a `tokio-tungstenite` socket; outbound only.
Registers with the relay by signing its challenge with the Mac's Ed25519 key, and after every
successful registration sends `paired_devices { device_ids }` — the full non-revoked set, which
the relay treats as authoritative. Backoff 1s→60s with jitter; reconnects on network change and
on wake. Status: `Disabled | Connecting | Connected | RelayUnreachable(since)`. Knows nothing
about message types.

**5.2 `PairingCoordinator`.**

1. The user clicks **Pair a device**. The coordinator mints a token and a PSK, sends
   `pairing_open { token_hash, expires_in: 300 }` to the relay, and renders the QR: relay URL,
   Mac device id, Mac Noise static public key, token, PSK.
2. The phone's handshake frames carry the token; the relay forwards them for the first device
   presenting it, to this Mac only.
3. The coordinator admits that device as a **pending candidate**: it may complete the pairing
   handshake and nothing else. A second device presenting the token is refused.
4. Both screens show the confirmation code. The Mac shows **Confirm** / **Decline** beside it
   and the phone's reported name, marked as informational.
5. On **Confirm**, the registry row is written and the Mac sends its updated `paired_devices`
   set to the relay. On **Decline**, expiry, or a dropped relay connection mid-pairing, the
   candidate is discarded and the token is dead. Three failed handshakes on one token also kill
   it.

**5.3 `DeviceRegistry`.** Trait plus JSONL impl over `remote_devices.jsonl` in the config dir:
`device_id`, `name`, `noise_public_key`, `identity_public_key`, `paired_at`, `revoked_at?`.
Public keys only. The file records pairing and revocation and nothing that changes per
connection. `revoke(device)` marks the row, tells `SessionManager` to drop the live session,
and sends the updated `paired_devices` set to the relay.

**5.4 `SessionManager`.** One live `Session` per connected device. On a handshake from a
registered device, runs `KK` against that device's pinned key; a new successful handshake from
the same device replaces the old session, whose further records fail to open. Records are
opened here and only plaintext envelopes leave the module. Requests arriving before handshake
completion are refused with `not_paired`. Also holds, in memory, each device's connection
state and the time of its last disconnect since the app launched; Settings reads it from here.

**5.5 Request side.**

- `RequestHandler` — decode, call `RemoteBackend`, encode the response or `error`. For
  `send_message` the order is fixed: `ensure_project_loaded`, then check that every recipient
  is in the project's roster (`unknown_recipient` otherwise, with nothing dispatched), then
  dispatch one recipient at a time. Validation comes second because it needs the roster.
- `SubscriptionRegistry` — `device_id → set<project_id>`.
- `SendLedger` — `send_id → per-recipient results`, in memory, bounded by count and age. A
  repeat `send_message` with a known `send_id` returns the stored results without dispatching,
  and `send_receipt` reads it. Request-level errors raised before any dispatch
  (`project_load_timeout`, `project_locked`, `unknown_recipient`) are not recorded, so the same
  request can be retried. After a Mac restart the ledger is empty, so `send_receipt` asks
  `RemoteBackend::journaled_recipients` for the request's `project_id`: a journaled `Send` for
  that `send_id` means that recipient's turn started. Otherwise it returns `unknown`. A send
  that was queued but never started is lost with the restart and correctly reports `unknown`.

**5.6 `RemoteEmitter`.** An `EventEmitter` decorator over the existing chain, the same shape as
`WakeLockEmitter` and `SessionMetaObservingEmitter`, installed where `AppState.emitter` is built.
It forwards everything unchanged, then:

- resolves `AgentId → ProjectId` through `AgentProjectResolver` (agent channels carry no
  project);
- for allowlisted events, assigns the project's next `seq` and enqueues fan-out to devices
  subscribed to that project. A device whose queue is full has events dropped rather than
  blocking; the phone sees the gap in `seq` and reloads;
- tracks each live turn from `turn_start` (agent, turn id, `started_at`) and buffers its
  forwarded events until `turn_end`, `message_failed`, or `message_cancelled`;
- answers `snapshot(project) -> LiveSnapshot { through_seq, taken_at, turns }` under one lock,
  so the snapshot and `through_seq` describe the same instant.

### `crates/app/` changes

**5.7 Listings and conversation for the phone.**

- **Projects.** `list_projects_impl`, plus a `status` per project: `busy` when any of its agents
  has work in the dispatcher (`Dispatcher::has_pending_work`, which covers a running turn, a
  queued backlog, and post-terminal enrichment) or `AppState.workflow_runs` holds a run for it.
  A project the desktop has not loaded has no registered agents and no run, so it is `idle`.
- **Agents.** `list_project_agents_readonly_impl`, which lists a roster without loading or
  locking the project (the desktop's cross-project pickers use it), not `list_agents_impl`,
  which fails with `ProjectNotLoaded`. `busy` comes from `Dispatcher::running_turn_kind`.
- **Conversation.** `load_project_conversation_impl` already merges the journal and every
  agent's transcript, groups a fan-out's user message once, carries failed and cancelled outcome
  markers, and opens projects the desktop hasn't loaded. `RemoteBackend::load_conversation`
  wraps it in four steps, in this order:
  1. The request handler takes `RemoteEmitter::snapshot(project)` **first**.
  2. `ConversationCache` returns the merged conversation, re-reading only if its key changed.
  3. **Disk copies of live turns are dropped.** A session file read mid-turn already holds part
     of the running turn, and only Claude has a key to match it against the live copy; Codex
     and Antigravity have none, which is why the desktop never re-reads them mid-session
     (`HarnessKind::supports_refresh`). So the snapshot is the only source for a live turn.
     Dropped: every disk `AgentTurn` whose agent has a live turn in the snapshot and whose
     `started_at` is at or after that live turn's `started_at`; and every disk `AgentTurn`
     whose `started_at` is after the snapshot's `taken_at`, because a turn that begins during
     the read reaches the phone entirely as events above `through_seq`. One turn in flight per
     agent is structural, so the first rule cannot catch an earlier turn.
  4. `TranscriptWindow`, a pure function beside `merge_project_conversation`, cuts the window
     and `next_before`. A tail load's reply adds the snapshot and its `through_seq`.

  Taking the snapshot before the read is what makes a turn that ends during the read safe: its
  disk copy is dropped, and its remaining events, including `turn_end`, are above
  `through_seq`.
- **`ConversationCache`.** The merged conversation per project, reused across pages because the
  loader re-parses every session file. Its key is captured before each parse and compared on
  every load: the journal's length and modification time, plus `fingerprint_of` over
  `resolve_session_file` for **every** agent, with no `supports_refresh` gate. It is not
  `project_session_fingerprints_impl`, which returns no fingerprint for Codex or Antigravity and
  never looks at the journal, and so would miss a finished Codex turn, a new send, and an
  outcome marker. Every one of those changes a file in the key, so no separate invalidation
  signal is needed.

**5.8 Sends from the phone.**

- **Loading.** `send_message_impl`, `list_agents_impl`, and `lookup_agent` need the project in
  `AppState.projects`, which only `open_project_impl` fills, and the desktop's first
  `hydrateProject` assumes no live turns exist while it reads. So the frontend does the loading:
  `ensure_project_loaded` emits `remote_load_project { request_id, project_id }`, the frontend
  loads the project without selecting it, and calls `remote_project_loaded(request_id, result)`;
  the backend awaits that for up to 10 seconds, which is inside the phone's 15-second send
  timeout so the phone hears the Mac's answer before giving up, and then answers
  `project_load_timeout`. The load keeps running, so a retry usually succeeds. It never polls
  `state.projects`, which fills before the frontend subscribes to agent channels.
- **Frontend load, precisely.** In `src/lib/state/workspace.svelte.ts`, `ensureProjectLoaded`
  (whose only caller is `activateProject`, which handles selection separately) stops discarding
  its hydration: `void hydrateProject(projectId)` becomes
  `firstHydration.set(projectId, hydrateProject(projectId))`, in a module-level
  `Map<ProjectId, Promise<HydrateOutcome>>`. A new exported `loadProjectForRemote(projectId)`
  awaits `ensureProjectLoaded(projectId)`, then awaits `firstHydration.get(projectId)`, then
  resolves. Calling `hydrateProject` again would not work: it is sticky through
  `hydrationStarted` and returns `"skipped"` immediately. The `remote_load_project` handler
  calls `loadProjectForRemote` and acknowledges `ok` once hydration has settled, whether it
  completed or failed — a failed hydration leaves the desktop showing a load error, but no read
  is still in flight, so dispatch is safe. If `ensureProjectLoaded` rejects (open, lock, or
  roster failure), it acknowledges the error. A project the desktop loaded earlier
  acknowledges immediately, because its stored promise has long settled. `firstHydration` is
  cleared wherever `loadStarted` and `hydrationStarted` are: on project removal and on state
  reset.
- **Origin, end to end.** `WorkPayload::Send` drops its `emit_user_message` flag and gains
  `origin: SendOrigin` (`Keyboard | Remote | Workflow`, defined in `crates/core` beside
  `JournalRecord`). `Dispatcher::send_message` takes it (compose passes `Keyboard`, the phone
  `Remote` with `OnBusy::Enqueue`); the two awaiting wrappers, which differed only by the flag,
  become one that takes it (workflows pass `Workflow`). The actor hands it to
  `JournalSink::record_send` and then **always** emits `user_message` carrying it — the journal
  write is inside the actor, so the payload is the only way the origin can reach it.
  `JournalRecord::Send`, `NormalizedEvent::UserMessage`, and `ConversationItem::UserMessage`
  gain the field with `#[serde(default)]` producing `keyboard` (the existing `attachments`
  field is the pattern), and `src/lib/types.ts` gains it on the `user_message` event and the
  conversation item. Only `remote` renders a chip, so old lines that default to `keyboard`
  change nothing visible, and the chip survives reload because it is journaled.
  `user_message` carries no attachments, so a send with attachments shows its chips on the
  other device only after a reload; accepted for the first version.
- **Desktop sends reach the phone.** Compose sends also emit `user_message`. The desktop reducer's
  `user_message` branch becomes a no-op when a user turn with that `send_id` and agent already
  exists, since compose's optimistic turn already rendered it. The dedupe depends on the
  optimistic turn carrying its `send_id`: the production wrapper `dispatchUserTurn`
  (`src/lib/state/index.svelte.ts`) already requires it, and `appendUserTurnImpl`'s `sendId`
  parameter (`src/lib/state/reducers.ts`) changes from optional to required so no future caller
  can omit it.
- **No Mac notification** for phone sends: they are never registered with
  `sendCompletion`, which registers compose sends with their recipients before any IPC call.

**5.9 Settings, power, and lifecycle.**

- Tauri commands: `remote_status`, `remote_enable`, `remote_disable`, `remote_begin_pairing`,
  `remote_confirm_pairing`, `remote_decline_pairing`, `remote_list_devices`,
  `remote_revoke_device`, `remote_project_loaded`.
- `config.yaml` gains `remote: { enabled: false, relay_url, keep_awake_when_plugged_in: false }`,
  following `auto_reading_mode` including the "old config loads with it off" test.
- **Keep-awake** reuses the existing `WakeLease` from `wake_lock.rs`. The remote lease is held
  while all four hold: the preference is on, remote access is enabled, at least one device is
  paired, and `PowerSource` reports AC. Dropping it never affects leases held by running turns
  or workflows. Settings copy: this prevents idle sleep, not lid-close sleep (except on AC with
  an external display), and the phone cannot wake a sleeping Mac.
- **Launch at login** — a Settings row backed by Tauri's autostart plugin, added with `cargo
  add` and `pnpm add`. Closing the window already hides it rather than quitting
  (`handle_macos_run_event`), so remote access survives a closed window; quitting or rebooting
  ends it, and the Settings copy says so. **Unverified:** the app holds no activity assertion
  while idle (its sleep assertion exists only during turns), so macOS App Nap may throttle a
  hidden window's webview and timers — and loading a project for a phone send runs in the
  webview. M2 and M4 test this; if it fails, the app holds an `NSProcessInfo` activity while
  remote access is enabled.

**5.10 Frontend (Svelte).**

- Settings → Remote access: `Switch` to enable; connection status; launch-at-login and
  keep-awake rows; paired devices with "connected" or the last disconnect time since launch
  (from `SessionManager`), and **Revoke**.
- Pairing modal: QR (`qrcode` package → SVG), 5-minute countdown, then the confirmation code
  with **Confirm** / **Decline** and the phone's reported name.
- The `remote_load_project` handler and `loadProjectForRemote` (§5.8).
- Transcript: an "iPhone" chip on user messages whose `origin` is `remote`, live and after reload.
- The `user_message` reducer de-duplication (§5.8).

**5.11 Tests**

- Unit: registry read/write; `TranscriptWindow` (fan-out yields one user row, outcome markers
  appear in their window, cursor past the end is empty, keyless-turn fallback); the live-turn
  drop rule, with one fixture per harness (Claude, Codex, Antigravity) of a session file read
  mid-turn — the window holds no disk agent turn for the live turn and keeps the earlier ones —
  and a turn that started after `taken_at`; the cache key changes on a journal append and on a
  Codex or Antigravity session-file change; `SendLedger` bounds; `WakeLease` toggling with the
  fake inhibitor and fake `PowerSource`, including revoking the last device while a turn holds
  its own lease.
- Fixture-driven: `RequestHandler` against the mock backend (a send calls
  `ensure_project_loaded` before validating recipients, an unknown recipient dispatches
  nothing, idempotent repeat, concurrent repeat, partial fan-out rejection, a request-level
  error is retryable); `RemoteEmitter` with `RecordingEmitter` (allowlist, subscription
  isolation, `seq` contiguous per project, a snapshot's `through_seq` equals the last `seq`
  assigned, the buffer cleared at each terminal, a full device queue drops without blocking
  emit); second page served from cache (count loader calls with a stub); `send_receipt` after a
  simulated restart reads the named project's journal; `list_agents` and `list_projects` for a
  project the desktop has not loaded; protocol fixtures.
- Integration (in-process relay, real local WebSocket): pair with confirmation → connect →
  subscribe → load → send → events → revoke mid-session → next frame refused; restart either
  endpoint and reconnect; **restart the relay** — the Mac re-registers, resends
  `paired_devices`, and the phone reconnects without re-pairing; replay a connection-1 frame
  into connection 2; a second device with the same pairing token refused; a pending candidate's
  `send_message` refused; unloaded-project send waits for the frontend acknowledgement;
  acknowledgement timeout; `project_locked`.
- Frontend: the load handler loads without changing selection; the acknowledgement is not sent
  until the first hydration has settled (hold the hydrate's `api` call open and assert no
  acknowledgement); a failed hydration still acknowledges `ok`; an open failure acknowledges the
  error; an already-loaded project acknowledges immediately; removing a project clears its
  `firstHydration` entry; compose send plus its echoed `user_message` renders one row; a
  workflow send still renders one row; old journal lines without `origin` parse.

## 6. Relay server — `crates/relay/` (separate binary)

Deliberately dumb; never sees plaintext; holds no conversation state.

- **Stack**: axum + `tokio-tungstenite`. Env config: `PORT`, `RATE_LIMIT`, `MAX_FRAME_BYTES`,
  `MAX_DEVICES_PER_MAC`. `crates/relay/Dockerfile`; TLS terminated by the host so clients use
  `wss://`. Locally: `cargo run -p switchboard-relay`.
- **Registration**: the client sends its Ed25519 public key; the relay checks that it hashes to
  the claimed `device_id` and that the client signs a fresh challenge. No trust-on-first-use
  map, so a relay restart gives an impostor nothing.
- **Session table**: in memory, `device_id → sender`. A new registration for an id replaces the
  stale socket. A restart makes everyone reconnect.
- **Forwarding**: stamp `from` with the sender's registered id, then forward a pair's records
  unchanged and in order. If `to` is not connected, reply
  `error { mac_offline | phone_offline }`, with `last_seen` for a Mac. Nothing is queued.
- **Pair scoping**: a phone may address a Mac only if it is in that Mac's current
  `paired_devices` set, or if its frame carries a token matching an open `pairing_open` from
  that Mac. The Mac sends the full set after every registration and whenever it changes, and
  the relay replaces what it held — so a relay restart loses nothing: the set arrives again
  when the Mac reconnects. A token is bound to the first device presenting it and cleared on
  expiry, first confirmation, or the Mac's disconnect.
- **Last seen**: per Mac id, the time of its last disconnect, in memory; documented as
  observational and absent after a relay restart.
- **Limits from day one**: per-device message rate, `MAX_FRAME_BYTES`, idle timeout,
  `MAX_DEVICES_PER_MAC`.
- **Observability**: `tracing` logs, `/healthz`, counters for connected devices and forwarded
  frames. Frame contents are never logged.
- **Deployment** of the shared pilot instance is a separately approved action, not part of a
  PR.
- **Tests**: forward and ordering; a client-supplied `from` is overwritten; offline error with
  `last_seen`; wrong signature refused; impostor refused after a restart; a phone outside the
  set refused; a new `paired_devices` set replaces the old one; token forwarding, expiry, and
  second-device refusal; rate and frame limits; reconnect replaces the stale session; the
  container builds and answers `/healthz` locally.

## 7. iOS app — SwiftUI, `SwitchboardMobile/`

iOS 17+. No third-party Swift packages; the only non-Apple code is the Rust xcframework from
§3. One `@Observable` store per screen over pure reducers, mirroring the desktop's
reducer-plus-component split.

**7.1 `Protocol/`** — `Codable` mirrors of §4, `RemoteEvent` with an `.unknown` case that
renders nothing. `ConversationItem`, `ContentBlock`, `ToolRow` model the merged conversation the
Mac sends. History containing attachments must decode and render (as a placeholder chip) even
though the phone cannot send attachments.

**7.2 `Transport/`**

- `RelayTransport` — `URLSessionWebSocketTask`; registration signs the relay challenge through
  the binding; backoff; disconnects after a grace period in the background, reconnects on
  foreground.
- **Local relay in development.** iOS App Transport Security blocks plain `ws://`, and iOS asks
  for Local Network permission before reaching a device on the LAN. The **Debug** build
  configuration's Info.plist carries `NSAppTransportSecurity` → `NSAllowsLocalNetworking = YES`
  and an `NSLocalNetworkUsageDescription`, and the dev relay URL uses the Mac's `.local`
  hostname (`ws://<mac-name>.local:<port>`). The **Release** configuration, which TestFlight
  builds use, carries neither and accepts only `wss://`. A test asserts the Release Info.plist
  has no ATS exception.
- `SecureSession` — wraps the binding's `Session`: runs `KK` on every connection, seals and
  opens records, reassembles fragments.
- `LiveRequestBroker` — matches responses by `id`; a 15-second timeout on `send_message` yields
  **outcome unknown**, resolved by `send_receipt`, never by resubmitting.
- `EventBus` — publishes decoded events to subscribed stores.

**7.3 `Pairing/`**

- `PairingScanner` — `AVCaptureSession` QR scan → `PairingOffer`.
- `PairingFlow` — runs the binding's `XXpsk2` handshake with the token, shows the confirmation
  code with "Confirm on your Mac", waits for `pairing_confirmed` / `pairing_declined`, then
  stores the phone's keys and the Mac's pinned keys in the Keychain. Handles expired offers,
  decline, wrong relay, and Mac not connected.
- One Mac per phone: pairing a new Mac replaces the old one after an explicit confirmation.

**7.4 `Stores/`**

- `ConnectionStore` — `connected | relayUnreachable | macOffline(lastSeen?) | notPaired`.
- `ProjectsStore` — `list_projects`, which carries each project's status; archived hidden by
  default with a toggle. Refreshes on open, on return to the foreground, and on pull-to-refresh.
- `TranscriptReducer` — pure. A tail load **replaces** the project's live state with the reply's
  window and snapshot; events are then applied only when their `seq` is above the reply's
  `through_seq`. A terminal event for an unknown turn is ignored; a `user_message` already
  present by `send_id` and agent is a no-op.
- `TranscriptStore` — coordinator. Order on open, reconnect, and foreground: **subscribe, buffer
  what arrives, `load_conversation`, then apply the buffer through the reducer**. A gap in `seq`
  triggers a tail reload. Pages backward with `next_before`; an older page touches no live
  state.
- `RecipientSelection` — one-agent projects preselect that agent; otherwise the user picks and
  may pick several. Remembers the last recipient set per project and drops agents that no
  longer exist.
- `SendGate` — `LAContext` with `.deviceOwnerAuthentication` (Face ID, passcode fallback)
  before each send, with a 60-second reuse window so a burst prompts once. The approval is
  bound to the exact prompt and recipients captured before the prompt appears; edits during
  authentication cannot change what is sent. A cancelled or failed check sends nothing.

**7.5 `Views/`**

- `PairView` — scan, then the confirmation code, plus failure explanations.
- `ProjectsView` — name, folder tail, status glyph, relative time; greyed with "Fix this on
  your Mac" when the folder is unavailable.
- `TranscriptView` — agent chips, the merged reverse-paginated list, `TurnRow`, collapsible
  `ToolRow`, outcome-marker rows, `StreamingRow`. Pinned at bottom; unpinned on user upward
  scroll; re-pinned on send.
- `ComposeBar` — recipient chips, `TextEditor`, Send through `SendGate`. Disabled with a reason
  when the folder is missing or the connection is down. An agent error (sign-in, failure after
  acceptance, rejection) shows on that agent's chip with the draft preserved and "Fix on your
  Mac" where it applies; a rate-limit warning does not disable sending.
- `AgentChip` — Stop while its agent has a live turn.
- `ConnectionBanner` — "Mac offline since 14:32" when the relay supplies `last_seen`, with no
  guessed cause; **Retry**; a route to `PairView` when not paired.
- `SettingsView` — paired Mac, relay URL, **Unpair** (wipes Keychain entries).

**7.6 Tests**

- Unit: `Protocol/` against the shared fixtures, including an unknown future event and history
  with attachments; `TranscriptReducer` (buffered events at or below `through_seq` dropped and
  those above applied, a turn that started after the snapshot applied from its `turn_start`,
  duplicate `user_message`, terminal event for an unknown turn); `RecipientSelection` (zero,
  one, many agents; remembered agent deleted); `SendGate` (cancelled, failed, biometrics
  unavailable, edit during authentication).
- Coordinator: `TranscriptStore` with a stub broker — events before the load reply resolves, a
  `seq` gap triggering a reload, reconnect during a load, suspend mid-turn and miss `turn_end`,
  the Mac restarting mid-session; `send_message` timeout resolved by receipt; partial fan-out
  failure.
- UI: XCTest pair → projects → transcript → send against the in-memory transport.
- Physical device: pairing and a send on a real iPhone are part of the definition of done; a
  simulator pass is not evidence that signing or distribution works.

**7.7 Later** — a Notification Service Extension for encrypted push. Not in the first build;
no App Group or push entitlements until then.

## 8. Cross-cutting

- **Default gate.** `Cargo.toml` globs `crates/*`, so `remote-crypto`, `remote`, and `relay`
  join `make test`, `make lint`, and `make check`. That is intended: the in-process relay test
  depends on it.
- **New make targets.** `make ios-crypto` (xcframework), `make check-ios` (depends on
  `ios-crypto`; `xcodebuild test`), `make protocol-fixtures` (regenerate). CI runs `check-ios`
  as a separate job on the existing `macos-15` runner so the main gate's time is unchanged.
- **Distribution.** Bundle id `<team-prefix>.switchboard.mobile` and signing team are
  placeholders until the owner's Apple Developer Program account is set up. TestFlight internal
  testing; both developers are internal testers. Builds expire after 90 days, so a release
  cadence of at least one build per quarter keeps the app usable.
- **Docs.** `docs/system-design.md` gains Remote access (the §2 threat model, what the relay can
  and cannot see, what the Mac enforces, the window-hide and quit behaviour). README gains the
  §2 disclosure. `AGENTS.md` gains the three crates in its architecture overview (each in the
  milestone that creates it), `remote_devices.jsonl` in its filesystem layout, and a corrected
  `make check` line: once `check-ios` is its own CI job, `make check` is no longer everything
  CI runs.
- **Not in the first version.** Approvals; attachments (sending); model or effort changes;
  creating agents or projects; workflows; forwarding; saved prompts; queuing while the Mac is
  offline; more than one Mac per phone; push notifications; cancelling a queued send from the
  phone; a project list that updates without a refresh; attachment chips on a user message
  that arrived live from the other device.
- **First follow-up.** Queued sends visible and cancellable on both devices: a `send_accepted`
  event at enqueue, queued sends included in the live snapshot, recipients tracked per send,
  and a `cancel_send` request.

## 9. Milestones

Dependency-ordered. Each ends with `make check` green and names its security-review boundary.

**M1 — Crypto crate and the iOS binding.**
`crates/remote-crypto` complete (§3) with its vector and negative tests; `make ios-crypto`;
a minimal `SwitchboardMobile` target calling the binding; the CI `check-ios` job.
*Done when:* the xcframework builds on the CI runner and the Swift binding test passes there.
*Review:* all of `remote-crypto`.
Proving the build first matters because everything after depends on it.

**M2 — Relay, pairing, reconnect, revocation.**
`crates/relay` (§6) with its Dockerfile; `RelayTransport`, `PairingCoordinator`,
`DeviceRegistry`, `SessionManager` (§5.1–5.4); the Settings section and pairing modal with
confirmation (§5.9–5.10, minus keep-awake and launch at login); the iOS pairing flow and
Keychain storage (§7.3); the protocol fixtures and `make protocol-fixtures`.
*Done when:* the in-process integration test covers pair → confirm → connect → revoke mid-session,
restart either side, restart the relay, cross-connection replay, and token misuse; a real iPhone
pairs with a dev build through a locally run relay (Debug configuration, §7.2); and with the
Mac's window closed for 30 minutes the relay connection is still up and the phone still
connects. If that last check fails, the app holds an `NSProcessInfo` activity while remote
access is enabled, and the check is repeated.
*Review:* the pairing confirmation flow, `DeviceRegistry`, `SessionManager`, Swift key storage.

**M3 — Read and sync.**
The listings, `load_conversation` with snapshot-first ordering, the live-turn drop rule,
`ConversationCache` with its own key, `TranscriptWindow`, and stable cursors (§5.7);
`RemoteEmitter` with the allowlist, `seq`, and the live snapshot (§5.6); subscriptions; the iOS
projects and transcript screens, read-only, with subscribe-buffer-load-apply (§7.4–7.5); the
relay `last_seen` and the offline banner.
*Done when:* the per-harness drop-rule fixtures and the reducer and coordinator race tests pass;
and on a real iPhone, for a project the desktop has not opened, the agent list appears, opening
it mid-turn shows the running reply exactly once for a Claude agent and for a Codex agent, and
backgrounding mid-turn then returning shows the finished turn once.
*Review:* the event allowlist (what leaves the Mac).

**M4 — Send and cancel.**
`loadProjectForRemote` and the retained first-hydration promise, then `remote_load_project`
(§5.8), `SendLedger` and receipts (§5.5), `SendOrigin` through the dispatcher, journal, events,
conversation, and `types.ts`, the desktop reducer de-duplication with the required `sendId`,
`SendGate`, recipient defaults, error display, `cancel_turn`.
*Done when:* integration and frontend tests in §5.11 for sends pass; on a real iPhone, a send to
a project the desktop hasn't opened succeeds, shows the iPhone chip on the Mac live and after
relaunch, and a desktop send appears on the phone; and the same send succeeds after the Mac's
window has been closed for 30 minutes.
*Review:* `SendGate`'s binding of the approval to the request.

**M5 — Distribution and operation.**
Keep-awake through `WakeLease` with `PowerSource`; launch at login; README and system-design
documentation; the pilot relay deployment (separately approved); TestFlight internal testing for
both developers.
*Done when:* a TestFlight build installed on both developers' phones pairs with the deployed
relay and completes a send from outside the home network.
*Review:* none new; confirm earlier boundaries are unchanged.
