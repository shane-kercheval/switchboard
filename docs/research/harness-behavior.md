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

| Harness | Observed shape (reactive, per-turn) | Classified | Proactive startup probe → project banner |
|---|---|---|---|
| Claude | `assistant` envelope top-level `"error":"authentication_failed"` + "Not logged in · Please run /login" | `AuthFailure` ✅ (classified) | **❌ none** — Claude is hardcoded `auth:"unsupported"` (`App.svelte`); no pre-flight, no banner. Discovered only by sending a turn. |
| Codex | stream `turn.failed.error.message` contains `"401 Unauthorized"` | `AuthFailure` ✅ | ✅ `check_codex_auth_impl` (file-presence of `~/.codex/auth.json`; has a documented stale-file false-positive) |
| Gemini | **bad token:** presumed `result.status:"error"` "401" — **never observed**. **logged-out:** exit **41** + stderr "Please set an Auth method…", **no stream** | bad-token→`AuthFailure` (guessed substrings); **logged-out→`AdapterFailure` ❌** (exit 41≠42; substring set misses it) | ✅ `check_gemini_auth_impl` (reads `settings.json` `security.auth.selectedType`) |
| Antigravity | stdout `Authentication required…` → (force-killed before the 30s OAuth wait) → `Error: authentication timed out.`, exit 0 | `AuthFailure` ✅ (`is_auth_failure_line`; break+`terminate_then_kill` bounds it) | ✅ `check_antigravity_auth_impl` (keychain `security find-generic-password -s gemini -a antigravity`) |

**Two disjoint auth surfaces** (cross-cutting): (a) a **startup probe** → a project-level red banner "X not authenticated — … reload Switchboard" (Codex/Gemini/Antigravity only; **reload-gated** — runs once in `onMount`, signing in mid-session doesn't clear it); (b) a **reactive per-turn `AuthFailure`** → renders as a *generic* red error in the transcript (see §2). They never interact.

### 1.4 Quota / usage-limit

| Harness | Observed shape | Current handling | Gap |
|---|---|---|---|
| Codex | `turn.failed.error.message` = "You've hit your usage limit … try again at 1:00 PM" (exit 1) | `HarnessError`, **verbatim message preserved** (reset time + upgrade link reach the user) | ✅ accurate message; just not *distinguished* from other harness errors |
| Claude | **turn SUCCEEDS** (overage served). Only signal: `rate_limit_event` `isUsingOverage:true` (+ `status:"rejected"`, `resetsAt`, `overageResetsAt`) | captured verbatim into `last_rate_limit` | ❌ **displayed nowhere** — no "spending usage credits" indication |
| Antigravity | `RESOURCE_EXHAUSTED` **only in the per-invocation CLI log** `~/.gemini/antigravity-cli/log/cli-*.log`; stdout/stderr/transcript all empty, exit 0 | **❌ misclassified** → falls through to generic `AdapterFailure` "agy exited without producing an answer" (`mod.rs:~1004`). No log scan exists. | ❌ the user's exact confusing case — a hard quota wall looks like a transient crash |
| Gemini | free-tier exhaustion observed to **retry-and-complete**, not hard-fail | nothing to classify | record as "stalls, no hard wall" (no action) |

### 1.5 Cancellation

Uniform and correct across all four: token-driven (`select!`), adapter kills its process group and ends the stream with **no** terminal event, dispatcher synthesizes `Cancelled { source }`. Codex/Gemini/Antigravity all exit 0 on SIGTERM, which the token path correctly sidesteps. ✅ (each has a `live_<harness>_cancel_*` test).

### 1.6 Adapter / parse failure

All four synthesize `AdapterFailure` for malformed JSON, stdout read errors, and EOF-without-terminal (with the stderr tail appended); the frontend `heartbeat_timeout` path also synthesizes one. ✅ This is the catch-all that Gemini-logged-out (1.3) and Antigravity-quota (1.4) **wrongly** land in.

---

## 2. How failures surface in the UI (cross-cutting — the biggest finding)

**`error_kind` is carried to the frontend but read by zero components.** The Rust classification (`AuthFailure` vs `HarnessError` vs `AdapterFailure`) flows through `reducers.ts` into `Turn.error_kind` — and `UnifiedTranscript.svelte` renders *every* failed turn identically: the verbatim `turn.error` message in red (`text-status-failed`) + a "failed" `StatusChip`. No branch on `error_kind` exists (grep: only a test references it). So:
- An `AuthFailure` looks exactly like a generic error — no "run `claude login` / `codex login`" affordance, despite `state/types.ts` documenting that intent and `events.rs` claiming a "subscription auth required banner."
- The per-turn auth failure and the project-level auth banner are unrelated code paths.

**"Transcript disappears" when unauthenticated (the screenshot):** not an explicit hide. An unauthenticated harness writes **no session file**, so hydration finds nothing and the agent renders with zero turns (empty), *and* the reload-gated red banner stacks above. The empty body is the project-state branch, not the banner replacing the transcript.

---

## 3. Metadata exposed vs displayed

| Field | Claude | Codex | Gemini | Antigravity |
|---|---|---|---|---|
| **Cost ($)** | ✅ `result.total_cost_usd` → sidebar "$" (gated `claude_code && cost>0`) | ❌ none (subscription model) | ❌ none | ❌ none (server-side) |
| **Context / tokens** | ✅ `modelUsage.contextWindow` + tokens → context bar | ⚠️ window only via **session-file enrichment** (bar hidden if enrichment failed); tokens from stream | ⚠️ tokens captured but **`context_window` always `None`** → bar **never renders**; `thoughts`/`tool` token buckets dropped | ❌ none |
| **Rate-limit / quota** | ⚠️ `isUsingOverage`/`resetsAt` captured into `last_rate_limit` but **not displayed** | ⚠️ sidebar shows **only** `primary.used_percent`; secondary/weekly window, `plan_type`, `resets_at` captured-in-raw but dropped at render. Sidebar quota cell gated `harness==="codex"` | ❌ none emitted | ❌ none (the CLI-log `RESOURCE_EXHAUSTED` line is not parsed) |
| **Model** | ✅ `system/init.model` → sidebar | ✅ session-file `turn_context.model` (first-turn only) | ✅ `init.model` → sidebar | ⚠️ fragile — string-scrape of a `<USER_SETTINGS_CHANGE>` sentence; **empty on resume turns**, degrades to prior/blank |
| **MCP servers** | ✅ count (stream + config) | ✅ count (config only) | ✅ count (config) | ✅ count (user-scope config; workspace-scope not scanned) |
| **Skills** | ✅ count | ✅ count | ✅ count | ✅ count |

**Sidebar absent-field convention (cross-cutting gap):** every metadata cell is `{#if}`-gated and **simply omitted** when its value is missing/zero — no `—`, no "n/a". So the UI **cannot distinguish** "this harness never reports cost" (permanent — e.g. Antigravity) from "no turn has run yet" (transient — e.g. a fresh Claude agent). MCP/skills show bare **counts**, no names/status.

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

**The C cells are the actual restart gaps.** Today: **Claude `rate_limit_event` (used %, `isUsingOverage`, reset times)** and **Claude `system/init.mcp_servers[].status`**. Closing the app and reopening drops them until the next interaction, so the agent bar shows less than the TUI would. (Mid-stream token deltas across all four are also class-C but have no UI surface today, so the loss is theoretical.)

**Class B is *not* a gap** — we already re-read the session file at turn-end (Codex rate-limits, Codex model/context, Gemini tool output) and on project open, so the data is durable. The latency cost (one extra file read) is the trade.

**Class D is what no implementation can fix without external data** — e.g. Gemini does not emit a context window anywhere; Antigravity emits no token usage to disk. Document these as TUI parity ceilings; workarounds (hardcoded model→context maps) are case-by-case decisions.

---

## 4. Gap register (actionable)

Grouped by theme; this is the candidate scope for the failure/metadata-surfacing milestone.

**Failure accuracy:**
- **G1 — Antigravity quota misclassified** as generic `AdapterFailure` "agy exited without producing an answer". Fix: on the no-answer branch, scan the invocation CLI log for the error line (`RESOURCE_EXHAUSTED`, and ideally any `rpc error: code = …`) and surface an accurate message. Best-effort (undocumented Go log; falls back to generic on miss).
- **G2 — Gemini logged-out misclassified** as `AdapterFailure` (exit 41 + "Please set an Auth method", no stream). Fix: recognize the exit-41/stderr shape as `AuthFailure` (the "401" path stays for bad-token, still unobserved).

**Failure rendering (cross-cutting):**
- **G3 — `error_kind` is never rendered.** Decide a consistent treatment (per decision: *not* a bespoke per-kind UI — quota/auth render like any failure, just with the accurate message). The lever is the message + possibly a single shared affordance, not N renders. Note this also subsumes the unbuilt "AuthFailure banner."
- **G4 — Claude has no auth pre-flight** (no project banner; only discovered per-turn). Decide whether to add one (the other three have it).
- **G5 — Auth banner is reload-gated** (only checked `onMount`); signing in mid-session needs a manual reload.
- **G6 — "Transcript disappears" on unauth** — an unauthenticated agent shows empty + a banner; decide the desired state (status icon? inline "not authenticated" in the agent's area rather than only a global banner?).

**Metadata surfacing:**
- **G7 — Claude overage invisible.** `isUsingOverage`/`resetsAt` reach `last_rate_limit` but nothing renders them.
- **G8 — Codex rate-limit under-surfaced** (only `primary.used_percent`; secondary window + `resets_at` dropped).
- **G9 — Gemini context bar never renders** (`context_window` never populated though tokens are captured).
- **G10 — Sidebar absent-field convention** can't express "permanent vs transient absence" — the core UX decision for Goal B (hide / `—` / "not reported by this harness").
- **G11 — Antigravity model metadata fragile** (string-scrape, empty on resume).

**Wire model:**
- **G12 — no `FailureKind::UsageLimit`.** Per the "render like any failure" decision, quota can stay `HarnessError` with an accurate message — so a new variant is likely **unnecessary**. Confirm before adding one.

**Restart continuity (TUI parity):**
- **G13 — Claude class-C metadata lost on restart.** `rate_limit_event` payload (used %, `isUsingOverage`, `overageResetsAt`, `resetsAt`) lives only in `Runtime.last_rate_limit` in memory. On app restart the agent bar shows no quota / overage state until the next event arrives. Fix: per-agent metadata sidecar that captures the latest snapshot (write on event, read on project open). Without this, any UI surface added for G7 would vanish-then-reappear across restarts — *worse* UX than no surface. See §3.1 for the parity class definition.
- **G14 — Claude MCP live-status lost on restart.** `system/init.mcp_servers[].status` is class-C. Config-loader fallback gives us the *registry* (servers exist) without status (up/down). Not in immediate plan scope; a stale "this server was up 3 days ago" indicator may be worse than no indicator — needs a UX decision before persisting.

---

## 5. Open captures / unverified

- **Gemini bad-token (401) auth shape** — never observed; our substring detector is a guess. Capture needs a *stale-but-present* token (a clean logout gives the exit-41 shape instead).
- **Gemini hard quota wall** — none observed (it stalls/retries). Treated as "nothing to classify."
- **Claude hard quota wall** (overage disabled/exhausted) — not yet hittable; only the soft `isUsingOverage` path is captured.
- **Antigravity network-failure / other RPC error codes** in the CLI log — only `RESOURCE_EXHAUSTED` captured; a generalized log scan would surface others (`UNAUTHENTICATED`, etc.).

## 6. Version notes

- **Codex 0.134.0 (2026-05-26) changed the usage-limit copy** — "Display workspace usage limit error copy from response header" ([#24114](https://github.com/openai/codex/pull/24114)): the out-of-credits / usage-limit `turn.failed` message is now workspace-specific with distinct credit vs. spend-cap variants, sourced from the response header. The §1.4 Codex capture ("You've hit your usage limit … try again at 1:00 PM") is from 0.133.0 and may now read differently. **No code impact** — Switchboard classifies Codex quota as `HarnessError` and passes the message **verbatim**, so display is resilient to the copy change; recapture the exact 0.134.0 string only if substring detection is ever added.
- Other 0.134.0 changes reviewed against our `codex exec --json` surface are **non-impacting**: `trace_id` added to `turn.started` (#23980) — we skip that event entirely; legacy `--profile`/profile-v1 removal (#24051/#24059) — we pass no `--profile` flag, though a user's legacy `[profiles]` config could surface as a harness error (user config, not our invocation); MCP `config.toml` additions (per-server env / OAuth, #23583/#24120) are additive.
