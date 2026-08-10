# OAuth live verification against tiddly prod (M5)

Verification notes for the OAuth MCP-provider integration
(`docs/implementation_plans/2026-08-08-mcp-oauth.md`, M5) against
`https://prompts-mcp.tiddly.me/mcp`. In the spirit of tiddly's
connector-verification notes: observations, not design — where behaviour
differed from the plan's assumptions, the code was corrected and the
difference is recorded here.

## Programmatic checks (2026-08-09, no credentials involved)

**Protected-resource metadata** (`/.well-known/oauth-protected-resource/mcp`):
`resource` is exactly the canonical URL (`https://prompts-mcp.tiddly.me/mcp`),
`authorization_servers: ["https://clerk.tiddly.me"]`,
`scopes_supported: ["openid", "offline_access"]`. An unauthenticated `GET /mcp`
returns 401 with `WWW-Authenticate: Bearer resource_metadata="…"` — a pointer
but no `scope` challenge, so Switchboard's scope resolution falls through to
the PRM's `scopes_supported`, normalizing to `offline_access openid` (matching
what Claude Code and the Claude connector registered per tiddly's notes).

**Authorization-server metadata** (`clerk.tiddly.me/.well-known/oauth-authorization-server`):
every preflight gate passes on the live document — `issuer` equals the AS
identifier byte-for-byte, all three endpoints are HTTPS on the issuer's
origin, `registration_endpoint` present, `code_challenge_methods_supported:
["S256"]`, `response_types_supported: ["code"]`,
`grant_types_supported: ["authorization_code", "refresh_token"]`,
`token_endpoint_auth_methods_supported` includes `none` (public client).

**Port-less loopback DCR — the plan's open unknown, settled: Clerk accepts
it.** A `POST /oauth/register` with the exact registration shape rmcp sends
(`redirect_uris: ["http://127.0.0.1/callback"]`, `token_endpoint_auth_method:
"none"`, `grant_types: ["authorization_code", "refresh_token"]`,
`scope: "offline_access openid"`) returned **201** with the port-less redirect
echoed verbatim. The plan's fallback (register the concrete port, re-register
when the bind differs) is not needed. The probe created orphan client
`V2NvZaq1FxqhQsf1` (client name "Switchboard (M5 port-less DCR probe)") —
delete it from Clerk when convenient.

**One watch item from the probe:** the DCR *response* echoed
`grant_types: ["authorization_code"]` only, despite `refresh_token` being
requested and advertised in `grant_types_supported`. Whether Clerk still
issues (and honours) refresh tokens for such a registration — i.e. whether
that field is a display artifact or a real grant restriction — is settled by
the live refresh check below.

## Live end-to-end run (manual — browser + human required)

- **Sign-in from Settings (fresh add, OAuth mode): works end to end.** The
  previous bearer-mode provider was removed (its keychain entry deleted), the
  OAuth provider added, and the browser flow completed on the first attempt.
- **Envelope after sign-in — as designed, and it settles the DCR watch item.**
  Registration: port-less `http://127.0.0.1/callback` accepted through the
  *real* flow (client `rbhCnKGcVUNj0ZV0`), fingerprint recorded as
  `scopes: ["offline_access", "openid"]` + `issuer: https://clerk.tiddly.me`.
  Tokens: access token (`expires_in: 86399` ≈ 24 h) **and a refresh token** —
  so the DCR response echoing only `authorization_code` in `grant_types` is a
  Clerk display artifact, not a grant restriction; `offline_access` governs
  refresh-token issuance as expected.
- Two UI defects found during the run (fixed in the working tree, see git):
  the Add-server button wrapped when the OAuth hint shared its flex row, and
  row-level Test results were being cleared by the next `prompts:synced`
  event (the transient-notice cleanup was too broad — probe outcomes are not
  transient).
- **Prompts listed and rendered into a real send: works** (tiddly prompts
  appeared in the compose picker and rendered into an agent send).
- **Restart persistence: works** — after quit + relaunch the provider listed
  without re-auth.
- **Forced refresh: verified live, twice.** `token_received_at` was rewound in
  the dev file store to force expiry; a Settings sync then triggered a real
  refresh against Clerk. The refresh token **rotated** and the rotation
  survived the envelope's read-modify-write (log: rmcp "Access token expired…
  refreshing" → "Refreshed access token"; envelope: new refresh token, fresh
  `expires_in`, unchanged client id). Repeated after a restart: the **rotated**
  token refreshed again from a cold start — the plan's full bar. This also
  confirms rmcp's missing RFC 8707 `resource` on refresh is inert against
  Clerk, and that the app's log filter passes rmcp's auth INFO lines while
  suppressing its code-leaking DEBUG line.
- **Sign-out → sign-in reuse: verified.** Sign-out kept the registration and
  cleared tokens; the re-sign-in reused client `rbhCnKGcVUNj0ZV0` (zero
  registration requests in the log) and minted a fresh grant. Clerk-side
  single-application check performed manually in the dashboard.

## ChatGPT-connector hypothesis

Already run and recorded in the tiddly repo (connector-verification notes,
2026-08-09 addendum): ChatGPT connects without the manual scope patch, but the
fix is OpenAI-side, not the metadata. Not repeated here.

## Point-of-use auto sign-in (added and verified after the initial run)

During verification the re-sign-in flow grew into a feature: a prompt render
that determines needs-sign-in now crosses IPC as a typed outcome, and the
composer **launches the browser sign-in itself** — for both send and preview —
continuing the operation once the user approves (send dispatches; preview
reruns). The Settings warning before re-sign-in was removed as part of this
(see the plan's M4 supersession note): an abandoned flow is now self-healing
at the next point of use.

Live-verified against prod, both paths — with two instructive failures on the
way. The first send test and the first preview test each completed the
sign-in (fresh tokens observed on disk at approval time, reused client id)
but then timed out on the post-sign-in retry; parallel probes showed the
server stalling in those exact windows (see the server-side finding below),
so the failures were the server's, not the flow's. A final run during a
verified-healthy window completed both paths end to end: preview
auto-launched the browser, exchanged, and rendered itself; send completed
without re-auth. Timeout semantics confirmed: the browser wait is not counted
against the 10 s render budget — each render attempt gets its own. The
failures also produced a message fix: a post-sign-in retry failure now says
"Signed in, but the preview/send then failed: …" so the user knows their
browser approval stuck.

## Server-side incident during the run (resolved: Railway platform)

During the run, `prompts-mcp` and `content-mcp` (Railway) intermittently
stalled 10–30 s or 502'd at Railway's edge with nothing in the app logs and
no process restarts — including a 9-minute window on `prompts-mcp` that ate
the first auto-sign-in test's post-sign-in render. **Root cause: a Railway
platform connectivity incident**, confirmed by status.railway.com reporting
(and patching) connectivity issues covering the affected window; the
symptoms — edge 502s never reaching the app, unauthenticated requests
stalling equally, both services affected independently, clean app logs, no
restarts, spontaneous recovery — all match a platform-network fault, and the
final verification run after the patch was fully healthy.

One latent code observation survives the reattribution, for the bookmarks
repo's backlog rather than as tonight's culprit: the shared auth path calls
`PyJWKClient.get_signing_key_from_jwt` — a synchronous network fetch (1 h
cache, default timeout) — directly on the async event loop. A slow JWKS
refetch would block the entire server for its duration. It did not cause this
incident, but it is a real single-point stall worth hardening (thread-pool
the JWT decode, set a short explicit JWKS timeout). Switchboard's 10 s budget
and generic-timeout classification behaved correctly throughout the incident.
