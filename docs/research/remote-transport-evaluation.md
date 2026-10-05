# Research: Remote transport evaluation for Switchboard

**Captured:** 2026-09-29
**Decision:** Custom relay server with end-to-end encryption, QR-code pairing, and outbound-only connections from the Mac.
**Affects system-design sections:** a future "Remote access" section; §10 (Form factor and distribution).
**Consumed by:** [implementation_plans/2026-09-29-remote-control.md](../implementation_plans/2026-09-29-remote-control.md).

## Question

An iPhone app should be able to list Switchboard's projects, watch a live transcript, and
continue work already in progress — send a message, cancel a turn. The Mac stays the only
thing that runs agents. How does the phone reach the Mac, both on the same Wi-Fi and away from
home?

Constraints that shaped the answer:

- Every agent runs with `--dangerously-skip-permissions` and `--add-dir /`, so whatever can
  reach the Mac can run anything on it. The transport's trust model matters more than its
  throughput.
- The Mac is often asleep or behind NAT. Nothing should require an open inbound port.
- Setup must be one step per device for a user who is not the developer.
- Payloads are small: JSON envelopes and streamed text. No video, no bulk file transfer.

## Options considered

| # | Option | Verdict | Why |
|---|---|---|---|
| 1 | **Custom relay server** (a small WebSocket forwarder, e.g. on Railway). Mac and phone both connect *out* to it. | **Chosen** | One code path for LAN and remote. No open ports. The same shape as Claude Code's Remote Control. The relay is small enough to embed in an integration test. Cost: a server to run, and the encryption is ours to get right. |
| 2 | **MQTT with a hosted broker** (HiveMQ, EMQX) | Runner-up | No server code to write. But it adds a third-party account per user or a shared broker we still have to secure; topic ACLs stand in for pairing but map poorly to "this phone may talk to this Mac only." The relay's forwarding rule is a few dozen lines; the broker doesn't save enough to justify the dependency. |
| 3 | **Peer-to-peer with relay fallback** (iroh) | Rejected | Direct connections only pay off for heavy data. For small JSON, the fallback path is the common path anyway, so we'd carry the P2P complexity for no gain. |
| 4 | **Tailscale / WireGuard** | Rejected for users; viable for the developer | Everything needed is in the free Personal plan (MagicDNS, HTTPS certificates, Serve), Tailscale has an iOS app, and Switchboard would never touch Tailscale's API — it would just listen on localhost. But every user must install and sign in to Tailscale on both devices. Too much friction for a feature aimed beyond the developer. |
| 5 | **XMPP** | Rejected | Built for person-to-person chat (accounts, contacts, presence). Rust and iOS library support is weak. More work than a relay and a worse fit. |
| 6 | **Direct connection on the same Wi-Fi only** | Rejected | Doesn't work away from home, which is the point. |

## Why not start with Tailscale and switch later

Considered and rejected. The switch would have been cheap *if* the Tailscale version was
built with a message-oriented protocol over one WebSocket, its own pairing, and the connection
code isolated — but those are the same rules the relay needs, and the relay's additional pieces
(the server, the outbound connection, encryption, pairing) are small-to-medium and well
defined. Building the relay first avoids redoing the connection and pairing code and gives one
setup flow from day one.

## Properties the decision commits us to

- **Outbound-only from the Mac**, automatic reconnect, no listening socket.
- **End-to-end encryption**: the relay forwards frames it cannot read. A fresh
  `Noise_KK_25519_ChaChaPoly_SHA256` handshake on every connection, with both static keys
  pinned at pairing; no session key or nonce counter is ever persisted. One Rust
  implementation (`snow`), shared with the iOS app through UniFFI — CryptoKit has neither
  Noise nor BLAKE2s, so a Swift-only implementation would mean hand-writing Noise. SHA-256
  rather than BLAKE2s keeps a pure-CryptoKit fallback possible.
- **QR-code pairing** (`Noise_XXpsk2_25519_ChaChaPoly_SHA256`) with a single-use,
  short-lived secret as the PSK, plus a confirmation code derived from the handshake and
  compared on both screens. The code is what defends a photographed QR: without it, pairing
  is a race won by whoever scans first.
- **Self-certifying relay identities**: a device id is the hash of an Ed25519 key, and the
  device proves possession at registration. X25519 keys cannot sign, so the Noise key is not
  reused for this.
- **Trust enforced on the Mac**, never delegated to the relay: the Mac completes handshakes
  only with keys in its own registry, and revocation drops a live session immediately,
  regardless of what the relay knows.
- **Refuse, don't queue**: a send while the Mac is offline gets an immediate `mac_offline`
  error. Nothing is stored for later delivery.
- **The Mac is the source of truth** for history; the phone fetches the tail of the
  transcript on connect and pages backward.
- **The Mac is offline while asleep**; reconnect on wake, with an optional
  keep-awake-while-plugged-in setting (which cannot override a closed lid).

## Settled after review (2026-10-03)

- **Who runs the relay** — the project's developers, for their own use, as a private pilot.
  This keeps the decision's premise intact: the relay beats Tailscale on setup only when the
  end user doesn't deploy it. If others ever need to run their own relay, redo this
  evaluation first.
- **Push notifications** stay deferred; they need the paid Apple Developer Program (which the
  TestFlight distribution brings anyway) and an APNs endpoint on the relay. The transport
  decision doesn't change either way.

## Settled after review (2026-10-05)

- **Audience** — the pilot serves the two developers, but users beyond them are a committed
  goal. That is the premise behind rejecting Tailscale (option 4), so the relay stands. If
  the goal is dropped, redo this evaluation.
