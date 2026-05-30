# Per-agent model selection

**Status:** proposed · **Created:** 2026-05-30

Let each agent run on a user-chosen model. The model is picked when the agent is created, changed later (for harnesses that support it) from the agent's actions menu, shown as the agent's current intent in the sidebar, and recorded **per turn** in the transcript so a mid-conversation switch is visible in history. Antigravity is the exception: its CLI exposes no model control, so the picker is disabled at creation, the change-model action is absent, and its per-turn model is best-effort (often blank).

## Background — read before implementing

This plan is the product of an empirical probe of all four harness CLIs. **Read `docs/research/harness-behavior.md` §3.3 ("Model selection — can we set the model on a turn?") first** — it is the ground truth for the per-harness behavior this plan encodes. The essentials:

- **Claude** (`--model <alias|id>`), **Codex** (`-m/--model <id>`), **Gemini** (`-m/--model <id>`) all accept a model on every invocation, including resume. Verify exact flag spelling against each CLI's `--help` before wiring it.
- **Model is per-invocation, not configured once.** Claude is session-sticky, Gemini reverts to its default without the flag, Codex re-derives per turn. The uniform rule — decided — is **send the flag on every dispatch when a model is set**, so we never depend on per-harness stickiness. One code path.
- **Codex models are gated by the user's plan**, not by us. The CLI forwards any `-m` value; the API rejects out-of-plan ones with a 400. We do not enumerate or validate model values (see "reactive validation").
- **Antigravity has no per-call model control and we will not build one.** Its only model lever is a global, harness-owned config file we've decided never to touch; its agents run on whatever model was last selected inside Antigravity itself. For **display**, though, Antigravity reaches per-turn parity: it writes a `USER_SETTINGS_CHANGE` "from X to Y" sentence on the turn where the model changes (verified 2026-05-30 — turn 1 `None → Gemini 3.1 Pro`, then a resume after a settings change wrote `Gemini 3.1 Pro → Claude Sonnet 4.6`). Unchanged turns carry no sentence, so the per-turn model is reconstructed by **carrying the last announced model forward** until the next change. So Antigravity is the one harness where the user can't *select* the model here, but its per-turn *history* is still accurate.

### Two model concepts — keep them distinct

This is the central design idea; everything else follows from it.

1. **Selected model (intent)** — the model the *user chose* for the agent. Lives on `AgentRecord.model` (`Option<String>`). Shown in the **sidebar / agent card**. Updates the instant the user changes it — immediate confirmation, no turn required.
2. **Per-turn model (history)** — the model that *actually produced* a given turn, as the harness reported it. Lives on each **`Turn`** and renders in the **transcript footer**, next to the existing per-message timestamp, on **every** turn. Preserves history: a conversation can read sonnet → sonnet → opus → opus across a switch.

The two surfaces never compete: the sidebar answers "what will the next turn use," the transcript answers "what did each past turn use." This split is *why* there is no "show both models on the card" reconciliation — the card shows only intent.

### Decisions locked for this plan

1. **Input mechanism: hybrid.** A per-harness dropdown of suggested models **plus** a free-text "custom…" escape hatch. Suggestions are convenience; free-text is the real contract (Codex models are plan-dependent and can't be enumerated).
2. **Validation: reactive.** We do **not** validate model strings. A bad/out-of-plan model surfaces as a normal failed turn carrying the harness's own message — same posture as auth. The only structural check is the harness-capability one (an Antigravity agent can never carry a model). One input state *is* guarded as a footgun, not a validation: an empty/whitespace custom entry must never persist as `Some("")` (it would dispatch `--model ""` and fail every turn) — normalize it to "no model" at the form boundary.
3. **Unset = harness default.** No model chosen → `None` → pass **no** flag → harness uses its own default. "Send every turn" applies only when a model is set.
4. **Per-turn model, every turn.** The transcript records and displays the model for every turn. For Claude/Codex/Gemini it comes straight from each turn's harness data; for Antigravity it's reconstructed by carry-forward (last announced model wins). Blank only when genuinely unknown (e.g. an attached Antigravity conversation truncated before its first model announcement). This subsumes the earlier idea of picking one "representative" model for the agent — there is no representative model, only per-turn models plus the user's selected intent.
5. **Change-model lives in the per-agent actions menu**, takes effect on the **next send** (never mutating an in-flight turn), and is absent for Antigravity.

### Shared conventions established early (reuse, don't reinvent)

- **`HarnessKind::supports_model_selection()`** (M1, `crates/core`) is the single backend truth for "can this harness take a model." The frontend mirrors it with one capability map (M5). Every gate — command validation, form picker, card action — derives from one of these two, never an ad-hoc `if harness == antigravity`.
- **"Send the flag every turn when `model` is `Some`, omit when `None`"** (M2) is the dispatch rule all model-capable adapters follow identically.
- **Selected model is `Option<String>` end to end** (`AgentRecord.model`, the `set_agent_model` command, the `setAgentModel` api, the picker's value). Empty/cleared is `None`/`undefined`, never `""` — normalized at the form boundary, not via a wire sentinel.
- **Per-turn model is a dedicated field on `Turn`** (M4), distinct from the agent-scoped `SessionMeta`. One model-picker component (M5) is built once and reused by the change-model editor (M6).

---

## Milestone 1 — Core: `model` field, capability helper, registration threading

### Goal & Outcome
Give the persisted agent a place to hold its chosen model, and establish the one backend fact the rest of the plan gates on.

- An `AgentRecord` carries an optional selected model; agents created before this change load with no model (backward-compatible).
- The codebase has a single authoritative answer to "does harness X support model selection."
- The model can be set at registration in one step, through every creation path (not created-then-mutated).

### Implementation Outline
- Add `pub model: Option<String>` to `AgentRecord` (`crates/core/src/agent.rs`). **No `#[serde(default)]`** — `Option<T>` already deserializes a missing field as `None`, and the sibling `session_id: Option<Uuid>` in this same struct carries no such attribute; match it (serialize `null` when `None`). Backward-compat is real but automatic; the DoD test below is the proof, not the attribute.
- Thread `model: Option<String>` through **all** registration entry points in `crates/core/src/project.rs` — there are five public ones plus the private inner, and an implementer who updates only `register_agent` would silently drop the model on every *attach* path: `register_agent` (86), `register_attached_claude_agent` (117), `register_attached_codex_agent_with_id` (152), `register_attached_gemini_agent` (166), `register_attached_antigravity_agent_with_id` (186), and `register_agent_inner_with_id` (203). The Antigravity attach variant always receives `None` (the capability invariant — it may assert this rather than accept arbitrary input).
- Add `pub fn supports_model_selection(self) -> bool` to `HarnessKind` (`crates/core/src/harness.rs`): `Antigravity => false`, all others `true`, as an exhaustive match (no `_` arm) so a future harness forces a deliberate decision. Put the rationale (Antigravity has no per-call model control — harness-behavior §3.3) in a doc comment on the method.

### Definition of Done
- Round-trip test covers `Some(model)` and `None`; **and a JSON object lacking the `model` key deserializes to `None`** (the backward-compat safeguard — this is the test that de-risks the upgrade path).
- One attach-with-model test (a model-capable harness) proving the model lands at registration, not via a follow-up call.
- Unit test asserts `supports_model_selection()` per variant.
- `make check` green.

---

## Milestone 2 — Adapters: pass the model on every dispatch

### Goal & Outcome
When an agent has a selected model, every turn it runs — first or resumed — uses that model; otherwise behavior is unchanged.

- Claude, Codex, and Gemini agents dispatch with their selected model on first turn and every resume.
- An agent with no selected model dispatches exactly as today (no flag).
- Antigravity ignores the field entirely.

### Implementation Outline
- **Refactor `build_args` for Codex and Antigravity to receive `&AgentRecord`.** Claude and Gemini already do; the `dispatch` methods all hold the `&AgentRecord`, so this is a signature/threading change at the call sites.
- For **Claude / Codex / Gemini**: when `agent.model` is `Some`, append the model flag with its value; when `None`, append nothing. Apply on **both** the first-turn and resume arg paths (these adapters build args differently per path — the flag goes on both). Confirm exact flag spelling per CLI `--help` (`--model` for Claude/Gemini; `-m`/`--model` for Codex, valid on `exec` and `exec resume`).
- For **Antigravity**: take `&AgentRecord` for signature uniformity but add no flag; leave a one-line comment stating why (no per-call model control — §3.3).
- No value validation here — an invalid model is the harness's to reject via the existing failure path.

### Definition of Done
- Unit tests on each `build_args`: model present → flag+value on first-turn and resume paths; absent → no flag; Antigravity never emits a model flag.
- **Live tests** (one per model-capable harness, named `live_<harness>_…`): construct the **real adapter**, `dispatch` with an `AgentRecord` carrying a chosen model, and assert the chosen model surfaces in the adapter's emitted `AdapterEvent` (`SessionMeta`/per-turn model) — driven through our code, **not** a bare CLI probe, so the test covers both the real CLI honoring the flag *and* our `build_args` passing it. Tiny prompts per cost discipline.
- `make check` green; `make test-live` covers the new live tests.

---

## Milestone 3 — Backend: create / attach with a model, and change it later

### Goal & Outcome
The selected model reaches the persisted record, and a later change persists and takes effect on the next send.

- Creating or attaching an agent can specify a model; the record stores it.
- A new command changes (or clears) an existing agent's model and re-persists it.
- A model can never attach to a harness that doesn't support one (enforced server-side, independent of the UI).

### Implementation Outline
- Extend `create_agent_impl` and `attach_agent_impl` (`crates/app/src/commands.rs`) to accept `model: Option<String>`, and update the `#[tauri::command]` wrappers.
- **Capability check runs first.** In `attach_agent_impl` the rejection `model.is_some() && !harness.supports_model_selection()` must be the first thing after project/active resolution — **before** the per-harness session lookup and sidecar writes (the attach flow writes sidecars before committing the registry record; a check placed inside the harness match would orphan a sidecar on rejection). Same check on the create path.
- Add `set_agent_model_impl` (+ wrapper) taking `model: Option<String>` (so clearing back to the harness default is expressible — a non-optional `String` could not say "clear"): look up the agent, set `AgentRecord.model`, re-persist via the same registry-write path the other mutating commands use (mirror `rename_agent_impl`), return the updated record. No effect on any in-flight turn — the new model applies on the next dispatch because M2 reads the field fresh each time. Reject for an unsupported harness.
- No model-string normalization or allow-list here; the user's string passes through verbatim. (The `Some("")` footgun is handled at the form boundary in M5, not here.)

### Definition of Done
- Tests on the free functions: create/attach with a model stores it; `set_agent_model` updates and persists (reload proves durability); **clearing persists `None`**; setting a model on an Antigravity agent (all three paths) returns the capability error **and leaves no orphan sidecar**; the attach rejection happens before any sidecar write.
- `make check` green.

---

## Milestone 4 — Per-turn model on the transcript

### Goal & Outcome
Every turn records the model that produced it, for all harnesses, on both the live stream and on reopen — so the transcript can show a faithful per-turn history of which model ran.

- Each completed turn carries the model the harness reported for that turn.
- A mid-conversation model switch is reflected turn-by-turn (e.g. sonnet, sonnet, opus, opus).
- Antigravity turns carry a model when available and `None` otherwise (accepted limitation), without breaking anything.

### Implementation Outline
This replaces the earlier "Codex latest-model" milestone: per-turn attachment makes "which model represents the agent" a non-question — every turn carries its own.

- **Add a dedicated `model: Option<String>` to the `Turn` wire shape** (`crates/harness/src/transcript.rs`, and the TS `LoadedTurn`/`Turn` in `src/lib/types.ts`). Keep it **separate from** the agent-scoped `SessionMeta.model`: `SessionMeta` is emitted as a standalone, non-turn-anchored event (events.rs:108) and carries agent-level registry data; the per-turn model is a property of the turn itself. Do not overload `Turn.meta` for it.
- **Populate per turn, per harness, on both paths** (live parse and session-file hydration):
  - **Claude** — the per-turn model is on each turn's assistant record / `init` (`claude-sonnet-4-6` etc.); attach it to that turn.
  - **Codex** — each turn's `turn_context.payload.model` (the per-turn value M-historically read first-wins for `SessionMeta`); attach each to its own turn. The old set-once `model_set` gate in `codex/session_file.rs` is removed in favor of per-turn attachment.
  - **Gemini** — `init.model` per invocation; attach to that turn.
  - **Antigravity** — **carry-forward.** Each turn carries a `USER_SETTINGS_CHANGE` "from X to Y" sentence only when the model *changed* on that turn (verified: the change is announced on the resume turn where it takes effect). So maintain a running "current model": on a turn whose records contain a settings-change, set it to the announced "to" value; otherwise inherit the prior turn's value; stamp every turn with the running value. On **hydrate**, walk turns chronologically and carry forward from the start of the transcript; **live**, hold the running value in the adapter's per-agent state across turns (the existing "empty-model-keeps-prior" rule is this pattern — extend it to stamp the turn). Leave `None` only before any model has ever been announced (e.g. an attached conversation truncated before its first announcement). The fragile string-scrape itself is unchanged; what's new is attaching its result per-turn with carry-forward.
- **Live path correlation:** for Claude/Codex/Gemini the model arrives as an agent-scoped `SessionMeta` event with no turn anchor. To land it on the right turn live, associate the most-recent model with the turn being finalized (at `TurnEnd`/turn close in the state reducer or dispatcher). Transmit this explicitly so the implementer doesn't assume the model is already turn-anchored — it is not. (Antigravity's carry-forward state, above, is the analogous mechanism for that harness.)
- **`SessionMeta.model` consumer audit.** After M6 the sidebar no longer displays `SessionMeta.model` (it shows `AgentRecord.model`). Check whether any consumer of `SessionMeta.model` remains; if none, remove the field for a clean architecture; if a harness's per-turn population reuses it as an intermediate, keep it only as that. Record the decision in the milestone notes.

### Definition of Done
- Per-harness unit/fixture tests: a session file with two turns on **different** models yields two turns whose `model` differ (Codex is the canonical case — extend/convert the former first-wins test to assert per-turn values, an intended behavior change). Claude and Gemini equivalents.
- An Antigravity carry-forward test: a transcript where turn 1 announces a model and turns 2–3 don't → all three carry that model; a later change-sentence flips the subsequent turns to the new model; a transcript with no announcement at all hydrates with `model: None` and does not error.
- **Live tests — the upstream-contract guard (required).** Per-turn model attribution and Antigravity carry-forward read harness output that can drift across CLI versions, so this milestone *must* land live coverage of the actual switching behavior, not just single-dispatch honoring. **Every live test here is adapter-driven:** construct the real adapter (`ClaudeCodeAdapter::new()` etc.), call `dispatch` through our production path, and assert on the **per-turn `model` in the emitted `AdapterEvent`s** — never grep the raw CLI output. That is what makes the test catch *both* upstream drift (the CLI stops emitting the contract) *and* our own regressions (`build_args` drops the flag, the parser stops extracting the model, carry-forward miscomputes) in one shot.
  - **Model switch (Claude, Gemini)** — `live_<harness>_model_changes_across_turns`: dispatch turn 1 with an `AgentRecord` model A, resume with model B, assert each turn's per-turn `model` in our events reflects the switch (A then B). Two valid models exist for both (verified: Claude `sonnet`/`opus`, Gemini `gemini-2.5-flash`/`-pro`).
  - **Codex** — plan-gating usually leaves only one valid model on a given account (only `gpt-5.5` accepted on the probe account), so its live test asserts the chosen model is honored and recorded **per turn** for a single model; document that a true A→B switch isn't reliably testable without a multi-model plan.
  - **Antigravity carry-forward** — `live_antigravity_model_change_announced_on_resume`: dispatch turn 1 through `AntigravityAdapter` (recording the dev's current settings model X), change `~/.gemini/antigravity-cli/settings.json`'s `model` to a different value Y, resume, and assert that **our adapter's emitted per-turn model** is X for turn 1 and Y for turn 2 (proving the `USER_SETTINGS_CHANGE … from X to Y` contract *and* our carry-forward both hold). This is the canary for Antigravity silently dropping the change-announcement on a CLI bump.

    **Config-mutation safety protocol (this test edits the real `settings.json`; isolated-`HOME` was tested and rejected — agy re-prompts for OAuth under a fresh HOME, so the dev's authenticated `~/.gemini` is the only workable home):**
    1. **Byte-for-byte backup** of `settings.json` to a **stable, well-known path** (not a random temp name) — restore the user's exact original bytes (their other keys/formatting), never a parsed-and-reserialized copy.
    2. **Self-heal at test start:** if the backup file already exists, a prior run was interrupted — restore `settings.json` from it *first* (the backup always holds the pristine original), and **fail loudly with the recovery path printed** so the interrupted run is visible, not silently swallowed.
    3. **`Drop`/scope guard** restores from backup then **deletes** it on exit — this covers normal completion *and* assertion-panic (Rust unwinds on panic, running `Drop`). Backup-absent ⇒ clean state; backup-present ⇒ recovery pending.
    4. **Known gap, mitigated:** `SIGKILL` / `Ctrl-C` (SIGINT) / crash / `panic=abort` bypass `Drop`, so a hard-killed run leaves `settings.json` clobbered — but step 2 repairs it on the next run, and the pristine value survives in the backup file until a clean restore deletes it. No signal handler (fragile in the test harness); the self-heal is the mitigation.
    5. Mark the test **serial** (the file is global — it must not interleave with other agy live tests).

    Document this protocol in the test itself (a module/test doc-comment), including the rationale that mutating harness config *in a test* does not violate the production rule that the app never writes harness config — it is the only way to exercise the real contract.
- `make check` green; `make test-live` (and `make test-live-<harness>`) cover the new live tests.

> Note: Codex `turn_context` is written at turn **start**, not on success (verified — a failed model turn still wrote its `turn_context`). So a failed/interrupted turn correctly carries the model it attempted; no special-casing needed.

---

## Milestone 5 — Frontend: model picker in the create-agent form

### Goal & Outcome
A user creating (or attaching) an agent can choose its model, with suggestions and a free-text fallback — except for Antigravity, where the control explains why it's unavailable.

- The form offers a hybrid model picker (suggestions + custom free-text) for Claude/Codex/Gemini.
- Selecting Antigravity disables the picker with a short note that model selection happens inside Antigravity itself; no model is submitted.
- The chosen model reaches the backend create/attach call; an empty/whitespace custom entry submits no model (never `""`).

### Implementation Outline
- Add a frontend capability map to `src/lib/harnessDisplay.ts` mirroring `supports_model_selection()` (exhaustive `Record<HarnessKind, …>`, matching the existing maps). This is the UI's gate.
- Build a **single reusable model-picker component** (hybrid: dropdown of suggested models for the selected harness + a "custom…" option revealing a free-text input). Its value is `string | undefined`, where `undefined`/empty/whitespace means "no model → harness default." Normalize empty → `undefined` **inside this component (the form boundary)**, so the `Some("")` footgun can never cross the IPC boundary. Built here so M6 reuses it.
- Add a per-harness suggested-model list (frontend constant), seeded minimally and treated as a maintained convenience — **not** authoritative or validated. Starting suggestions: Claude `opus` / `sonnet` / `haiku`; Gemini `gemini-2.5-pro` / `gemini-2.5-flash` (+ `auto`); Codex `gpt-5.5`. Confirm current aliases against each CLI `--help` / harness-behavior before finalizing; comment that the list is convenience-only and may drift.
- Wire the picker into `CreateAgentForm.svelte` after the harness selector; when the selected harness is not model-capable, render it disabled with the note and omit the model from submit. Thread `model?: string` through `createAgent` / `attachAgent` in `api.ts` and the form's submit types.

### Definition of Done
- Component tests (mock `invoke`): a suggested model and a custom model each produce the right `model` in the create payload; switching to Antigravity disables the picker and submits no model; an untouched picker submits no model; **an empty/whitespace custom entry submits no model, not `""`**.
- The picker is a standalone component (reuse-ready for M6), not inlined.
- `make check` green.

---

## Milestone 6 — Frontend: change the model, and surface intent vs. history

### Goal & Outcome
For a model-capable agent the user can change its model after creation; the sidebar shows the selected model (immediate), and the transcript shows the per-turn model (history).

- The per-agent actions menu offers "Change model" for Claude/Codex/Gemini and omits it for Antigravity.
- Choosing/clearing a model calls the backend, updates the record, and the **sidebar reflects the selected model immediately** — before any turn runs.
- The **transcript footer shows each turn's model** next to its timestamp, on every turn.

### Implementation Outline
- Add a "Change model" item to `AgentActionsMenu.svelte`, gated on the frontend capability map (absent for Antigravity). It opens an editor reusing the M5 picker, pre-filled with the agent's current model; submit calls a new `setAgentModel(agentId, model?: string)` api → `set_agent_model` command → updated `AgentRecord` flows back into state.
- **Sidebar shows the selected model.** Change the sidebar model line (`Sidebar.svelte`, currently reading `runtime.meta.model`) to read `AgentRecord.model`. This is intent, so it updates the moment the change lands — the immediate confirmation that resolves the old "did my change take?" gap. When no model is selected (`None`), clean-hide per the sidebar's existing absent-field convention.
- **Transcript footer shows the per-turn model.** In the per-message footer that already renders the timestamp (`UnifiedTranscript.svelte`'s `messageMeta`), render `Turn.model` for every agent turn; omit only when `None` (rare — an Antigravity conversation truncated before its first announcement). Keep it subtle (muted, small) consistent with the timestamp styling.
- Comment that a model change applies on the next send (dispatch reads the field fresh each turn — M2), so no in-flight handling is needed.

### Definition of Done
- Component tests: the action is present for a model-capable agent, absent for Antigravity; submitting a new model invokes `set_agent_model` with the right args; **the sidebar reflects the selected model on an agent that has already run a turn** (the real post-turn state — not a fresh agent), proving intent shows immediately; clearing the model hides the sidebar line.
- Transcript test: an agent turn renders its `model` in the footer; a turn with `model: None` renders no model.
- **Docs:** update `docs/research/harness-behavior.md` §3.3 status note (per-agent model selection ships; Antigravity per-turn model is best-effort/blank) and the README "Harness support and limitations" model bullet; update `docs/system-design.md` §9 if model selection belongs in the capability matrix.
- `make check` green.

---

## Out of scope (explicitly)

- Enumerating or validating model values, or detecting plan-gated Codex models — reactive failure is the design.
- Any mechanism to set Antigravity's model (global harness-owned config is off-limits; per-`HOME`/config-dir isolation was considered and declined — §3.3). Antigravity model selection is read-only here; its per-turn *display* is handled via carry-forward (M4) and is not a limitation.
- Changing a model mid-turn / interrupting an in-flight turn to re-model it.
- A global or project-level default model — selection is strictly per-agent.
- "Show both models on the agent card" reconciliation — obviated by the sidebar-intent / transcript-history split.
