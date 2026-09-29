# Remote control: an iPhone app for Switchboard

**Status:** proposed · **Created:** 2026-09-29

An iPhone app that lists Switchboard's projects, shows a live transcript, and lets the user
continue work already in progress — send a message, cancel a turn. The Mac stays the only
thing that runs agents; the phone is a remote screen for it. A small relay server carries
encrypted frames between the two, so nothing on the Mac listens for inbound connections and
the same code path serves the same Wi-Fi and the other side of the world.

Transport alternatives and the reasons for the relay are in
[research/remote-transport-evaluation.md](../research/remote-transport-evaluation.md). This
document is the component inventory: what gets written, where, and against which interfaces.
The iOS app lives in this repository at `SwitchboardMobile/`, beside `crates/`, so the JSON
fixtures that keep the Rust and Swift protocol types in step are shared in one tree.

Milestones are not yet written; see the end of the document.

## 0. Decisions

| Question | Status | Effect |
|---|---|---|
| **Who runs the relay** | **Open** | *Hosted for everyone:* keep TOFU device registration, per-Mac device limits, rate limits, `/healthz`, plus a hosting-cost line. *Self-hosted per user:* drop those, add a deploy guide and a relay-URL field in pairing. §5 is written for the hosted case; self-hosted deltas are marked. |
| **Face ID before send** | **Open** | The only defence for a stolen unlocked phone (§2). Small iOS addition (`LAContext` around the send action). If required, it moves from "later" into the first build. |
| Sends while the Mac is asleep | **Refuse, don't queue** | Relay returns `mac_offline`; no store-and-forward anywhere. |
| "Needs approval" notifications | **Dropped** | Agents never ask; nothing to notify. |
| Phone's default recipient | **Last agent sent to, per project** | Desktop selection is not mirrored; it changes too often to be predictable remotely. |
| Push notifications | **Deferred** | Components listed (§4.7, §5, §6.6) but not part of the first build. |
| Connection topology | **Relay from the start** | No Tailscale phase. Transport is still behind an interface so a direct transport can exist later. |
| iOS app location | **This repository, `SwitchboardMobile/`** | Shared protocol fixtures; one PR can change both sides. |

## 1. Interfaces and their test doubles

The commitments that make the rest testable. Each row is a mock that has to exist for the
tests in §4–§6 to be writable. Everything not in this table is a plain module with pure
functions, tested directly — the repo's convention is to abstract where a second
implementation or a test needs it, not everywhere.

| Interface | Production | Test double | Where |
|---|---|---|---|
| `Transport` | `RelayTransport` (WebSocket to relay) | in-memory pair | Rust `crates/remote`, Swift `Transport/` |
| `Sealer` (Rust) / `SecureChannel` protocol (Swift) | Noise via `snow` / CryptoKit | pass-through | both |
| `RemoteBackend` | impl over `AppState` | recording mock | Rust |
| `EventEmitter` *(existing)* | `AppHandleEmitter` + `RemoteEmitter` | `RecordingEmitter` *(existing)* | Rust |
| `SecretStore` *(existing, `secret_store.rs`)* | Keychain / dev plaintext | existing | Rust |
| `DeviceRegistry` | JSONL-backed | in-memory | Rust |
| `RequestBroker` protocol | real, over `Transport` | stub returning fixtures | Swift |

```rust
pub trait Transport: Send + Sync {
    async fn send(&self, to: DeviceId, frame: Frame) -> Result<(), TransportError>;
    fn inbound(&self) -> impl Stream<Item = (DeviceId, Frame)>;
    fn status(&self) -> watch::Receiver<TransportStatus>;
}
pub trait Sealer: Send + Sync {
    fn seal(&mut self, to: DeviceId, envelope: &Envelope) -> Result<Frame, CryptoError>;
    fn open(&mut self, from: DeviceId, frame: &Frame) -> Result<Envelope, CryptoError>;
}
pub trait RemoteBackend: Send + Sync {
    async fn list_projects(&self) -> Result<Vec<ProjectListing>, BackendError>;
    async fn list_agents(&self, project: ProjectId) -> Result<Vec<AgentSummary>, BackendError>;
    async fn load_transcript(&self, agent: AgentId) -> Result<LoadedTranscript, BackendError>;
    async fn send_message(&self, agents: &[AgentId], prompt: &str, send_id: SendId) -> Result<Vec<MessageId>, BackendError>;
    async fn cancel_turn(&self, agent: AgentId) -> Result<(), BackendError>;
    async fn cancel_send(&self, send: SendId) -> Result<(), BackendError>;
    fn project_status(&self, project: ProjectId) -> ProjectStatus;
}
```

```swift
protocol Transport {
    func send(_ frame: Frame, to: DeviceID) async throws
    var inbound: AsyncStream<(DeviceID, Frame)> { get }
    var status: AsyncStream<TransportStatus> { get }
}
protocol SecureChannel {
    func seal(_ envelope: Envelope) throws -> Frame
    func open(_ frame: Frame) throws -> Envelope
}
protocol RequestBroker {
    func request<R: Decodable>(_ type: String, _ payload: Encodable) async throws -> R
}
```

## 2. Security

**What's at stake.** A paired phone can run anything on the Mac. Agents run with
`--dangerously-skip-permissions` and `--add-dir /`, so a send from the phone has the same
reach as a send from the keyboard. This feature is remote code execution by design; the
security work is making sure only the right phone can do it.

**Threat model** — what we defend against, and what we don't:

| Threat | Defended? | By |
|---|---|---|
| Relay operator reads conversations or prompts | Yes | End-to-end encryption; relay sees only `to`, `from`, sizes, timing |
| Relay operator injects a send | Yes | Frames are authenticated under the pair's session key; a forged frame fails `open()` |
| Relay operator replays an old send | Yes | Monotonic nonces per direction; replays dropped and logged |
| Relay operator impersonates a Mac to a phone (or vice versa) | Yes | Noise `XX` authenticates both static keys; the phone pins the Mac's key from the QR |
| Someone photographs the QR code | Yes | Single-use `pairing_secret` mixed in as a PSK, 5-minute expiry, consumed on first successful handshake |
| Someone registers a random device id and probes whether a Mac is online | Yes | Pair scoping: the relay forwards to a Mac only from device ids that Mac announced |
| Someone claims an existing device id on the relay | Yes | Trust-on-first-use key pinning at registration |
| Stolen unlocked phone | **No** (open — §0) | Whoever holds the phone holds the session. Mitigation is revocation from the Mac. Optional: Face ID before send. |
| Compromised Mac | **No** | Out of scope; the Mac is the trusted end |
| Relay denial of service | Partial | Rate limits and per-Mac device caps (hosted case); worst outcome is loss of remote access, not compromise |
| Traffic analysis (who talks to whom, when, how much) | **No** | Inherent to a relay; accepted |

**Where trust is enforced.** Every trust decision is made on the Mac, never delegated to the
relay:

- The Mac accepts frames only from device ids in its `DeviceRegistry` with a live session
  key. The relay's pair scoping is a second line, not the first.
- Revocation takes effect at the next frame on the Mac, regardless of whether the relay
  learned about it.
- A compromised or malicious relay can therefore interrupt service but cannot cause the Mac to
  execute anything.

**Key material**

- Mac static key and per-device session keys: `SecretStore` (Keychain in release, plaintext
  dev store in debug, same as MCP credentials). Never in `config.yaml` or the JSONL registry.
- Phone: Keychain, `kSecAttrAccessibleAfterFirstUnlock`; the notification extension shares it
  through an App Group.
- Pairing secret: memory only, on the Mac, for its 5-minute window.
- No key ever crosses the relay in plaintext; Noise `XX` sends static keys encrypted under
  ephemerals.

**Cryptography choices** — one proven scheme, no bespoke construction: Noise `XXpsk2`,
X25519, ChaCha20-Poly1305, BLAKE2s. `snow` on the Mac, CryptoKit on the phone. Test vectors
generated from `snow` and checked by the Swift side, so the two can't quietly diverge. The
`Sealer` is the only module that touches keys; everything above it handles plaintext
envelopes.

**Off by default.** Disabled until the user turns it on in Settings; nothing connects outbound
and no key is generated until then. Disabling drops the relay connection immediately.

**Disclosure.** README's harness-limitations list gains: *"A paired phone has the same reach
as the Mac. Agents run without approval prompts, so anyone with a paired phone can run commands
on your Mac. Revoke lost devices in Settings → Remote access."* `docs/system-design.md`'s
Remote access section carries the threat-model table.

**Review requirement.** `Sealer`, `PairingHandshake`, and `DeviceRegistry` get a dedicated
review pass separate from the feature review, since their bugs don't surface in normal use.
The repo's `/security-review` skill is the gate.

## 3. Shared protocol

One envelope, carried over WebSockets on every leg.

**Envelope** — `{ id, type, payload }`. `id` is a sender-minted UUID; responses and errors
echo the request's `id`. `type` is snake_case, mirroring the repo's
`#[serde(tag = "type", rename_all = "snake_case")]` convention.

**Requests (phone → Mac)**

| Type | Payload | `RemoteBackend` method |
|---|---|---|
| `list_projects` | – | `list_projects` → `ProjectListing` (id, name, directory, `directory_available`, last activity) |
| `list_agents` | `project_id` | `list_agents` → id, name, harness, model/effort, busy/idle |
| `load_transcript` | `agent_id`, `before_turn?`, `limit` | `load_transcript`, then `TranscriptWindow` slices |
| `send_message` | `agent_ids[]`, `prompt`, `send_id` | `send_message` (one turn per recipient, shared `send_id`, as the desktop compose bar does) |
| `cancel_turn` | `agent_id` | `cancel_turn` |
| `cancel_send` | `send_id` | `cancel_send` |
| `project_status` | `project_id` | `project_status` — busy/idle from dispatcher state plus running workflow runs |

**Events (Mac → phone)** — the existing `NormalizedEvent` variants wrapped with `agent_id` +
`project_id`: `turn_start`, `user_message`, `content_chunk`, `tool_started`,
`tool_completed`, `tool_facet_updated`, `turn_end`, `message_failed`, `message_cancelled`,
`agent_idle`, `session_meta`, `context_report`, `rate_limit_event`. Two new:
`project_list_changed`, `remote_send` (§4.6).

**Control (both ways)** — `hello` (`protocol_version`, `device_id`), `subscribe` /
`unsubscribe` (`project_id`), `error` (`code`, `message`), `ping` / `pong`. Error codes:
`mac_offline`, `phone_offline`, `not_paired`, `directory_missing`, `agent_busy_elsewhere`,
`harness_not_signed_in`, `protocol_mismatch`.

**Rules**

- No HTTP semantics. Errors travel in the payload.
- `mac_offline` is generated by the *relay* when no Mac session exists — this is how "refuse,
  don't queue" is enforced without the relay understanding anything else.
- The envelope is plaintext. What crosses the relay is `Frame { to, from, nonce, ciphertext }`;
  the relay reads only `to` and `from`.
- `protocol_version` mismatch yields `protocol_mismatch` with which side needs updating, not a
  decode failure.
- **Fixtures are the contract**: a Rust test writes one JSON fixture per message type; the
  Swift decoding tests consume the same files. This is the drift detector between the two
  codebases.

## 4. Switchboard (Mac) — new `crates/remote/` + wiring in `crates/app/`

`switchboard-remote` has no Tauri dependency, like `harness` and `dispatcher`.

**4.1 `RelayTransport`** — the one production `Transport`. One tokio task owning a
`tokio-tungstenite` socket; outbound only. Exponential backoff 1s→60s with jitter; reconnects
on network change and on wake (via `NSWorkspace` wake notification from the app handle).
Status: `Disabled | Connecting | Connected | RelayUnreachable(since)`. Knows nothing about
message types.

**4.2 `PairingHandshake`** — pure protocol module. Noise `XXpsk2` via `snow`, with the QR's
single-use `pairing_secret` as the PSK. Input: our static key, their ephemeral messages.
Output: per-device session key + their static public key. No I/O, no persistence; tested with
vectors.

**4.3 `DeviceRegistry`** — trait + JSONL impl over `remote_devices.jsonl` in the config dir:
`device_id`, `name`, `public_key`, `paired_at`, `last_seen`, `revoked_at?`. Session keys go
through `SecretStore`, keyed by device id. `revoke(device)` marks the row, drops the key, and
is consulted by `NoiseSealer` on the next frame.

**4.4 `NoiseSealer`** — the production `Sealer`. One `snow` transport state per paired
device; monotonic nonce per direction; a nonce ≤ last-seen is dropped and logged. The only
module that touches key material.

**4.5 Request side** — three small pieces:

- `RequestHandler` — decode envelope → call `RemoteBackend` → encode response or `error`. Pure
  over its inputs.
- `SubscriptionRegistry` — `device_id → set<project_id>`. Two methods and a lookup.
- `TranscriptWindow` — pure function `(LoadedTranscript, before_turn?, limit) → (turns,
  next_cursor)`. Lives in `crates/harness` beside the type, because the desktop's "Earlier
  messages" window wants the same slice.

**4.6 Event side**

- `RemoteEmitter` — `EventEmitter` impl wrapping `AppHandleEmitter`
  (`crates/app/src/lib.rs`): forwards to Tauri unchanged, then hands `(name, payload)` to a
  fan-out task that consults `SubscriptionRegistry`, seals, and sends. Installed at the one
  site `AppState.emitter` is built.
- `remote_send` event — emitted when a send originates from a phone, so the desktop
  transcript shows the user message immediately with a "from iPhone" origin *and* the
  desktop's send bookkeeping (notification gate, `projectIsIdle`, reading mode) sees it. Today
  that bookkeeping only knows about compose-bar sends.

**4.7 `crates/app` changes**

- `RemoteBackend` impl over `AppState` (a thin file: each method calls the existing `*_impl`).
- Tauri commands for Settings: `remote_status`, `remote_enable`, `remote_disable`,
  `remote_pairing_offer`, `remote_list_devices`, `remote_revoke_device`.
- `config.yaml`: `remote: { enabled (default false), relay_url, keep_awake_when_plugged_in
  (default false) }`, following the `auto_reading_mode` pattern including the "old config loads
  with it off" test.
- `KeepAwake` — `IOPMAssertionCreateWithName` held while enabled *and* on AC. Settings copy
  states the closed-lid limitation.
- *Later:* `PushSender` — seals a notification body for a device and hands the blob to the
  relay's APNs endpoint.

**4.8 Frontend (Svelte) — Settings → Remote access**

- `Switch` primitive for enable; status line for the four transport states.
- **Pair a device** → modal with QR (`qrcode` package → SVG), 5-minute countdown, closes on
  success.
- Paired devices list with **Revoke**.
- Transcript: "from iPhone" chip on user messages carrying a `remote_send` origin.

**4.9 Tests**

- Unit: nonce/replay rejection; pairing-secret expiry and single-use; registry read/write;
  revocation takes effect before the next frame; `TranscriptWindow` edge cases (empty, cursor
  past end, limit larger than history).
- Fixture-driven: `RequestHandler` against the mock `RemoteBackend`; `RemoteEmitter` fan-out
  with `RecordingEmitter` and a seeded `SubscriptionRegistry`; the fixture-writing test that
  produces the shared JSON contract.
- Integration: an in-process relay plus a fake phone end, over the in-memory `Transport` pair:
  pair → subscribe → send → events → revoke → refused. Plus the negative cases from §2: forged
  frame rejected, replayed frame rejected, expired pairing secret rejected, second use of a
  pairing secret rejected.

## 5. Relay server — `crates/relay/` (separate binary)

Deliberately dumb; never sees plaintext; holds no conversation state.

- **Stack**: axum + `tokio-tungstenite`; env-var config (`PORT`, `MAX_DEVICES_PER_MAC`,
  `RATE_LIMIT`); runs as a container on Railway or locally with
  `cargo run -p switchboard-relay`.
- **Registration**: `{ device_id, role: mac|phone, proof }` where `proof` signs a server nonce
  with the device's static key. *Hosted:* trust-on-first-use — the key is remembered so a later
  connection can't impersonate the id. *Self-hosted:* still worth keeping; it's one map.
- **Session table**: in-memory `device_id → sender`. Restart = everyone reconnects.
- **Forwarding**: on a `Frame` from `from`, look up `to`; forward verbatim if connected, else
  reply `error { mac_offline | phone_offline }`. Nothing queued.
- **Pair scoping**: a phone may only address Macs that have announced it via `pair_added` /
  `pair_revoked` control messages. *Self-hosted:* optional.
- **Limits** *(hosted)*: per-device rate limit, max frame size, idle timeout,
  `MAX_DEVICES_PER_MAC`.
- **Observability**: `tracing` structured logs, `/healthz`, counters for connected devices and
  forwarded frames. Message contents never logged.
- **Tests**: forward; offline error; pair-scoping refusal; rate limit; reconnect replaces
  stale session; device-id claim with wrong key refused.
- *Later:* `apns` module — accepts an already-encrypted blob from a Mac and posts it to Apple's
  push service for the target device token.

## 6. iOS app — SwiftUI, `SwitchboardMobile/`

iOS 17+, no third-party dependencies. One `@Observable` store per screen; pure reducers
underneath, mirroring the desktop's reducer-plus-component split.

**6.1 `Protocol/`** — `Envelope`, every request/response/event payload as `Codable`,
`RemoteEvent` as a `Codable` enum keyed on `type` with an `.unknown` case that renders
nothing. Transcript model: `Turn`, `ContentBlock`, `ToolRow` — the rendered subset of
`LoadedTranscript`. Hand-written; `typeshare` if drift becomes a problem.

**6.2 `Transport/`**

- `RelayTransport` — `URLSessionWebSocketTask` impl of `Transport`; backoff;
  foreground/background lifecycle.
- `NoiseChannel` — `SecureChannel` over CryptoKit (`Curve25519`, `ChaChaPoly`, HKDF); nonce
  tracking identical to the Mac's.
- `LiveRequestBroker` — `RequestBroker` impl: matches responses by `id`, 15s timeout →
  `.macUnreachable`.
- `EventBus` — publishes decoded `RemoteEvent`s to subscribed stores.

**6.3 `Pairing/`**

- `PairingScanner` — `AVCaptureSession` QR scan → `PairingOffer`.
- `PairingHandshake` — pure Noise `XXpsk2`, mirror of the Mac module; tested with the same
  vectors.
- `PairingFlow` — runs handshake over `Transport`, stores session key + Mac public key in
  Keychain, records `mac_device_id` and relay URL in `UserDefaults`. Handles: expired offer,
  wrong relay, Mac not connected.

**6.4 `Stores/`**

- `ConnectionStore` — app-wide state driving the banner: `connected | relayUnreachable |
  macOffline | notPaired`.
- `ProjectsStore` — `list_projects` + per-project `project_status`; reacts to
  `project_list_changed`; pull-to-refresh.
- `TranscriptReducer` — pure `(TranscriptState, RemoteEvent) → TranscriptState`:
  `content_chunk` appends to the streaming turn, `turn_end` finalises,
  `message_failed`/`message_cancelled` add outcome markers, `user_message` groups by `send_id`
  so a fan-out's message renders once. This is where the ordering-race tests live.
- `RecipientSelection` — the per-project last-used-agent default, persisted in
  `UserDefaults`.
- `TranscriptStore` — coordinator only: fetch agents, fetch newest 30 turns per agent, merge
  by turn start time, page backward with `before_turn`, feed events to the reducer, expose
  state.
- *If Face ID is required (§0):* `SendGate` — wraps the send action in
  `LAContext.evaluatePolicy`; a failed or cancelled check sends nothing.

**6.5 `Views/`**

- `PairView` — scan button plus the three failure explanations.
- `ProjectsView` — name, folder tail, status glyph (spinner / check / idle), relative time;
  greyed with "Fix this on your Mac" when `directory_available` is false.
- `TranscriptView` — agent chips (`Claude · Opus 5.5 · high`, `Codex · working…`),
  reverse-paginated `List`, `TurnRow` (markdown via `AttributedString`, collapsible `ToolRow`,
  outcome-marker rows), `StreamingRow`. Pinning follows the desktop's input-based rule: pinned
  at bottom, unpinned on user upward scroll, re-pinned on send.
- `ComposeBar` — recipient chips from `RecipientSelection`, `TextEditor`, Send; disabled with
  reason when the folder is missing or the connection is down; shows Queued when the recipient
  is busy.
- `AgentChip` — Stop while its agent has a live turn → `cancel_turn`.
- `ConnectionBanner` — four states; **Retry**, or a route to `PairView` for `notPaired`.
- `SettingsView` — paired Mac name, relay URL, **Unpair** (wipes Keychain entries).

**6.6 Later: `NotificationExtension/`** — Notification Service Extension that decrypts the
APNs blob with the Keychain session key and rewrites title/body before display. Needs an App
Group entitlement to share Keychain items.

**6.7 Tests**

- Unit: `Protocol/` decoding against the Rust-exported fixtures; `NoiseChannel` against
  `snow`-produced vectors, including forged and replayed frames; `TranscriptReducer` with
  events arriving before the `load_transcript` reply resolves, duplicate `user_message` for one
  `send_id`, terminal events for unknown turns.
- UI: XCTest flows pair → projects → transcript → send, with the stub `RequestBroker` and
  in-memory `Transport`.

## 7. Cross-cutting

- **Docs**: `docs/system-design.md` gains "Remote access" (the §2 threat-model table; what the
  relay can and cannot see; what the Mac enforces). README's harness-limitations list gains the
  §2 disclosure, beside the existing one about agents running without approval prompts.
- **Build**: `SwitchboardMobile/` is an Xcode project; `make check` does not build it. A
  `make check-ios` target (xcodebuild test, macOS runners only) is added alongside, and CI runs
  it in a separate job so the Rust/frontend gate stays as fast as it is.
- **Explicitly not built**: approvals; creating projects/agents from the phone; model/effort
  changes; workflows; forwarding; attachments; saved prompts; queuing while the Mac is
  offline.
- **Expected iteration hotspots**: `remote_send` integration with the desktop's send
  bookkeeping (§4.6) — that logic spans several frontend modules with documented races (see
  the reading-mode plans); and `TranscriptReducer`'s `send_id` grouping (§6.4), which must
  reproduce the desktop's journal-plus-harness-file merge closely enough that a fan-out's user
  message renders once.

## Milestones

Not yet written. The open decisions in §0 (relay hosting, Face ID) should be settled first,
because the first one changes what the relay milestone contains.
