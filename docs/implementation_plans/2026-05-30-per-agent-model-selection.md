# Per-agent model selection

**Status:** proposed · **Created:** 2026-05-30

Let each agent run on a user-chosen model. The model is picked when the agent is created, and — for harnesses that support it — changed later from the agent's actions menu. Antigravity is the exception: its CLI exposes no model control, so the picker is disabled at creation and the change-model action is absent.

## Background — read before implementing

This plan is the product of an empirical probe of all four harness CLIs. **Read `docs/research/harness-behavior.md` §3.3 ("Model selection — can we set the model on a turn?") first** — it is the ground truth for the per-harness behavior this plan encodes, and the decisions below depend on it. The essentials:

- **Claude** (`--model <alias|id>`), **Codex** (`-m/--model <id>`), **Gemini** (`-m/--model <id>`) all accept a model on every invocation, including resume. Verify exact flag spelling against each CLI's `--help` before wiring it.
- **Model is per-invocation, not configured once.** Claude happens to be session-sticky (a resumed session keeps its last model), but Gemini reverts to its default without the flag and Codex re-derives per turn. The uniform rule — decided — is **send the flag on every dispatch when a model is set**, so we never depend on per-harness stickiness. One code path, no special cases.
- **Codex models are gated by the user's plan**, not by us. The CLI forwards any `-m` value; the API rejects out-of-plan ones with a 400. We do not enumerate or validate model values (see "reactive validation" below).
- **Antigravity has no per-call model control and we will not build one.** Its only model lever is a global, harness-owned config file we've decided never to touch. Antigravity agents run on whatever model was last selected inside Antigravity itself. In our UI, model selection is simply unavailable for this harness.

### Decisions locked for this plan

1. **Input mechanism: hybrid.** A per-harness dropdown of suggested models **plus** a free-text "custom…" escape hatch. Suggestions are a convenience for the common case; free-text covers plan-specific or newly-released models we haven't catalogued.
2. **Validation: reactive.** We do **not** validate model strings. A bad/out-of-plan model surfaces as a normal failed turn carrying the harness's own message — identical posture to auth (discover on send, fix, re-send). The only structural check is the harness-capability one (an Antigravity agent can never carry a model).
3. **Unset = harness default.** No model chosen → store `None` → pass **no** model flag → the harness uses its own default (today's behavior). The "send every turn" rule only applies when a model is set.
4. **Codex stale-model display is fixed here** (Milestone 4): the sidebar must read the model from the **latest** turn, not the first, or a changed Codex model would display stale.
5. **Change-model lives in the per-agent actions menu** and takes effect on the **next send** — never mutating an in-flight turn. The item is absent for Antigravity.

### Shared conventions established early (reuse, don't reinvent)

- **`HarnessKind::supports_model_selection()`** (Milestone 1, `crates/core`) is the single backend source of truth for "can this harness take a model." The frontend mirrors it with one capability map (Milestone 5). Every gate — command validation, form picker, card action — derives from one of these two, never an ad-hoc `if harness == antigravity`.
- **"Send the flag every turn when `model` is `Some`, omit when `None`"** (Milestone 2) is the dispatch rule all model-capable adapters follow identically.
- **One model-picker component** (Milestone 5) is built once and reused by both the create form and the change-model editor (Milestone 6).

---

## Milestone 1 — Core: `model` field + capability helper

### Goal & Outcome
Give the persisted agent a place to hold its chosen model, and establish the one backend fact the rest of the plan gates on.

- An `AgentRecord` can carry an optional model string; agents created before this change load with no model (backward-compatible).
- The codebase has a single authoritative answer to "does harness X support model selection," used everywhere a gate is needed.

### Implementation Outline
- Add `model: Option<String>` to `AgentRecord` (`crates/core/src/agent.rs`). Optional so existing on-disk records deserialize as `None` with no migration. Serde handles the JSONL round-trip; confirm the existing round-trip test exercises the new field.
- Thread the model through `Project::register_agent` (and any sibling constructor used by attach) so a record can be created with a model in one step rather than created-then-mutated.
- Add `pub fn supports_model_selection(self) -> bool` to `HarnessKind` (`crates/core/src/harness.rs`): `Antigravity => false`, all others `true`. Write it as an exhaustive match (no `_` arm) so a future harness variant forces a deliberate decision here. The rationale (Antigravity has no per-call model control — see harness-behavior §3.3) must live in a doc comment on this method, not just this plan.

### Definition of Done
- Round-trip test covers a record with `Some(model)` and one with `None`, and a record serialized *without* the field still deserializes (the backward-compat case).
- Unit test asserts `supports_model_selection()` per variant.
- `make check` green.

---

## Milestone 2 — Adapters: pass the model on every dispatch

### Goal & Outcome
When an agent has a chosen model, every turn it runs — first or resumed — uses that model; when it doesn't, behavior is unchanged.

- Claude, Codex, and Gemini agents dispatch with their chosen model applied on first turn and on every resume.
- An agent with no chosen model dispatches exactly as today (no flag).
- Antigravity ignores the field entirely (no flag, no behavior change).

### Implementation Outline
- **Refactor `build_args` for Codex and Antigravity to receive `&AgentRecord`.** Claude and Gemini already do. The adapter `dispatch` methods all already hold the `&AgentRecord`, so this is a signature/threading change at the call sites — load-bearing only in that the model isn't reachable without it.
- For **Claude / Codex / Gemini**: when `agent.model` is `Some`, append the model flag with its value; when `None`, append nothing. Apply it on **both** the first-turn and resume arg paths (these adapters build args differently per path — the flag goes on both). Confirm exact flag spelling per CLI `--help` (`--model` for Claude/Gemini; `-m`/`--model` for Codex, valid on `exec` and `exec resume`).
- For **Antigravity**: take the `&AgentRecord` for signature uniformity but do **not** add any flag. Leave a one-line comment stating why (no per-call model control — harness-behavior §3.3), so a future reader doesn't "helpfully" add one.
- Do not validate the model value here; an invalid value is the harness's to reject (it surfaces through the existing failure path).

### Definition of Done
- Unit tests on each `build_args`: model present → flag+value emitted on first-turn and resume paths; model absent → no flag. Antigravity: never emits a model flag regardless of the field.
- **Live tests** (one per model-capable harness, named `live_<harness>_…` per the live-test convention) that dispatch with a chosen model and confirm the harness honors it — assert against the model the harness reports back (Claude/Gemini `init.model`; Codex `turn_context.model`). Keep prompts tiny per cost discipline. These are the regression guard if a CLI renames the flag.
- `make check` green; `make test-live` covers the new live tests.

---

## Milestone 3 — Backend: create / attach with a model, and change it later

### Goal & Outcome
The model chosen in the UI reaches the persisted record, and a later change persists and takes effect on the next send.

- Creating or attaching an agent can specify a model; the record stores it.
- A new command changes an existing agent's model and re-persists it.
- A model can never be attached to a harness that doesn't support one (structural invariant enforced server-side, not just hidden in the UI).

### Implementation Outline
- Extend `create_agent_impl` and `attach_agent_impl` (`crates/app/src/commands.rs`) to accept `model: Option<String>`, and update their `#[tauri::command]` wrappers.
- Add `set_agent_model_impl` (+ wrapper): look up the agent, update `AgentRecord.model`, re-persist via the same registry-write path the other mutating commands use (follow `rename_agent_impl`'s structure), return the updated record. No effect on any in-flight turn — the new model applies on the next dispatch because Milestone 2 reads the field fresh each time.
- **Capability check, all three entry points:** if `model.is_some()` and `!agent.harness.supports_model_selection()`, reject with a typed `AppError`. This is the *only* validation — model **strings** are not checked (reactive posture). The check keeps an Antigravity record from ever carrying a dead model value.
- No model-string normalization, no allow-list lookup. Pass the user's string through verbatim.

### Definition of Done
- Tests on the free functions: create/attach with a model stores it; `set_agent_model` updates and persists (reload proves durability); setting a model on an Antigravity agent (any of the three paths) returns the capability error; setting `None`/clearing is allowed for all harnesses.
- `make check` green.

---

## Milestone 4 — Codex: display the current model, not the first

### Goal & Outcome
After a Codex agent's model is changed mid-conversation, the sidebar shows the model it's actually running now.

- The model surfaced for a Codex agent reflects its most recent turn, not its first.

### Implementation Outline
Small, self-contained, and independent of Milestones 1–3 (it concerns *reading back* the model, not setting it). In `crates/harness/src/codex/session_file.rs`, the enrichment uses a set-once gate (`model_set`, "first-turn_context wins") so the first `turn_context.payload.model` becomes `SessionMeta.model`. Codex writes a fresh `turn_context.model` **per turn** (verified — harness-behavior §3.3), so switch this to **last-wins**: let each `turn_context` overwrite, leaving the final one as the session-level model. Update the now-incorrect doc comments in this file (the lines stating "first one in file" / "first-turn model is authoritative") to describe last-wins and *why* (per-turn overrides mean the latest turn is the live model).

### Definition of Done
- Unit test: a session file with two `turn_context` records carrying different models yields the **second** model in the enrichment / `SessionMeta`. (Add or extend a fixture; the existing first-wins test, if any, is updated to assert last-wins — this is an intended behavior change, not a regression to preserve.)
- `make check` green.

---

## Milestone 5 — Frontend: model picker in the create-agent form

### Goal & Outcome
A user choosing a harness when creating an agent can also choose its model, with sensible suggestions and a free-text fallback — except for Antigravity, where the control explains why it's unavailable.

- The create/attach form offers a hybrid model picker (suggested values + custom free-text) for Claude/Codex/Gemini.
- Selecting Antigravity disables the picker and shows a short note that model selection happens inside Antigravity itself; no model is submitted.
- The chosen model reaches the backend create/attach call.

### Implementation Outline
- Add a frontend capability map to `src/lib/harnessDisplay.ts` mirroring `supports_model_selection()` (exhaustive `Record<HarnessKind, …>`, matching the existing maps in that file). This is the UI's gate.
- Add a **single reusable model-picker component** (hybrid: a dropdown of suggested models for the selected harness + a "custom…" option revealing a free-text input). Build it here so Milestone 6 reuses it rather than duplicating. Its value is `string | undefined` where `undefined`/empty means "no model → harness default."
- Add a per-harness suggested-model list (a frontend constant). Seed it minimally and treat it as a maintained convenience, **not** an authoritative or validated set — free-text is the real contract (Codex especially, since valid models are plan-dependent). Starting suggestions: Claude `opus` / `sonnet` / `haiku` (stable aliases); Gemini `gemini-2.5-pro` / `gemini-2.5-flash` (+ `auto` default); Codex `gpt-5.5`. Confirm current aliases against each CLI `--help` / harness-behavior before finalizing, and add a comment that the list is convenience-only and may drift.
- Wire the picker into `CreateAgentForm.svelte` after the harness selector. When the selected harness is not model-capable, render the picker disabled with the explanatory note and ensure the submit payload omits the model. Thread `model?: string` through `createAgent` / `attachAgent` in `api.ts` and the form's submit types.

### Definition of Done
- Component tests (per the project's component-test guidance — mock `invoke`): selecting a suggested model and a custom model each produce the right `model` in the create payload; switching to Antigravity disables the picker and submits no model; leaving the picker untouched submits no model. Reuse-readiness: the picker is a standalone component, not inlined in the form.
- `make check` green.

---

## Milestone 6 — Frontend: change a model from the agent's actions menu

### Goal & Outcome
For a model-capable agent, the user can change its model after creation; the change persists and applies to the next send.

- The per-agent actions menu offers "Change model" for Claude/Codex/Gemini agents and omits it for Antigravity.
- Choosing a new model calls the backend, updates the record, and the agent card reflects the change.

### Implementation Outline
- Add a "Change model" item to `AgentActionsMenu.svelte`, gated on the frontend capability map (absent for Antigravity). It opens an editor that reuses the Milestone 5 picker, pre-filled with the agent's current model.
- On submit, call a new `setAgentModel` api function → `set_agent_model` command → updated `AgentRecord` flows back into frontend state; the card re-renders.
- **Selected vs. reported model in the card:** the card's existing model line shows the model the harness *reported running* (`runtime.meta.model`). Keep that as the primary display. The newly *selected* model is the editable setting (`AgentRecord.model`); surface it as the pre-turn fallback (before any turn has produced a reported model) so a freshly-set model is visible immediately, and let the reported model take over once a turn runs. Do not conflate the two — reported is what ran, selected is intent. (For Antigravity there is only a reported model, read-only, unchanged by this plan.)
- Note in a comment that the change applies on the next send (the dispatch path reads the field fresh each turn — Milestone 2), so no in-flight handling is needed.

### Definition of Done
- Component tests: the action is present for a model-capable agent and absent for Antigravity; submitting a new model invokes `set_agent_model` with the right args and the card reflects the returned record; the editor pre-fills the current model.
- `make check` green.
- **Docs:** update `docs/research/harness-behavior.md` §3.3 status note and, if warranted, the README "Harness support and limitations" model bullet, to reflect that per-agent model selection now ships (with Antigravity's exception intact). `docs/system-design.md` §9 capability matrix updated if model selection belongs there.

---

## Out of scope (explicitly)

- Enumerating or validating model values, or detecting plan-gated Codex models — reactive failure is the design.
- Any mechanism to set Antigravity's model (global harness-owned config is off-limits; per-`HOME`/config-dir isolation was considered and declined — harness-behavior §3.3).
- Changing a model mid-turn / interrupting an in-flight turn to re-model it.
- A global or project-level default model — selection is strictly per-agent.
