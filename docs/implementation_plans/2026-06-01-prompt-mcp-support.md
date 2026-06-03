# Implementation Plan: MCP Prompt Support (local + generic HTTP MCP providers)

> **Scope note (revised mid-implementation).** V1 ships **local prompts + generic HTTP MCP-server support**. The **first-class Tiddly preset** (one-click "Connect Tiddly" browser/Auth0 login + PAT minting) is **deferred to a post-V1 follow-up** ([M5](#m5--tiddly-first-class-preset-deferred-post-v1-follow-up)). In V1, Tiddly is configured like any other MCP server: the user mints a Tiddly PAT in Tiddly's own UI/CLI and pastes it as the bearer token in the generic "Add MCP server" form. Rationale: prove the generic-MCP value first and remove the entire Auth0/device-code/PAT-minting/revocation machinery (the most complex, most external-dependency-heavy, most drift-prone part of the plan) from the V1 critical path. The deferred Tiddly material is retained in §2.3, the deferred §4 decisions, and M5 as the blueprint for that follow-up.

## 1. Background and goal

Switchboard should let a user invoke **prompts** — reusable, optionally parameterized text templates — from the compose box, regardless of which harness (Claude Code, Codex, Gemini, Antigravity) the recipient agent runs on. Prompts come from two kinds of **provider**:

- **Local file store** — markdown files with YAML frontmatter, in user-configured directories. Rendered by Switchboard.
- **MCP-server provider** — any MCP server that exposes prompts via the standard `prompts/list` / `prompts/get` RPCs. The server renders; Switchboard receives finished text.

[Tiddly](https://tiddly.me) is the canonical MCP provider and the development reference. In **V1 it is configured as a generic MCP server** — mint a Tiddly PAT in Tiddly's own UI/CLI, paste it as the bearer token. A **first-class "Connect Tiddly" preset** (browser/Auth0 login, no token handling) is the deferred follow-up (see the scope note above and M5).

The user selects a prompt from the compose area — a `+` button, or `/` when the box is empty. The composer then switches to a **prompt mode**: it keeps the recipients header, lets the user change the prompt, renders each declared argument as a labeled input, and offers an **Appended text** field and a **Preview**. At send time, Switchboard renders the prompt with the entered arguments, combines it with the appended text, and the agent receives only that final text — never the template, provider, or arguments. (This deliberately avoids a rich-text editor; inline/interleaved prompt chips and multiple prompts per message are deferred to v2 — see §5.)

This plan implements the design specified in **`docs/system-design.md` §6 ("Prompts and prompt providers")**; read that section first. The design is directional, not gospel — this plan refines points it did not fully settle (all confirmed with the engineer and recorded in §4): prompt config is **user-global** (the doc's directory-scoping was dropped and §6 amended in place) and secrets live in the **OS keychain** (not `config.yaml`). The design's "Tiddly is a first-class one-click preset" framing is **deferred to a post-V1 follow-up** (M5); V1 treats Tiddly as a generic MCP server with a pasted bearer token. `docs/system-design.md` §6 should be amended to reflect this.

### Why this matters across harnesses

Codex and Gemini have limited or absent native MCP-prompt support. Because Switchboard resolves prompts itself and sends rendered text as a plain user message, every harness gets a uniform, Claude-Code-style prompt library with no per-harness configuration. This is a core value proposition, not a nicety.

## 2. Key facts the implementer needs (verified during research)

### 2.1 MCP prompts protocol (spec 2025-06-18)

- `prompts/list` → `{ prompts: [{ name, title?, description?, arguments: [{ name, description?, required? }] }], nextCursor? }`. Paginated via opaque `cursor`. `required` defaults `false`. **All arguments are strings; the protocol has no type system and no default-value field.**
- `prompts/get` with `{ name, arguments: { k: v } }` → `{ description?, messages: [{ role, content: { type:"text", text } }] }`. The client receives **already-rendered text**.
- Capability `prompts.listChanged: true` ⇒ server emits `notifications/prompts/list_changed` when its prompt set changes.
- Errors are JSON-RPC: `-32602` invalid params (bad name / missing required arg), `-32603` internal.
- Spec: https://modelcontextprotocol.io/specification/2025-06-18/server/prompts (the live Tiddly server negotiates protocol `2025-11-25` via mcp-SDK 1.26.0 — newer, but the `prompts/list` / `prompts/get` shapes above are unchanged across these versions).

### 2.2 Tiddly's prompt MCP server (the reference target)

- **Transport: Streamable HTTP.** Prod endpoint `https://prompts-mcp.tiddly.me/mcp`; local dev `http://localhost:8002/mcp`. (The notes/bookmarks server `content-mcp.tiddly.me` / `:8001` is **out of scope** — we use the prompts server only.)
- **Auth: `Authorization: Bearer <token>`** on every request. Production validates a Tiddly PAT (`bm_` prefix); **dev mode accepts any non-empty string** (this is what makes the local dev server a cheap integration target). The bearer token IS the account scoping — nothing else to configure.
- Implements `prompts/list` (paginated, 100/page) and `prompts/get` per spec, **plus** prompt-management *tools* (`get_prompt_content`, `search_prompts`, …). Switchboard uses **only the prompts capability**; the tools are ignored. Templates are Jinja2; the server renders and returns one `user` text message. Missing required arg → `InvalidParams`.
- **No live list updates.** The server advertises `prompts.listChanged: false`, never sends `notifications/prompts/list_changed`, and runs stateless (JSON responses, no held-open SSE stream) — so it has no mechanism to push prompt-set changes. (`listChanged` has been in MCP since the first spec, `2024-11-05`; it is not a feature Tiddly is missing for being out of date — most simple servers leave it off. Receiving it at all would require the client to hold an SSE stream open via an HTTP GET, which a stateless/JSON server like Tiddly does not offer.) Practical consequence: a user who edits a prompt in Tiddly must **Sync** (or restart) to see it — the concrete reason v1 uses build-once + Sync rather than a subscription.

### 2.3 Tiddly login + PAT minting (for the "Connect Tiddly" preset)

> **Deferred to the post-V1 follow-up (M5) — not implemented in V1.** This subsection is retained verbatim as the implementation blueprint for the deferred Tiddly preset. None of it is built in M1–M4. In V1 a Tiddly user mints a PAT in Tiddly's own UI/CLI and pastes it into the generic "Add MCP server" form (M3); the Auth0 device flow, PAT minting, and revocation below are M5's concern.

Talks to the Tiddly **API host** (`https://api.tiddly.me`, dev `http://localhost:8000`), distinct from the MCP host.

1. **Auth0 device-code login.** `POST https://tiddly.us.auth0.com/oauth/device/code` with `client_id=Gpv1ZrySgEeoTHlPyq3vSqHdFkS1vPwI`, `scope=openid profile email offline_access`, `audience=tiddly-api`. Returns `{ device_code, user_code, verification_uri, verification_uri_complete?, expires_in (~600), interval (~5) }`. Show the user the code + URL and open `verification_uri_complete` in their browser.
2. **Poll** `POST https://tiddly.us.auth0.com/oauth/token` with `grant_type=urn:ietf:params:oauth:grant-type:device_code`, `device_code`, `client_id`. Handle `authorization_pending` (keep polling), `slow_down` (increase interval by 5s, per RFC 8628), `expired_token` / `access_denied` (fail). Success → `{ access_token (JWT), refresh_token, expires_in }`.
3. **Mint a PAT** `POST https://api.tiddly.me/tokens/` with the **Auth0 JWT** as bearer (this endpoint is **Auth0-only — it rejects PATs with 403**), body `{ name: "switchboard-<short-suffix>", expires_in_days: <bounded> }`. Response (`TokenCreateResponse`) includes a stable `id` (UUID) and `token: "bm_..."` **shown exactly once**. May return `402` (PAT quota exceeded) or `451` (consent required) — surface these as actionable errors. **Mint with a bounded expiry (e.g. 365 days), not `null`** — see the revocation constraint below; a bounded lifetime backstops a token we may be unable to revoke server-side.
4. **Store the `bm_` PAT, its `id`, and the Auth0 refresh token in the OS Keychain.** The PAT serves all prompt operations, so the hot path never touches Auth0 — no JWT refresh to babysit in steady state (the point of minting a long-lived PAT rather than driving requests off short-lived Auth0 tokens). The `id` (non-secret) and the refresh token are retained solely for the disconnect/revoke path below; the short-lived access token is discarded. This mirrors the Tiddly CLI, which keeps the refresh token and silently refreshes when it needs an Auth0-only operation.
5. Validate/identify with `GET https://api.tiddly.me/users/me` (accepts both PAT and Auth0 JWT bearer). Returns `{ id, email }` — useful to show "Connected as <email>".

**Revocation constraint (important).** Token management is **Auth0-only**: `DELETE /tokens/{id}` (hard delete, 204 on success, 404 if already gone) and `GET /tokens/` both reject PAT auth with 403 — a PAT cannot revoke itself. Revoking therefore needs a live Auth0 token, obtained by refreshing the retained refresh token (`POST /oauth/token`, `grant_type=refresh_token`). The Tiddly CLI works exactly this way — `tiddly tokens delete` / `tiddly mcp remove --delete-tokens` refresh silently, no re-login. Switchboard differs in one respect: its hot path uses the PAT, so the refresh token sits unused between connect and disconnect and can lapse if Auth0's refresh-token inactivity window is exceeded first. Hence revoke-on-disconnect is **best-effort with a graceful fallback** (settled — see §4 decision 9 and M3 "Disconnect").

Constants are overridable via env (`TIDDLY_AUTH0_DOMAIN`, `TIDDLY_AUTH0_CLIENT_ID`, `TIDDLY_AUTH0_AUDIENCE`); the API base should be overridable too so the integration can point at local dev. **Re-verify these endpoints and constants against the current `bookmarks` repo at implementation time** — they were read on 2026-06-01 and Tiddly is independently maintained.

The `client_id` is a **public** value — the device flow uses no client secret — so it ships as a baked constant (env-overridable). Switchboard registers its own dedicated Auth0 application (§4 decision 11), with reuse of the CLI's client as a zero-code fallback; the runbook is in M3's **Auth0 prerequisite**.

### 2.4 Switchboard substrate (what exists today)

- **No prompt providers and no MCP client of its own exist yet.** Switchboard currently only reads harnesses' MCP server *status* for display badges (`crates/harness/src/events.rs` `McpServerStatus`); it never calls `prompts/list`. `DirectoryConfig` (`crates/core/src/directory.rs`) is a v1 placeholder.
- **Workspace pattern:** `crates/core`, `crates/harness`, `crates/dispatcher` are pure Rust (no Tauri). `crates/app` owns Tauri commands as thin `#[tauri::command]` shims over free `*_impl` functions (`commands.rs` / `lib.rs`); frontend calls `invoke()` and subscribes to events.
- **Compose box** (`src/lib/components/ComposeBar.svelte`) is a **plain `<textarea>`** with recipient chips in a flex row *above* it, draft persistence in `composeStore.ts`, and an `@`-typeahead menu that selects recipients (it inserts nothing inline). **No slash commands, no inline tokens, no rich-text input exist.**
- **Settings** (`src/lib/components/SettingsView.svelte`) is minimal (theme + shortcuts). Switchboard stores no secrets today; it probes harness credentials read-only.

## 3. Architecture

Introduce a new pure-Rust workspace crate **`crates/prompts`** (no Tauri dependency, consistent with `core`/`harness`/`dispatcher`) that owns:

- **Provider config model + resolution** — parse `mcp_providers` and `local_prompt_dirs` from the **user-global** `config.yaml` whose path is **injected by `crates/app`** (see below), not resolved inside this pure crate. Prompt config is global — no directory/project scope. Reuse `crates/core` YAML I/O.
- **`PromptProvider` trait** with two implementations:
  - `LocalProvider` — scans prompt dirs, parses frontmatter (`name`, `description`, `arguments`, `tags`), renders via **MiniJinja** (already the project's chosen engine, see §6).
  - `McpProvider` — wraps an MCP client over Streamable HTTP, implements `list`/`render` via `prompts/list` / `prompts/get`. Its listing feeds the global prompt cache (see "Prompt list lifecycle" below).
- **MCP client** — use the **official `rmcp` crate** (`transport-streamable-http-client` feature). Do not hand-roll JSON-RPC.
- **Secret store** — a small `SecretStore` trait (store/fetch/delete a bearer string by provider key) that `PromptService` resolves bearers through. **The trait is the platform seam**: `crates/prompts` never names a keychain crate, and the concrete backend is built and injected by `crates/app` (the only crate that should know the host OS). M2 ships the trait + an in-memory impl (tests, and the not-yet-populated app); the **OS-secure-store backend (`KeyringSecretStore`) lands in M3** with the "Add MCP server" form that populates it. The `keyring` crate is **cross-platform** (macOS Keychain / Windows Credential Manager / Linux Secret Service), so one backend impl covers all three; a platform with no usable store falls back to a different `SecretStore` impl behind the trait without touching providers. (The deferred Tiddly preset reuses this same store, extended to a multi-field credential — M5.)

`crates/app` exposes Tauri command shims over this crate and owns the side effects that need the Tauri host. The frontend orchestrates: it lists prompts, previews/renders via commands, and at send time splices rendered text into the outgoing message so the **existing `send_message` path is unchanged** and receives final plain text.

`crates/app` also owns **config-directory resolution**: it resolves the user-global config dir honoring the existing `SWITCHBOARD_CONFIG_DIR` override (production `switchboard`; debug uses `SWITCHBOARD_CONFIG_DIR`, else `switchboard-dev` — the same mechanism `workspace_config_path()` uses, so two dev instances and the test suite stay isolated) and **injects the resolved `config.yaml` path and prompt dirs into the prompt service**. The pure `crates/prompts` never calls `directories`/`ProjectDirs` itself — this is what keeps dev-instance isolation and test hermeticity intact.

**Shared contract — the two core commands** (established in M1, reused by every later milestone). Both are **global — no project/directory argument** (prompt providers are user-global):
- `list_prompts() -> [{ provider, name, description?, arguments, tags? }]` — reads the in-memory prompt cache (see lifecycle below); never hits the network on the hot path. An unreachable provider contributes nothing to the cache (degrade-to-empty-with-warning, matching the existing registry-failure policy).
- `render_prompt(provider, name, args) -> { text }` — serves **both preview and send** (local → MiniJinja, MCP → `prompts/get`). One operation, provider-dispatched. The only prompt command that may touch the network (MCP `prompts/get`).

Plus a `sync_prompts()` command that rebuilds the cache (M2), generic MCP-provider management commands (M3), and prompt-config read commands as needed. (The deferred Tiddly preset adds its own auth commands in M5.) The prompt/argument **data model mirrors the MCP `prompts/list` shape** (§2.1) so local and MCP providers share one type — load-bearing: M2's `McpProvider` reuses M1's model rather than inventing a parallel one.

**Prompt list lifecycle (build-once + Sync).** The prompt list is **built once and cached**, never rebuilt on a `/` keystroke:
- Built in the **background** at app load and whenever a provider is newly added (e.g. via the "Add MCP server" form) — so a slow or cold MCP server never blocks app startup or the compose menu.
- A **Sync** action in the Settings prompt/MCP section forces a rebuild (the user's path to pick up prompts edited on the server mid-session). `prompts/list_changed` live subscription stays deferred (§5).
- The `/` slash menu reads the cache only — instant, offline. The cache is a single global list across all projects.

### Addressing & resolution

- Providers addressed by prefix: `local:<name>`, `<provider-name>:<name>`. The `local` prefix is reserved.
- Prefixed lookup is **strict** — resolves only against the named provider, errors if absent, no cross-provider fallback.
- Local resolution order: each user-global `local_prompt_dirs` entry in declared order; earlier shadows later. All prompt config is user-global — no directory/project scope, which is why the two commands take no project argument.
- The slash-command UI may accept a bare name when it matches exactly one provider; this is a UI affordance only.

## 4. Decisions already made (confirmed with engineer)

1. **[Deferred to M5.] Tiddly login flow:** browser/Auth0 device-code login → mint a long-lived `bm_` PAT → store PAT in Keychain → use as MCP bearer. **In V1, Tiddly is configured as a generic MCP server with a user-pasted PAT** (minted in Tiddly's own UI/CLI); the browser-login preset is the post-V1 follow-up. The blueprint stays in §2.3 and M5.
2. **All provider secrets live in the OS Keychain, never in `config.yaml`** (which is git-tracked). This holds for *every* provider — generic MCP servers in V1 (and the deferred Tiddly preset later) alike; `config.yaml` holds only non-secret entries (a generic provider's `name`/`url`; later a `preset: tiddly` entry), each keyed by provider name to its keychain secret. There is **no `${ENV}` token mechanism** (dropped — see decision 12). `docs/system-design.md` §6 amended in place (it previously showed `token: ${TIDDLY_PAT}` / `${TEAM_MCP_TOKEN}`).
3. **MCP client = official `rmcp` crate**, Streamable HTTP.
4. **Sequencing is backend-first** — providers, MCP client, and auth land before the prompt-mode composer UI; the backend is fully testable without the UI.
5. **stdio MCP providers are deferred** (see Non-goals). Tiddly is HTTP; v1 ships local + HTTP MCP.
6. **Prompt UI = a structured "prompt-mode" composer (Option C), not inline chips.** When a prompt is selected the compose area switches to a layout with the recipients header, a prompt selector, argument inputs (required-validated), an "Appended text" field, and a preview; at send the prompt is rendered and combined with the appended text. **One prompt per send.** This deliberately avoids a `contenteditable` rich-text editor — inline/interleaved chips and multiple prompts per message are deferred to v2 (that is where the rich-text question would return). Considered and rejected for v1: inline chips in free text (needs `contenteditable`, the highest-risk path) and plain-text tokens (poor fit for arguments).
7. **Prompt providers are user-global — no directory/project scope.** `list_prompts` / `render_prompt` take no project argument. §6 amended in place to drop the directory-scoped prompt config it previously described. Repo-shipped per-directory prompts can return additively in v2.
8. **Prompt list is built once and cached** (background build at app load + on provider connect), with a **Sync button** in Settings to rebuild; the `/` menu reads the cache and never fetches.
9. **[Deferred to M5.] PAT revocation on disconnect = best-effort silent revoke** (mirrors the CLI): refresh the retained token → `DELETE /tokens/{id}` → wipe local entries; on a lapsed refresh token or 404, fall back to local-delete + a "manage at tiddly.me/settings" notice. (Moot in V1: a generic provider's bearer is removed locally on remove; there is nothing to revoke server-side.)
10. **[Deferred to M5.] PAT expiry = long (~365 days) + 401-driven silent reconnect; no user-facing expiry picker.** (V1 generic providers don't auto-renew: an expired/invalid bearer surfaces as an actionable "auth failed / unreachable" provider status, and the user re-enters the token — no renewal machinery.)
11. **[Deferred to M5.] Auth0 = a dedicated "Switchboard" Native app** (own branding/analytics/revocation), reusing the existing `tiddly-api` audience. CLI-client reuse is a zero-code fallback. (No Auth0 application is needed for V1.)
12. **Generic MCP providers are managed through a Settings UI and store their bearer in the keychain — no `${ENV}`.** `${ENV}` was dropped: a Finder/Launchpad-launched macOS app doesn't inherit the user's shell environment, so env-var secrets fail silently in the installed app (and `config.yaml` is git-tracked, so plaintext is out). Instead Settings gets an **"Add MCP server"** form (name + URL + bearer token) that writes the non-secret entry to `config.yaml` and the token to the keychain. In V1 this is the **only** provider-management surface (and the first consumer of the keychain store + programmatic `config.yaml` writes); a generic provider is *simpler* than the deferred Tiddly preset (no OAuth). A missing/invalid credential surfaces as an actionable provider status (never a silent empty list; token never logged).
13. **[Deferred to M5.] Account-wide PAT accepted.** Tiddly's `POST /tokens/` issues a general-capability PAT; Switchboard uses it only for prompts. (Relevant only when Switchboard *mints* the PAT — V1 has the user paste one, so this is the user's concern, not Switchboard's.)

## 5. Scope / non-goals

**In scope (v1):** local prompt provider; generic HTTP MCP prompt provider; generic MCP-server management UI (add/remove name+URL+token → keychain); keychain for all provider secrets; prompt-mode composer (slash/`+` selection, argument inputs, appended text, preview); single prompt per send; send-time render-and-combine; user-global provider config; build-once prompt cache + Settings Sync.

**Out of scope / deferred (state explicitly; `log`/document rather than silently drop):**
- **Tiddly first-class preset** (one-click "Connect Tiddly": Auth0 browser device-code login + Switchboard-minted PAT, disconnect/revoke, 401 silent reconnect) — **deferred to a post-V1 follow-up (M5).** V1 configures Tiddly as a generic MCP server with a pasted PAT. The blueprint is retained in §2.3, the deferred §4 decisions (1, 9–11, 13), and M5. Rationale: prove generic-MCP value first; keep the Auth0/PAT machinery (and its standing "re-verify against the bookmarks repo" burden) off the V1 critical path.
- **stdio MCP providers** — deferred; revisit when a real stdio prompt source appears.
- **MCP tools and resources** — only `prompts/*`. Tools remain per-harness (model-invoked mid-turn; Switchboard cannot proxy them).
- **Prompt versioning / history** — references resolve to whatever the provider returns at invocation time (§6).
- **Prompt library browse/search view** — v2+ (§6 "Future direction"). `tags` is parsed and stored but unused in the v1 slash UI.
- **`prompts/list_changed` live subscription** — v1 builds once + Sync. Moot for Tiddly regardless (advertises `listChanged: false`, stateless — see §2.2), and for any provider it would require holding a persistent SSE stream open per provider for the app's lifetime; deferred.
- **Non-text prompt content** (image/audio/embedded resource `PromptMessage` parts) — v1 handles text content; other parts are dropped with a warning.
- **Multimedia/typed arguments, argument autocompletion (completion API)** — strings only, no completion.
- **Inline/interleaved prompt chips and multiple prompts per message** — v1 uses the prompt-mode composer (one prompt + appended text). A `contenteditable` rich-text editor for chips embedded in free text is the v2 path if multiple/interleaved prompts are wanted.

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
- Config model: parse `local_prompt_dirs` from the user-global `config.yaml` whose path is **injected by `crates/app`** (which resolves the config dir honoring `SWITCHBOARD_CONFIG_DIR`; the pure crate takes a path and never calls `directories`/`ProjectDirs` — keeps dev instances and tests isolated). Implement resolution = declared-order shadowing across `local_prompt_dirs`. **`mcp_providers` is left as an *ignored* unknown key, not modeled** — M1 never consumes it, so modeling it would be speculative and would couple its parse to `local_prompt_dirs` (a typo in the inert MCP section would otherwise discard the user's valid local dirs and break local prompts). The MCP config model is introduced by M2/M3 when they read and write it.
- `LocalProvider`: directory scan, frontmatter parse, MiniJinja render. All args are strings; required-missing is an error; optional-missing renders empty.
- `provider:name` address parsing; strict prefixed lookup; `local` reserved.
- Tauri command shims `list_prompts` and `render_prompt` (local providers only this milestone) — fixing the no-project-argument signatures the rest of the plan depends on.
- **Doc/code reconciliation:** `crates/core`'s `DirectoryConfig` and the `AGENTS.md` reference to a per-directory `.switchboard/prompts/` predate the global model — drop or de-special-case the per-directory prompts dir so docs and code agree with §6 (already amended).
- **Seed example prompts (first-run only).** Bundle `code-review.md` (and any other examples) as a Tauri resource and seed them into the user-global default prompts dir **once on first run** — gated on the dir being *absent* (or a one-time seed marker), **not** on "empty" (so deleting an example never triggers re-seeding). Never overwrite an existing file. Seeded files are real, user-editable local prompts — serving §2's file-first, zero-setup onboarding. This adds first-run write infra to `crates/app`.

**Definition of Done.**
- Unit/fixture tests (deterministic, no network): frontmatter parsing (valid / invalid / missing required fields); `local_prompt_dirs` declared-order shadowing; MiniJinja render with required + optional args; missing-required-arg error; unknown-arg handling; `provider:name` address parsing (`local:x`, bare, malformed).
- Behavior verified: a prompt file in a configured dir appears in `list_prompts` with correct metadata; `render_prompt("local", name, args)` returns correctly substituted text.
- Docs: `DirectoryConfig`/`AGENTS.md` reconciled to the global model.
- Recorded limitation: `mcp_providers` is an ignored config section until M2 (not modeled in M1, so a malformed MCP entry can never break local prompts).

---

### M2 — MCP client + generic HTTP MCP provider

**Goal & Outcome.** Add the MCP-server provider over Streamable HTTP and the build-once prompt cache, proven **hermetically** (an in-process `rmcp` server in tests — no external server dependency). When done:
- A configured generic HTTP MCP prompt server (URL + a keychain-stored bearer token) has its prompts appear in the listing under the provider's prefix, alongside local prompts. (The Settings UI for *adding* such a provider lands in M3; M2 proves the client/provider mechanics using the keychain secret store introduced here.)
- Selecting and rendering an MCP prompt returns the **server-rendered** text.
- The prompt list is built once in the background at app load and cached (the "on add a provider" trigger arrives with M3's Settings form); a slow or cold server never blocks startup, and a down provider contributes nothing to the cache (with a warning) without breaking local prompts.
- A sync operation rebuilds the cache on demand.

**Implementation Outline.**
- Add `rmcp` (with the streamable-HTTP **client** feature, plus the **server** feature for the in-process test server) via `cargo add` (per the dependency policy — never hand-edit manifests). Add a direct `reqwest` only if `rmcp`'s transport doesn't surface what's needed — the Auth0 calls that previously motivated `reqwest` are deferred with the Tiddly preset, so it may not be needed at all.
- `McpProvider` (implements the M1 `PromptProvider` trait — reuse, don't fork): connect to a Streamable HTTP endpoint with a bearer token; `prompts/list` following `nextCursor` to completion under a **per-provider timeout**; `prompts/get` for render; drop non-text content parts with a warning.
- **Async boundary (deferred from M1 by design).** M1 ships the provider/service/command path **synchronous** — correct there (local-only, no network) and safe (the `render_prompt` command has no consumer until M4, and the `PromptProvider` trait isn't dispatched anywhere). M2 introduces the async boundary **together with the cache**, where it's coherent: make the **render** path async (`PromptProvider::render`, `PromptService::render`, `render_prompt_impl`; the Tauri shim is already `async` and just awaits) — via the workspace's existing `async-trait`, consistent with `HarnessAdapter`. `McpProvider::render` must do real async I/O (`prompts/get`) and **must not `block_on` a Tauri worker**. `PromptService::list` **stays synchronous** — it reads the build-once cache; the providers' network `list` runs only inside the async background refresh / `sync_prompts()`, never on the `list_prompts` hot path.
- Wire generic `mcp_providers` HTTP entries (non-secret `name`/`url` in config) into `list_prompts`/`render_prompt`, resolving each provider's bearer from a **keychain secret store** keyed by provider name. Introduce the thin `keyring`-crate store primitive here (store/fetch/delete by key) as the first consumer; the generic-provider Settings form that *populates* it lands in M3. Define the store behind a small `SecretStore` trait so fixture/unit tests inject an in-memory store and never touch the real OS keychain (mirrors the config-path injection pattern); the `keyring`-backed impl is the production wiring. The in-process test server accepts any bearer.
- Re-model `mcp_providers` in `PromptConfig` now (M1 left it an ignored key): a non-secret entry is `{ name, transport: { type: http, url } }`. Keep it lenient so a malformed entry degrades that one provider (actionable status) rather than failing the whole config parse / discarding `local_prompt_dirs`.
- Prompt cache + `sync_prompts()`: build the global cache in the **background**; `sync_prompts()` rebuilds it (wired to the Settings Sync button in M3). A slow/cold provider must not block the build — per-provider timeout, partial results allowed. `list_prompts` reads the cache; the `/` menu must never trigger a fetch.
- Failure handling: an unreachable/erroring provider contributes nothing to the cache + a warning; `render_prompt` failures return an actionable error and **never leak the bearer token**.
- **List/render consistency once the cache lands.** With a cached list, a prompt can appear in the list yet fail to resolve at render time (e.g. a local file edited to malformed after the cache was built). Decide deliberately whether render distinguishes "named-but-malformed" from `PromptNotFound` so the user gets a diagnostic for a prompt they can see. (In M1, list and render both read live from disk and stay consistent; the local provider's `find` already logs a `debug` diagnostic when it skips an unparseable file mid-resolution.)

**Definition of Done.** All hermetic — runs in default `make test`/CI, no external server. (Rationale for no live test: MCP prompts is a **standardized, versioned wire protocol** spoken through the **official `rmcp` SDK**, not a hand-parsed vendor CLI stream — the silent-drift risk that justifies harness live tests barely applies, and an in-process SDK-on-both-ends round-trip gives high confidence. A server-specific live test belongs with the Tiddly preset follow-up (M5), not here.)
- **In-process `rmcp` server test:** stand up a real Streamable-HTTP MCP server on an ephemeral `127.0.0.1` port serving canned prompts; point `McpProvider` at it. Covers `initialize`, the `prompts` capability advertisement, `prompts/list` following `nextCursor` across **multiple pages**, and `prompts/get` render. A genuine HTTP round-trip through the real SDK on both ends.
- **Pure mapping unit tests** for logic awkward to elicit from a well-behaved server: an `rmcp` `GetPromptResult` with **mixed content parts → text extracted, non-text parts dropped with a warning**; a JSON-RPC `-32602` (missing required arg) → actionable `PromptError` (no bearer in the message).
- **Dead-port test:** a provider pointed at a closed port (connection refused) **contributes nothing to the cache** (with a warning) and does **not** break local prompts or other healthy providers.
- Behavior verified: a configured generic HTTP provider's prompts appear under the provider prefix and render via the server; provider-down degrades gracefully.
- Recorded limitations: `prompts/list_changed` live subscription deferred (Sync covers refresh); no live MCP test in V1 (hermetic coverage is sufficient; server-specific drift checks land with M5).

---

### M3 — Generic MCP-server management (Settings)

**Goal & Outcome.** Let users add and manage generic MCP prompt servers from Settings, credential-backed by the keychain — the user-facing surface that makes M2's provider mechanics usable without hand-editing `config.yaml`. When done:
- A user can **add a generic MCP server** from Settings (name + URL + bearer token); the non-secret entry is written to `config.yaml` and the token to the keychain. They can see its status and remove it (which deletes both).
- Adding or removing a provider rebuilds the cached prompt list in the background.
- Every provider's prompts list and render the same way; a missing/invalid credential or unreachable server shows an **actionable provider status, never a silent empty list**.
- All credentials live in the OS keychain — never in `config.yaml`, logs, or error text.
- A **Sync** button in Settings refreshes the cached prompt list (e.g. after editing prompts on the server mid-session).

(Tiddly is configured through this same generic form in V1 — URL `prompts-mcp.tiddly.me/mcp` + a pasted PAT. The one-click "Connect Tiddly" preset is the deferred M5.)

**Implementation Outline.**
- Secret store: M2 shipped the `SecretStore` trait + in-memory impl. **M3 implements `KeyringSecretStore`** (the OS-secure-store backend) and the app injects it instead of the in-memory store, then the Add form *populates* it. **Pin `keyring` 3.x** (mature, simple `Entry` API, feature-gated cross-platform stores: `apple-native` / `windows-native` / `sync-secret-service`), **not** the just-released 4.0.1 rearchitecture (`keyring_core` + separately-registered store + global init — heavier, immature store-provider ecosystem); re-evaluate 4.x later. `keyring` is cross-platform, so one impl covers macOS/Windows/Linux; the trait seam means a no-store environment (e.g. headless Linux without Secret Service) can fall back to a different impl without touching providers. `SecretStore::get` returns `Result<Option<…>>` so the status UI can distinguish "no credential" from "secure store unavailable."
- **Generic MCP-server management** (`crates/app` commands + Settings UI): add / update / remove a generic provider. Add writes the non-secret `mcp_providers` entry (`name`, `transport.http.url`) to the user-global `config.yaml` and stores the bearer token in the keychain keyed by provider name; remove deletes both. Form fields: name, URL, token, optional "test connection." Adding/removing triggers a background cache rebuild (`sync_prompts()`).
- **Config writes round-trip the file** via `crates/core` YAML I/O — this normalizes formatting and does not preserve comments in `config.yaml`; acceptable, and power users who hand-maintain a commented config should avoid mixing in UI-driven edits.
- **Provider status:** each configured provider surfaces a credential/connection status (`ok` / `token missing` / `unreachable` / `auth failed` / `errored`) derived from the last cache build, rather than failing silently. An expired or invalid bearer surfaces as "auth failed" and the user re-enters the token — V1 generic providers do **not** auto-renew (that's M5's reconnect machinery). **This requires sharpening the M2 error taxonomy:** M2's `McpProvider` currently maps *all* listing failures (transport failure *and* a server that responded with a JSON-RPC error) to `McpConnect` ("could not reach"). M3 must distinguish transport-level failure ("unreachable"), a server-responded error ("errored"), and an auth rejection ("auth failed") — likely a distinct `McpListFailed { provider, message }` variant and matching on the `rmcp` `ServiceError` kind — so the status the user sees is accurate. (M2 left this as a log-only imprecision since nothing consumed it yet.) Also: surface a missing credential (`SecretStore::get` → `Ok(None)`) as "token missing" and a secure-store read failure (`Err`) as a distinct store-unavailable state — the `Result<Option<…>>` return exists for exactly this distinction.
- Settings UI (`SettingsView.svelte`): an **"Add MCP server"** form; a list of configured providers with status and remove; and the **Sync** action (`sync_prompts()`).

**Definition of Done.**
- Keychain round-trip test (skip cleanly where no keyring backend exists, per the project's environment-dependent-test handling); the `SecretStore` trait lets the rest of the suite use the in-memory impl.
- Behavior verified: adding a generic MCP server (name+URL+token) writes its config entry + keychain token and its prompts appear under the prefix; removing it deletes both; a provider with a missing/invalid token shows an actionable status and contributes nothing to the cache; **no secret appears in any log or error string**.
- Config round-trip test: adding/removing a generic provider leaves `config.yaml` valid and preserves the other `mcp_providers` / `local_prompt_dirs` entries (formatting/comment normalization is acceptable and documented).
- Docs: amend `docs/system-design.md` §6 to reflect generic-MCP-in-V1 + Tiddly-preset-deferred; add a one-line user-facing note to `README.md` "Harness support and limitations" (Codex/Gemini gain prompt support via Switchboard), and note that Tiddly is configured as a generic MCP server in V1.

---

### M4 — Prompt-mode composer (selection, arguments, appended text, preview, send)

**Goal & Outcome.** Deliver the full user-facing prompt flow as a structured composer — no rich-text editor. When done:
- From the compose area the user opens prompt selection via a `+` button (or `/` when the box is empty) and picks a prompt from the cached list across providers.
- Selecting a prompt switches the compose area to **prompt mode**: it keeps the "To" recipients header, shows the chosen prompt with a way to change or remove it, renders each declared argument as a labeled multi-line input (required fields validated), and shows an **Appended text** field.
- A **Preview** shows exactly what each recipient will receive — the rendered prompt plus the appended text.
- Sending renders the prompt **once** with the entered arguments, combines it with the appended text, and dispatches to all recipients; the agent receives only the final text, never the template/provider/arguments.
- Removing the prompt returns to the normal text composer (appended text carried back); plain, no-prompt messages are completely unchanged.

**Implementation Outline.**
- Entry: a `+` affordance in the compose area, plus `/` when the textarea is empty, opens a typeahead reading the **cached** global prompt list (**no network on open**; reuse the existing `@`-menu/keyboard scaffolding). Selecting a prompt enters prompt mode; if the user had already typed text, pre-fill **Appended text** with it (nothing lost).
- Prompt mode is a **state of `ComposeBar.svelte`, not a modal** (a layout swap; taller). It retains the recipients header; a prompt selector to change/remove the prompt; argument inputs generated from the prompt's declared `arguments` (multi-line, all strings; required validated and block send; optional may be empty; show each argument's description); an **Appended text** textarea. Removing the prompt reverts to the plain textarea, carrying the appended text back. This folds the former separate argument dialog into the composer — there is no modal.
- **Preview** button calls `render_prompt` and shows the full combined message (`rendered prompt` + blank line + `appended text`) — what the agent receives. Handle MCP latency/errors.
- Draft model (`composeStore.ts`): persist the structured prompt-mode state `{ provider, name, args, appendedText }` alongside the plain-text draft. **Add a `version` discriminant to the stored snapshot** (unversioned/string-only today) so the persisted shape can evolve without corrupting older drafts. Plain mode and prompt mode are distinct persisted states.
- Send — **render before dispatch**: an async `prepareSubmit` phase renders the prompt **once** via `render_prompt`, combines it with the appended text, and only **then** calls the existing `send_message` path — before any optimistic transcript turn or journal write. (Today `ComposeBar` appends the optimistic user turn and clears the draft synchronously before IPC; folding rendering in naively would leave a phantom user turn for text never sent on a render failure — worse on multi-recipient sends.) On failure: compose-level error, preserve the composer state, append no transcript/journal state. During `prepareSubmit` (an MCP `prompts/get` is a network round-trip), show a **disabled/pending send state** so the composer doesn't read as frozen; clear it on success or failure. Multi-recipient: render once, the same final text to all. **One prompt per send** (combine rule: rendered prompt + blank line + appended text).

**Definition of Done.**
- Component-level tests per the project's IPC-component rule (mock `invoke`/`listen`, capture callbacks): prompt menu opens from cache **without a network call**; entering prompt mode pre-fills appended text from prior textarea content; required-argument validation blocks send; preview renders the full combined message (and handles error); remove-prompt returns to the plain composer carrying appended text back; send combines `prompt + appended text` and dispatches **once** to N recipients; **render-fails-at-send surfaces an error, preserves composer state, and appends no transcript turn**; **send enters a pending/disabled state during an awaiting `render_prompt` and clears on success and on error**; draft round-trip preserves prompt-mode state. Use `tick()`/`waitFor` for presence assertions.
- Behavior verified: select `tiddly:<prompt>` and `local:<prompt>`, fill arguments, preview, and send to one and to many agents — each receives `rendered prompt + appended text`; plain (no-prompt) sends are unaffected.
- Recorded limitation: **one prompt per send**; inline/interleaved chips and multiple prompts are deferred (that is where a `contenteditable` editor would return — v2).

---

### M5 — Tiddly first-class preset (deferred post-V1 follow-up)

**Not part of V1.** Sequence this only after V1 (M1–M4) ships and the generic-MCP prompt flow has proven its value. It is a pure *addition* — V1's generic "Add MCP server" path keeps working for Tiddly throughout; M5 layers a one-click convenience on top. Its full blueprint already lives in this plan: **§2.3** (Auth0 device flow + PAT minting + the revocation constraint), **§4 decisions 1, 9–11, 13** (login flow, best-effort revoke, long-PAT + 401 reconnect, dedicated Auth0 app, account-wide PAT), and **§9** (the `bookmarks`-repo reference files). Re-verify §2.3's endpoints/constants against the current `bookmarks` source before starting — they were last read 2026-06-01 and Tiddly is independently maintained.

**Goal & Outcome.** A user clicks "Connect Tiddly," completes a browser login, and ends in a "Connected as `<email>`" state — no token handling at any point; disconnect best-effort revokes server-side; an expired credential renews silently on next *interactive* use (never from a background refresh).

**Scope (carried from the original M3 Tiddly half):**
- **Auth0 application prerequisite** (one-time operator task — Native/public app, Device Code + Refresh Token grants; or reuse the CLI's public client as a zero-code fallback). See the original §4 decision 11 detail.
- **Auth flow** (pure, HTTP-client-injected, in `crates/prompts`): device-code request → poll (`slow_down`/`expired`/`pending`) → mint PAT via `POST /tokens/` with the Auth0 JWT → store PAT + PAT `id` + Auth0 refresh token + email in the keychain (reusing M2's `SecretStore`, extended to a multi-field credential). Map `402`/`451`/`access_denied` to actionable errors. Identify via `GET /users/me`.
- **Tauri commands + events:** `tiddly_login_start` (returns `user_code` + `verification_uri_complete`, opens the browser via the Tauri host), `tiddly_login_poll` / progress events, `tiddly_disconnect`, `tiddly_status`.
- **`preset: tiddly` config entry:** baked-in MCP URL (`prompts-mcp.tiddly.me/mcp`) + API base (`api.tiddly.me`), env-overridable; resolves its bearer from the keychain like any provider.
- **Disconnect** (best-effort silent revoke) and **reconnect/renewal** (401 → refresh → re-mint; background context never launches a browser) per §4 decisions 9–10.
- **Settings UI:** a "Connect Tiddly" section (device-code spinner; connected state with email + disconnect; quota/consent/timeout error states).

**Definition of Done (for the follow-up):**
- Device-flow state-machine unit tests over a mock HTTP client (inject clock/interval): `authorization_pending` loop, `slow_down` backoff, expiry, `access_denied`, success, then mint success and `402`/`451` paths.
- A `#[ignore]`-gated **Tiddly live test** — this is where the live-test runner-home work lands (per §7): add `switchboard-prompts` to `LIVE_PKGS` + a `make test-live-tiddly` target and document a `live_tiddly_` category in `AGENTS.md`, **or** house it in `switchboard-app`. **This live test is a load-bearing obligation, not optional polish, and must not itself be descoped.** V1's MCP coverage is entirely hermetic (an in-process server we wrote to be compliant), so **nothing automated ever validates the *real* Tiddly prompt server** — in particular, that it advertises the MCP `prompts` capability rather than exposing prompts only as *tools* (a concrete, observed risk: Tiddly's MCP server is tool-capable too, and an upstream change could drop or alter the prompts capability). The §8 one-time manual probe confirms this as of 2026-06-02 but does not guard future drift. This live test (assert capability advertised + `prompts/list` returns + a known prompt renders via `prompts/get`) is the sole standing guard; run it before M5 ships and on every Tiddly / `rmcp` version bump.
- Behavior verified: browser login → "Connected as `<email>`" → Tiddly prompts list/render through the keychain PAT; disconnect removes access; no secret in any log/error.
- Docs: amend `docs/system-design.md` §6 back to "Tiddly is a first-class preset" and update `README.md`.

## 7. Cross-cutting requirements

These apply across milestones. Carry the *why* into the code (comments/commit messages), not just the plan.

- **No secrets in logs or error messages** — bearer tokens and PATs never appear in tracing, error strings, or test output. Redact in any diagnostic.
- **Graceful degradation** — a misconfigured or unreachable provider never breaks the compose box or local prompts; it degrades to empty-with-warning.
- **Argument semantics (settle once; touches M1 local, M2 MCP, M4 preview).** Omit unfilled optional args from the `prompts/get` call (let the server template apply its own conditionals/defaults); local MiniJinja renders missing optionals as empty. **Unknown supplied args are rejected** (error listing the valid argument names) for *both* local and MCP providers — local matches Tiddly's strict behavior so a prompt behaves identically whichever store it lives in (portability). Missing *required* args are a typed error at render; the composer also blocks send on empty required fields. **Preview and send must pass the identical argument map** so the preview never diverges from what the agent receives.
- **Determinism in tests** — inject clocks/intervals for the device-flow polling; no wall-clock or time-of-day dependencies.
- **Dependency hygiene** — add all crates via `cargo add` / `pnpm add` per `AGENTS.md`; commit manifest + lockfile together.
- **Wire-format conventions** — new IPC types follow the existing `#[serde(rename_all = "snake_case")]` + TS discriminated-union pattern; mark evolving enums `#[non_exhaustive]`.
- **No live MCP test in V1; hermetic coverage instead.** M2 proves the MCP client against an **in-process `rmcp` server** (real Streamable-HTTP round-trip, SDK on both ends) — no external server, runs in default `make test`/CI. This is appropriate because MCP prompts is a standardized, versioned protocol consumed via the official SDK, not a hand-parsed vendor CLI stream (the silent-drift risk that justifies the harness live tests barely applies). The live-test runner-home work (`live_<x>_` naming, adding `switchboard-prompts` to `LIVE_PKGS` or housing in `switchboard-app`, a `make test-live-*` target) is therefore **deferred to M5**, where a *server-specific* Tiddly live test actually needs it.

## 8. Decisions (settled) and standing tasks

**V1 (M1–M4), settled:** keychain over `config.yaml`; `rmcp`; backend-first sequencing; stdio deferred; user-global provider scope (no project arg, §6 amended); build-once cache + Settings Sync (the `/` menu never fetches); keychain for all provider secrets + a generic MCP-server management UI (no `${ENV}`); hermetic MCP testing (in-process `rmcp` server, no live test); prompt-mode composer (Option C) — single prompt + appended text, no inline chips/`contenteditable` (§4 decision 6).

**Deferred to M5 (post-V1), settled but not built in V1:** Tiddly browser device-code login → minted PAT in keychain; retain the Auth0 refresh token for disconnect/renewal; best-effort silent revoke on disconnect; long PAT expiry (~365d) + 401 reconnect, no expiry picker; dedicated Auth0 app (CLI-client reuse as a zero-code fallback); background-context 401 never launches a browser login; account-wide PAT accepted. Tiddly implements the MCP `prompts` capability and its management *tools* are unused (true regardless of milestone). See §4 decisions 1, 9–13 for rationale.

No open product/scope decisions remain.

**Standing verification obligation (accepted risk from descoping the live test).** V1 validates the MCP client only hermetically; no automated test ever exercises the *real* Tiddly prompt server. The lone guard against the most likely production failure — Tiddly's server not advertising the `prompts` capability (it is also tool-capable) after an upstream change — is the §8 one-time manual probe (current as of 2026-06-02) plus the **M5 `live_tiddly_` drift check**, which is therefore load-bearing and must not be descoped. Re-probe Tiddly's capability if a Tiddly or `rmcp` change is suspected before M5.

**Re-verified 2026-06-02** (the standing task — now mainly relevant to the deferred M5): the §2.2/§2.3 constants and endpoints were unchanged in the then-current `bookmarks` source — Auth0 domain/client/audience, `/tokens/` (Auth0-only POST/GET/PATCH/DELETE), `GET /users/me`, and the prompt MCP server (`StreamableHTTPSessionManager(json_response=True, stateless=True)`, port 8002, with `@server.list_prompts()` / `@server.get_prompt()` implemented). A **live probe** confirmed over the wire that it advertises `prompts = PromptsCapability(listChanged=False)` (and `tools`, which Switchboard ignores). **Re-verify again before starting M5** (the §2.3 Auth0/PAT endpoints especially) — V1 does not depend on these, so they may have drifted by the time the preset is built.

## 9. Reference docs (read before implementing)

- `docs/system-design.md` §6 (Prompts and prompt providers) — authoritative design; §3 (filesystem layout, config scopes); §9 (harness integration).
- MCP prompts spec: https://modelcontextprotocol.io/specification/2025-06-18/server/prompts
- `rmcp` (official Rust MCP SDK): https://github.com/modelcontextprotocol/rust-sdk
- Tiddly reference implementation (sibling `bookmarks` repo):
  - **V1-relevant** (understanding the MCP server we connect to generically): `backend/src/prompt_mcp_server/`, `README_DEPLOY.md`.
  - **M5-only** (the deferred Auth0/PAT preset): `backend/src/api/routers/tokens.py`, `backend/src/core/auth.py`, `cli/internal/auth/device_flow.go`, `cli/internal/mcp/configure.go`.
