# Hydration robustness: visible failures + a stable hydration identity

**Status:** proposed (2026-06-05). Precedes M5 of the per-agent-model-selection plan.
**Owner area:** `crates/harness` (parsers), `crates/app` (load commands), `src/lib/state` (hydration merge + runtime), `src/lib/components` (failure surface).

## Why this exists

While scoping the attach flow for per-agent model selection we asked a simple question — "what happens if loading an attached session's history fails?" — and the answer exposed two real gaps in how Switchboard *hydrates* (reads a harness's on-disk session file into the UI):

1. **Hydration failures are invisible and unrecoverable.** When `load_transcript` / `load_project_conversation` fails, the error is `console.warn`'d and dropped. The only consumer of `hydration_status: "failed"` is `ComposeBar` (to unblock composing). Nothing renders the failure where the user is looking, and the per-agent / per-project hydration guards are *sticky across failure*, so there is **no retry** for the session. Net effect: an agent shows an empty transcript with no explanation, and a hydration failure — which means we can't trust what's on screen — is silent. This is the gap the user cares most about: "if hydration fails, I don't know if the rest of the app works… I need the exact error so I can report it."

2. **Re-reading a session file duplicates its turns**, because `turn_id` is minted fresh (`Uuid::now_v7()`) on every parse and the hydrate merge dedups by `turn_id`. That is *why* the guards are sticky — re-parsing is unsafe. This blocks two things: a clean retry, and (later) picking up changes a user made by continuing the session in the harness's own TUI.

This plan fixes both, and then builds the staleness-refresh feature on top (M3): re-reading a session file when the user has continued it in the harness's own TUI, gated by the file's modification time so it only re-reads on real change. (An earlier reservation about "auto re-hydration" was about blindly re-reading on *every* switch; the modification-time gate removes that — it re-reads only when the file actually changed.)

### Sequencing & delivery

**One PR, one branch, sequential commits** (M1 → M2 → M3) — same model as M4. "Milestone" means a commit boundary, not a separate release, review, or ship cycle.

- **M1 commits first.** It is universal across all harnesses, independent of M2/M3, and addresses the most-felt pain. It is independently useful but is *not* shipped separately — it is the first commit on the shared branch.
- **Two cheap probes gate M2, inline — not a checkpoint.** Before wiring M2's dedup, run one tiny live Codex turn and one tiny live Gemini turn to confirm the live stream carries the same per-turn id the session file stores. This is just checking the contract before building on it, not a process step.
- **M2 and M3 land in the same PR** unless the probes + the engineer conclude refresh isn't worth building. The only realistic "stop after M1" trigger is the probes showing Codex/Gemini *cannot* carry the live id, leaving refresh Claude-only — and even then it may be worth shipping. That go/no-go is the one decision left open past M1.

### Explicitly out of scope (do not build)

- **Project-switch render delay.** Established during discussion to be frontend *render/derive* cost of a large transcript, not I/O — switching is already display-only and hydration is cached for the session. It has a known mitigation (the switch spinner / `loadingProjectId`). Not this plan's problem.
- **A general application-wide error/diagnostics bus.** Tempting, but speculative right now. See M1's scoping note — we build a *reusable presentational dialog component*, not a global error pipeline. If a future milestone (e.g. dispatch errors in M5/M6) wants a shared bus, that is a deliberate, separate expansion to confirm with the user then.

## Authoritative reading — read before implementing

Internal (ground truth — read first):
- `docs/harness-behavior.md` **§3.1-cost** — the existing `stable_message_id` join key: Claude's final non-subagent assistant `message.id`, **verified to round-trip identically between the live stream and the on-disk session file**. M2 generalizes this; do not reinvent it.
- `crates/harness/src/claude_code/session_file.rs` — Claude session-file parser; `last_message_id` → `stable_message_id` (already populated; round-trip test in-file). `crates/harness/src/parser.rs` — the live Claude parser (`last_assistant_message_id`).
- `crates/harness/src/codex/session_file.rs` and `crates/harness/src/gemini/session_file.rs` — Codex and Gemini session-file parsers; both currently emit `stable_message_id: None`, **but the on-disk id exists and is ignored, not absent**: Codex writes a harness-local `turn_id` on `task_started` records, Gemini a record `id` (`g1`/`g2`). M2 Step 1 captures these and probes whether the *live* stream exposes the same id. `crates/harness/src/antigravity/session_file.rs` mints `Uuid::now_v7()` per parse with no native per-turn id — the one harness with none.
- `docs/research/archive/*-cli-observed.md` — raw session-file shapes (the probes `harness-behavior.md` distills from). Cite these for the exact on-disk fields when capturing Codex/Gemini stable ids.
- `src/lib/state/reducers.ts` (`case "hydrate"`) and `src/lib/state/index.svelte.ts` (`hydrateAgent`, `applyAgentHydrate`, `hydrationAttempted`), `src/lib/state/workspace.svelte.ts` (`activateProject`, `hydrationStarted`, `conversations`) — the hydration lifecycle and the dedup that M2 changes.

External:
- `uuid` crate — `Uuid::now_v7`: https://docs.rs/uuid/latest/uuid/struct.Uuid.html#method.now_v7 (why re-parse mints non-stable ids today).
- Tauri v2 command error handling (how a Rust command `Err` reaches the JS `catch` as a string): https://v2.tauri.app/develop/calling-rust/#error-handling — relevant to M1 surfacing the exact backend message.

## Shared conventions established here (later code reuses these)

- **Stable hydration identity (M2).** Every hydrated turn carries a *stable hydration key* with **two distinct properties that must not be conflated** (they gate different things):
  - **re-parse-stable** — byte-identical across repeated parses of the *same* file. This alone makes reopen/refresh dedup of disk-only turns safe.
  - **live-matched** — identical to the key the *live* turn carries at `TurnEnd`. This is the stronger property, required to dedup a turn that streamed live *and* is on disk. Only harnesses that satisfy this can do whole-file M3 refresh.

  The key is used **only** by the hydrate merge to recognize "I already have this turn." `turn_id` is unchanged and stays the live-correlation / dedup id (it is load-bearing for the dispatcher, footer stamping, and heartbeat correlation — do not repurpose it). Do **not** overload the existing private `stable_message_id` (Claude's cost-join key, `skip_serializing`'d off the wire) — add a clearly-named sibling that is frontend-facing. Rationale for the two-key split must survive into code comments at the merge site.
- **Failure is a first-class, surfaced, recoverable state (M1).** Any hydration failure (per-agent or per-project) retains its **error text**, renders an inline failed+retry affordance where the missing content would be, and is reportable verbatim via a copyable dialog. Retry clears the relevant hydration guard and re-runs.

---

## M1 — Hydration failures: visible, reportable, retryable

### Goal & Outcome

Make hydration failure impossible to miss, understandable, and recoverable — for both per-agent transcript loads and the per-project conversation load.

- When an agent's history fails to load, its transcript region shows a clear failed state ("Couldn't load this agent's history") with a **Retry** action and a **Details** action — not an empty pane.
- When a project's conversation fails to load, the center pane shows the same shape (this partly exists via `activationError`; unify it).
- "Details" opens a dialog showing the **exact error message** verbatim with a **Copy** action, so the user can paste it into a bug report.
- **Retry** re-attempts the load; on success the content appears, on repeated failure the failed state remains. No app restart required.
- The failure surface is decoupled from any dialog lifecycle — it lives on agent/project state, so it is correct whether the failure happened during the attach dialog, on project open, or on a later retry.

### Implementation Outline

This milestone is mostly frontend; it does not depend on M2.

1. **Unify and surface the existing error — do not reintroduce it.** A `hydration_error` field **already exists** on the runtime (`src/lib/state/types.ts`), is already set by the *project-batch* hydration path (`workspace.svelte.ts`, from `meta.load_error`), and is **already rendered in the Sidebar** (`Sidebar.svelte`, "history failed to load: …"). The gap is the *standalone* `hydrateAgent` catch (`index.svelte.ts`), which drops the error to `console.warn` instead of setting the same field. So M1 is **"make the two per-agent paths consistent and surface the existing field more prominently,"** not "add error state": fix the standalone catch to set `hydration_error`; do the equivalent for the per-project `ProjectConversationState` if it isn't already carried; and have the new inline transcript surface **and** the existing Sidebar line read the same field (no second, parallel error field). Separately, **verify the backend error text is descriptive** (read `load_transcript_impl` / `load_project_conversation_impl` error variants) — if a failure mode produces an opaque message, improve the `thiserror` `Display`. That is the one backend change in scope here.

2. **Make retry safe to wire.** Per-agent retry must clear `hydrationAttempted` for that agent (and reset status to a loading state) before re-calling. Per-project retry must clear `hydrationStarted` / the `loadStarted` entry. **Important sequencing fact:** a failed hydration today applies *nothing* (the load is all-or-nothing at the IPC boundary — `loadTranscript` either returns a complete value that is then applied, or throws and applies nothing), so retry-after-failure cannot duplicate turns *even before M2*. M1 may therefore ship retry independently. (M2 is what makes re-reading an *already-successfully-loaded* file safe; that is a different path and not required here.)

3. **Surface it in the transcript region.** The agent/project conversation area renders the failed state with Retry + Details inline, where the user expects the content. Reuse existing `ui/` primitives; conform to surrounding patterns (the agent reads the components — do not prescribe markup).

4. **Reportable error dialog (reusable component, not a global bus).** Build one presentational dialog component that takes a title, a human message, and a details string, and offers Copy. Drive it from hydration-failure state. **Scoping decision (matched to the problem):** this is a *component*, reusable by later features that want it, **not** a centralized error store/pipeline that every subsystem pushes into — that broader abstraction was discussed but not adopted; building it now would be speculative. If M5/M6 later want a shared error surface, they adopt this component; expanding to a bus is a separate decision to raise with the user.

### Definition of Done

- **Component tests (mock `invoke`):** `load_transcript` rejects → the agent's pane shows the failed state with the error text; **Retry** triggers a second `invoke` and, on success, renders the turns and clears the failed state; **Retry** that fails again keeps the failed state. Same coverage for the project-conversation load path. Details opens the dialog with the verbatim message; Copy is wired.
- **Reducer/state unit tests:** the failure error is retained on runtime / `ProjectConversationState`; retry clears the correct guard (`hydrationAttempted` / `hydrationStarted`) so a re-attempt actually re-runs.
- **Backend:** if any `load_*_impl` error variant was opaque, a unit test asserts the improved message names the failure cause. No behavior change beyond message text.
- **Docs:** `harness-behavior.md` — update the note that hydration failures are surfaced (the gap register currently treats this as unhandled; record it as closed). Record any known remaining limitation explicitly.
- Known limitation to record (not fix here): retry re-reads from scratch; incremental re-read is out of scope until M2.

---

## M2 — A stable hydration identity: idempotent, live-safe re-hydration

### Goal & Outcome

Give every hydrated turn an identity that survives re-parsing, so reading the same session file twice never duplicates turns — the foundation the M3 staleness-refresh feature is built on.

**M2 has no standalone user-facing payoff, and that's expected — its only consumer is M3.** With M1's sticky guards and failed-load-applies-nothing retry, nothing in the running app re-reads an already-loaded file today (first activation is lazy, so no live turn exists at first hydrate either — the live-vs-disk race is not reachable). So M2 is not fixing a bug visible now; it is making re-reading *safe* so M3 can do it. M2 and M3 are effectively one feature; do not scope or justify M2 on its own.

- Re-parsing a session file and merging it again produces **no duplicate turns**.
- A turn that both streamed **live this session** and is present **on disk** is recognized as one turn, not two — for harnesses whose key is *live-matched* (see Shared conventions).
- `turn_id` semantics are unchanged; nothing that depends on `turn_id` (dispatcher correlation, footer stamping, heartbeat) is affected.
- The per-harness stable-id story is documented: which harnesses are live-matched (re-read fully safe) and which are re-parse-stable only.

### Implementation Outline

**Step 1 — Establish the key per harness; answer two questions, not one (do this first — it gates the rest).** For each harness, the plan needs *two* separate answers, because they gate different things (see Shared conventions): **(Q1) is there an on-disk per-turn id that is stable across re-parses?** and **(Q2) does the live stream carry that same id, so a live turn can be stamped with it?** Q1 gates reopen/refresh dedup of disk-only turns; **Q2 gates whole-file M3 refresh.** Known going in (verify, then record in `harness-behavior.md`):
- **Claude:** Q1 ✅ and Q2 ✅ — the final non-subagent assistant `message.id`, already parsed into `stable_message_id` and round-trip-verified (§3.1-cost). M3-eligible, confirmed.
- **Codex:** Q1 ✅ — a harness-local `turn_id` is on `task_started` records (the parser currently ignores it and selects turns positionally; capture it). Q2 ❓ — **probe**: dispatch one tiny live Codex turn, compare the stream's turn id to the on-disk `turn_id`. (Note: the `codex/session_file.rs` "the two never match by design" comment is about *Switchboard's dispatcher* `TurnId` vs the harness id — it is **not** a negative result for Q2.)
- **Gemini:** Q1 ✅ — a record `id` (`g1`/`g2`) is on disk and stable across re-parses (live parser emits `None` today). Q2 ❓ — **probe**: dispatch one tiny live Gemini turn, compare the stream's message id to the on-disk `g`-id.
- **Antigravity:** Q1 ❌ — no native per-turn id (`now_v7` per parse). Not M3-eligible; it gets the `turn_id` fallback and stays once-per-session. **It is the only never-eligible harness** — the "if a harness has no native id" contingency in this plan is *just Antigravity*, not a broad risk.

So realistic coverage is **3-of-4**: Claude confirmed, Codex + Gemini feasible pending their one-turn Q2 probe, Antigravity never. If a probe comes back negative, that harness drops to re-parse-stable-only (no whole-file refresh); surface that to the user as a coverage change, not a silent degrade.

**Step 2 — Carry the stable key on hydrated turns, and thread it to the frontend.** Add a **clearly-named, frontend-facing sibling key** (do **not** overload the private `stable_message_id`). Threading it through the parser alone is insufficient and would *pass unit tests while still duplicating in the real app* — the key is currently stripped at the IPC boundary. It must reach every surface the merge and refresh see:
- live `NormalizedEvent::TurnEnd` — currently the carrier is **dropped** in `into_normalized` (the `..` arm in `events.rs`); un-drop it.
- the hydrated `Turn` / `LoadedTurn` serialization — `Turn::Agent`'s cost-join id is `skip_serializing`'d (`transcript.rs`); the new sibling must actually serialize onto the wire.
- the project-conversation path — `ConversationItem::AgentTurn` (`commands.rs`) is a separate wire shape with no key slot; add one, plus the TS `ConversationItem`.
- `hydrateProject`'s manual `LoadedTurn` remap (`workspace.svelte.ts`) — it reconstructs turns by hand and will drop any field not threaded explicitly.
- the reducer merge (Step 3).

Claude already populates the parser side; Codex and Gemini are the capture work; Antigravity stays on the `turn_id` fallback.

**Step 3 — Dedup the hydrate merge by the stable key.** Change the merge (`reducers.ts` `case "hydrate"`) so dedup keys on the **stable hydration key** for turns that have one, instead of on `turn_id`. Preserve the existing invariant verbatim in behavior and in a comment: **live in-flight turns take precedence** — a turn already in the slice (live) is kept, and a disk turn whose stable key matches an existing turn is dropped (not appended, not used to overwrite a live turn). Today's contract:

```ts
// current — dedups by turn_id, which is fresh per parse → re-read duplicates
const existingIds = new Set(turns.map((t) => t.turn_id));
const fromDisk = input.turns.filter((t) => !existingIds.has(t.turn_id)).map(loadedTurnToTurn);
return [...fromDisk, ...turns];
```

becomes a merge keyed on the stable hydration key (falling back to `turn_id` only for turns that genuinely lack one), with the same "disk turns first, then existing live turns" ordering and the same live-takes-precedence rule. The comment must state *why* the key changed (re-parse mints fresh `turn_id`s; stable key is parse-invariant).

**Step 4 — Foundation only; the trigger is M3.** M2 makes re-reading *safe*; it does **not** add a trigger to re-read a successful load — that is M3. The new idempotency is exercised and tested via the merge directly here. Leave the `hydrationAttempted` / `hydrationStarted` guards in place for now — M1's retry path (failed loads) is the only re-entry until M3 makes the guards mtime-aware.

### Definition of Done

- **Parser unit tests (per harness with an on-disk id — Q1):** parsing the same fixture twice yields turns whose stable key is **identical** across both parses (today they would differ — this is the regression guard). For Claude, assert the key equals the final assistant `message.id` (lock the §3.1-cost contract). For Codex/Gemini, assert against the captured on-disk id (`task_started.turn_id` / record `g`-id).
- **Serialization test:** the new sibling key is present on the IPC wire for both the per-agent and project-conversation paths — guards against the "stripped at the boundary" trap (the very thing a parser-only implementation would miss).
- **Merge unit tests (`reducers.ts`):** (a) applying a hydrate batch, then applying the *same* batch re-parsed (fresh `turn_id`s, same stable keys) → no duplicate turns; (b) a live completed turn already in the slice + the same turn arriving from disk (matching stable key) → one turn, the live one preserved; (c) turns lacking a stable key still merge via the `turn_id` fallback without regression.
- **Live test (`live_<harness>_…`, per live-matched harness — Q2 confirmed):** dispatch a tiny turn, hydrate from the real file, assert the hydrated turn's stable key matches the id the live `TurnEnd` carried — this *is* the Q2 probe, promoted to a permanent drift guard. Naming per AGENTS.md (`live_claude_…`, `live_codex_…`, `live_gemini_…`).
- **Docs:** `harness-behavior.md` — record each harness's Q1/Q2 answers and the resulting M3 eligibility. Update §3.1-cost if the key's role broadened.
- Known limitation recorded: re-read is still whole-file parse (no incremental); acceptable because parse cost is small and the merge now updates surgically (only new turns enter reactive state). Incremental parse is deferred.

---

## M3 — Staleness refresh: pick up TUI-continued turns

### Goal & Outcome

Switchboard reflects turns the user added by continuing a session in the harness's own TUI — automatically, without duplicates, and without needless re-reading.

- After continuing a session in the terminal and switching back to that project in Switchboard, the new turns appear.
- Re-reading happens **only when the session file actually changed** (a cheap modification-time check), so switching between projects does not re-parse unchanged history.
- Only **live-matched** harnesses (M2 Step 1, Q2 confirmed — Claude, and Codex/Gemini if their probe passed) refresh; a harness without a live-matched key (Antigravity, plus any failed probe) stays once-per-session (documented), to avoid live-vs-disk duplication.
- An in-flight or completed **live** turn is never disturbed, dropped, or duplicated by a refresh.

### Implementation Outline

Builds directly on M2's stable-id merge. Read M2 first.

- **Trigger.** On project **re**-activation (`activateProject` for an already-loaded project), for each agent whose harness supports refresh, compare the session file's current modification time to the value recorded at last hydration. If it advanced, re-run that agent's hydration and merge (M2 guarantees no duplicates). This supersedes the per-agent sticky `hydrationAttempted` guard for refresh-capable harnesses — the guard becomes "don't re-read unless the file changed" rather than "never re-read."
- **The freshness check is backend-owned.** The frontend cannot resolve a session file's path itself — that logic is harness-specific and already lives backend-side (Codex partitions by local date; Codex/Gemini can involve candidate selection). So the freshness check is a backend operation using the **same resolver as transcript loading**, returning a small record: `{ source_path, modified_at, byte_len, refresh_capable }`. Gate "changed" on **`(source_path, modified_at, byte_len)`**, not mtime alone — `byte_len` is a near-free, more reliable signal for append-only JSONL (and the offset baseline if incremental is ever added). Store this record at hydrate time as the baseline. Prefer the gate *inside* the load command (it returns a cheap "unchanged" result), so path logic stays in one place and "never re-parse an unchanged file" is enforced backend-side. Docs: https://doc.rust-lang.org/std/fs/struct.Metadata.html#method.modified
- **Per-harness gate.** `refresh_capable` is the Q2 (live-matched) capability from M2 — a single capability check (mirror the existing `supports_*` pattern), not branches scattered across the lifecycle. A non-eligible harness (Antigravity, and any harness whose Q2 probe failed) keeps today's once-per-session behavior.
- **Whole-file re-parse (v1); incremental is deferred and is *not* a probe-free shortcut.** Refresh re-parses the whole file and dedups via the M2 key. A tempting alternative — read only bytes appended past the `byte_len` baseline — looks like it would avoid live-vs-disk dedup (and so let Codex/Gemini refresh without the Q2 probe), **but it does not**: turns *you* dispatched in Switchboard this session are also appended past the baseline, so an incremental read re-encounters them and still needs the live-matched key to avoid duplicating them (closing that gap fully means advancing the baseline on every live `TurnEnd`, with its own races — that's the deferred incremental-parse complexity). So whole-file + the M2 key + the Q2 gate is the correct v1; keep incremental deferred and do not treat it as removing the probe requirement.
- **Preserve the live-takes-precedence invariant** from M2 verbatim: a refresh merges *new* disk turns only; it never overwrites or removes a live turn already in the slice.
- **Lean on benign failure modes (document them):** an mtime false-positive → a safe, deduped no-op re-read; reading the file mid-write → parsers already degrade per-line, so a half-written trailing line is ignored and picked up on the next refresh.

### Definition of Done

- **Integration/unit:** a session file that gains a turn (advanced mtime) → reactivation re-reads and the new turn appears **exactly once**; unchanged mtime → the load/parse path is **not** called (assert it); a refresh while a live turn is in-flight → the live turn is preserved untouched.
- **Live test (`live_<harness>_…`, live-matched harness):** dispatch a turn and hydrate; add a turn to the real session out-of-band (a second CLI dispatch, or appending to the file); reactivate; assert the new turn appears exactly once. Naming per AGENTS.md.
- **Docs:** `harness-behavior.md` — record which harnesses refresh and the mtime-gate behavior. `README.md` "Harness support and limitations" — add a plain-language line **only if** a user-visible asymmetry results (e.g. "Codex/Gemini history won't auto-update from terminal edits").
- **Known limitation (record, do not fix):** refresh fires on project switch, not while you remain inside a project — there is no live file-watch. Acceptable for v1; the switch covers the "go to the TUI, come back" path.

---

## Deferred decisions (capture now, build later — require the user's call)

Discussed but deliberately not built here.

1. **Central diagnostics/error bus.** A shared store any subsystem pushes structured errors into. Deferred deliberately (M1 ships a reusable *component* instead). Revisit only if a concrete second consumer (e.g. M5/M6 dispatch errors) materializes — and confirm the expansion then.
2. **Live in-project file-watch.** Refreshing while the user is sitting *in* a project (not just on switch-back) — would need a filesystem watcher. M3's switch-trigger covers the common case; add a watcher only if in-project staleness proves to be a real annoyance.
3. **Incremental (append-only) parse.** Reading only new lines on refresh rather than re-parsing the whole file. An optimization on top of M2/M3; only worth it if profiling shows whole-file re-parse is a real cost. Not now.
