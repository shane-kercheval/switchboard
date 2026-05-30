# Harness behavior & Switchboard handling — canonical reference

The single lookup surface for **how each harness behaves in the scenarios Switchboard cares about, how our adapter/frontend currently handles it, and where the gaps are.** Built 2026-05-27 from a code+research audit of all four adapters and the frontend.

**How this relates to the other docs:**
- **system-design §9** is the *design-level* capability matrix (native / derived / unavailable per capability) and the process model. It stays the canonical design statement; this doc is the *operational* companion (observed shapes → our handling → gaps).
- **`docs/research/archive/<harness>-cli-observed.md`** are the *captured-in-time provenance* — the raw probes (actual strings, exit codes, fixtures) behind the cells here. They are frozen (no longer updated) and live under `archive/`; treat **this** doc as the lookup surface and the archived `-observed` files as the evidence it cites: [claude](archive/claude-code-cli-observed.md), [codex](archive/codex-cli-observed.md), [gemini](archive/gemini-cli-observed.md), [antigravity](archive/antigravity-cli-observed.md).

**Legend:** ✅ handled correctly · ⚠️ partial / stale · ❌ gap. "Displayed where" distinguishes the **transcript** (per-turn) from the **sidebar** (per-agent) from a **project banner** (startup auth probe).

---

## 1. Failures

Each turn ends in exactly one terminal outcome: `Completed` · `Failed { kind, message }` · `Cancelled { source }`. `kind ∈ { HarnessError, AdapterFailure, AuthFailure }` (`crates/harness/src/events.rs:347`, `#[non_exhaustive]`). **There is no `UsageLimit`/quota kind.**

### 1.1 Turn-completion detection

| Harness | Signal | Detection |
|---|---|---|
| Claude | `result` event, `is_error:false` | `claude_code/parser.rs:parse_result` ✅ |
| Codex | `turn.completed` | `codex/parser.rs:parse_turn_completed` ✅ |
| Gemini | `result.status:"success"` | `gemini/parser.rs:parse_result` ✅ |
| Antigravity | process exit **+** terminal `PLANNER_RESPONSE` answer in `transcript.jsonl` (exit code is useless — 0 on everything) | `antigravity/parser.rs:is_terminal_answer` + `classify_outcome` ✅ |

### 1.2 Generic harness error

| Harness | Observed shape | Classified | Message | Display | Gap |
|---|---|---|---|---|---|
| Claude | `result.is_error:true` and/or `api_error_status` (`subtype` stays `"success"` — don't trust it) | `HarnessError` | verbatim `result` text | transcript red text + failed chip | exit-code/`api_error_status` mismatch is log-only |
| Codex | `turn.failed.error.message`, exit 1 | `HarnessError` | verbatim (JSON-unwrapped) | transcript | — |
| Gemini | `result.status:"error"`, `error.message`, exit 1 | `HarnessError` | verbatim | transcript | — |
| Antigravity | `Error:` line on stdout (e.g. timeout), exit 0 | `HarnessError` | verbatim `Error:` line | transcript | errors with **no** `Error:` line escape (see 1.4) |

### 1.3 Auth failure

| Harness | Observed shape (reactive, per-turn) | Classified | Surfaced message | Backend auth-presence probe (retained for getting-started surface) |
|---|---|---|---|---|
| Claude | `assistant` envelope top-level `"error":"authentication_failed"` (Claude's own text is `"Not logged in · Please run /login"`) | `AuthFailure` ✅ | Authored: `"Claude authentication required — run `claude auth login`"` (Claude's `/login` is the in-app slash command; the authored copy names the terminal CLI command — verified `claude auth login`, top-level `claude login` does not exist as of CLI 2.1.156) | `check_claude_auth_impl` (keychain `security find-generic-password -s "Claude Code-credentials"`, queried by service only; macOS-only presence heuristic). Drives only the getting-started surface, never the working UI. |
| Codex | stream `turn.failed.error.message` contains `"401 Unauthorized"` | `AuthFailure` ✅ | Authored: `"Codex authentication required — run `codex login`"` (raw 401 text replaced) | `check_codex_auth_impl` (file-presence of `~/.codex/auth.json`; has a documented stale-file false-positive). Backend command retained; **no longer drives any working-UI surface**. |
| Gemini | **bad token:** presumed `result.status:"error"` "401" — **never observed**. **logged-out:** exit **41** + stderr "Please set an Auth method…", **no stream**. **bad-token (alt):** exit **42** + 401 on stderr | All three → `AuthFailure` ✅ | Authored (uniform across all three shapes): `"Gemini authentication required — run `gemini` interactively to sign in"` | `check_gemini_auth_impl` (reads `settings.json` `security.auth.selectedType`). Backend command retained; **no longer drives any working-UI surface**. |
| Antigravity | stdout `Authentication required…` → (force-killed before the 30s OAuth wait) → `Error: authentication timed out.`, exit 0 | `AuthFailure` ✅ (`is_auth_failure_line`; break+`terminate_then_kill` bounds it) | Authored: `"Antigravity authentication required — run `agy` to authenticate"` (verified: `agy` has no login subcommand — you authenticate by running the CLI, which triggers its OAuth flow; the user need not have a desktop app) | `check_antigravity_auth_impl` (keychain `security find-generic-password -s gemini -a antigravity`). Backend command retained; **no longer drives any working-UI surface**. |

**Uniform authored auth messages (cross-cutting).** Every adapter authors its `AuthFailure` message — naming the harness and the recovery command — instead of surfacing the raw harness text (`"401 Unauthorized"`, `"Please set an Auth method"`, `"Not logged in · Please run /login"`). The user sees one consistent actionable line regardless of which harness's auth surface fired. **None mention "reload Switchboard"** — reactive-auth posture: discover on send, sign in, send again. Constants: `CLAUDE_AUTH_MESSAGE` / `CODEX_AUTH_MESSAGE` / `GEMINI_AUTH_MESSAGE` (`parser.rs` / `codex/parser.rs` / `gemini/parser.rs`); `ANTIGRAVITY_AUTH_MESSAGE` (`antigravity/mod.rs`).

**Reactive-only auth (working UI).** Auth has exactly one user-facing surface in the working UI: a failed turn in the transcript carrying the authored message. There is no proactive startup probe, no project-level "X not authenticated" banner, no per-agent status icon, and no agent-creation auth gate. A logged-out harness is discovered when the user sends to one of its agents; sign in, send again. This is the same path for "never tried" and "tried and failed" — one mental model, nothing to go stale. The backend `check_*_auth` Tauri commands feed the getting-started surface (no-project state), not the working UI.

**Proactive status (`HarnessStatusList`, two opt-in placements).** Install/auth status is shown proactively in exactly two places — the **no-project orientation surface** and a **Settings → "Supported CLIs"** section — both rendering the shared `HarnessStatusList`. Per harness it shows install status (present-on-PATH + version, via `get_harness_install_status` over the trait `HarnessAdapter::version()`) and a best-effort auth ✓/✗ (the four `check_*_auth` probes, including the Claude keychain probe). All four harnesses are uniform ✓/✗ — no "unsupported"/"?" case. The auth marks are **presence heuristics, not validity checks** (the authoritative test is a successful send); a ✗ never blocks anything, and an API-key user may show ✗ yet send fine. Probes refresh on mount and on window refocus (`visibilitychange`), so both placements stay current and neither reintroduces the removed interruptive/stale mid-work banner. The **version** is the parsed semver token (`subprocess::parse_cli_version`), not the raw `--version` line (CLIs pad differently: `2.1.156 (Claude Code)`, `codex-cli 0.134.0`, bare `0.44.0`); **no "update available" detection** — the CLIs self-update/notify, and a remote latest-version comparison is external-data-currency burden (§37) we don't take on. The "fix" hints name the **verified** recovery command per harness (`claude auth login`; `codex login`; run `gemini` / `agy`, which have no login subcommand). Caveat parity with the reactive probes: Claude's keychain presence heuristic is macOS-only and can false-positive on a stale entry (as Codex's `auth.json` check already does).

### 1.4 Quota / usage-limit

| Harness | Observed shape | Current handling | Gap |
|---|---|---|---|
| Codex | `turn.failed.error.message` = "You've hit your usage limit … try again at 1:00 PM" (exit 1) | `HarnessError`, **verbatim message preserved** (reset time + upgrade link reach the user) | ✅ accurate message; just not *distinguished* from other harness errors |
| Claude | normal turn emits `rate_limit_event` `{status:"allowed", resetsAt, rateLimitType}`; **overage turn** adds `isUsingOverage:true` (+ `status:"rejected"`, `overageResetsAt`). The window reset is **independent of overage** (present every turn). | captured into `last_rate_limit`. Sidebar shows a **neutral** "`<label>` resets `<clock>`" window line on every turn (label from `rateLimitType`), plus an **amber** "⚡ using credits" escalation only when `isUsingOverage`. **Staleness = reset-passed**: a window whose reset is in the past clean-hides (no dim, no time threshold). Tooltip carries full reset dates (5-hour + weekly `overageResetsAt`) + a "snapshot from …" line when rehydrated. Restart-durable via the metadata sidecar. | ✅ G7 closed — surfaced informationally (not a failure render; the turn succeeded) |
| Antigravity | `rpc error: code = ResourceExhausted desc = Individual quota reached. … Resets in <duration>` on the per-dispatch CLI log; stdout/stderr/transcript all empty, exit 0 | ✅ `HarnessError` with authored prefix + the log line's `Resets in …` tail. The adapter passes `--log-file <per-dispatch-temp>` so the scan reads only *this* turn's log (no cross-attribution under concurrent dispatch). Unknown `rpc error: code = <CODE>` lines pass through as `"Antigravity error: <line>"`. | Display-only: we surface "quota exhausted" + the reset duration the log carries, but never parse it into structured retry metadata or schedule a retry. |
| Gemini | free-tier exhaustion observed to **retry-and-complete**, not hard-fail | nothing to classify | record as "stalls, no hard wall" (no action) |

### 1.5 Cancellation

Uniform and correct across all four: token-driven (`select!`), adapter kills its process group and ends the stream with **no** terminal event, dispatcher synthesizes `Cancelled { source }`. Codex/Gemini/Antigravity all exit 0 on SIGTERM, which the token path correctly sidesteps. ✅ (each has a `live_<harness>_cancel_*` test).

### 1.6 Adapter / parse failure

All four synthesize `AdapterFailure` for malformed JSON, stdout read errors, and EOF-without-terminal (with the stderr tail appended); the frontend `heartbeat_timeout` path also synthesizes one. ✅ Gemini-logged-out and Antigravity-quota used to mis-land here; both now route to their correct `AuthFailure` / `HarnessError` classifications with authored messages (see G1/G2 in §4 — closed).

---

## 2. How failures surface in the UI (cross-cutting — the biggest finding)

**`error_kind` is carried to the frontend but read by zero components.** The Rust classification (`AuthFailure` vs `HarnessError` vs `AdapterFailure`) flows through `reducers.ts` into `Turn.error_kind` — and `UnifiedTranscript.svelte` renders *every* failed turn identically: the verbatim `turn.error` message in red (`text-status-failed`) + a "failed" `StatusChip`. This is by design now: a per-kind render would split the auth surface across two places (transcript + something else), reopening the disjoint-surfaces problem we just removed. The classification is still wire-carried (kept for future retry-policy decisions); the lever is the *message*, not the render.

**"Transcript disappears" when unauthenticated:** under reactive-only auth, an unauthenticated harness writes no session file, so hydration shows zero turns (correctly empty); the user discovers the auth state by sending, at which point the transcript renders the failed turn with the authored message. No banner stacks above to compete with the transcript content. The empty body is just the no-history state, not a "something is wrong" signal.

---

## 3. Metadata exposed vs displayed

| Field | Claude | Codex | Gemini | Antigravity |
|---|---|---|---|---|
| **Cost ($)** | ✅ `result.total_cost_usd` → sidebar "$" (gated `claude_code && cost>0`) | ❌ none (subscription model) | ❌ none | ❌ none (server-side) |
| **Context / tokens** | ✅ `modelUsage.contextWindow` + tokens → context bar | ⚠️ window only via **session-file enrichment** (bar hidden if enrichment failed); tokens from stream | ⚠️ tokens captured but **`context_window` always `None`** → bar **never renders**; `thoughts`/`tool` token buckets dropped | ❌ none |
| **Rate-limit / quota** | ✅ independent neutral window line (`resetsAt`/`rateLimitType`, every turn) + amber "⚡ using credits" escalation (`isUsingOverage`) → G7 closed; reset-passed clean-hide (no dim/threshold), both reset windows + snapshot age in the tooltip; restart-durable via the metadata sidecar | ✅ both windows surfaced — primary + secondary gauge lines (labeled from `window_minutes`) with both `resets_at` in the tooltip (G8 closed); `plan_type` still unused. Reset-passed gated; class B (durable). | ❌ none emitted | ❌ none (the CLI-log `RESOURCE_EXHAUSTED` line drives the failure message but is not a sidebar metric) |
| **Model** | ✅ `system/init.model` → sidebar | ✅ session-file `turn_context.model` (first-turn only) | ✅ `init.model` → sidebar | ⚠️ fragile — string-scrape of a `<USER_SETTINGS_CHANGE>` sentence; **empty on resume turns**, degrades to prior/blank |
| **MCP servers** | ✅ count (stream + config) | ✅ count (config only) | ✅ count (config) | ✅ count (user-scope config; workspace-scope not scanned) |
| **Skills** | ✅ count | ✅ count | ✅ count | ✅ count |

**Sidebar absent-field convention (decided — clean-hide):** every metadata cell is `{#if present}`-gated and **simply omitted** when its value is missing/zero — no `—`, no "n/a". The UI deliberately does **not** distinguish "this harness never reports cost" (permanent — e.g. Antigravity) from "no turn has run yet" (transient — e.g. a fresh Claude agent): both render nothing, which is the intended behavior (G10 closed by decision, not by a capability map). MCP/skills show bare **counts**, no names/status.

### 3.1 Event ⟂ on-disk parity (what survives restart)

For metadata-flavored fields, where a datum lives determines whether Switchboard can re-show it after the app restarts. The TUI never has this question — it doesn't restart mid-session — so any field Switchboard loses on restart is a **TUI-parity gap**. Four classes:

| Class | Meaning | Restart behavior |
|---|---|---|
| **A. Full parity** | Stream + harness session file agree | ✅ Survives — re-read from session file |
| **B. Disk-canonical (we enrich)** | Stream lacks it; session file has it; we re-read at turn-end | ✅ Survives — same source on rehydrate |
| **C. Stream-only, in-memory** | Lives in events; no on-disk equivalent; held in `Runtime` per agent | ❌ **Lost** until next event |
| **D. Absent everywhere** | Not in stream, not on disk, not in any harness file | — Impossible — TUI parity ceiling (workaround at best) |

| Field | Claude | Codex | Gemini | Antigravity |
|---|---|---|---|---|
| Assistant text / tool calls / per-turn final usage | A | A | A (B for read-tool output) | (file is the only source — A by construction) |
| `rate_limit` snapshot | **C** | B (session file) | — (none emitted) | D (CLI log only) |
| Overage flag (`isUsingOverage`) | **C** | n/a | n/a | n/a |
| Context window | A (`modelUsage`) | B (session file) | D | D |
| Model | A | B | A | D-ish (fragile `USER_SETTINGS_CHANGE` scrape, empty on resume) |
| MCP servers — live connection status | **C** (`system/init`) | D (config-only) | D (config-only) | D (config-only) |
| Tools registry | A (`system/init`) | D | A | D |

**The C cells are the restart gaps.** Two were identified: **Claude `rate_limit_event` (used %, `isUsingOverage`, reset times)** and **Claude `system/init.mcp_servers[].status`**. The first is now **closed** by the per-agent metadata sidecar (G13) — it's written on each `StreamOnly` rate-limit event and re-read on project open, with an "as of …" qualifier on the rehydrated snapshot. The second (MCP live status, G14) remains open — also class-C, but deferred because a stale "up 3 days ago" indicator may be worse than none. (Mid-stream token deltas across all four are also class-C but have no UI surface today, so the loss is theoretical.) The sidecar persists **only** the class-C `rate_limit` payload; class-B sources (Codex's session-file rate-limit) are not duplicated into it — the gate is the event's `RateLimitSource`.

**Class B is *not* a gap** — we already re-read the session file at turn-end (Codex rate-limits, Codex model/context, Gemini tool output) and on project open, so the data is durable. The latency cost (one extra file read) is the trade.

**Class D is what no implementation can fix without external data** — e.g. Gemini does not emit a context window anywhere; Antigravity emits no token usage to disk. Document these as TUI parity ceilings; workarounds (hardcoded model→context maps) are case-by-case decisions.

### 3.2 Reasoning / thinking content (exposed vs surfaced)

Distinct from the `thoughts`/`reasoning` **token buckets** in the §3 table (those are counts) — this is the model's chain-of-thought **prose**, normalized to `ContentKind::Thinking` turn items (`crates/harness/src/events.rs`) so the frontend can render reasoning subordinate to the answer. Evidence cited from the frozen `archive/<harness>-cli-observed.md` probes.

| Harness | On-disk / wire source | Availability | We emit `Thinking`? | Notes |
|---|---|---|---|---|
| **Antigravity** | `thinking` string on `PLANNER_RESPONSE` records (`transcript.jsonl`) | **Always present** when the model reasons — and the tool-calling record's narration lives in `thinking`, not `content` | ✅ **live + disk** — `antigravity/parser.rs` (live tail) and `antigravity/session_file.rs` (hydrate) | Best-grounded path; verified, tested (archive antigravity §256/265/470) |
| **Gemini** | `thoughts:[{subject, description, timestamp}]` on `gemini` session records | **Opportunistic** (per-turn model decision); **disk-only — never in the live stream** (re-confirmed 2026-05-29 @ 0.44.0, model `auto`: stream carried no thoughts, the session file did) | ⚠️ currently emits **on hydrate** (`gemini/session_file.rs`); **M4.10 decision: remove** — reopened-only reasoning is stale UX (valuable at stream time, not hours later) | Open: the **`[Thought: true]`** live literal (different model config; not reproduced under `auto`) — see §5 |
| **Claude** | `thinking` content block (`type:"thinking"`) — **text redacted to empty `""`, only a `signature`** | Reasoning *event* arrives (the block, plus `reasoning_output_tokens`), but the **text is server-stripped** in `-p`/programmatic mode | ❌ **nothing to surface** — the block carries no prose | **Tracked upstream regression (§7)** — *not* "we drop it": the CLI withholds the text. Probed 2026-05-29 @ 2.1.157 (`MAX_THINKING_TOKENS` forces a block; `--verbose`): `thinking_len=0` live + on disk. `showThinkingSummaries: true` in **global** settings tested → still empty (workaround dead for us). Server-flag-gated, so possibly temporary — re-probe on CLI bump |
| **Codex** | `reasoning` `response_item` with **`encrypted_content`** | Reasoning *happened* is visible; the content is **encrypted** — unrecoverable | ❌ **impossible by design** — a class-D ceiling, not unbuilt work | Only the `reasoning_output_tokens` count surfaces; the prose can never be shown (archive codex §60/162) |

**Surfacing (frontend).** A `Thinking` item renders with a visually subordinate treatment (muted, labeled, collapsible) distinct from the answer `Text` that follows; the reasoning prose inside still renders through the shared `<Markdown>` primitive. **Antigravity is the only harness that gives us live reasoning text**, so it is the one M4.10 actually serves. **Gemini** is disk-only (never streams — confirmed 2026-05-29 @ 0.44.0: stream had no thoughts, the session file did), so reopened-only reasoning is dropped rather than shown as stale (decision). **Claude** withholds the text (redaction regression, §7); **Codex** encrypts it. The widget is built for Antigravity and is forward-compatible: if Claude's redaction lifts, it picks Claude up for free.

---

## 4. Gap register (actionable)

Grouped by theme; this is the candidate scope for the failure/metadata-surfacing milestone.

**Failure accuracy:**
- ✅ **G1 — Antigravity quota misclassified.** **Closed.** Per-dispatch `--log-file` isolation + a `rpc error: code = …` scan on the no-answer branch yields `HarnessError` with an authored "quota exhausted" prefix and the log line's `Resets in <duration>` tail. Unknown codes pass through as `"Antigravity error: <line>"`. Fixture-driven concurrent-isolation test asserts no cross-attribution. See `antigravity/mod.rs::scan_agy_log_for_error`.
- ✅ **G2 — Gemini logged-out misclassified.** **Closed.** `synthesize_terminal_failure` now recognizes exit 41 + "Please set an Auth method" as `AuthFailure`; the existing exit-42 401 path also rewrites to the same authored `AuthFailure` message. All three Gemini auth surfaces (in-stream 401, exit-41, exit-42) emit the same authored copy. See `gemini/mod.rs::synthesize_terminal_failure`.

**Failure rendering (cross-cutting):**
- **G3 — `error_kind` is never rendered.** Decide a consistent treatment (per decision: *not* a bespoke per-kind UI — quota/auth render like any failure, just with the accurate message). The lever is the message + possibly a single shared affordance, not N renders. Note this also subsumes the unbuilt "AuthFailure banner."
- ✅ **G4 — Claude has no auth pre-flight.** **Closed.** No auth pre-flight is the *intended* working-UI behavior for every harness now — auth is reactive-only. A presence-only Claude keychain probe (`check_claude_auth_impl`, service `"Claude Code-credentials"`, macOS-only) now feeds the getting-started surface (no-project state), not a working-UI banner — so all four harnesses show uniform ✓/✗ there.
- ✅ **G5 — Auth banner is reload-gated.** **Closed by removal.** The banner that needed reloading no longer exists.
- ✅ **G6 — "Transcript disappears" on unauth.** **Closed by reframing.** An unauthenticated agent's empty transcript is now what the user sees alongside no banner; sending surfaces the authored `AuthFailure` turn in the transcript itself. The "transcript disappeared" perception was driven by the stacked banner + empty body; with the banner gone, the empty state and the failed-turn-on-send are the same coherent reactive surface.

**Metadata surfacing:**
- ✅ **G7 — Claude overage invisible.** **Closed.** The Sidebar reads the opaque `last_rate_limit` payload (`Sidebar.svelte::rateLimitView`, defensive shape-read) into **two independent signals**: a **neutral** primary-window line ("`<label>` resets `<clock>`", label from `rateLimitType`) shown on *every* turn — **not** gated on overage — and an **amber** (`warning` token) "⚡ using credits" escalation layered on only when `isUsingOverage === true` (informational, not a red failure render; Claude serves overage transparently). **Staleness is reset-passed, not age-based**: each window renders only while its reset timestamp is in the future; a window whose reset has elapsed clean-hides (the reset time is absolute, so it's accurate until it passes — there is no dim and no time threshold; `isOverageStale` was considered and removed). Survives restart via the M3 metadata sidecar; an always-present tooltip carries the full reset dates (5-hour + the weekly `overageResetsAt`, which has no inline home) plus a "snapshot from …" line whenever the value was rehydrated (`as_of` set), regardless of age. A rehydrated snapshot whose windows have *all* elapsed clean-hides entirely — no standalone refresh nudge — which is the intended clean-hide behavior.
- ✅ **G8 — Codex rate-limit under-surfaced.** **Closed.** The Sidebar now renders **both** windows as independent gauge lines — primary (~5-hour) and secondary (weekly), labeled from each window's `window_minutes` (300 → "5-hour", 10080 → "weekly", else "quota") — with both `resets_at` times in the tooltip (`Sidebar.svelte::codexRateLimitView`). Same reset-passed rule as Claude: a window whose `resets_at` has elapsed is dropped (its % is from a cycled window). Session-file-backed (class B), so no snapshot-age qualifier. A bare `{primary:{used_percent}}` payload (no `window_minutes`) still falls back to the legacy "quota used: N%" copy.
- ✅ **G9 — Gemini context bar never renders** (`context_window` never populated). **Reaffirmed as correct, not a gap.** Gemini exposes no context window anywhere, so the bar's `{#if util}` gate hides it cleanly — a capable-absence, the intended clean-hide behavior (G10).
- ✅ **G10 — Sidebar absent-field convention.** **Closed by decision: clean-hide for both permanent and transient absence.** Every metadata cell is `{#if present}`-gated and simply omitted when its value is missing; no `—` / "not reported" placeholder and no per-harness capability map (a value a harness can never report and a value a fresh agent hasn't produced yet both render nothing — intended). Pinned by the `Sidebar clean-hide for absent metadata` tests.
- **G11 — Antigravity model metadata fragile** (string-scrape, empty on resume). Out of scope here; unchanged.

**Wire model:**
- **G12 — no `FailureKind::UsageLimit`.** Per the "render like any failure" decision, quota can stay `HarnessError` with an accurate message — so a new variant is likely **unnecessary**. Confirm before adding one.

**Restart continuity (TUI parity):**
- ✅ **G13 — Claude class-C metadata lost on restart.** **Closed.** A per-agent **metadata sidecar** at `<directory>/.switchboard/projects/<project-id>/sessions/<agent-id>.meta.json` (harness-agnostic, keyed on `AgentId`) captures the latest stream-only snapshot. Persistence is gated by an internal `RateLimitSource` discriminator on `AdapterEvent::RateLimitEvent` — `StreamOnly` (Claude) is written to the sidecar by the dispatcher; `SessionFileBacked` (Codex, class B) is not (the harness file is already durable). The gate is on `source`, not harness identity, so the dispatcher stays harness-agnostic. On project open the app overlays the sidecar onto the loaded transcript (`load_agent_transcript`, fill-if-empty so a class-B session value always wins) and surfaces `last_rate_limit` + a `last_rate_limit_as_of` capture time. The frontend surfaces a "snapshot from …" qualifier in the tooltip whenever `as_of` is set (i.e. the value was rehydrated from disk), not gated on an age threshold; a live `rate_limit_event` clears `as_of` to null (the value is no longer an on-disk snapshot). Note: M4 uses `as_of` only for that tooltip qualifier — the *display/hide* of each window is driven by reset-passed (window reset in the past → hidden), not by `as_of` age. The sidecar is best-effort (missing/corrupt → absent, never blocks hydration) and atomic (`.tmp` + same-dir `rename`). See §3.1 for the parity class definition. **G14 (MCP live status) remains open** — also class-C but deferred pending a UX decision (a stale "up 3 days ago" may be worse than nothing).
- **G14 — Claude MCP live-status lost on restart.** `system/init.mcp_servers[].status` is class-C. Config-loader fallback gives us the *registry* (servers exist) without status (up/down). Not in immediate plan scope; a stale "this server was up 3 days ago" indicator may be worse than no indicator — needs a UX decision before persisting.

---

## 5. Open captures / unverified

- **Gemini `[Thought: true]` live-stream literal (§3.2)** — observed in-app inside a `message` stream-json event's `content`, under a newer/different model config than the original headless probe (which yielded `thoughts:[]` in the stream, never reasoning). Unknown whether it's a reasoning marker we should reclassify to `ContentKind::Thinking` (strip the prefix, emit the remainder) or plain model-authored text. Capture needs `gemini -p "<prompt>" --output-format stream-json` against that same config. Until captured, the live stream surfaces no Gemini reasoning and `[Thought: true]` falls through as `Text`.
- **Gemini bad-token (401) auth shape** — never observed; our substring detector is a guess. Capture needs a *stale-but-present* token (a clean logout gives the exit-41 shape instead).
- **Gemini hard quota wall** — none observed (it stalls/retries). Treated as "nothing to classify."
- **Claude hard quota wall** (overage disabled/exhausted) — not yet hittable; only the soft `isUsingOverage` path is captured.
- **Antigravity network-failure / other RPC error codes** in the CLI log — only `RESOURCE_EXHAUSTED` captured; a generalized log scan would surface others (`UNAUTHENTICATED`, etc.).

## 6. Version notes

- **Claude 2.1.150+ (2026-05-27) — session-file `tool_use` / `tool_result` ordering is NOT strictly time-ordered.** A `tool_result` record can appear in the file *before* its matching `tool_use` (within the same turn; ~1s gap observed in session `22300f1b-3efe-4dbc-a4a0-7c1c954d1da2.jsonl` lines 1406/1408 and 1607/1609). Our session-file parser (`crates/harness/src/claude_code/session_file.rs`) tolerates this via a deferred-results queue on `ReconstructionState`: tool_results that can't immediately bind to a known tool_use are queued; `handle_assistant` drains matches when the corresponding tool_use arrives; `finalize` flushes anything still unmatched as warnings. Per-session scope — if a future Claude version delays a `tool_use` across a turn boundary, the queue still binds but possibly to the wrong turn's tool_use; revisit if observed. Test coverage: `out_of_order_tool_result_before_tool_use_binds_via_deferred_queue`, `tool_result_before_any_assistant_record_binds_when_assistant_arrives`, `unmatched_tool_result_surfaces_as_warning_on_returned_transcript`, plus the live `live_claude_tool_results_bind_after_restart`.
- **Claude 2.1.153 (2026-05-27) — subagent stream/disk behavior re-verified.** A fresh probe against 2.1.153 shows the live stream emits **three** distinct parent-tagged record shapes during a delegating turn (vs. the two shapes documented from the May 24 probes against 2.1.149/150): a `user` envelope with `text` content (the subagent's task instruction relayed), an `assistant` envelope with the subagent's internal `tool_use` (e.g. `Bash`), and a `user` envelope with the subagent's internal `tool_result`. **Disk-side behavior unchanged and verified at scale**: zero non-null `parent_tool_use_id` values across 317 historical main session files + their `<session-id>/subagents/agent-<id>.jsonl` sidecars — subagent internals always go to the sidecar, the main file always collapses to the parent's `Agent` `tool_use` + aggregate `tool_result`. Validates the conservative skip-on-non-null rule in [`2026-05-27-claude-session-file-parser-fidelity.md`](../implementation_plans/2026-05-27-claude-session-file-parser-fidelity.md) M1 and confirms its "no disk change expected" claim.
- **Codex 0.134.0 (2026-05-26) changed the usage-limit copy** — "Display workspace usage limit error copy from response header" ([#24114](https://github.com/openai/codex/pull/24114)): the out-of-credits / usage-limit `turn.failed` message is now workspace-specific with distinct credit vs. spend-cap variants, sourced from the response header. The §1.4 Codex capture ("You've hit your usage limit … try again at 1:00 PM") is from 0.133.0 and may now read differently. **No code impact** — Switchboard classifies Codex quota as `HarnessError` and passes the message **verbatim**, so display is resilient to the copy change; recapture the exact 0.134.0 string only if substring detection is ever added.
- Other 0.134.0 changes reviewed against our `codex exec --json` surface are **non-impacting**: `trace_id` added to `turn.started` (#23980) — we skip that event entirely; legacy `--profile`/profile-v1 removal (#24051/#24059) — we pass no `--profile` flag, though a user's legacy `[profiles]` config could surface as a harness error (user config, not our invocation); MCP `config.toml` additions (per-server env / OAuth, #23583/#24120) are additive.
- **Rate-limit event contracts live-verified — claude 2.1.156 / codex 0.134.0 (2026-05-28).** The metadata surfaces M3/M4 depend on are now asserted in the live tests (run on these versions), not just fixtures: **Claude** emits a `rate_limit_event` on a *normal* turn (no overage needed) carrying `resetsAt` (epoch) + `rateLimitType` (`"five_hour"`), lifted to a `StreamOnly` `RateLimitEvent` (→ persisted to the metadata sidecar). **Codex** emits `token_count.rate_limits` with `primary.{used_percent, window_minutes, resets_at}`, marked `SessionFileBacked`. `live_claude_basic_turn_completes` / `live_codex_basic_turn_completes` assert these shapes so a future CLI rename/drop is caught live. **Not live-coverable (forcing required, fixture-only):** Claude overage (`isUsingOverage` — needs an exhausted window), Antigravity `RESOURCE_EXHAUSTED` quota, Gemini exit-41 logged-out, and all auth-failure shapes — these need a destructive/rare state to trigger, so they stay fixture-backed + recapture-on-bump per `harness-update-review.md`.

---

## 7. Tracked upstream harness issues

Live upstream bugs we watch because they change what Switchboard can **show** or **do**. Re-check on each CLI bump (`harness-update-review.md`); update the State column from `gh issue view <n> --repo <repo>`. Status snapshot **2026-05-29**.

### 7.1 Claude extended-thinking redaction & resume-wedge

All one root cause: since **v2.1.69** Claude Code requests the `redact-thinking-2026-02-12` API beta, which **strips the text from `thinking` blocks** (empty `thinking:""`, `signature` retained). Decompiled gate (#31326): `showThinkingSummaries !== true && getFeatureFlag("tengu_quiet_hollow", …)` — the flag is **server-controlled and currently on**, so the redact header ships unless the user sets `showThinkingSummaries: true`. Two consequences: (a) reasoning text is invisible to any programmatic consumer (us), and (b) the signed-but-empty blocks, re-sent on `--resume`, get a permanent `400`.

| Issue | What | State (2026-05-29) | Switchboard exposure |
|---|---|---|---|
| [#63147](https://github.com/anthropics/claude-code/issues/63147) | Resume of an extended-thinking session fails permanently with `400 "thinking blocks cannot be modified"` (empty-text-but-signed blocks re-sent) | **OPEN** — very active; confirmed 2.1.145–2.1.154, opus-4-7 **and** 4-8 | **Verified NOT exposed on the normal path @ 2.1.157 (2026-05-29).** Reproduced the precondition (Switchboard's exact `--session-id`→`--resume` pattern, dev's global `alwaysThinkingEnabled`, no `MAX_THINKING_TOKENS`): turn 1 wrote interleaved empty-text-but-signed thinking blocks to disk; **7 chained resume turns, blocks accumulating 2→7, all `is_error=false`** — the 2.1.152 signature-stripping safety-net covers the completed-turn resume flow. **Residual untested risk:** the **cancel/interrupt** path — reporters' worst repros were *interrupted* turns (closing message never generated, 27 parallel tool calls), and Switchboard *does* cancel turns. Low-to-moderate, version-dependent. **No mitigation needed now**; re-probe on CLI bump, and test the cancel→resume sequence if chasing full coverage. If ever exposed: set `DISABLE_INTERLEAVED_THINKING=1` in the adapter's spawn env (keeps reasoning, scoped to Switchboard). |
| [#31326](https://github.com/anthropics/claude-code/issues/31326) | Thinking text empty since v2.1.69 — signature only; root-cause decomp + `showThinkingSummaries` workaround | **CLOSED** (`NOT_PLANNED`, 2026-04-13) | Reasoning text **unavailable** to us → Claude surfaces no reasoning (§3.2). |
| [#20127](https://github.com/anthropics/claude-code/issues/20127) | `--output-format stream-json` stopped emitting thinking blocks since v2.1.8 | **CLOSED** | Same root cause; our exact invocation. |
| [#32810](https://github.com/anthropics/claude-code/issues/32810) | Thinking empty in session JSONL since 2.1.72 | **CLOSED** (`NOT_PLANNED`) | Affects the disk path we hydrate from. |
| [#32997](https://github.com/anthropics/claude-code/issues/32997) | `tengu_quiet_hollow` redaction flag (filed as a safety concern) | **CLOSED** | Names the server flag driving the redaction. |

**Workaround verdict (our test, 2026-05-29 @ 2.1.157).** `showThinkingSummaries: true` set in **global** `~/.claude/settings.json` + `--verbose` + a reliable agentic trigger → thinking blocks still `thinking_len=0`. It **worked for early reporters** (~2.1.69, March 2026) but **fails on 2.1.150+** (Slider-8 hit the bug with the setting on; Hillier98 "partial") and **fails for us**. The empty-thinking issues were closed `NOT_PLANNED` and maintainers signal the restriction is **intentional** ("defense against distillation"), so treat Claude reasoning text as **unavailable until proven otherwise**, and re-probe (not re-assume) on each bump.

### 7.2 Gemini live-stream reasoning

| Issue | What | State | Switchboard exposure |
|---|---|---|---|
| (no upstream issue) | `[Thought: true]` literal seen in a live `message` stream under a non-default model config; not reproduced under `auto` (2026-05-29 @ 0.44.0) | n/a | Open capture (§5). If it proves to be a live reasoning marker, parse → `Thinking`; else leave as text. Disk-only thoughts are being dropped regardless (§3.2). |
