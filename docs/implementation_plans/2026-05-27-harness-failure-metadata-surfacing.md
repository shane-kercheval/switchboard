# Harness failure & metadata surfacing

Supersedes the unreviewed **M4.9 draft** ("Harness quota / usage-limit surfacing") in [`2026-05-12-v1-m4-dispatcher-contention-cancel.md`](2026-05-12-v1-m4-dispatcher-contention-cancel.md). That draft was an AI guess from a single quota probe; this plan replaces it after a full per-harness audit and a design discussion that reframed the goal.

## Why this exists (the reframe)

The original draft framed the work as "detect quota exhaustion and give it a distinct `FailureKind` + bespoke UI." The audit ([`docs/research/harness-behavior.md`](../research/harness-behavior.md)) and the discussion changed that framing in two ways:

1. **Quota/auth failures are just failures.** The point is not to *handle* them differently — it's to **display them accurately**, rendered exactly like any other failure in the transcript. So there is **no new `FailureKind`**, no distinct quota/auth render, no banner component. A quota failure is a `HarnessError` with a *correct message* instead of a misleading one.
2. **The goal is symmetric, per-harness:** for each harness, (A) surface every failure it gives us with an accurate message, and (B) surface the metadata it exposes. Each harness differs; the adapter is the central translation point; some harnesses expose nothing for a given field, and that's fine.

The concrete gaps this closes (from the audit's gap register): G1 (Antigravity quota misclassified as "agy exited without producing an answer"), G2 (Gemini logged-out misclassified as adapter failure), the proactive-auth-banner UX the user flagged (G4/G5/G6 — resolved by *removing* it), and G7 (Claude overage captured but invisible).

## Required reading (before implementing)

- [`docs/research/harness-behavior.md`](../research/harness-behavior.md) — the canonical matrix; the gap register (G1–G12) is the scope source. **Read first.**
- [`docs/system-design.md`](../system-design.md) §7 "Failure handling" (failures = harness/template/orchestration errors; the transcript is the surface) and §9 (harness adapter pattern; keep the dispatcher harness-agnostic).
- The captured shapes behind the cells you'll touch (archived provenance): `docs/research/archive/antigravity-cli-observed.md` §"Quota exhaustion (RESOURCE_EXHAUSTED)" + §"Unauthenticated shape"; `docs/research/archive/gemini-cli-observed.md` §"Pending items #1" (logged-out exit-41 capture); `docs/research/archive/claude-code-cli-observed.md` §`rate_limit_event` overage shape; `docs/research/archive/codex-cli-observed.md` §"usage-limit error shape".
- [`docs/ui-conventions.md`](../ui-conventions.md) — for the Sidebar metadata work (semantic tokens, `ui/` primitives).

## Principles established here (reused across milestones)

- **No new `FailureKind`.** Quota classifies as `HarnessError`; auth as the existing `AuthFailure`. The lever is the *message*, not a new wire variant or a per-kind render. (If a future retry-policy ever needs to branch on "quota vs other," add a variant additively then — not now.)
- **Detection stays in adapters; the dispatcher and `commands.rs` get zero `match harness {…}` branches** (the M2/M3 abstraction principle — a regression if violated).
- **Verbatim vs authored messages:** a harness error that carries actionable text (quota's reset time + upgrade links, a model error) is preserved **verbatim**. An auth failure whose raw text is *not* actionable (Codex's bare "401 Unauthorized") gets a **Switchboard-authored** actionable message (name the harness + the fix). The adapter already knows which kind it is, so it authors accordingly.
- **Auth is reactive, surfaced in the transcript.** There is no proactive auth probe/banner/status-icon after Milestone 2 — a logged-out harness is discovered when the user sends, and the failed turn carries the actionable message. This is the same path for "never tried" and "tried and failed" (one mental model), and nothing goes stale.
- **Metadata renders cleanly-hidden-when-absent.** A field is shown only when its value is present; absence (whether the harness *can't* report it or simply *hasn't yet*) renders **nothing** — never a blank label or empty widget. New cells follow the existing `{#if present}` convention.

---

## Milestone 1 — Accurate failure messages (harness adapters)

### Goal & Outcome

Every failure a harness gives us surfaces in the transcript with a message that tells the user what actually happened. Backend-only, per-adapter, fixture-tested.

Outcomes:
- An **Antigravity** turn that fails on quota shows "quota exhausted" (the real reason) instead of "agy exited without producing an answer."
- A **Gemini** turn dispatched while logged out is classified as an auth failure with an actionable message, not a generic adapter failure.
- **Auth** failures across all harnesses carry an actionable message (which harness, how to fix), not raw text like "401 Unauthorized."
- **Codex** quota (already accurate — verbatim usage-limit message) and all harnesses' generic errors are unchanged where they're already correct.
- The dispatcher and `commands.rs` remain free of harness-specific failure branches.

### Implementation Outline

**Antigravity quota (G1) — give each dispatch its own log via `--log-file`, then scan that file.** Today `classify_outcome` falls through to a generic `AdapterFailure` ("agy exited without producing an answer") when no terminal answer was read; quota exhaustion lands there because `RESOURCE_EXHAUSTED` appears only in `agy`'s per-invocation CLI log, never on stdout/stderr/transcript (exit 0). **Do not** scan the shared default log dir by mtime (the draft sketch's approach): with parallel agents / fan-out, "most recent log after spawn" can read a *different* concurrent `agy`'s log and misattribute its failure — exactly the confusion this milestone exists to remove. Instead:
- `agy` supports `--log-file <path>` (confirmed in the CLI surface). Pass a **per-dispatch** log path in `build_args` so this turn's log is isolated; on the no-answer branch, scan **only that file**. This removes the cross-agent misattribution *and* any spawn-time/mtime-window race by construction (no directory scan, no timestamp guessing). The log path is a value we choose (a temp / managed path) — not `agy`'s default dir — so no default-log-dir path helper is needed.
- Only scan when `!saw_terminal_answer` (a successful turn never scans). Best-effort: missing/unreadable file → fall back to the existing generic message. **Never panic or propagate.**
- **Generalize beyond `RESOURCE_EXHAUSTED`:** the log is the canonical "why did this invocation produce nothing" source. Match `rpc error: code = <CODE>` lines (case-insensitive); map known codes to human messages (`ResourceExhausted`/`RESOURCE_EXHAUSTED` → "Antigravity quota exhausted — Google Cloud individual quota reached; wait for the reset or check your usage limits"), pass unknown codes through as "Antigravity error: <line>". Classify **`HarnessError`** (not a new kind, not `AdapterFailure`). The displayed string is the human message, not the raw log line.
- The `OutcomeSignals` field + the `classify_outcome` ordering (check the log error *before* the generic "no answer" fallback) are as the draft sketch describes; the implementing agent confirms exact names against the code.
- **Honest limitation to record in a comment:** the log carries the RPC error, not the TUI's "Resets in 123h…" countdown — we surface "quota exhausted," not a reset time.

**Gemini logged-out (G2).** A clean logout produces **exit 41 + stderr "Please set an Auth method…" + no stream-json** (captured 2026-05-27). The current terminal-failure synthesis only special-cases exit 42, and the auth substring set keys on "401 Unauthorized" — so this falls to `AdapterFailure`. Add recognition of the logged-out shape (exit 41 with the "Please set an Auth method" / "auth method" stderr signal) → classify `AuthFailure` with an actionable message. **Keep** the existing "401" path — that's the separate *expired-token-mid-call* shape (still unobserved on Gemini; don't remove it).

**Auth message authoring (all adapters).** For `AuthFailure`, author an actionable message rather than surfacing raw harness text: name the harness and the fix (Codex → "run `codex login`"; Gemini → "run `gemini` interactively to sign in"; Antigravity → "sign in via the Antigravity desktop app"; Claude → "run `claude login`" / its "Please run /login" is already actionable — verify). Keep this in each adapter (it already detects auth there); **no `match harness` in app/dispatcher**. Two specifics not to miss:
- **Gemini has *two* auth paths.** Besides the new logged-out (exit 41) one, the existing bad-token path (`synthesize_terminal_failure`, exit 42 + "401 Unauthorized") already classifies `AuthFailure` but currently sets the message to the **raw stderr**. Both paths must emit the same authored message.
- **Drop "reload Switchboard"** from auth messages (e.g. Antigravity's current text says "…and reload Switchboard"). Under reactive auth the recovery is "sign in, then send again" — there's no proactive state to refresh, so telling the user to reload is wrong advice.

**Codex quota + the others — verify, don't rebuild.** Confirm Codex's usage-limit message reaches the UI verbatim (it does — `HarnessError` with the reset time + links). Confirm Claude/Antigravity auth detection still fires with the captured shapes. Gemini quota: it stalls/retries with no hard wall — **record that as the answer** (nothing to classify), don't force a classification.

### Definition of Done

- **Fixture/unit tests (no live harness needed):**
  - Antigravity: with a per-dispatch `--log-file`, a fake log containing a `RESOURCE_EXHAUSTED` line → quota `HarnessError` with the human message; an unknown `rpc error: code=…` line → passed-through message; missing/unreadable log file → generic fallback (non-fatal); a successful turn → log never scanned; **two concurrent dispatches read their own logs (no cross-attribution)**. (No `#[ignore]` live test — forcing real quota is impractical.)
  - Gemini: exit 41 + "Please set an Auth method" stderr + no stream → `AuthFailure` with the actionable message; the existing exit-42 "401" path yields `AuthFailure` **with the authored message, not the raw stderr**; an unrelated non-zero exit still → `AdapterFailure`.
  - Auth message content asserted per adapter (names the harness + fix; contains **no** "reload Switchboard").
  - Verbatim preservation: a quota/`HarnessError` message is surfaced unchanged (reset time/links intact).
- **Cross-check:** `grep` confirms zero harness-specific failure branches in the dispatcher / `commands.rs`.
- **Docs:** update `harness-behavior.md` (G1/G2 → closed) — the single source of truth; the archived `*-cli-observed.md` files are frozen, do not edit them. Record the Antigravity "no reset countdown" limitation in a code comment at the scan site.

---

## Milestone 2 — Reactive-only auth: remove the proactive surface

### Goal & Outcome

Auth stops **interrupting normal work**: the global red "X not authenticated — reload Switchboard" banner and the Add-Agent picker auth-gating are removed, so a logged-out harness is discovered reactively (the send fails in the transcript, Milestone 1). The backend `check_*_auth` probe commands are **retained** — they stop driving the banner/gate but are the inputs the getting-started surface (a planned follow-up milestone — see "Out of scope") will consume; deleting and re-adding them would be churn. Depends on Milestone 1 (so the reactive path carries good messages before the proactive surfaces go away).

Outcomes:
- A logged-out harness is discovered when the user **sends** to one of its agents: the turn fails in the transcript with the actionable message from Milestone 1. No banner, no per-agent status icon, no reload-gated state during normal work.
- The Add-Agent picker no longer disables a harness on auth grounds (you can create the agent; you find out on first send) — **binary-not-installed detection is unaffected** (a genuinely missing CLI is a separate, real install problem and stays).
- The `check_*_auth` probes remain (no longer wired to a banner/gate), ready for the getting-started surface.

### Implementation Outline

**Frontend.** Remove the auth-banner derivation + its `Banner` rendering, the auth branch of the harness-picker gating (`isHarnessSelectable` / the CreateAgentForm gating), the auth copy in `harnessAvailability.ts`, and the now-unused `onMount` `checkXAuth()` wiring / `auth: "missing"` state. **Preserve** the binary-presence path (CLI-not-installed) untouched — only the *auth* dimension goes reactive. (The audit found auth and binary checks are separate concerns sharing the availability surface; keep the binary half.)

**Backend.** **Retain** `check_codex_auth` / `check_gemini_auth` / `check_antigravity_auth` (Tauri commands + `*_impl` + `lib.rs` registration) and their tests — they no longer drive a banner/gate but are the inputs the getting-started surface (next milestone) will consume. Just confirm nothing in M2 still *renders* their result after the frontend cleanup.

**Scope discipline:** this milestone *only* removes the mid-work auth surfaces (banner + picker gate); it adds no new auth UI. No per-agent status icon or empty-state (decided against — stale/context-loss). The getting-started orientation surface is a separate milestone.

### Definition of Done

- **Frontend component tests (mock `invoke` + `listen`):**
  - With a harness "logged out," no auth banner renders at startup and the Add-Agent picker does **not** disable that harness on auth grounds.
  - Sending to an agent whose backend `send`/turn yields an `AuthFailure` (mock the event sequence) renders the failed turn in the transcript with the actionable message — confirming the reactive path is the surface.
  - A genuinely missing binary still surfaces its existing not-installed treatment (binary path intact).
- **Backend:** `check_*_auth` commands **retained** (kept for the getting-started milestone); no frontend caller renders their result after M2; `make check` green.
- **Docs:** `harness-behavior.md` updated (auth → reactive only). **`system-design.md` §37 reframed to the auth-agnostic policy**: Switchboard does **not** manage harness authentication — the user authenticates each CLI however they prefer (interactive login, API key, Vertex, …); Switchboard just invokes it and surfaces auth failures reactively in the transcript. **Drop both the "clear error at agent-creation" claim and the "API-key auth unsupported" framing** — neither matches reality (we have no API-key machinery and don't police auth method). What *stays* is the **cost** decision: v1 ships no pricing tables, so dollar cost appears only where the harness reports it (subscription Claude's `total_cost_usd`, Codex quota); other auth modes simply show no cost figure — not an error. Also drop the Codex "API-key-only auth is not supported" copy. Note in the relevant component the deliberate absence of a mid-work auth banner.
- **Known limitation (record):** a logged-out harness is only discovered on send — intended, not a gap.

---

## Milestone 3 — Surface exposed-but-hidden metadata

### Goal & Outcome

Display the metadata harnesses already hand us but the Sidebar doesn't show — and confirm the clean-hide convention so the per-agent card never renders an empty widget. Frontend (Sidebar); independent of Milestones 1–2.

Outcomes:
- A **Claude** agent that crosses into overage shows a per-agent indication ("spending usage credits; 5-hour window resets at …"), derived from the `isUsingOverage` signal already in `last_rate_limit`. When not overaging, nothing shows.
- *(Light)* A **Codex** agent's quota cell can show its reset time / the second window if cheaply available (we currently show only `primary.used_percent`).
- Every metadata cell is **cleanly hidden when its value is absent** — no blank labels, no empty bars. A field a harness can never report (Antigravity cost/quota/context, Gemini context bar) simply never appears; this is correct, not a gap.

### Implementation Outline

**Surface Claude overage (G7).** The signal already reaches the frontend: `rate_limit_event` copies `rate_limit_info` verbatim into `runtime.last_rate_limit` (opaque). The discriminator is `isUsingOverage: true` (absent entirely on normal-quota turns); `resetsAt` (5-hour window) and `overageResetsAt` are alongside it. Add a Sidebar indication rendered **only when `isUsingOverage === true`** — a defensive shape-read of the opaque `last_rate_limit` (same pattern as the existing Codex `rateLimitPercent` reader), not a typed field. This is a Claude-applicable cell; it sits alongside the existing cost/context cells. Keep the copy simple and preserve the reset time from the payload (epoch → human time at the boundary). Do **not** parse or schedule anything off the reset time (no auto-retry — out of scope). Also **fix the stale `types.ts` comment** on `last_rate_limit` (it claims Claude never populates it — false): it's an opaque payload populated by both Claude (`rate_limit_event`, carries `isUsingOverage`) and Codex (carries `primary.used_percent`).

**(Optional, light) Codex reset / second window (G8).** `last_rate_limit` already carries the secondary window + `resets_at`; if it's a small addition to the existing `quota used: %` cell, surface the reset time. Skip if it's not clean — it's a nice-to-have, not the milestone's point.

**Confirm the clean-hide convention (G10 / Fork A).** The audit found cells are already `{#if present}`-gated (cleanly hidden) — so this is mostly verification, not a refactor. Audit the Sidebar metadata cells for any that render a label/container/bar when the underlying value is absent (a blank-widget bug); fix any found so absence renders nothing. **Do not** add "—" / "not reported" placeholders and **do not** build a per-harness capability map — the decision is clean-hide for both permanent and transient absence, which the data-presence gate already achieves. Confirm in a comment that Gemini's hidden context bar (no `context_window` exposed) and Antigravity's absent cost/quota/context are *correct* absences.

### Definition of Done

- **Component tests (mock state):**
  - A Claude runtime whose `last_rate_limit` has `isUsingOverage: true` renders the overage indication (with the reset time); one without it (normal quota) renders nothing.
  - A Codex agent renders no cost cell; an Antigravity agent renders no cost/quota/context cells (capable-absence stays hidden) — assert no empty label/bar is present in the DOM for those.
  - If the Codex reset-time addition is done: it renders when present, hidden when absent.
- **Docs:** `harness-behavior.md` updated (G7 → closed; G9/Antigravity-absence reaffirmed as correct, not gaps); `ui-conventions.md` only if a new shared pattern emerged (it shouldn't — this reuses existing cells/tokens).
- **Manual verification (or explicit can't-run note):** in `make dev`, confirm an overage Claude agent shows the warning and a fresh agent shows nothing odd.

---

## Milestone 4 — Getting-started surface (the no-project state)

### Goal & Outcome

Turn the bare no-project state into a concise getting-started panel that orients a new user: per harness, is the CLI installed (which version), is it authenticated, and how to fix what's missing. This is the **proactive counterpart to Milestone 2** — M2 stops *interrupting* with auth mid-work; this surfaces install/auth status up front, in the one place it belongs (no project open, user trying to get going). Depends on M2 (consumes the retained `check_*_auth` probes). Status-only: Switchboard links and instructs, it launches nothing.

Outcomes:
- Whenever **no project is active** (including projects-exist-but-none-selected, not just first run), the user sees a getting-started panel with one row per harness.
- Each row shows: harness name + icon; **install status** — installed (with the CLI version) or not-installed (with a clickable install URL); and **auth status**.
- **Auth status**: all four harnesses show a best-effort authed (✓) / not-authed (✗) indicator with a fix hint ("run `codex login`" / "run `gemini` interactively" / "sign in via the Antigravity app" / "run `claude login`"). These are **presence heuristics, not validity checks** — a hint for a fresh user, not a guarantee (the authoritative test is a successful send). Auth-agnostic: a ✗ never blocks anything, and an API-key user may show ✗ yet still send fine.
- Status **refreshes** when the panel appears and when the window regains focus — installing a CLI or logging in via the terminal and returning updates the panel with no manual reload.
- The installed **version** is shown; **no "update available" detection** (rationale below).
- Status / version / URLs / instructions only — Switchboard opens an install page in the browser (existing opener) but never launches a terminal or runs a login.

### Implementation Outline

**Where it renders.** `App.svelte` has **two** no-active-project branches (`activeProjectId === null`): `projects.list.length === 0` → `WelcomeScreen`, else `EmptyState "Select a project."`. The getting-started panel must render in **both** (every no-project state), preserving the project-selection affordance (the sidebar list) in the projects-exist case.

**Install + version.** `check_*_binary` returns presence only (`Result<(), AppError>`) and gates agent creation — **leave it untouched**. Add a **new structured command** `get_harness_install_status(harness) -> { installed: bool, version: Option<String> }` (missing binary = `installed: false`, version absent — *data*, not an error path), so existing binary-gating callers don't churn. Source the version via a new `fn version(&self) -> Option<String>` on `HarnessAdapter` (default `None` on the mock; co-locates the `--version` shell with the binary that owns it — Gemini/Antigravity already do this internally; add Claude `claude --version` / Codex `codex --version`). Not-on-PATH → "not installed" + install URL.

**Auth status.** Consume the **retained** M2 `check_*_auth` probes for Codex / Gemini / Antigravity → ✓/✗, and **build a Claude probe too**: `check_claude_auth_impl` mirroring Antigravity's keychain lookup — a presence check of the macOS Keychain service `"Claude Code-credentials"` (confirmed present when logged in). Best-effort like the others (not-found / lookup error → not-authed, never crash); **macOS-only** (a Linux build would also check `~/.claude/.credentials.json` — out of v1 scope, note it). This replaces Claude's hardcoded `auth: "unsupported"` so all four are uniform ✓/✗ — no special "?" case. **Don't narrow any harness's accepted auth** (Gemini's `gemini-api-key`/`vertex-ai` stay valid) — we're auth-agnostic. Re-run install + auth checks on panel mount **and on window refocus via the native `visibilitychange` event** (`document.visibilityState === "visible"`), registered in the getting-started component with an `$effect` + cleanup — no Tauri focus-event dependency, and inactive when a project is open. This is where proactive status lives now, so the staleness the removed banner had is solved here.

**Per-harness descriptor.** Keep install-URL, version-flag, and login-command knowledge in one place (alongside `harnessDisplay` / `HarnessIcon`); the panel renders generically over it — no `match harness` in the view. The install URL has **one source**: today URLs are embedded inline in `harnessAvailability.ts`'s `BINARY_COPY` prose — extract each to the descriptor (M2 removes the binary-missing *banner* anyway), so the URL isn't duplicated.

**No update-availability detection (decided — record why).** "An update is available" needs a remote latest-version source per harness, kept current (the external-data-currency burden §37 avoids), and is redundant with the CLIs' own self-update/notify. We show the installed version and stop. (No CLI offers a cheap offline "am I current?" check — verified.)

### Definition of Done

- **Backend tests:** `get_harness_install_status` → installed harness yields a version; not-installed yields `{ installed: false, version: None }` (not an error). `check_claude_auth_impl` → keychain entry present ⇒ authed, absent ⇒ not-authed; a lookup error ⇒ not-authed (no panic).
- **Component tests (mock `invoke` + the injectable opener):**
  - Renders on **both** no-project branches: empty workspace, *and* projects-exist-but-none-active (`activeProjectId === null` with a non-empty list).
  - Installed harness → version + ✓ auth; not-installed → install URL (opens via the opener wrapper, not a terminal); not-authed → ✗ + login hint. **All four harnesses use ✓/✗ uniformly** — no "?" case.
  - Re-check on focus: dispatching `visibilitychange` (visible) re-invokes the install/auth probes (assert re-probe).
- **Capture (do during impl):** verify `claude logout` actually removes the `"Claude Code-credentials"` keychain entry. If it does, the presence heuristic is sound; if a stale entry persists, document the false-positive caveat (as Codex's `auth.json` check already carries).
- **Manual verification (or can't-run note):** log a harness out via its TUI, refocus Switchboard, confirm the row flips to ✗ without a reload; log back in, confirm it flips back.
- **Docs:** record the auth-status caveat (presence heuristic, not validity; Claude macOS-only) and the deliberate no-update-check decision in the component.

---

## Out of scope (do not build)

- A new `FailureKind` (`UsageLimit`/quota) — dropped by decision; quota is `HarnessError` + accurate message.
- Reset-time parsing into a structured retry/queue affordance; auto-retry-at-reset.
- A *mid-work* auth surface — a global banner, per-agent status icons, or an unauthenticated empty-state during normal use. (Proactive install/auth status lives in the **Milestone 4** getting-started surface — the no-project state — not in the working UI.)
- Any **API-key machinery** — entering, storing, injecting, or *blocking* keys. Switchboard is **auth-agnostic**: it invokes the CLI and the CLI owns its auth. (No `env_clear` to "enforce no API keys" either — that's hard-blocking, against the philosophy and out of scope.)
- "Update available" detection for the harness CLIs (M4 shows installed version only — the CLIs self-update/notify; a remote latest-version comparison is maintenance burden we don't take on).
- A per-harness metadata **capability map** — clean-hide-on-absence already satisfies the decision; a map would be unused machinery.
- Per-agent token-count display, cross-harness cost aggregation (per system-design §2).
