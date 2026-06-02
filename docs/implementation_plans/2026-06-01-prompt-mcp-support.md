# Implementation Plan: MCP Prompt Support (local + remote) with first-class Tiddly integration

## 1. Background and goal

Switchboard should let a user invoke **prompts** — reusable, optionally parameterized text templates — from the compose box, regardless of which harness (Claude Code, Codex, Gemini, Antigravity) the recipient agent runs on. Prompts come from two kinds of **provider**:

- **Local file store** — markdown files with YAML frontmatter, in user-configured directories. Rendered by Switchboard.
- **MCP-server provider** — any MCP server that exposes prompts via the standard `prompts/list` / `prompts/get` RPCs. The server renders; Switchboard receives finished text.

[Tiddly](https://tiddly.me) is the canonical MCP provider and the development reference. It is integrated as a **first-class preset**: the user clicks "Connect Tiddly," logs into their Tiddly account in a browser, and Switchboard handles everything else.

The user selects a prompt via a **slash command** in the compose box. The prompt renders as an **inline chip within the message text** (not a block above it, unlike recipient chips). Prompts with arguments open a small dialog to collect values, with a preview affordance. At send time, Switchboard resolves the chip to rendered text and the agent receives only that text — never the template, provider, or arguments.

This plan implements the design specified in **`docs/system-design.md` §6 ("Prompts and prompt providers")**; read that section first. The design is directional, not gospel — this plan refines three points it did not fully settle (all confirmed with the engineer and recorded in §4): prompt config is **user-global** (the doc's directory-scoping was dropped and §6 amended in place), secrets live in the **OS keychain** (not `config.yaml`), and the Tiddly preset is a **browser login** (not a pasted token).

### Why this matters across harnesses

Codex and Gemini have limited or absent native MCP-prompt support. Because Switchboard resolves prompts itself and sends rendered text as a plain user message, every harness gets a uniform, Claude-Code-style prompt library with no per-harness configuration. This is a core value proposition, not a nicety.

## 2. Key facts the implementer needs (verified during research)

### 2.1 MCP prompts protocol (spec 2025-06-18)

- `prompts/list` → `{ prompts: [{ name, title?, description?, arguments: [{ name, description?, required? }] }], nextCursor? }`. Paginated via opaque `cursor`. `required` defaults `false`. **All arguments are strings; the protocol has no type system and no default-value field.**
- `prompts/get` with `{ name, arguments: { k: v } }` → `{ description?, messages: [{ role, content: { type:"text", text } }] }`. The client receives **already-rendered text**.
- Capability `prompts.listChanged: true` ⇒ server emits `notifications/prompts/list_changed` when its prompt set changes.
- Errors are JSON-RPC: `-32602` invalid params (bad name / missing required arg), `-32603` internal.
- Spec: https://modelcontextprotocol.io/specification/2025-06-18/server/prompts

### 2.2 Tiddly's prompt MCP server (the reference target)

- **Transport: Streamable HTTP.** Prod endpoint `https://prompt-mcp.tiddly.me/mcp`; local dev `http://localhost:8002/mcp`. (The notes/bookmarks server `content-mcp.tiddly.me` / `:8001` is **out of scope** — we use the prompts server only.)
- **Auth: `Authorization: Bearer <token>`** on every request. Production validates a Tiddly PAT (`bm_` prefix); **dev mode accepts any non-empty string** (this is what makes the local dev server a cheap integration target). The bearer token IS the account scoping — nothing else to configure.
- Implements `prompts/list` (paginated, 100/page) and `prompts/get` per spec, **plus** prompt-management *tools* (`get_prompt_content`, `search_prompts`, …). Switchboard uses **only the prompts capability**; the tools are ignored. Templates are Jinja2; the server renders and returns one `user` text message. Missing required arg → `InvalidParams`.
- **No live list updates.** The server advertises `prompts.listChanged: false`, never sends `notifications/prompts/list_changed`, and runs stateless (JSON responses, no held-open SSE stream) — so it has no mechanism to push prompt-set changes. (`listChanged` has been in MCP since the first spec, `2024-11-05`; it is not a feature Tiddly is missing for being out of date — most simple servers leave it off. Receiving it at all would require the client to hold an SSE stream open via an HTTP GET, which a stateless/JSON server like Tiddly does not offer.) Practical consequence: a user who edits a prompt in Tiddly must **Sync** (or restart) to see it — the concrete reason v1 uses build-once + Sync rather than a subscription.

### 2.3 Tiddly login + PAT minting (for the "Connect Tiddly" preset)

Talks to the Tiddly **API host** (`https://api.tiddly.me`, dev `http://localhost:8000`), distinct from the MCP host.

1. **Auth0 device-code login.** `POST https://tiddly.us.auth0.com/oauth/device/code` with `client_id=Gpv1ZrySgEeoTHlPyq3vSqHdFkS1vPwI`, `scope=openid profile email offline_access`, `audience=tiddly-api`. Returns `{ device_code, user_code, verification_uri, verification_uri_complete?, expires_in (~600), interval (~5) }`. Show the user the code + URL and open `verification_uri_complete` in their browser.
2. **Poll** `POST https://tiddly.us.auth0.com/oauth/token` with `grant_type=urn:ietf:params:oauth:grant-type:device_code`, `device_code`, `client_id`. Handle `authorization_pending` (keep polling), `slow_down` (increase interval by 5s, per RFC 8628), `expired_token` / `access_denied` (fail). Success → `{ access_token (JWT), refresh_token, expires_in }`.
3. **Mint a PAT** `POST https://api.tiddly.me/tokens/` with the **Auth0 JWT** as bearer (this endpoint is **Auth0-only — it rejects PATs with 403**), body `{ name: "switchboard-<short-suffix>", expires_in_days: <bounded> }`. Response (`TokenCreateResponse`) includes a stable `id` (UUID) and `token: "bm_..."` **shown exactly once**. May return `402` (PAT quota exceeded) or `451` (consent required) — surface these as actionable errors. **Mint with a bounded expiry (e.g. 365 days), not `null`** — see the revocation constraint below; a bounded lifetime backstops a token we may be unable to revoke server-side.
4. **Store the `bm_` PAT, its `id`, and the Auth0 refresh token in the OS Keychain.** The PAT serves all prompt operations, so the hot path never touches Auth0 — no JWT refresh to babysit in steady state (the point of minting a long-lived PAT rather than driving requests off short-lived Auth0 tokens). The `id` (non-secret) and the refresh token are retained solely for the disconnect/revoke path below; the short-lived access token is discarded. This mirrors the Tiddly CLI, which keeps the refresh token and silently refreshes when it needs an Auth0-only operation.
5. Validate/identify with `GET https://api.tiddly.me/users/me` (accepts both PAT and Auth0 JWT bearer). Returns `{ id, email }` — useful to show "Connected as <email>".

**Revocation constraint (important).** Token management is **Auth0-only**: `DELETE /tokens/{id}` (hard delete, 204 on success, 404 if already gone) and `GET /tokens/` both reject PAT auth with 403 — a PAT cannot revoke itself. Revoking therefore needs a live Auth0 token, obtained by refreshing the retained refresh token (`POST /oauth/token`, `grant_type=refresh_token`). The Tiddly CLI works exactly this way — `tiddly tokens delete` / `tiddly mcp remove --delete-tokens` refresh silently, no re-login. Switchboard differs in one respect: its hot path uses the PAT, so the refresh token sits unused between connect and disconnect and can lapse if Auth0's refresh-token inactivity window is exceeded first. Hence revoke-on-disconnect is **best-effort with a graceful fallback** — see open question 8.4.

Constants are overridable via env (`TIDDLY_AUTH0_DOMAIN`, `TIDDLY_AUTH0_CLIENT_ID`, `TIDDLY_AUTH0_AUDIENCE`); the API base should be overridable too so the integration can point at local dev. **Re-verify these endpoints and constants against the current `bookmarks` repo at implementation time** — they were read on 2026-06-01 and Tiddly is independently maintained.

### 2.4 Switchboard substrate (what exists today)

- **No prompt providers and no MCP client of its own exist yet.** Switchboard currently only reads harnesses' MCP server *status* for display badges (`crates/harness/src/events.rs` `McpServerStatus`); it never calls `prompts/list`. `DirectoryConfig` (`crates/core/src/directory.rs`) is a v1 placeholder.
- **Workspace pattern:** `crates/core`, `crates/harness`, `crates/dispatcher` are pure Rust (no Tauri). `crates/app` owns Tauri commands as thin `#[tauri::command]` shims over free `*_impl` functions (`commands.rs` / `lib.rs`); frontend calls `invoke()` and subscribes to events.
- **Compose box** (`src/lib/components/ComposeBar.svelte`) is a **plain `<textarea>`** with recipient chips in a flex row *above* it, draft persistence in `composeStore.ts`, and an `@`-typeahead menu that selects recipients (it inserts nothing inline). **No slash commands, no inline tokens, no rich-text input exist.**
- **Settings** (`src/lib/components/SettingsView.svelte`) is minimal (theme + shortcuts). Switchboard stores no secrets today; it probes harness credentials read-only.

## 3. Architecture

Introduce a new pure-Rust workspace crate **`crates/prompts`** (no Tauri dependency, consistent with `core`/`harness`/`dispatcher`) that owns:

- **Provider config model + resolution** — parse `mcp_providers` and `local_prompt_dirs` from the **user-global** `~/.config/switchboard/config.yaml` (via the `directories` crate). Prompt config is global — no directory/project scope. Reuse `crates/core` YAML I/O.
- **`PromptProvider` trait** with two implementations:
  - `LocalProvider` — scans prompt dirs, parses frontmatter (`name`, `description`, `arguments`, `tags`), renders via **MiniJinja** (already the project's chosen engine, see §6).
  - `McpProvider` — wraps an MCP client over Streamable HTTP, implements `list`/`render` via `prompts/list` / `prompts/get`. Its listing feeds the global prompt cache (see "Prompt list lifecycle" below).
- **MCP client** — use the **official `rmcp` crate** (`transport-streamable-http-client` feature). Do not hand-roll JSON-RPC.
- **Tiddly login + keychain** — Auth0 device-flow polling and PAT minting (pure logic over an injected HTTP client, so it is unit-testable), and a thin secret store over the **`keyring` crate**.

`crates/app` exposes Tauri command shims over this crate and owns the side effects that need the Tauri host (opening the browser, emitting login-progress events). The frontend orchestrates: it lists prompts, previews/renders via commands, and at send time splices rendered text into the outgoing message so the **existing `send_message` path is unchanged** and receives final plain text.

**Shared contract — the two core commands** (established in M1, reused by every later milestone). Both are **global — no project/directory argument** (prompt providers are user-global):
- `list_prompts() -> [{ provider, name, description?, arguments, tags? }]` — reads the in-memory prompt cache (see lifecycle below); never hits the network on the hot path. An unreachable provider contributes nothing to the cache (degrade-to-empty-with-warning, matching the existing registry-failure policy).
- `render_prompt(provider, name, args) -> { text }` — serves **both preview and send** (local → MiniJinja, MCP → `prompts/get`). One operation, provider-dispatched. The only prompt command that may touch the network (MCP `prompts/get`).

Plus Tiddly auth commands (M3), a `sync_prompts()` command that rebuilds the cache (M2/M3), and prompt-config read commands as needed. The prompt/argument **data model mirrors the MCP `prompts/list` shape** (§2.1) so local and MCP providers share one type — load-bearing: M2's `McpProvider` reuses M1's model rather than inventing a parallel one.

**Prompt list lifecycle (build-once + Sync).** The prompt list is **built once and cached**, never rebuilt on a `/` keystroke:
- Built in the **background** at app load and whenever a provider is newly connected (e.g. Connect Tiddly) — so a slow or cold MCP server never blocks app startup or the compose menu.
- A **Sync** action in the Settings prompt/MCP section forces a rebuild (the user's path to pick up prompts edited in Tiddly mid-session). `prompts/list_changed` live subscription stays deferred (§5).
- The `/` slash menu reads the cache only — instant, offline. The cache is a single global list across all projects.

### Addressing & resolution

- Providers addressed by prefix: `local:<name>`, `<provider-name>:<name>`. The `local` prefix is reserved.
- Prefixed lookup is **strict** — resolves only against the named provider, errors if absent, no cross-provider fallback.
- Local resolution order: each user-global `local_prompt_dirs` entry in declared order; earlier shadows later. All prompt config is user-global — no directory/project scope, which is why the two commands take no project argument.
- The slash-command UI may accept a bare name when it matches exactly one provider; this is a UI affordance only.

## 4. Decisions already made (confirmed with engineer)

1. **Tiddly login flow:** browser/Auth0 device-code login → mint a long-lived `bm_` PAT → store PAT in Keychain → use as MCP bearer. No Auth0 token refresh in steady state. (Chosen over hand-pasting a PAT, and over driving requests with short-lived Auth0 tokens that would need constant refresh.)
2. **Secrets live in the OS Keychain, never in `config.yaml`.** The Tiddly preset stores its PAT in the keychain keyed by provider name; `config.yaml` holds only a non-secret `preset: tiddly` reference. Generic MCP providers may *additionally* support `${ENV}`-style token references for power users, but the keychain is the path for the one-click preset. `docs/system-design.md` §6 amended in place (it previously showed `token: ${TIDDLY_PAT}`).
3. **MCP client = official `rmcp` crate**, Streamable HTTP.
4. **Sequencing is backend-first, the inline-chip editor last** — it is the riskiest piece and everything else is testable without it.
5. **stdio MCP providers are deferred** (see Non-goals). Tiddly is HTTP; v1 ships local + HTTP MCP.
6. **Prompt chip is embedded inline within free message text** (the user may type text before/after the chip). M4 ships the full slash/argument/preview/send flow with a simple interim representation; M5 upgrades the compose input to render the chip truly inline.
7. **Prompt providers are user-global — no directory/project scope.** `list_prompts` / `render_prompt` take no project argument. §6 amended in place to drop the directory-scoped prompt config it previously described. Repo-shipped per-directory prompts can return additively in v2.
8. **Prompt list is built once and cached** (background build at app load + on provider connect), with a **Sync button** in Settings to rebuild; the `/` menu reads the cache and never fetches.

## 5. Scope / non-goals

**In scope (v1):** local prompt provider; HTTP MCP prompt provider (generic + Tiddly preset); Tiddly browser login (device-code → minted PAT); slash-command selection; argument dialog (required/optional); preview; inline chip; send-time resolution; user-global provider config; build-once prompt cache + Settings Sync.

**Out of scope / deferred (state explicitly; `log`/document rather than silently drop):**
- **stdio MCP providers** — deferred; revisit when a real stdio prompt source appears.
- **MCP tools and resources** — only `prompts/*`. Tools remain per-harness (model-invoked mid-turn; Switchboard cannot proxy them).
- **Prompt versioning / history** — references resolve to whatever the provider returns at invocation time (§6).
- **Prompt library browse/search view** — v2+ (§6 "Future direction"). `tags` is parsed and stored but unused in the v1 slash UI.
- **`prompts/list_changed` live subscription** — v1 builds once + Sync. Moot for Tiddly regardless (advertises `listChanged: false`, stateless — see §2.2), and for any provider it would require holding a persistent SSE stream open per provider for the app's lifetime; deferred.
- **Non-text prompt content** (image/audio/embedded resource `PromptMessage` parts) — v1 handles text content; other parts are dropped with a warning.
- **Multimedia/typed arguments, argument autocompletion (completion API)** — strings only, no completion.

## 6. Milestones

Milestones are dependency-ordered. Each is independently reviewable and should leave the tree green; complete and review one before starting the next. Each milestone states its **Goal & Outcome** (the alignment surface — validate the plan by confirming these outcomes are the ones you want), an **Implementation Outline** (handoff to an agent that will read the code but was not in this discussion — it carries the decisions that can't be recovered from the code, not naming/local-structure choices that can), and a **Definition of Done**.

---

### M1 — Provider framework + local prompt provider (offline)

**Goal & Outcome.** Stand up the prompt-provider foundation and make local file-based prompts fully work, with zero network or auth. When done:
- A user can drop a markdown prompt file (frontmatter: `name`, `description`, optional `arguments`, optional `tags`) into a configured prompts directory and Switchboard discovers it.
- Listing prompts returns that prompt with its metadata and declared arguments.
- Rendering a local prompt substitutes provided argument values — required arguments enforced, unfilled optional arguments render as empty — and returns finished text.
- Prompts are addressed as `local:<name>`; a name present in an earlier-listed directory shadows a later one.
- All prompt configuration is read from user-global config; there is no per-project or per-directory prompt scope.

**Implementation Outline.**
- New `crates/prompts` crate. Define the `PromptProvider` trait, the prompt/argument **data model** (mirrors the MCP `prompts/list` shape — §2.1 — so local and MCP share one type; this is the contract M2 reuses), and typed errors (`thiserror`).
- Config model: parse `local_prompt_dirs` and `mcp_providers` from the user-global `config.yaml` (read via the `directories` crate). Implement resolution = declared-order shadowing across `local_prompt_dirs`. Leave `mcp_providers` parsed-but-unused until M2.
- `LocalProvider`: directory scan, frontmatter parse, MiniJinja render. All args are strings; required-missing is an error; optional-missing renders empty.
- `provider:name` address parsing; strict prefixed lookup; `local` reserved.
- Tauri command shims `list_prompts` and `render_prompt` (local providers only this milestone) — fixing the no-project-argument signatures the rest of the plan depends on.
- **Doc/code reconciliation:** `crates/core`'s `DirectoryConfig` and the `AGENTS.md` reference to a per-directory `.switchboard/prompts/` predate the global model — drop or de-special-case the per-directory prompts dir so docs and code agree with §6 (already amended).
- Ship the example prompt(s) the design references (e.g. `code-review.md`) at the user-global default prompts path.

**Definition of Done.**
- Unit/fixture tests (deterministic, no network): frontmatter parsing (valid / invalid / missing required fields); `local_prompt_dirs` declared-order shadowing; MiniJinja render with required + optional args; missing-required-arg error; unknown-arg handling; `provider:name` address parsing (`local:x`, bare, malformed).
- Behavior verified: a prompt file in a configured dir appears in `list_prompts` with correct metadata; `render_prompt("local", name, args)` returns correctly substituted text.
- Docs: `DirectoryConfig`/`AGENTS.md` reconciled to the global model.
- Recorded limitation: `mcp_providers` is parsed but inert until M2.

---

### M2 — MCP client + generic HTTP MCP provider

**Goal & Outcome.** Add the MCP-server provider over Streamable HTTP and the build-once prompt cache, proven against the local Tiddly dev server. When done:
- A user can configure a generic HTTP MCP prompt server (URL + bearer token via `${ENV}`) in user-global config, and its prompts appear in the listing under the provider's prefix, alongside local prompts.
- Selecting and rendering an MCP prompt returns the **server-rendered** text.
- The prompt list is built once in the background (app load + on provider connect) and cached; a slow or cold server never blocks startup, and a down provider contributes nothing to the cache (with a warning) without breaking local prompts.
- A sync operation rebuilds the cache on demand.

**Implementation Outline.**
- Add `rmcp` (`transport-streamable-http-client`) and an HTTP client (`reqwest` if not already present) via `cargo add` (per the dependency policy — never hand-edit manifests).
- `McpProvider` (implements the M1 `PromptProvider` trait — reuse, don't fork): connect to a Streamable HTTP endpoint with a bearer token; `prompts/list` following `nextCursor` to completion under a **per-provider timeout**; `prompts/get` for render; drop non-text content parts with a warning.
- Wire generic `mcp_providers` HTTP entries (`transport: {type: http, url}`, `auth: {type: bearer, token}`) into `list_prompts`/`render_prompt`. Support `${ENV}` token references here; the keychain path lands in M3.
- Prompt cache + `sync_prompts()`: build the global cache in the **background**; `sync_prompts()` rebuilds it (wired to the Settings Sync button in M3). A slow/cold provider must not block the build — per-provider timeout, partial results allowed. `list_prompts` reads the cache; the `/` menu must never trigger a fetch.
- Failure handling: an unreachable/erroring provider contributes nothing to the cache + a warning; `render_prompt` failures return an actionable error and **never leak the bearer token**.

**Definition of Done.**
- Fixture-driven tests (in default `make test`): a mock/stub MCP endpoint (or recorded responses) covering pagination, render, missing-required-arg (`InvalidParams`) mapping, and the unreachable-server → contributes-nothing path.
- Live test (developer-local, `#[ignore]`-gated; needs a runner home per §7): against local Tiddly dev (`http://localhost:8002/mcp`, any bearer), assert the endpoint **advertises the `prompts` capability**, `prompts/list` returns, and a known prompt renders via `prompts/get`. (The capability assertion guards against upstream drift — Tiddly is confirmed prompts-capable, but the CLI vendors change contracts.)
- Behavior verified: a configured generic HTTP provider's prompts appear under the provider prefix and render via the server; provider-down degrades gracefully without breaking local prompts.
- Recorded limitation: `prompts/list_changed` live subscription deferred (Sync covers refresh).

---

### M3 — Keychain secret store + Tiddly preset + "Connect Tiddly" login

**Goal & Outcome.** Make Tiddly a first-class, one-click integration. When done:
- A user clicks "Connect Tiddly," completes a browser login, and ends in a "Connected as `<email>`" state — no token handling at any point.
- Tiddly's prompts then list and render exactly like any other provider.
- The credential (a minted PAT) lives in the OS keychain — never in `config.yaml`, logs, or error text.
- Disconnecting removes Switchboard's access and best-effort revokes the token server-side.
- An expired or externally-revoked credential renews silently on next use; the user only sees another browser login after a long idle period.
- A **Sync** button in Settings refreshes the cached prompt list (e.g. after editing prompts in Tiddly mid-session).

**Implementation Outline.**
- Keychain secret store over the `keyring` crate (store/fetch/delete a provider secret keyed by provider name). `McpProvider` resolves the bearer for keychain-backed providers through it.
- Tiddly preset: baked-in MCP URL (`prompt-mcp.tiddly.me/mcp`) and API base (`api.tiddly.me`), both env-overridable for local dev. A `preset: tiddly` config entry resolves its bearer from the keychain.
- Auth flow (pure, HTTP-client-injected, in `crates/prompts` so it is unit-testable): device-code request → poll with `slow_down`/`expired`/`pending` handling → mint PAT via `POST /tokens/` with the Auth0 JWT → **discard the short-lived access token; store the PAT, PAT `id`, the Auth0 refresh token, and the connected-account email in the keychain** (the refresh token is required for disconnect/renewal — it is *not* discarded). Map `402`/`451`/`access_denied` to actionable errors. Identify via `GET /users/me`.
- Tauri commands + events (`crates/app`): `tiddly_login_start` (returns `user_code` + `verification_uri_complete`, opens the browser via the Tauri host), `tiddly_login_poll` / progress events, `tiddly_disconnect`, `tiddly_status`.
- **Disconnect** (best-effort silent revoke): refresh the retained refresh token → `DELETE /tokens/{id}` → delete all local keychain entries. If the refresh fails (lapsed) or the delete 404s, fall back to local delete + a notice the token may stay active on Tiddly until expiry (manageable at `tiddly.me/settings`). Never block disconnect on the network.
- **Reconnect / renewal** (one path, not a separate feature): triggered by a 401 from the prompt server *or* a manual "Reconnect" action → refresh → mint a fresh PAT → swap it in → best-effort `DELETE` the old `id`. If the refresh token has also lapsed, fall back to a fresh device-login. Mint tokens with a recognizable `switchboard-<suffix>` name so orphaned tokens are identifiable.
- Settings UI (`SettingsView.svelte`): "Connect Tiddly" section (device code + polling spinner; connected state with email + disconnect; clear error states for quota / consent-required / timeout), plus the **Sync** action (`sync_prompts()`).

**Definition of Done.**
- Device-flow state-machine unit tests over a mock HTTP client (no real network, no wall-clock — inject clock/interval): `authorization_pending` loop, `slow_down` backoff, expiry, `access_denied`, success, then mint success and `402`/`451` paths.
- Keychain round-trip test (skip cleanly where no keyring backend exists, per the project's environment-dependent-test handling).
- Live test (`#[ignore]`-gated): full Connect-Tiddly against local Tiddly dev where reachable; otherwise record manual verification steps.
- Behavior verified: browser login → "Connected as `<email>`" → Tiddly prompts list and render through the keychain PAT; disconnect removes access; **no secret appears in any log or error string**.
- Docs: confirm §6 matches built behavior (already amended); add a one-line user-facing note to `README.md` "Harness support and limitations" if warranted (e.g. Codex/Gemini gain prompt support via Switchboard).

---

### M4 — Compose: slash menu + argument dialog + preview + send (interim representation)

**Goal & Outcome.** Wire the full user-facing flow, using a simple (not-yet-inline) chip so the logic is validated before the editor rewrite. When done:
- Typing `/` in the compose box opens an **instant** menu (read from cache) of all prompts across providers, filterable, selectable by keyboard or click.
- Selecting a prompt that declares arguments opens a dialog to fill them (required enforced; optional may be left blank), and the user can **preview** the rendered result before sending.
- Sending resolves the prompt to text **once** and dispatches; each recipient (one or many) receives the same rendered text; the agent never sees the template, provider, or arguments.
- If rendering fails at send, the user sees an error and **no phantom transcript entry** is created.
- A draft containing a pending prompt survives an app reload.

**Implementation Outline.**
- Slash-command menu in `ComposeBar.svelte`, mirroring the existing `@`-menu pattern but reading the **cached** global prompt list — **no network on open**. Reuse the existing menu/keyboard scaffolding.
- On select: if the prompt has ≥1 argument, open an argument dialog (required fields block submit; optional allowed empty; show each argument's description). A preview affordance calls `render_prompt` (handle MCP latency/errors).
- Draft model (`composeStore.ts`): represent a selected prompt as a structured token `{ provider, name, args }` persisted with the draft. **Add a `version` discriminant to the stored compose snapshot** (it is unversioned/string-only today) so M5's reader can degrade an M4-era draft gracefully instead of corrupting it.
- Interim UI: render the selected prompt as a single pill (above the textarea is acceptable this milestone) — **not** yet inline.
- Send — **render before dispatch**: introduce an async `prepareSubmit` phase that resolves all prompt tokens via `render_prompt` **once** and assembles the final plain text **before** any optimistic transcript turn or journal write, then calls the existing `send_message` path unchanged. (Today `ComposeBar` appends the optimistic user turn and clears the draft synchronously before IPC; folding rendering in naively would leave a phantom user turn for text never sent on a render failure — worse on multi-recipient sends.) On failure: compose-level error, preserve the draft, append no transcript/journal state. The rendered text is the user's send content for history/journal.

**Definition of Done.**
- Component-level tests per the project's IPC-component rule (mock `invoke`/`listen`, capture callbacks): slash menu opens from cache without a network call; argument-dialog required-field validation; preview success and error; token persistence across draft save/restore; send assembles correct final text; multi-recipient renders **once**; **render-fails-at-send appends no transcript turn, preserves the draft, surfaces an error**. Use `tick()`/`waitFor` for presence assertions.
- Behavior verified: select `tiddly:<prompt>` and `local:<prompt>`, fill args, preview, send to one and to multiple agents — each receives the correct rendered text; a draft with a pending prompt token survives reload.

---

### M5 — Inline chip in the compose input

**Goal & Outcome.** Replace M4's interim pill with a true inline chip. When done:
- The user can type free text **before and after** a prompt chip in the compose box; the chip is a compact, removable widget showing the prompt name and a preview affordance.
- Multiple chips can coexist in one message; each resolves independently at send and is spliced in at its position in the surrounding text.
- Every prior compose behavior still works: recipient chips, the `@` menu, send/clear keyboard shortcuts, and draft persistence.

**Implementation Outline.**
- Upgrade the compose input from a plain `<textarea>` to a token-aware inline editor (e.g. a `contenteditable` region) that renders text segments and prompt-chip tokens in document order. The draft model stays the structured `{ text-segments, prompt-tokens }` sequence from M4; the M4 `version` discriminant lets the reader degrade older drafts.
- Preserve and re-verify every existing compose behavior against the new editor (draft persistence, `@` recipient menu, keyboard shortcuts, recipient chips above the input).
- Send assembly splices each chip's rendered text into its position in the surrounding free text (multiple chips resolve independently).

**Definition of Done.**
- Editor-behavior tests: insert chip inline, type around it, delete a chip, multiple chips; caret/selection sanity; paste of plain text; draft round-trip with mixed text+chips; **an M4-era (older `version`) saved draft degrades gracefully rather than corrupting**; send produces correctly ordered final text. Regression-cover the `@` menu and keyboard shortcuts under the new editor. Keep assertions deterministic via the reactive-flush helpers.
- Behavior verified: a message like `please review this then summarize: [tiddly:code-review] and also check the tests` sends with the chip's rendered text spliced in at its position; all prior compose behaviors pass.
- **Risk note (record, don't hide):** this is the highest-risk milestone — `contenteditable` interacts subtly with IME, paste, undo, and selection. It is intentionally last so M1–M4 deliver working prompts behind the interim representation even if this milestone needs iteration.

## 7. Cross-cutting requirements

These apply across milestones. Carry the *why* into the code (comments/commit messages), not just the plan.

- **No secrets in logs or error messages** — bearer tokens and PATs never appear in tracing, error strings, or test output. Redact in any diagnostic.
- **Graceful degradation** — a misconfigured or unreachable provider never breaks the compose box or local prompts; it degrades to empty-with-warning.
- **Optional-argument semantics (settle once; touches M1 local, M2 MCP, M4 preview).** Omit unfilled optional args from the `prompts/get` call (let the server template apply its own conditionals/defaults); local MiniJinja renders missing optionals as empty. **Preview and send must pass the identical argument map** so the preview never diverges from what the agent receives.
- **Determinism in tests** — inject clocks/intervals for the device-flow polling; no wall-clock or time-of-day dependencies.
- **Dependency hygiene** — add all crates via `cargo add` / `pnpm add` per `AGENTS.md`; commit manifest + lockfile together.
- **Wire-format conventions** — new IPC types follow the existing `#[serde(rename_all = "snake_case")]` + TS discriminated-union pattern; mark evolving enums `#[non_exhaustive]`.
- **Live-test naming/runner** — the live-test convention (`live_<harness>_`, runner `LIVE_PKGS` = harness/dispatcher/app) is built around the four *harnesses*; Tiddly is a prompt *provider*, not a harness, and `crates/prompts` is not in `LIVE_PKGS`. Before adding Tiddly live tests, give them a runner home: add `switchboard-prompts` to `LIVE_PKGS` + a `make test-live-tiddly` target and document a `live_tiddly_` provider category in `AGENTS.md`, **or** house them in `switchboard-app`. Otherwise a `live_tiddly_` test silently never runs under `make test-live`.

## 8. Open questions / decisions to confirm in review

Resolved with the engineer (treat as settled): Tiddly login via browser device-code → minted PAT in keychain; keychain over `config.yaml`; `rmcp`; backend-first sequencing; stdio deferred; inline chip embedded in free text; user-global provider scope (no project arg, §6 amended); build-once cache + Settings Sync (the `/` menu never fetches); retain the Auth0 refresh token for disconnect/renewal; Tiddly implements the MCP `prompts` capability and its management *tools* are unused.

Still open — raise in review:

1. **Multiple prompt chips per message** — the plan allows multiple independent inline chips (M5). Confirm, or restrict to one chip per send for v1 simplicity.
2. **Minted PAT is account-wide, not prompts-scoped** — `POST /tokens/` issues a general PAT (full account capability), used only for prompts. Matches the Tiddly CLI; no narrower scope exists. Recommend accepting; confirm.
3. **`${ENV}` token references for generic MCP providers** — kept for power users alongside the keychain. Confirm both, or keychain-only.
4. **PAT revocation on disconnect** — `DELETE /tokens/{id}` is Auth0-only; revoking needs the retained refresh token. Recommended default: best-effort silent revoke on disconnect (refresh → delete), falling back to local-delete + a "manage at tiddly.me/settings" notice if the refresh token has lapsed (likelier here than for the CLI, since the hot path uses the PAT and leaves the refresh token idle). Bounded mint expiry (~365 days) backstops it regardless. **Confirm: best-effort silent revoke (recommended) vs. local-delete-only.**
5. **Re-verify Tiddly endpoints/constants** (§2.3) against the current `bookmarks` repo before building M2/M3.
6. **PAT expiry policy** — recommend long expiry (~365 days) + 401-driven reconnect over short expiry (~90 days) + auto-renew (the PAT and refresh token share the keychain, so a short PAT expiry is not a strong security boundary on its own). Recommend **no** user-facing expiry picker in v1. Renewal mechanics are identical either way (M3).

## 9. Reference docs (read before implementing)

- `docs/system-design.md` §6 (Prompts and prompt providers) — authoritative design; §3 (filesystem layout, config scopes); §9 (harness integration).
- MCP prompts spec: https://modelcontextprotocol.io/specification/2025-06-18/server/prompts
- `rmcp` (official Rust MCP SDK): https://github.com/modelcontextprotocol/rust-sdk
- Tiddly reference implementation (sibling `bookmarks` repo): `backend/src/prompt_mcp_server/`, `backend/src/api/routers/tokens.py`, `backend/src/core/auth.py`, `cli/internal/auth/device_flow.go`, `cli/internal/mcp/configure.go`, `README_DEPLOY.md`.
