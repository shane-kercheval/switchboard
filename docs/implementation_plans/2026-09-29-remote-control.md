# Remote control: an iPhone app for Switchboard

**Status:** proposed · **Revision:** 11 · **Created:** 2026-09-29 · **Revised:** 2026-10-07

An iPhone app that lists Switchboard's projects, shows a live transcript, and lets the user
continue work already in progress — send a message, cancel a turn. The Mac stays the only
thing that runs agents; the phone is a remote screen for it. A small relay server carries
encrypted frames between the two, so nothing on the Mac listens for inbound connections and
the same code path serves the same Wi-Fi and the other side of the world.

Transport alternatives and the reasons for the relay are in
[research/remote-transport-evaluation.md](../research/remote-transport-evaluation.md). This
document is the component inventory — what gets written, where, and against which interfaces
— followed by the milestones. The iOS app lives in this repository at `ios/`, beside the
desktop app in `desktop/` and the shared Rust in `crates/`, so the protocol fixtures and the
shared Rust cryptography are in one tree.

## Changelog

- **Revision 11 (2026-10-07)** — four M1 design decisions, approved before implementation (§2,
  §3, §5.2, §7.5).
  - Keys: one opaque `DeviceKeys` object owned by Rust holds a device's X25519 and Ed25519
    private keys and is zeroized on drop. Swift handles its storage blob only inside the
    Keychain wrapper; the Mac stores the same blob, base64-encoded, under one `KeyStore`
    entry, so the §2 table's two Mac rows become one.
  - Pairing payloads: a fixed binary layout with a version byte. Message 2 carries the Mac's
    name, which nothing delivered to the phone before; message 3 carries the phone's identity
    key, a signature over the handshake hash and the phone's Noise key proving it holds that
    identity key, and the phone's name.
  - Concurrency: `Session` and `PairingHandshake` keep their state behind a lock, `seal`
    fragments and seals a whole envelope under one acquisition, and a poisoned lock reports
    the session dead. The Swift `SecureSession` actor seals and sends in one step.
  - Libraries: `snow`, `ed25519-dalek` 3, `data-encoding`, `zeroize`; randomness from the
    operating system through `getrandom`. Signatures are domain-separated by purpose.
- **Paths (2026-10-06)** — file paths updated for the repository's move to `desktop/` and
  `ios/`: `crates/app` is now `desktop/src-tauri`, the frontend's `src/` is `desktop/src/`,
  and the iOS app is in `ios/`. No content change; the revision is unchanged, and the entries
  below keep the paths they were written with.
- **Revision 10 (2026-10-05)** — seventh review, verdict GO (one minor).
  - Finding 1: **Reset remote identity** revokes every device before it disables remote
    access. Disabling drops the relay connection, so in the old order the relay never got
    the empty set and kept answering the old phones `mac_offline` instead of `not_paired`
    (§5.9, §5.11).
- **Revision 9 (2026-10-05)** — sixth review, verdict SIMPLIFY (one major, two minor).
  - Finding 1: the relay answers `not_paired` only when it holds a set for the Mac and the
    phone is not in it. With no set held, which is every Mac's state after a relay restart
    until it re-registers, the answer is `mac_offline`. **Narrows** revision 8's "whether or
    not that Mac is connected", under which a restarted relay told every paired phone it was
    unpaired. `notPaired` on the phone is a displayed state that keeps retrying, so a wrong
    answer heals itself (§4, §6, §7.4, §5.11).
  - Finding 2: on pairing confirmation the Mac sends the updated `paired_devices` set before
    `pairing_confirmed`, so the relay knows the phone before its first handshake (§5.2).
  - Finding 3: errors the relay generates carry the `device_id` the failed frame was
    addressed to, so a Mac with several phones knows which session `phone_offline` ends (§4,
    §5.4, §6).
- **Revision 8 (2026-10-05)** — fifth review, verdict SIMPLIFY (one major, one minor).
  - Finding 1: the relay sends `peer_disconnected` exactly once per registration. A socket
    that was replaced sends nothing when it closes later, so a phone that reconnects over a
    half-open connection does not have its new session ended by the old socket's timeout.
    Revision 7's "closed, timed out, or replaced" left that open (§6, §5.11).
  - Finding 2: §5.4 no longer says both "refused" and "discarded". A record with no session
    is discarded, since it cannot be opened to find an `id` to answer. `not_paired` is the
    answer to an unregistered or revoked device, from the Mac for a handshake and from the
    relay for a frame outside the Mac's set; the phone keeps its keys on receiving it,
    because it is unauthenticated (§4, §5.4, §6, §7.4).
- **Revision 7 (2026-10-05)** — fourth review, verdict SIMPLIFY (two major, two minor).
  - Finding 1: a full device queue no longer evicts. A bounded channel cannot remove what it
    already holds, so the new event is dropped and its project marked, and the device's writer
    sends one `resync` per marked project once the queue has drained. **Replaces** revision 6's
    "cleared and replaced with one `resync`", which could not be built on the channel §1 names
    (§1, §4, §5.6, §5.11).
  - Finding 2: a session now has an end. The relay sends `peer_disconnected` to the other side
    of a pair when a connection ends, and the Mac then ends the session, clears the device's
    subscriptions, and re-evaluates the wake lease. Nothing told the Mac a phone had gone, so
    revision 6's connected-session lease would never have been released (§4, §5.4, §5.5, §5.9,
    §6).
  - Found during triage of finding 2: the same notice goes to a phone when its Mac's connection
    ends, so a phone that is only watching does not sit on a dead session after a Mac restart
    (§6, §7.2, §7.4).
  - Finding 3: `RemoteEmitter::forget_agent` releases an agent's snapshot entries when its
    actor is shut down, because the actor's shutdown paths emit no `agent_idle` (§5.6).
  - Finding 4: the M3 contingency has a number, 2 seconds without a file modification; the
    phone never opens a connection while in the background, since its keys are readable only
    while unlocked (§7.2, §9).
- **Revision 6 (2026-10-05)** — third review, verdict GO, operational findings.
  - Sizes are bounded with numbers: an 8 MiB reassembly limit, a 1 MiB byte budget per
    window alongside the item count, tool output capped at 32 KiB for the phone, and adjacent
    `content_chunk`s coalesced in the live-turn buffer, with `liveness` not buffered (§3, §5.6,
    §5.7).
  - A full device queue no longer drops single events. It is cleared and replaced with one
    `resync { project_id }` control, so a dropped tail is always followed by a reload signal.
    The phone runs at most one tail reload at a time, at least 2 seconds apart (§4, §5.6, §7.4).
  - An ended turn stays in the live snapshot until its agent's next `turn_start` or
    `agent_idle`, which the actor emits only after the post-terminal drain. Adds an M3 check
    that each harness's session file is complete by then (§5.6, §9).
  - Pairing confirmation is typed: the user enters the phone's six digits on the Mac (§0, §2,
    §5.2, §5.10, §7.3).
  - Relay: a `DENY_DEVICE_IDS` denylist for revoking a lost phone away from the Mac, a
    `relay_protocol` version on registration, WebSocket keepalive numbers, and a global
    connection cap. Deployment details are listed in M5. The end-to-end `ping` / `pong`
    controls are removed because WebSocket pings cover them (§4, §5.1, §6, §7.2).
  - Mac: an `internal` error code; `ConversationCache` keeps the 4 most recently used
    projects; the paging-across-a-key-change test is restored (it was dropped in revision 4);
    the wake lease is also held while a phone session is connected; launch at login starts
    hidden; **Reset remote identity** (§4, §5.7, §5.9, §5.11).
  - iOS: `NSCameraUsageDescription` and `NSFaceIDUsageDescription`; keys use
    `WhenUnlockedThisDeviceOnly`; all non-view code goes in a Swift package; export compliance
    is decided in M5 (§2, §7, §9).
  - The disclosure moves to its own README section (§2, §8).
  - The audience-versus-transport question is settled by the owner (2026-10-05): users beyond
    the developers are a committed goal, so the relay stands (§0).
- **Revision 5 (2026-10-05)** — revision check of revision 4, two items.
  - Check 1: the live-turn drop rule no longer rests on a raw timestamp comparison. The live
    `started_at` is stamped before the spawn, so a harness record cannot precede it in real
    time, but harness timestamps are coarser: Codex records milliseconds and Antigravity whole
    seconds. The anchor is now floored to the second, and a clock-free rule is added: drop
    what the merge attributed to the live turn's send. `Outcome` items for live turns are
    dropped too. `TurnStatus::Streaming` is not used, because only Claude's parser emits it
    (§5.6, §5.7, §5.11).
  - Check 2: the load acknowledgement waits on the existing `projectLoadChain`, with the first
    hydration joined to it, so a retry or any queued read is awaited too. This **replaces** the
    `firstHydration` map from revisions 3 and 4 — same intent, existing mechanism, no new
    state (§5.8).
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
| Transport premise | **Users beyond the developers are a committed goal (owner, 2026-10-05)** | The pilot serves the two developers, but the feature is built for a wider audience, which is what justifies the relay over Tailscale ([evaluation](../research/remote-transport-evaluation.md), option 4: too much setup for users). If that goal is dropped, redo the transport evaluation before investing further in the relay. |
| Distribution | **TestFlight, internal testing** | Paid Apple Developer Program on the owner's team; both developers are App Store Connect users on that team. Internal builds need no Beta App Review and expire after 90 days. Bundle id and team are placeholders until the account is set up (§8). |
| Who runs the relay | **The developers, for their own use, as a private pilot** | One deployment, `wss://` with TLS from the host, rate and frame-size limits from day one. If anyone else ever needs to deploy their own relay, redo the transport evaluation first — a self-deployed relay has setup friction comparable to Tailscale's. |
| Authentication before send | **Required** | Face ID with passcode fallback before each send, with a short reuse window (§7.4). Viewing and cancelling are not gated. |
| Pairing confirmation | **Required, typed** | A six-digit code derived from the handshake shows on the phone, and the user types it into the Mac. There is no one-click confirm (§2, §5.2). |
| Lost phone, away from the Mac | **Relay denylist** | The operator adds the ids to `DENY_DEVICE_IDS` on the relay. This only refuses service, so it adds no trust in the relay. Revoking on the Mac remains the real fix (§6). |
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
| iOS app location | **This repository, `ios/`** | Shared fixtures and crypto crate; one PR can change both sides. |

## 1. Interfaces and their test doubles

Each row is a seam a test needs. Everything else is a plain module with pure functions, tested
directly — abstract where a second implementation or a test needs it, not everywhere.

| Interface | Production | Test double | Where |
|---|---|---|---|
| `Transport` | `RelayTransport` (WebSocket to relay) | in-memory pair | `crates/remote`; Swift `Transport/` |
| `RemoteBackend` | impl over `AppState` | recording mock | `crates/remote` (trait), `desktop/src-tauri` (impl) |
| `EventEmitter` *(existing)* | `RemoteEmitter` decorating the existing chain | `RecordingEmitter` *(existing)* | `crates/remote`, installed in `desktop/src-tauri` |
| `KeyStore` | over the existing secret store | in-memory | `crates/remote` (two-method trait), `desktop/src-tauri` (impl) |
| `DeviceRegistry` | JSONL-backed | in-memory | `crates/remote` |
| `AgentProjectResolver` | over `AppState.agents_by_id` | map | `crates/remote` (trait), `desktop/src-tauri` (impl) |
| `PowerSource` | IOKit AC-power observer | toggled fake | `desktop/src-tauri` |
| `RequestBroker` | over `Transport` | stub returning fixtures | Swift |

**Ownership model**, matching `HarnessAdapter`: async traits use `#[async_trait]` and are held as
`Arc<dyn …>`. `EventEmitter::emit` stays synchronous; `RemoteEmitter` offers fan-out work to a
bounded channel per device with `try_send`, so a slow phone can never block dispatch (§5.6).

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
    fn delete(&self, name: &str) -> Result<(), KeyStoreError>;
}
```

`send_message` is one recipient per call; the request handler loops over recipients (one
`send_message_impl` per recipient, shared `send_id`), exactly as the desktop compose bar does.

A transport's constructor returns the `Arc<dyn Transport>` together with its inbound
`mpsc::Receiver<(DeviceId, Frame)>`: a receiver has one owner, so it is handed over once rather
than fetched through `&self`.

`crates/remote` cannot depend on `desktop/src-tauri`, so the trait speaks in protocol types defined in
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
| Someone photographs the QR code and pairs first | Yes | The confirmation code comes from the handshake hash, so the user's phone shows a code the attacker's session cannot match. The user types the phone's code into the Mac, and the registry row is written only if it equals the Mac's own code. Three wrong entries kill the token |
| An unpaired device probes whether a Mac is online, or sends it frames | Yes | The relay forwards to a Mac only from devices it announced, or from the holder of an open pairing token; the Mac independently refuses unknown devices |
| Someone claims an existing device id on the relay | Yes | Self-certifying ids: `device_id` is a hash of an Ed25519 public key, and registration signs the relay's challenge |
| A revoked phone that is still connected | Yes | Revocation drops the device's live session; its next frame fails to open and future handshakes are refused |
| Stolen unlocked phone | Partly | Device-owner authentication before each send. Viewing and cancelling are not gated. The remedy is revoking on the Mac. Away from the Mac, the relay operator can deny the phone's id (§6) |
| Compromised Mac | No | Out of scope; the Mac is the trusted end. If its keys may have leaked, **Reset remote identity** (§5.9) replaces them |
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
| Mac Noise static key (X25519) and identity key (Ed25519) | One `KeyStore` entry (Keychain in release, dev store in debug): the `DeviceKeys` storage blob, base64-encoded | Yes |
| Phone Noise static key, phone identity key | One iOS Keychain item, the same `DeviceKeys` storage blob, `kSecAttrAccessibleWhenUnlockedThisDeviceOnly` (the app has no background work until push arrives) | Yes |
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

**Disclosure.** README gains its own **Remote access** section, separate from the
harness-limitations list, opening with: *"A paired phone has the same reach as
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

- `keys` — `DeviceKeys`, an opaque object holding one device's X25519 Noise static key and
  Ed25519 identity key, zeroized on drop. Created only by `generate()` (operating-system
  randomness) or `restore(bytes)`; exposes `device_id()`, `noise_public_key()`,
  `identity_public_key()`, and `storage_bytes()`, a versioned blob — version byte, the X25519
  private key, the Ed25519 seed — from which the public keys are re-derived, the X25519 one by
  `snow`'s own resolver. `restore` rejects an unknown version byte or a wrong length with a
  typed `CryptoError`. No accessor returns a
  private key, and the handshakes take `DeviceKeys`, never key bytes. The Mac stores the blob
  base64-encoded under one `KeyStore` entry (the store holds strings). On iOS, Swift handles
  the blob only inside the Keychain wrapper and treats it as opaque; Swift's `Data` cannot be
  zeroized, so the bytes exist in Swift memory for that one call.
- `pairing` — the `XXpsk2` handshake as a state machine over bytes, the phone initiating.
  Output: the peer's Noise static key, the phone's Ed25519 identity key, both names, and the
  handshake hash. Payloads, in a fixed binary layout produced and parsed only here:
  - message 1 (phone → Mac): empty; it is not yet encrypted.
  - message 2 (Mac → phone): version byte `1`, then the Mac's name as a one-byte length and at
    most 64 bytes of UTF-8. Encrypted, because the PSK is mixed in before it.
  - message 3 (phone → Mac): version byte `1`; the phone's 32-byte Ed25519 public key; a
    64-byte Ed25519 signature over the handshake hash as it stands before message 3,
    concatenated with the phone's Noise static public key; the phone's name, encoded as in
    message 2. The phone signs after reading message 2; the Mac takes the hash after writing
    message 2 and before reading message 3, because reading message 3 changes it. The
    signature proves the phone holds the identity key it registers, and says by itself that
    this identity key vouches for this Noise key.
  - The phone refuses a Mac whose static key differs from the one in the QR code, and the
    Mac refuses a non-empty message 1. The prologue is `switchboard pairing v1`.
  - Either side rejects a wrong version, a bad signature, a short or over-long field,
    invalid UTF-8, and trailing bytes. Names are informational: control characters are
    stripped before display.
- `confirmation_code(handshake_hash) -> String` — six digits, identical on both ends of one
  handshake and different for any other.
- `session` — the `KK` handshake and the resulting transport: `seal(envelope) -> Vec<Record>`,
  `open(record) -> Option<Envelope>`. Handshake payloads are empty, and the prologue is
  `switchboard session v1`. The first record that fails to open — tampered, repeated, out of
  order, from an earlier connection, or a malformed fragment — closes the session for good;
  every later call returns `SessionClosed`, and the caller reconnects with a new handshake. An
  envelope above the size limit is refused without closing the session.
- `fragment` — splits a serialized envelope into records that each fit the Noise limit
  (65,535 bytes on the wire, so 65,519 bytes of plaintext after the tag). Each record's header —
  message id, index, count — is inside the encrypted payload, so it is authenticated. The
  reassembler accepts at most **8 MiB** per message and at most **4** incomplete messages, and
  discards partial state on disconnect. The fragmenter does not truncate. Payload size is
  bounded higher up, by the window budget and tool-output cap in §5.7.
- `identity` — Ed25519 signing and strict verification, and
  `device_id = base32(sha256(public_key))[..26]`: lower-case RFC 4648 base32 without padding,
  26 characters (130 bits), always compared as an exact string, never case-folded. Every
  signature is domain-separated by purpose — the relay challenge and the pairing binding sign
  different length-prefixed contexts — so a signature made for one never verifies for the
  other.

**Concurrency.** UniFFI objects can be called from any thread. `PairingHandshake` and
`Session` keep their state behind a `Mutex`; a call out of order, or on a finished handshake,
returns a typed error rather than panicking. `seal` fragments and seals all of an envelope's
records under one lock acquisition, so concurrent calls can never interleave records. A
poisoned lock reports the session dead through a typed error, and the caller reconnects.
Records must be transmitted in the order `seal` returns them: the Swift `SecureSession` (§7.2)
is an actor that seals and sends in one step.

**Libraries.** `snow` with its pure-Rust resolver limited to the primitives in use (X25519,
ChaChaPoly, SHA-256), so iOS links no system crypto library and no unused cipher; `ed25519-dalek` 3 with its default `zeroize` feature;
`data-encoding` for base32; `zeroize`. Key seeds come from the operating system's generator
through `getrandom`, because `ed25519-dalek` 3's `rand_core` 0.10 has no `OsRng`. Both
`ed25519-dalek` 3 and this crate use `sha2` 0.11, so the build carries one copy.

**Ordering assumption.** `snow`'s transport requires in-order delivery. One WebSocket per side
through one relay preserves order; the relay must forward a pair's frames in arrival order and
never fan them across workers.

**UniFFI binding.** `make ios-crypto` builds an xcframework for `aarch64-apple-ios` and
`aarch64-apple-ios-sim`; both targets are added to `rust-toolchain.toml`. Swift gets opaque
`DeviceKeys`, `PairingHandshake`, and `Session` objects plus the free functions. Keychain access stays in
Swift.

**Tests**

- Rust: known-answer vectors for both handshakes; negative vectors (wrong PSK, wrong static
  key, tampered handshake message, tampered transport record, record from a previous
  connection, repeated record); confirmation codes equal for one handshake and different across
  two; fragmentation round trip for a large envelope; missing, duplicate, out-of-range, and
  oversize fragments rejected; a message above 8 MiB and a fifth incomplete message rejected;
  signature verification with a wrong key fails; `DeviceKeys` round-trips through
  `storage_bytes` and `restore` rejects an unknown version and a wrong length; a device id is
  26 lower-case characters; pairing payloads with a wrong version, a bad or misbound signature,
  a short or over-long name, invalid UTF-8, or trailing bytes are rejected; concurrent `seal`
  calls never interleave records.
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
and on the reply drops every event with `seq ≤ through_seq` and applies the rest. When the
Mac has had to drop events for a device, it sends `resync { project_id }` once that device's
queue has drained (§5.6). `resync` therefore comes after every dropped event, so a lost final
event, such as a `turn_end` followed by silence, is still followed by a signal. The phone
reloads the tail on `resync`, and also on a gap in `seq`, which is what it sees first when
events keep flowing after a drop.

**Control** — `hello` (`protocol_version`), `subscribe` / `unsubscribe` (`project_id`),
`resync` (`project_id`), `error` (`code`, `message`, optional `last_seen`; an error the relay
generates also carries `device_id`, the device the failed frame was addressed to). Pairing:
`pairing_hello`, `pairing_confirmed`, `pairing_declined`. Connection liveness is WebSocket
ping/pong between each client and the relay (§5.1, §7.2), not an envelope. From the relay,
outside any session: `peer_disconnected { device_id }`, sent to the other side of a pair when
a connection ends (§6).

**Error codes** — `mac_offline` (from the relay, with `last_seen` when known), `phone_offline`,
`not_paired` (from the Mac for a handshake from an unregistered or revoked device, and from
the relay for a phone outside a set it holds, §6; unencrypted, with no `id`),
`pairing_expired`, `directory_missing`, `agent_busy_elsewhere` (the harness
session lock: `AppError::SessionInUse`), `project_locked` (another Switchboard process holds the
project: `AppError::ProjectLocked`), `project_load_timeout`, `protocol_mismatch` (says which
side to update; also returned by the relay for an unsupported `relay_protocol`, §6),
`unknown_recipient`, and `internal`, with the error's text as `message`. `internal` covers
every `AppError` without its own code, such as `ProjectUnderMaintenance`, and a reply that
would exceed the reassembly limit. A harness that is not signed in is not a request error:
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
Registers with the relay with `relay_protocol: 1`, signing its challenge with the Mac's Ed25519
key. It sends a WebSocket ping every 25 seconds and reconnects if two in a row go
unanswered, so a half-open connection shows as `Connected` for at most about 50 seconds.
After every
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
4. The phone shows the confirmation code. The Mac does **not** show its own code. It shows a
   six-digit entry field, the phone's reported name (marked as informational), and
   **Decline**.
5. When the user enters six digits equal to the Mac's code, the registry row is written, and
   the Mac sends the relay its updated `paired_devices` set and **then** `pairing_confirmed`.
   The order matters: the Mac's frames reach the relay in order over one socket, so the relay
   knows the phone before the phone can start its first handshake. A
   mismatch says so and clears the field. A third mismatch, **Decline**, expiry, or a dropped
   relay connection mid-pairing discards the candidate, kills the token, and sends
   `pairing_declined`. Three failed handshakes on one token also kill it.

**5.3 `DeviceRegistry`.** Trait plus JSONL impl over `remote_devices.jsonl` in the config dir:
`device_id`, `name`, `noise_public_key`, `identity_public_key`, `paired_at`, `revoked_at?`.
Public keys only. The file records pairing and revocation and nothing that changes per
connection. `revoke(device)` marks the row, tells `SessionManager` to drop the live session,
and sends the updated `paired_devices` set to the relay.

**5.4 `SessionManager`.** One live `Session` per connected device. On a handshake from a
registered device, runs `KK` against that device's pinned key; a new successful handshake from
the same device replaces the old session, whose further records fail to open. Records are
opened here and only plaintext envelopes leave the module. A transport record from a device
with no session cannot be opened, so it is discarded without a reply; that includes records
sent while a handshake is still in progress. A handshake from a device that is not in the
registry, or is revoked, is answered with `not_paired`. Also holds, in memory, each device's
connection state and the time of its last disconnect since the app launched; Settings reads
it from here.

A session **ends** when any of these happens: the relay sends `peer_disconnected` for the
device (§6); a frame sent to it comes back `phone_offline`, which names the device (§4); the
Mac's own relay connection
leaves `Connected`; the device is revoked; or a new handshake replaces it. Ending a session
discards its keys, records the disconnect time, clears the device's subscriptions (§5.5), and
re-evaluates the wake lease (§5.9). Discarding records without a session is safe because the
phone learns of each of these endings itself: its own connection ended, the relay sent it
`peer_disconnected` for the Mac, or the relay answers its next frame `not_paired` after a
revocation. The relay is trusted here for availability only: a false notice costs the phone
a reconnect, which a relay can already force by dropping frames.

**5.5 Request side.**

- `RequestHandler` — decode, call `RemoteBackend`, encode the response or `error`. For
  `send_message` the order is fixed: `ensure_project_loaded`, then check that every recipient
  is in the project's roster (`unknown_recipient` otherwise, with nothing dispatched), then
  dispatch one recipient at a time. Validation comes second because it needs the roster.
- `SubscriptionRegistry` — `device_id → set<project_id>`, cleared for a device when its
  session ends (§5.4). A phone subscribes again after every handshake (§7.4).
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
- for allowlisted events, assigns the project's next `seq` and offers the envelope to each
  subscribed device's queue with `try_send`. A queue is a bounded `mpsc` channel of 1,000
  envelopes, created with the device's session and drained by that device's writer task. When
  `try_send` finds it full, emit does not block: the event is dropped for that device, and its
  project is added to the device's *needs-resync* set. Whenever the writer finds the queue
  empty, it sends one `resync { project_id }` for each project in the set and clears the set.
  Emit's `try_send`-and-mark and the writer's empty-check-and-clear run under one mutex, so a
  mark cannot land after the writer's last look. A queue is empty only after everything
  accepted before the drop has gone out, so `resync` follows every dropped event, and this is
  how the phone learns of a dropped final event (§4). Events accepted between the drop and
  the drain reach the phone with a gap in `seq`, which triggers the same reload;
- caps `tool_completed.output` at 32 KiB when forwarding and buffering, the same cap §5.7
  applies to loaded history;
- tracks each live turn from `turn_start` (agent, turn id, send id, `started_at`) and buffers its
  forwarded events. Adjacent `content_chunk`s of the same `kind` are concatenated into one
  buffered event, which renders the same because the reducers append chunks. `liveness` is not
  buffered. A turn's buffer, and the turn itself, leave the snapshot at that agent's next
  `turn_start` or `agent_idle`, **not** at `turn_end`. The actor emits either one only after
  the post-terminal drain (`agent_actor` in `crates/dispatcher/src/lib.rs`), and some harnesses
  read the session file during that drain. Until then the drop rule (§5.7) keeps serving the
  turn from the snapshot rather than from a possibly incomplete file;
- answers `snapshot(project) -> LiveSnapshot { through_seq, taken_at, turns }` under one lock,
  so the snapshot and `through_seq` describe the same instant;
- `forget_agent(agent)` removes that agent's turns from the snapshot. The actor's shutdown
  paths (`TurnAfter::Shutdown`, `IdleAfter::Shutdown`) break out of `agent_actor` without
  emitting `agent_idle`, so an agent shut down after its turn ended would otherwise hold that
  turn forever. `desktop/src-tauri` gains one helper that calls `Dispatcher::shutdown_agent` and then
  `forget_agent`, and the four existing callers of `shutdown_agent` use it:
  `set_project_directory_impl`, `delete_project_impl`, `remove_agent_impl`, and
  `drain_agents_then_release_locks`.

### `desktop/src-tauri/` changes

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
     For each live turn in the snapshot, these items of its agent are dropped:
     - every `AgentTurn` the merge attributed to the live turn's send (`send_id` equal), which
       needs no clock;
     - every `AgentTurn` whose `started_at` is at or after the live turn's `started_at`
       **floored to the whole second**;
     - the `Outcome` carrying the live turn's `turn_id`, present when the turn ended during
       the read.

     And for every agent: each `AgentTurn` or `Outcome` that starts at or after the snapshot's
     `taken_at`, floored the same way, because a turn that begins during the read reaches the
     phone entirely as events above `through_seq`.

     *Why the floor.* The live `started_at` is stamped at the top of the turn, before the
     journal write and the spawn, so no record of that turn is written at an earlier instant.
     But harness timestamps are coarser than it: Codex records milliseconds (the existing
     `tail_turn_is_fresh` floors its anchor for exactly this) and Antigravity's `created_at` is
     whole seconds, so a raw `>=` keeps a record written in the anchor's own second. Flooring
     the anchor is the permissive direction. It can reach an earlier turn only if that turn
     started in the same second the next was dispatched; the cost then is that turn's disk
     content missing from one window, with its `Outcome` still shown, until the next load.

     *Why the attribution rule.* Whether Antigravity's `created_at` is stamped by the local
     client or by its server is unverified, and a skewed clock defeats any time comparison.

     *Not used:* `TurnStatus::Streaming`. Only Claude's parser emits it; Codex and Antigravity
     close an unfinished tail as `Failed`, the same as a turn that really failed. Nor is the
     rule limited to the agent's newest disk turn: a compaction mid-turn splits one dispatched
     turn into two disk turns, and both must go.
  4. `TranscriptWindow`, a pure function beside `merge_project_conversation`, cuts the window
     and `next_before`. It stops at `limit` items or once the window's serialized size passes
     **1 MiB**, whichever comes first, and always includes at least one item. Before measuring,
     every tool output longer than 32 KiB is cut to its first 32 KiB, with the suffix
     `… [truncated, N bytes total — full output on the Mac]`. The full output stays on the
     Mac, and there is no schema change. A tail load's reply adds the snapshot and its
     `through_seq`. If the encoded reply would still be over the 8 MiB reassembly limit, which
     takes one huge text block, the handler answers `internal` rather than sending something
     the phone cannot reassemble.

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
  signal is needed. It holds at most **4** projects and evicts the least recently used.
- **Cursors across a re-read.** A live turn changes the key between two pages, so the second
  page may be cut from a fresh parse. `(at, id)` is compared by value, not looked up by
  position, so a keyed item is never repeated or skipped across a re-read. Keyless turns that
  share an `at` may repeat or skip at that boundary, which is the documented limit of the
  fallback.

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
- **Frontend load, precisely.** In `desktop/src/lib/state/workspace.svelte.ts`, every conversation read
  after a project's first already runs through `chainProjectLoad`: a retry after a failed
  hydration, a staleness refresh, a fork-history read. `ensureProjectLoaded` (whose only caller
  is `activateProject`, which handles selection separately) joins the first one to it:
  `void hydrateProject(projectId)` becomes
  `void chainProjectLoad(projectId, () => hydrateProject(projectId))`. `projectLoadChain` then
  holds every read that is running or queued. A new exported `loadProjectForRemote(projectId)`
  awaits `ensureProjectLoaded(projectId)` and then awaits `projectLoadChain.get(projectId)`
  until the project has no entry. No state is added; the chain already removes its own
  entries. Awaiting a second `hydrateProject` call would not work: it is sticky through
  `hydrationStarted` and returns `"skipped"` immediately. The `remote_load_project` handler
  calls `loadProjectForRemote` and acknowledges `ok` once the chain is empty, whether those
  reads completed or failed — a failed hydration leaves the desktop showing a load error, but
  no read is in flight, so dispatch is safe. If `ensureProjectLoaded` rejects (open, lock, or
  roster failure), it acknowledges the error. A project with nothing running or queued
  acknowledges immediately.
- **Origin, end to end.** `WorkPayload::Send` drops its `emit_user_message` flag and gains
  `origin: SendOrigin` (`Keyboard | Remote | Workflow`, defined in `crates/core` beside
  `JournalRecord`). `Dispatcher::send_message` takes it (compose passes `Keyboard`, the phone
  `Remote` with `OnBusy::Enqueue`); the two awaiting wrappers, which differed only by the flag,
  become one that takes it (workflows pass `Workflow`). The actor hands it to
  `JournalSink::record_send` and then **always** emits `user_message` carrying it — the journal
  write is inside the actor, so the payload is the only way the origin can reach it.
  `JournalRecord::Send`, `NormalizedEvent::UserMessage`, and `ConversationItem::UserMessage`
  gain the field with `#[serde(default)]` producing `keyboard` (the existing `attachments`
  field is the pattern), and `desktop/src/lib/types.ts` gains it on the `user_message` event and the
  conversation item. Only `remote` renders a chip, so old lines that default to `keyboard`
  change nothing visible, and the chip survives reload because it is journaled.
  `user_message` carries no attachments, so a send with attachments shows its chips on the
  other device only after a reload; accepted for the first version.
- **Desktop sends reach the phone.** Compose sends also emit `user_message`. The desktop reducer's
  `user_message` branch becomes a no-op when a user turn with that `send_id` and agent already
  exists, since compose's optimistic turn already rendered it. The dedupe depends on the
  optimistic turn carrying its `send_id`: the production wrapper `dispatchUserTurn`
  (`desktop/src/lib/state/index.svelte.ts`) already requires it, and `appendUserTurnImpl`'s `sendId`
  parameter (`desktop/src/lib/state/reducers.ts`) changes from optional to required so no future caller
  can omit it.
- **No Mac notification** for phone sends: they are never registered with
  `sendCompletion`, which registers compose sends with their recipients before any IPC call.

**5.9 Settings, power, and lifecycle.**

- Tauri commands: `remote_status`, `remote_enable`, `remote_disable`, `remote_begin_pairing`,
  `remote_confirm_pairing`, `remote_decline_pairing`, `remote_list_devices`,
  `remote_revoke_device`, `remote_reset_identity`, `remote_project_loaded`.
  `remote_confirm_pairing` takes the typed code.
- `config.yaml` gains `remote: { enabled: false, relay_url, keep_awake_when_plugged_in: false }`,
  following `auto_reading_mode` including the "old config loads with it off" test.
- **Keep-awake** reuses the existing `WakeLease` from `wake_lock.rs`. The remote lease is held
  in either of two cases. The first is when all four of these hold: the preference is on,
  remote access is enabled, at least one device is paired, and `PowerSource` reports AC. The
  second is when any device has a live session in `SessionManager`, on any power source. A
  session ends when the phone's connection does (§5.4), and a phone disconnects shortly after
  it goes to the background (§7.2), so that second case lasts only while someone is looking
  at the app, which covers a follow-up after a turn ends.
  Dropping the lease never affects leases held by running turns or workflows. Settings copy:
  this prevents idle sleep, not lid-close sleep (except on AC with an external display), and
  the phone cannot wake a sleeping Mac.
- **Reset remote identity** — a Settings action, behind a confirmation dialog. In this order,
  it revokes every device, which ends their sessions and sends the relay an empty
  `paired_devices` set (§5.3); then disables remote access; then deletes both Mac keys
  through `KeyStore::delete`. The order matters: disabling drops the relay connection, and
  the relay keeps the old id's set while the Mac is disconnected (§6). Emptying the set
  first is what lets the relay answer the old phones `not_paired`, so they offer pairing
  again. If the relay is unreachable when the reset runs, the empty set cannot be sent, and
  those phones show "Mac offline" until they are paired again. Enabling again generates new
  keys and a new device id, so every phone must pair again.
- **Launch at login** — a Settings row backed by Tauri's autostart plugin, added with `cargo
  add` and `pnpm add`. The plugin is registered with a `--hidden` argument, and setup leaves
  the main window hidden when that argument is present, which is the closed-window state
  below. Closing the window already hides it rather than quitting
  (`handle_macos_run_event`), so remote access survives a closed window; quitting or rebooting
  ends it, and the Settings copy says so. **Unverified:** the app holds no activity assertion
  while idle (its sleep assertion exists only during turns), so macOS App Nap may throttle a
  hidden window's webview and timers — and loading a project for a phone send runs in the
  webview. M2 and M4 test this; if it fails, the app holds an `NSProcessInfo` activity while
  remote access is enabled.

**5.10 Frontend (Svelte).**

- Settings → Remote access: `Switch` to enable; connection status; launch-at-login and
  keep-awake rows; paired devices with "connected" or the last disconnect time since launch
  (from `SessionManager`), and **Revoke**; **Reset remote identity**.
- Pairing modal: QR (`qrcode` package → SVG), 5-minute countdown, then a six-digit entry field
  ("Enter the code shown on your iPhone"), **Decline**, the phone's reported name, and the
  mismatch message.
- The `remote_load_project` handler and `loadProjectForRemote` (§5.8).
- Transcript: an "iPhone" chip on user messages whose `origin` is `remote`, live and after reload.
- The `user_message` reducer de-duplication (§5.8).

**5.11 Tests**

- Unit: registry read/write; `TranscriptWindow` (fan-out yields one user row, outcome markers
  appear in their window, cursor past the end is empty, keyless-turn fallback); the live-turn
  drop rule, with one fixture per harness (Claude, Codex, Antigravity) of a session file read
  mid-turn — the window holds no disk agent turn for the live turn and keeps the earlier ones —
  plus: a disk `started_at` a few milliseconds before the live one, in the same second, is
  dropped; an Antigravity whole-second `created_at` in the anchor's second is dropped; a disk
  turn attributed to the live send but timestamped earlier (a skewed clock) is dropped; a
  completed earlier turn that is the agent's newest disk turn, while the live turn has written
  nothing, is kept; both fragments of a turn compacted mid-flight are dropped; the `Outcome`
  of a turn that ended during the read is dropped; and a turn that started after `taken_at`;
  `TranscriptWindow` stops at the 1 MiB budget but always returns one item, and caps a 1 MB
  tool output at 32 KiB with the suffix; the cache key changes on a journal append and on a
  Codex or Antigravity session-file change; the cache evicts the least recently used fifth
  project; `SendLedger` bounds; `WakeLease` toggling with the
  fake inhibitor and fake `PowerSource`, including revoking the last device while a turn holds
  its own lease, and a connected session holding the lease on battery with the preference off;
  reset identity revokes every row and sends the empty set before the connection is
  dropped, then deletes both keys.
- Fixture-driven: `RequestHandler` against the mock backend (a send calls
  `ensure_project_loaded` before validating recipients, an unknown recipient dispatches
  nothing, idempotent repeat, concurrent repeat, partial fan-out rejection, a request-level
  error is retryable); `RemoteEmitter` with `RecordingEmitter` (allowlist, subscription
  isolation, `seq` contiguous per project, a snapshot's `through_seq` equals the last `seq`
  assigned, adjacent chunks coalesced and `liveness` not buffered, an ended turn kept in the
  snapshot until the agent's next `turn_start` or `agent_idle`, `forget_agent` removing an
  ended turn that never saw `agent_idle`, a full device queue dropping the new event without
  blocking emit, the writer then sending exactly one `resync` per marked project after the
  last queued event, including when the dropped event is a `turn_end`, and an overflow on
  project B while the queue holds only project A's events yielding a `resync` for B);
  second page served from cache (count loader calls with a stub); paging across a cache-key
  change, where no keyed item is repeated or skipped; an unmapped `AppError` answers
  `internal`; `send_receipt` after a
  simulated restart reads the named project's journal; `list_agents` and `list_projects` for a
  project the desktop has not loaded; protocol fixtures.
- Integration (in-process relay, real local WebSocket): pair with confirmation → connect →
  subscribe → load → send → events → revoke mid-session → next frame refused; restart either
  endpoint and reconnect; **restart the relay** — the phone reconnects first and is answered
  `mac_offline`, not `not_paired`; then the Mac re-registers, resends `paired_devices`, and
  the phone connects without re-pairing; with two phones connected, `phone_offline` for one
  ends only that phone's session; after a reset of the Mac's identity, a previously paired
  phone is answered `not_paired`; replay a connection-1 frame into connection 2; a second
  device with the same pairing token refused; a wrong typed code writes no row and a third
  wrong code kills the token; a pending candidate's `send_message` discarded with nothing
  dispatched; a client that stops answering pings is reconnected; the phone's connection
  closes and the Mac's session, its subscriptions, and the connected-session wake lease are
  all released; the Mac's relay connection drops and the phone receives `peer_disconnected`,
  then reconnects with a new handshake; the phone reconnects while its old socket is still
  open on the relay, the old socket then closes, and the new session keeps delivering
  events; removing an agent whose turn has ended but not idled leaves no snapshot entry;
  unloaded-project send waits for the frontend acknowledgement; acknowledgement timeout;
  `project_locked`.
- Frontend: the load handler loads without changing selection; the acknowledgement is not sent
  until the first hydration has settled (hold the hydrate's `api` call open and assert no
  acknowledgement); a retry after a failed first hydration delays the acknowledgement until the
  retry settles; a read queued behind a running one is awaited too; a failed hydration still
  acknowledges `ok`; an open failure acknowledges the error; an already-loaded project
  acknowledges immediately; compose send plus its echoed `user_message` renders one row; a
  workflow send still renders one row; old journal lines without `origin` parse.

## 6. Relay server — `crates/relay/` (separate binary)

Deliberately dumb; never sees plaintext; holds no conversation state.

- **Stack**: axum + `tokio-tungstenite`. Env config: `PORT`, `RATE_LIMIT`, `MAX_FRAME_BYTES`
  (default 131,072, above one 65,535-byte Noise record plus the frame header),
  `MAX_DEVICES_PER_MAC`, `MAX_CONNECTIONS` (global, default 64, since registration is open to
  any key), `DENY_DEVICE_IDS` (comma-separated). `crates/relay/Dockerfile`; TLS terminated by
  the host so clients use `wss://`. Locally: `cargo run -p switchboard-relay`.
- **Registration**: the client sends `relay_protocol` and its Ed25519 public key. The relay
  refuses an unsupported `relay_protocol` with `protocol_mismatch`, refuses an id in
  `DENY_DEVICE_IDS`, and checks that the key hashes to the claimed `device_id` and that the
  client signs a fresh challenge. No trust-on-first-use map, so a relay restart gives an
  impostor nothing.
- **Denylist**: a listed id cannot register, and frames addressed to it are refused. Denying a
  phone cuts it off. Denying a Mac cuts off all remote access to it until the list changes.
  To find the ids, the relay logs each registration's `device_id` and each Mac's
  `paired_devices` set at info level. These are ids only, never names or contents. Changing
  the list means restarting the relay, which every client already survives.
- **Keepalive**: clients ping every 25 seconds (§5.1, §7.2). The relay's idle timeout is 90
  seconds without any frame or ping.
- **Session table**: in memory, `device_id → sender`. A new registration for an id replaces the
  stale socket. A restart makes everyone reconnect.
- **Forwarding**: stamp `from` with the sender's registered id, then forward a pair's records
  unchanged and in order. If `to` is not connected, reply
  `error { mac_offline | phone_offline }` carrying the addressed `device_id`, with `last_seen`
  for a Mac. Nothing is queued.
- **Pair scoping**: a phone may address a Mac only if it is in that Mac's current
  `paired_devices` set, or if its frame carries a token matching an open `pairing_open` from
  that Mac. The Mac sends the full set after every registration and whenever it changes, and
  the relay replaces what it held — so a relay restart loses nothing: the set arrives again
  when the Mac reconnects. The relay *holds a set* for a Mac from that Mac's first
  registration after the relay started, and keeps it while the Mac is disconnected. A frame
  with no pairing token from a phone that may not address the Mac is answered one of two
  ways. If the relay holds a set for that Mac and the phone is not in it, the answer is
  `not_paired`. If the relay holds no set, the answer is `mac_offline` without `last_seen`,
  the same as for any Mac it does not know: after a relay restart, a paired phone that
  reconnects before its Mac is told the Mac is offline, not that it is unpaired. Neither
  answer says whether the Mac is connected. A token is bound to the first device presenting
  it and cleared on expiry, first confirmation, or the Mac's disconnect.
- **Peer notices**: the relay sends `peer_disconnected { device_id }` **exactly once for each
  registration that ends**, to every connected device paired with it: to the Mac for a phone,
  and to each connected phone in the Mac's set for a Mac. A registration ends when a newer
  registration for the same id replaces it, or when its socket closes or times out while it
  is still the one registered. On a replacement the notice goes out before any frame from
  the new connection is forwarded, so the receiver ends the old session before the new
  handshake arrives. A replaced socket that closes or times out later sends nothing: each
  connection keeps the generation number its registration was given, and its close handler
  acts only if the session table still holds that generation for the id.
- **Last seen**: per Mac id, the time of its last disconnect, in memory; documented as
  observational and absent after a relay restart.
- **Limits from day one**: per-device message rate, `MAX_FRAME_BYTES`, idle timeout,
  `MAX_DEVICES_PER_MAC`, `MAX_CONNECTIONS`.
- **Observability**: `tracing` logs, `/healthz`, counters for connected devices and forwarded
  frames. Frame contents are never logged.
- **Deployment** of the shared pilot instance is a separately approved action, not part of a
  PR. M5 records the host, the domain, log retention, who is alerted when `/healthz` fails,
  and where `DENY_DEVICE_IDS` is edited.
- **Tests**: forward and ordering; a client-supplied `from` is overwritten; offline error with
  `last_seen`; wrong signature refused; impostor refused after a restart; an unsupported
  `relay_protocol` refused; a denied id refused at registration and as a destination;
  `MAX_CONNECTIONS` enforced; a silent client closed at the idle timeout; a phone outside the
  set refused; a new `paired_devices` set replaces the old one; a closed phone connection
  notifies its Mac and a closed Mac connection notifies its connected phones; a replacement's
  notice precedes the new connection's first frame; a phone is replaced and its old socket
  then closes, and the Mac receives exactly one notice; an unpaired device gets no notice; a
  phone outside a held set is answered `not_paired` whether that Mac is connected or not;
  after a restart, a phone addressing a Mac that has not re-registered is answered
  `mac_offline`; relay errors carry the addressed `device_id`; token
  forwarding, expiry, and
  second-device refusal; rate and frame limits; reconnect replaces the stale session; the
  container builds and answers `/healthz` locally.

## 7. iOS app — SwiftUI, `ios/`

iOS 17+. No third-party Swift packages; the only non-Apple code is the Rust xcframework from
§3. One `@Observable` store per screen over pure reducers, mirroring the desktop's
reducer-plus-component split.

**Layout.** `ios/` holds a hand-created Xcode project, `SwitchboardMobile.xcodeproj`, with one
thin app target in `ios/SwitchboardMobile/` (the `@main` entry, `Views/`, `Info.plist`,
entitlements), plus a local Swift package,
`SwitchboardMobileKit`, holding `Protocol/`, `Transport/`, `Pairing/` (except the camera
view), and `Stores/`, and depending on the §3 xcframework as a `binaryTarget`. Most code
changes then touch the package, not the `.xcodeproj`. The package's tests still run through
`xcodebuild test` on an iOS simulator destination, because the xcframework has no macOS
slice.

**Info.plist.** Both configurations carry `NSCameraUsageDescription` (QR scan, from M2) and
`NSFaceIDUsageDescription` (`SendGate`, from M4); iOS terminates the app on first use
without them. Debug alone adds the local-relay keys in §7.2.

**7.1 `Protocol/`** — `Codable` mirrors of §4, `RemoteEvent` with an `.unknown` case that
renders nothing. `ConversationItem`, `ContentBlock`, `ToolRow` model the merged conversation the
Mac sends. History containing attachments must decode and render (as a placeholder chip) even
though the phone cannot send attachments.

**7.2 `Transport/`**

- `RelayTransport` — `URLSessionWebSocketTask`; registration sends `relay_protocol: 1` and
  signs the relay challenge through the binding; `sendPing` every 25 seconds, reconnecting
  after two unanswered; backoff; disconnects after a grace period in the background,
  reconnects on foreground. It never opens a connection while the app is in the background:
  one that drops during the grace period stays down until the app returns, because the keys
  the handshake needs are readable only while the phone is unlocked (§2). On
  `peer_disconnected` for the paired Mac it discards the session and retries the handshake
  with the same backoff.
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
  code with "Type this code on your Mac", waits for `pairing_confirmed` / `pairing_declined`, then
  stores the phone's keys and the Mac's pinned keys in the Keychain. Handles expired offers,
  decline, wrong relay, and Mac not connected.
- One Mac per phone: pairing a new Mac replaces the old one after an explicit confirmation.

**7.4 `Stores/`**

- `ConnectionStore` — `connected | relayUnreachable | macOffline(lastSeen?) | notPaired`.
  `peer_disconnected` for the paired Mac moves it to `macOffline`; the next handshake the Mac
  answers restores `connected`, and the open transcript reloads as on any reconnect.
  `not_paired` moves it to `notPaired` and offers pairing again. That is a displayed state,
  not a stored one. The keys are kept until a new pairing replaces them, because `not_paired`
  is not authenticated and a relay must not be able to make the phone forget its Mac. And
  the transport keeps retrying the handshake with its backoff, exactly as in `macOffline`,
  so a handshake the Mac answers restores `connected` without the user doing anything.
- `ProjectsStore` — `list_projects`, which carries each project's status; archived hidden by
  default with a toggle. Refreshes on open, on return to the foreground, and on pull-to-refresh.
- `TranscriptReducer` — pure. A tail load **replaces** the project's live state with the reply's
  window and snapshot; events are then applied only when their `seq` is above the reply's
  `through_seq`. A terminal event for an unknown turn is ignored; a `user_message` already
  present by `send_id` and agent is a no-op.
- `TranscriptStore` — coordinator. Order on open, reconnect, and foreground: **subscribe, buffer
  what arrives, `load_conversation`, then apply the buffer through the reducer**. A `resync`
  or a gap in `seq` triggers a tail reload. At most one tail reload runs at a time. A trigger
  during a reload marks one more reload as pending; further triggers do not stack. Reloads
  start at least 2 seconds apart, so a phone that keeps falling behind settles into
  periodic snapshots instead of a reload loop. Pages backward with `next_before`; an older page touches no live
  state.
- `RecipientSelection` — one-agent projects preselect that agent; otherwise the user picks and
  may pick several. Remembers the last recipient set per project and drops agents that no
  longer exist.
- `SendGate` — `LAContext` with `.deviceOwnerAuthentication` (Face ID, passcode fallback)
  before each send, with a 60-second reuse window so a burst prompts once. The approval is
  bound to the exact prompt and recipients captured before the prompt appears; edits during
  authentication cannot change what is sent. A cancelled or failed check sends nothing.

**7.5 `Views/`**

- `PairView` — scan, then the confirmation code to type on the Mac, plus failure explanations.
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
- `SettingsView` — the paired Mac's name (from pairing message 2), relay URL, **Unpair** (wipes Keychain entries).

**7.6 Tests**

- Unit: `Protocol/` against the shared fixtures, including an unknown future event and history
  with attachments; `TranscriptReducer` (buffered events at or below `through_seq` dropped and
  those above applied, a turn that started after the snapshot applied from its `turn_start`,
  duplicate `user_message`, terminal event for an unknown turn); `RecipientSelection` (zero,
  one, many agents; remembered agent deleted); `SendGate` (cancelled, failed, biometrics
  unavailable, edit during authentication); a test that both configurations' Info.plists
  carry the camera and Face ID usage descriptions.
- Coordinator: `TranscriptStore` with a stub broker — events before the load reply resolves, a
  `seq` gap triggering a reload, a `resync` with no later event triggering a reload, repeated
  triggers during a reload yielding exactly one more reload no sooner than 2 seconds later,
  reconnect during a load, suspend mid-turn and miss `turn_end`, the Mac restarting
  mid-session, signalled by `peer_disconnected` with no request in flight; a connection that
  drops in the background is not reopened until foreground; `not_paired` keeps the keys and
  keeps retrying, and a later answered handshake restores `connected`; `send_message` timeout
  resolved by receipt; partial fan-out failure.
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
  placeholders until the owner's Apple Developer Program account is set up. Developers sign
  their own device builds through a gitignored `Local.xcconfig`; the distribution identity is
  committed separately, in M5's distribution build configuration. TestFlight internal
  testing; both developers are internal testers. Builds expire after 90 days, so a release
  cadence of at least one build per quarter keeps the app usable.
- **Docs.** `docs/system-design.md` gains Remote access (the §2 threat model, what the relay can
  and cannot see, what the Mac enforces, the window-hide and quit behaviour). README gains a
  Remote access section opening with the §2 disclosure. `AGENTS.md` gains the three crates in
  its architecture overview (each in the milestone that creates it), `remote_devices.jsonl`
  in its filesystem layout, and a corrected `make check` line: once `check-ios` is its own CI
  job, `make check` is no longer everything CI runs.
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
the `SwitchboardMobile` project and `SwitchboardMobileKit` package laid out as in §7, with a
minimal app target calling the binding; the CI `check-ios` job.
*Done when:* the xcframework builds on the CI runner and the Swift binding test passes there.
*Review:* all of `remote-crypto`.
Proving the build first matters because everything after depends on it.

**M2 — Relay, pairing, reconnect, revocation.**
`crates/relay` (§6) with its Dockerfile; `RelayTransport`, `PairingCoordinator`,
`DeviceRegistry`, `SessionManager` (§5.1–5.4) with keepalive and session end on
`peer_disconnected`; the Settings section, pairing
modal with typed confirmation, and **Reset remote identity** (§5.9–5.10, minus keep-awake and
launch at login); the iOS pairing flow, Keychain storage, and `NSCameraUsageDescription`
(§7, §7.3); the protocol fixtures and `make protocol-fixtures`.
*Done when:* the in-process integration test covers pair → typed confirm → connect → revoke
mid-session, a wrong typed code, restart either side, restart the relay with the phone
reconnecting before the Mac, a client that stops
answering pings, a phone disconnect ending its session on the Mac, a replaced socket's late
close leaving the new session intact, cross-connection replay, and token misuse; a real iPhone
pairs with a dev build through a locally run relay (Debug configuration, §7.2); and with the
Mac's window closed for 30 minutes the relay connection is still up and the phone still
connects. If that last check fails, the app holds an `NSProcessInfo` activity while remote
access is enabled, and the check is repeated.
*Review:* the pairing confirmation flow, `DeviceRegistry`, `SessionManager`, Swift key storage.

**M3 — Read and sync.**
First, an investigation step: for each harness (Claude, Codex, Antigravity), establish
whether the session file holds the complete turn by the time the actor emits the next
`turn_start` or `agent_idle`. This is when §5.6 releases an ended turn from the snapshot. The
code shows Claude reads no session file after the terminal and Codex finishes its enrichment
read before `TurnEnd`; Antigravity is unchecked. Record the per-harness answer in
`docs/harness-behavior.md`. Where the file can lag, hold that harness's ended turns in the
snapshot until its session file has gone **2 seconds** without a modification, and add a
fixture for it; the write-up states where that check runs before it is built. Then:
the listings, `load_conversation` with snapshot-first ordering, the live-turn drop rule,
`ConversationCache` with its own key and LRU bound, `TranscriptWindow` with its byte budget
and tool-output cap, and stable cursors (§5.7); `RemoteEmitter` with the allowlist, `seq`,
the coalesced live snapshot, `resync` on overflow, and `forget_agent` (§5.6); subscriptions,
cleared when a session ends (§5.5); the iOS projects
and transcript screens, read-only, with subscribe-buffer-load-apply and reload pacing
(§7.4–7.5); the relay `last_seen` and the offline banner.
*Done when:* the investigation result is recorded; the per-harness drop-rule fixtures, the
paging-across-a-key-change test, and the reducer and coordinator race tests pass;
and on a real iPhone, for a project the desktop has not opened, the agent list appears, opening
it mid-turn shows the running reply exactly once for a Claude agent, a Codex agent, and an
Antigravity agent, and backgrounding mid-turn then returning shows the finished turn once.
*Review:* the event allowlist (what leaves the Mac).

**M4 — Send and cancel.**
`loadProjectForRemote` over the project load chain, then `remote_load_project`
(§5.8), `SendLedger` and receipts (§5.5), `SendOrigin` through the dispatcher, journal, events,
conversation, and `types.ts`, the desktop reducer de-duplication with the required `sendId`,
`SendGate` with `NSFaceIDUsageDescription`, recipient defaults, error display, `cancel_turn`.
*Done when:* integration and frontend tests in §5.11 for sends pass; on a real iPhone, a send to
a project the desktop hasn't opened succeeds, shows the iPhone chip on the Mac live and after
relaunch, and a desktop send appears on the phone; and the same send succeeds after the Mac's
window has been closed for 30 minutes.
*Review:* `SendGate`'s binding of the approval to the request.

**M5 — Distribution and operation.**
Keep-awake through `WakeLease` with `PowerSource` and the connected-session condition, which
rests on M2's session end; launch
at login, starting hidden; README and system-design documentation; the pilot relay deployment
(separately approved), with its host, domain, log retention, `/healthz` alert recipient, and
denylist procedure written down; the App Store Connect export-compliance answer
(`ITSAppUsesNonExemptEncryption`), decided and recorded. The app implements its own
end-to-end encryption, so the "exempt encryption only" answer cannot be assumed. TestFlight
internal testing for both developers. A committed distribution identity: a dedicated
distribution build configuration carrying the owner's team id and the App Store bundle id,
which does not include a developer's `Local.xcconfig`, so a TestFlight archive never depends
on one developer's local settings. Only ids are committed, never a certificate or key.
*Done when:* a TestFlight build installed on both developers' phones pairs with the deployed
relay and completes a send from outside the home network; adding a test phone's id to
`DENY_DEVICE_IDS` on the deployed relay cuts it off; and an archive from a clean checkout with
no `Local.xcconfig` carries the committed team and bundle id.
*Review:* none new; confirm earlier boundaries are unchanged.
