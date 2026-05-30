# Per-agent model & effort selection

**Status:** proposed · **Created:** 2026-05-30

Let each agent run on a user-chosen **model** and **reasoning-effort level**. Both are picked when the agent is created, changed later (for harnesses that support them) from the agent's actions menu, shown as the agent's current intent in the sidebar, and recorded **per turn** in the transcript so a mid-conversation switch is visible in history. The two axes have **different per-harness capability sets** (below), so each is independently gated.

## Background — read before implementing

This plan is the product of empirical probes of all four harness CLIs. **Read `docs/research/harness-behavior.md` §3.3–§3.4 (model selection; reasoning effort) first** — they are the ground truth for the per-harness behavior this plan encodes. The essentials:

**Model** — `--model <alias|id>` (Claude), `-m/--model <id>` (Codex), `-m/--model <id>` (Gemini), and **nothing** for Antigravity (its model is a global, harness-owned config we never touch; its agents run on whatever was last selected inside Antigravity). Model is per-invocation, not configured once — Claude is session-sticky, Gemini reverts to its default without the flag, Codex re-derives per turn — so the rule is **send the flag every turn when set**. Model **values are not enumerable**: no harness has a usable "list models" command (`claude models` only makes the LLM recite a training-frozen table; the others have none), and Codex values are **plan-gated** (only `gpt-5.5` accepted on the probe account; out-of-plan ids 400 from the API, not from us). So model input is **free-text with hardcoded suggestions**, never a validated list.

**Effort / reasoning level** — a *separate* axis with a *different* capability set:
- **Claude** — `--effort <level>` flag. Valid: `low, medium, high, xhigh, max` (empirically enumerated). `max` is session-only and has a historical "flag ignored" bug ([#50099](https://github.com/anthropics/claude-code/issues/50099), v2.1.113) — re-check on current CLI.
- **Codex** — `-c model_reasoning_effort=<level>` config override (there is **no** `-e`/`--effort` flag — a web claim to the contrary was verified false). Valid: `none, minimal, low, medium, high, xhigh`. Recorded **per turn** in `turn_context` right next to `model`.
- **Gemini** — thinking is `settings.json`-only (`thinkingConfig`/`thinkingBudget`/`thinkingLevel`); **no CLI flag exists** (confirmed; open upstream feature requests for one). That's harness-owned config we never touch → **no effort control for Gemini.**
- **Antigravity** — the level is **baked into the model display name** (`"Gemini 3.1 Pro (High)"`, `"Claude Sonnet 4.6 (Thinking)"`); there is no separate effort axis, and we can't set the Antigravity model anyway.

### Capability matrix — what *we* can set per agent

| | Claude | Codex | Gemini | Antigravity |
|---|:---:|:---:|:---:|:---:|
| **Model selection** | ✅ `--model` | ✅ `-m` | ✅ `-m` | ❌ (config-only, off-limits) |
| **Effort selection** | ✅ `--effort` | ✅ `-c model_reasoning_effort=` | ❌ (config-only, off-limits) | ❌ (folded into model name) |

Note the mirror: Gemini gives a model flag but locks thinking behind config; Antigravity locks the model behind config but folds effort into the model name. Both end up with one axis we can't drive — different axis each.

### Two concepts per axis — keep them distinct

This is the central design idea, and it applies to **both** model and effort.

1. **Selected (intent)** — what the *user chose* for the agent. Lives on `AgentRecord.{model,effort}` (`Option<String>` each). Shown in the **sidebar / agent card**. Updates the instant the user changes it — immediate confirmation, no turn required.
2. **Per-turn (history)** — what *actually produced* a given turn, as the harness reported it. Lives on each **`Turn`** and renders in the **transcript footer** next to the timestamp, on **every** turn. Preserves history across a mid-conversation switch.

The two surfaces never compete: the sidebar answers "what will the next turn use," the transcript answers "what did each past turn use." This is why there is no "show both values on the card" reconciliation.

### Decisions locked

1. **Input mechanisms differ by axis.** **Model = hybrid** (per-harness dropdown of *suggested* models + free-text "custom…", because values aren't enumerable and are plan-dependent). **Effort = fixed enum dropdown** (the levels are a known, closed set per harness — no free-text, so no empty-string footgun). A "no selection / default" state is distinct from any explicit level; for Codex note that `none` is a *real* level (forces no reasoning) and is **not** the same as leaving effort unset (which passes no flag and uses the harness default).
2. **Validation: reactive.** We don't validate values server-side beyond the structural capability check (an Antigravity agent can't carry a model; a Gemini/Antigravity agent can't carry an effort). A bad/out-of-plan model surfaces as a normal failed turn with the harness's message. Effort is UI-constrained to its enum, so it has no footgun; for model, an empty/whitespace custom entry must normalize to "unset" at the form boundary, never persist as `Some("")` (which would dispatch `--model ""` and fail every turn).
3. **Unset = harness default.** No selection → `None` → pass **no** flag → harness uses its own default. "Send every turn" applies only when a value is set.
4. **Per-turn display, every turn.** The transcript records and displays both model and effort per turn (carry-forward for Antigravity's model; blank only when genuinely unknown). For Codex both come straight from `turn_context`; for Claude the *model* is per-turn from its records, but whether the *effort* it ran at is exposed per-turn is unverified (its `init` carries model, not obviously effort) — if absent, Claude's effort is shown as selected-intent only, with no per-turn footer value (acceptable).
5. **Change from the per-agent actions menu**, taking effect on the **next send** (never mutating an in-flight turn). Each control is absent for harnesses lacking that capability.

### Shared conventions established early (reuse, don't reinvent)

- **Two capability helpers** in `crates/core` (M1): `HarnessKind::supports_model_selection()` (Claude/Codex/Gemini) and `supports_effort_selection()` (Claude/Codex). The frontend mirrors each with one capability map (M5). Every gate — command validation, picker visibility, card action — derives from these, never an ad-hoc `if harness == …`.
- **`model` and `effort` are sibling `Option<String>` fields**, threaded identically through registration, dispatch, commands, and the per-turn `Turn`. They are two explicit fields (not a generic settings map — their value spaces and pickers differ), but they share every pattern: capability-gated, sent every turn when `Some`, displayed per turn.
- **"Send the flag every turn when `Some`, omit when `None`"** (M2) is the dispatch rule for both axes on every model/effort-capable adapter.
- **Two reusable pickers** (M5) — a hybrid model picker and a fixed-enum effort picker — built once and reused by the change-editors (M6).

---

## Milestone 1 — Core: `model` + `effort` fields, capability helpers, registration threading

### Goal & Outcome
Give the persisted agent a place to hold both selected values, and establish the two backend facts the rest of the plan gates on.

- An `AgentRecord` carries an optional selected `model` and `effort`; agents created before this change load with neither (backward-compatible).
- The codebase has a single authoritative answer to "does harness X support model selection" and "…effort selection."
- Both values can be set at registration in one step, through every creation path.

### Implementation Outline
- Add `pub model: Option<String>` and `pub effort: Option<String>` to `AgentRecord` (`crates/core/src/agent.rs`). **No `#[serde(default)]`** — `Option<T>` already deserializes a missing field as `None`, and the sibling `session_id: Option<Uuid>` carries no such attribute; match it (serialize `null` when `None`). Backward-compat is automatic; the DoD test is the proof.
- Thread both through **all** registration entry points in `crates/core/src/project.rs` — five public plus the private inner; updating only `register_agent` would silently drop the values on every *attach* path: `register_agent` (86), `register_attached_claude_agent` (117), `register_attached_codex_agent_with_id` (152), `register_attached_gemini_agent` (166), `register_attached_antigravity_agent_with_id` (186), `register_agent_inner_with_id` (203). The Antigravity attach variant always receives `model: None`; Antigravity and Gemini attach always receive `effort: None` (capability invariants — may assert rather than accept arbitrary input).
- Add to `HarnessKind` (`crates/core/src/harness.rs`): `supports_model_selection(self) -> bool` (`Antigravity => false`, else `true`) and `supports_effort_selection(self) -> bool` (`Claude/Codex => true`, `Gemini/Antigravity => false`). Both exhaustive matches (no `_` arm) so a future harness forces deliberate decisions. Put the rationale (per harness-behavior §3.3/§3.4) in doc comments.

### Definition of Done
- Round-trip tests cover `Some`/`None` for both fields **and a JSON object lacking either key deserializes to `None`** (the backward-compat safeguard).
- One attach-with-values test proving both land at registration, not via a follow-up call.
- Unit tests assert both capability helpers per variant.
- `make check` green.

---

## Milestone 2 — Adapters: pass model + effort on every dispatch

### Goal & Outcome
When an agent has a selected model and/or effort, every turn — first or resumed — uses them; otherwise behavior is unchanged.

- Claude dispatches with `--model` and `--effort` when set; Codex with `-m` and `-c model_reasoning_effort=<level>`; Gemini with `-m` only; Antigravity with neither.
- An agent with nothing selected dispatches exactly as today.

### Implementation Outline
- **Refactor `build_args` for Codex and Antigravity to receive `&AgentRecord`** (Claude and Gemini already do); the `dispatch` methods hold it.
- Apply on **both** the first-turn and resume arg paths (these adapters build args differently per path):
  - **Claude** — `agent.model` → `--model <v>`; `agent.effort` → `--effort <v>`.
  - **Codex** — `agent.model` → `-m <v>`; `agent.effort` → `-c model_reasoning_effort=<v>` (valid on `exec` and `exec resume`).
  - **Gemini** — `agent.model` → `--model <v>`. **Ignore effort** (no flag exists).
  - **Antigravity** — ignore both; one-line comment why (no per-call control — §3.3/§3.4).
- When a field is `None`, append nothing. No value validation here — invalid values are the harness's to reject. Confirm exact flag spelling against each CLI `--help` before wiring.

### Definition of Done
- Unit tests on each `build_args`: each field present → its flag+value on first-turn and resume paths; absent → no flag; Gemini never emits an effort flag; Antigravity never emits either.
- **Live tests** (named `live_<harness>_…`): construct the **real adapter**, `dispatch` with an `AgentRecord` carrying a model (and, for Claude/Codex, an effort), driven through our code, **not** a bare CLI probe. **Scope the assertions to what carries a value at this milestone:** model surfaces in the emitted `SessionMeta`, so assert it there; **effort rides no emitted event until the M4 `TurnEnd` carrier exists**, so at M2 assert effort only via a `build_args` unit check + "dispatch completes without error." The end-to-end *emitted* per-turn effort assertion lands in M4. State this M2→M4 dependency so the sequencing is intentional. Tiny prompts per cost discipline.
- `make check` green; `make test-live` covers the new live tests.

---

## Milestone 3 — Backend: create / attach with model + effort, and change them later

### Goal & Outcome
The selected values reach the persisted record, and later changes persist and take effect on the next send.

- Creating or attaching an agent can specify model and/or effort; the record stores them.
- New commands change (or clear) an existing agent's model and effort and re-persist.
- A model can never attach to a harness without model support; an effort can never attach to a harness without effort support (enforced server-side, independent of the UI).

### Implementation Outline
- Extend `create_agent_impl` and `attach_agent_impl` (`crates/app/src/commands.rs`) to accept `model: Option<String>` and `effort: Option<String>`; update the `#[tauri::command]` wrappers.
- **Capability checks run first.** In `attach_agent_impl`, reject `model.is_some() && !supports_model_selection()` *and* `effort.is_some() && !supports_effort_selection()` as the first thing after project/active resolution — **before** the per-harness session lookup and sidecar writes (attach writes sidecars before committing the registry record; a check inside the harness match would orphan a sidecar on rejection). Same checks on create.
- Add `set_agent_model_impl` and `set_agent_effort_impl` (+ wrappers), each taking `Option<String>` (so clearing back to the harness default is expressible — a non-optional `String` couldn't say "clear"): look up the agent, set the field, re-persist via the same registry-write path the other mutating commands use (mirror `rename_agent_impl`), return the updated record; reject for an unsupported harness. No effect on any in-flight turn — the new value applies on the next dispatch (M2 reads the field fresh each time).
- **Normalize the empty-model footgun here, not only in the UI.** `set_agent_model_impl` and the create/attach `model` param trim and map empty/whitespace → `None` before persisting, so no caller (a future IPC path, a workflow step, a test) can bypass the M5 form and persist `Some("")` — which would dispatch `--model ""` and fail every turn with a non-obvious cause. The IPC command is the real trust boundary; the M5 form normalization stays as the UX layer. This is **footgun-normalization, not value validation** — we still don't judge whether a non-empty model is "valid" (reactive). Effort is enum-constrained, so it needs no equivalent; no allow-list for either.

### Definition of Done
- Tests on the free functions: create/attach with each field stores it; `set_agent_model`/`set_agent_effort` update and persist (reload proves durability); **clearing persists `None`**; **a whitespace/empty model persists `None`, never `Some("")`**; setting a model on Antigravity or an effort on Gemini/Antigravity (all paths) returns the capability error **and leaves no orphan sidecar**; the attach rejection happens before any sidecar write.
- `make check` green.

---

## Milestone 4 — Per-turn model + effort on the transcript

### Goal & Outcome
Every turn records the model and effort that produced it (where the harness exposes them), for both live streaming and reopen — so the transcript shows a faithful per-turn history.

- Each completed turn carries the model (all harnesses) and effort (where available) it ran with.
- A mid-conversation switch of either is reflected turn-by-turn.

### Implementation Outline
This replaces any "pick one representative value for the agent" notion — every turn carries its own.

- **Add `model: Option<String>` and `effort: Option<String>` to the agent-role turn shape** — the **`Turn::Agent` variant** (`crates/harness/src/transcript.rs`, a `#[serde(tag="role")]` enum), the **agent-role branch** of the hydration `LoadedTurn` (`src/lib/types.ts`), **and the agent-role branch of the live-state `Turn` (`src/lib/state/types.ts`)**. Not the `User` branch (a user turn has neither), and not only the hydration type — the live transcript renders from the live-state `Turn`, which is distinct from `LoadedTurn`. Keep these **separate from** the agent-scoped `SessionMeta` (a standalone, non-turn-anchored event, events.rs:108).
- **Populate per turn, per harness, on both paths** (live parse + session-file hydration):
  - **Claude** — model from each turn's assistant record / `init`. Effort: **verify** whether it's exposed per turn anywhere (init carries model, not obviously effort); if not, leave `Turn.effort = None` for Claude (its effort is shown as sidebar intent only).
  - **Codex** — both `turn_context.payload.model` and `turn_context.payload.model_reasoning_effort` (the `effort` field observed next to model), attached per turn. The old set-once `model_set` gate in `codex/session_file.rs` is removed in favor of per-turn attachment.
  - **Gemini** — model from `init.model` per invocation. Effort always `None` (no such concept exposed).
  - **Antigravity** — model via **carry-forward** (below). Effort always `None` (folded into the model name — already visible *in* the model string, so no separate field).
- **Antigravity model carry-forward:** Antigravity writes a `USER_SETTINGS_CHANGE` "from X to Y" sentence only on the turn where the model *changed* (verified: turn 1 `None → X`, and a resume after a settings change wrote `X → Y`). Maintain a running "current model": set it to the announced "to" value on a turn carrying a change, inherit otherwise, and stamp every turn. On **hydrate**, walk turns chronologically from the start; **live**, hold the running value in per-agent state. **Implement this carry-forward — don't assume it exists**: the codebase's existing "empty-keeps-prior" logic (reducers.ts:446) is for the *agent-scoped* `meta.model`, not a per-turn turn-stamp; model the new per-turn carry-forward on that pattern but build it. `None` only before any announcement (e.g. an attached conversation truncated before its first).
- **Live-path carrier (load-bearing — the per-turn value has no turn-anchored carrier today).** No live event carries a per-turn model/effort: `TurnEnd` (events.rs) holds only outcome+usage, and `SessionMeta` is agent-scoped (and for Codex first-turn-only, emitted *after* `TurnEnd` in the enrichment cycle). So:
  - Add `model: Option<String>` / `effort: Option<String>` to **`AdapterEvent::TurnEnd` and `NormalizedEvent::TurnEnd`**, and have the live reducer's `turn_end` case **stamp the live-state `Turn`** (this is why the fields must also land on `src/lib/state/types.ts`, above).
  - Populate that carrier per harness at the point `TurnEnd` is built: **Claude / Gemini** from the most-recent pre-`TurnEnd` `SessionMeta`/`init` (their init fires at stream start, so this is correct); **Codex** from the enrichment session-file re-read's current-turn `turn_context.{model, model_reasoning_effort}` (session_file.rs:~547) — **not** from `SessionMeta`, which is first-turn-only and post-`TurnEnd`; **Antigravity** from its carry-forward state. Transmit this explicitly — the model is *not* already turn-anchored, and the naïve "most-recent `SessionMeta` at `TurnEnd`" is wrong for Codex.
- **Gate removal does not disturb the sidebar's current model source.** Removing the set-once `model_set` reconstruction gate (session_file.rs:323) is safe: the sidebar's pre-M6 `SessionMeta.model` comes from a *separate* first-`turn_context` parse in `parse_session_content`, not from this gate. (The decision on whether to retire `SessionMeta.model` itself moves to M6, once the sidebar reads `AgentRecord.model`.)

### Definition of Done
- Per-harness tests: a Codex session with two turns on **different** model+effort yields two turns whose values differ (convert the former first-wins model test to per-turn, an intended change; add effort). Claude/Gemini model equivalents.
- An Antigravity carry-forward test: turn 1 announces a model, turns 2–3 don't → all three carry it; a later change-sentence flips subsequent turns; a transcript with no announcement hydrates `model: None` without error.
- A **reducer test** that a `turn_end` event carrying `model`/`effort` stamps the live-state `Turn` (the live carrier, not just hydration) — proving the footer populates during streaming, not only on reopen.
- **Live tests — the upstream-contract guard (required), all adapter-driven** (construct the real adapter, `dispatch`, assert on emitted `AdapterEvent::TurnEnd` per-turn `model`/`effort` — never grep raw CLI output; this catches both upstream drift *and* our build_args/parser/carry-forward regressions):
  - **Model + effort switch (Claude, Codex)** — `live_<harness>_model_and_effort_change_across_turns`: turn 1 with model A / effort E1, resume with model B / effort E2 (Codex: same valid model if plan-gated, but effort *can* vary `medium`→`high`), assert each turn's per-turn values reflect the switch. (Claude model `sonnet`/`opus`; effort e.g. `low`/`high`. Codex effort `medium`/`high` in `turn_context`.)
  - **Gemini** — `live_gemini_model_changes_across_turns`: model `gemini-2.5-flash`→`-pro` per turn; no effort.
  - **Antigravity carry-forward** — `live_antigravity_model_change_announced_on_resume`: dispatch turn 1 through `AntigravityAdapter` (model X), change `~/.gemini/antigravity-cli/settings.json`'s `model` to Y, resume, assert our adapter's emitted per-turn model is X then Y. **Config-mutation safety protocol** (this test edits the real `settings.json`; isolated-`HOME` was tested and rejected — agy re-prompts for OAuth under a fresh HOME, so the dev's authenticated `~/.gemini` is the only workable home):
    1. **Byte-for-byte backup** to a **stable, well-known path** — restore exact original bytes, never reserialized.
    2. **Self-heal at start:** if the backup already exists (interrupted prior run), restore from it first and **fail loudly with the recovery path printed**.
    3. **`Drop`/scope guard** restores then **deletes** the backup on exit — covers normal completion *and* assertion-panic (Rust unwinds on panic).
    4. **Known gap, mitigated:** `SIGKILL`/`Ctrl-C`/crash/`panic=abort` bypass `Drop`; step 2 repairs on the next run and the pristine value survives in the backup until a clean restore. No signal handler (fragile).
    5. Mark the test **serial** (the file is global). Document this protocol in the test, including that mutating harness config *in a test* doesn't violate the production rule that the app never writes harness config — it's the only way to exercise the real contract.
- `make check` green; `make test-live` (and `make test-live-<harness>`) cover the new live tests.

> Note: Codex `turn_context` is written at turn **start**, not on success (verified — a failed model turn still wrote its `turn_context`), so a failed/interrupted turn correctly carries the model+effort it attempted.

---

## Milestone 5 — Frontend: model + effort pickers in the create-agent form

### Goal & Outcome
A user creating (or attaching) an agent can choose its model and effort, each gated to the harnesses that support it, with a short note where a control is unavailable.

- The form offers a hybrid **model** picker (Claude/Codex/Gemini) and a fixed-enum **effort** picker (Claude/Codex).
- For a harness lacking a capability, that control is disabled with a short note and submits nothing for it (Antigravity: both disabled; Gemini: effort disabled, model enabled).
- The chosen values reach the backend; an empty/whitespace custom model submits nothing, never `""`.

### Implementation Outline
- Add two frontend capability maps to `src/lib/harnessDisplay.ts` mirroring `supports_model_selection()` and `supports_effort_selection()` (exhaustive `Record<HarnessKind, …>`).
- Build **two reusable pickers**: a **hybrid model picker** (suggested dropdown + "custom…" free-text; value `string | undefined`; normalize empty/whitespace → `undefined` inside the component so `Some("")` can't cross the IPC boundary) and a **fixed-enum effort picker** (per-harness level list, plus an explicit "default / unset" choice = `undefined`). Built here for reuse in M6.
- Per-harness constants: suggested models (Claude `opus`/`sonnet`/`haiku`; Gemini `gemini-2.5-pro`/`-flash` + `auto`; Codex `gpt-5.5`) — convenience-only, **comment that there is no queryable model list (no harness exposes one) and Codex values are plan-dependent, which is why this is hardcoded + free-text**; effort levels (Claude `low/medium/high/xhigh/max`; Codex `none/minimal/low/medium/high/xhigh` — note `none` is a real level distinct from unset). Confirm current aliases/levels against each CLI before finalizing.
- Wire both into `CreateAgentForm.svelte` after the harness selector; disable+note where unsupported and omit from submit. Thread `model?: string` and `effort?: string` through `createAgent`/`attachAgent` in `api.ts` and the form's submit types.

### Definition of Done
- Component tests (mock `invoke`): a suggested model, a custom model, and an effort level each produce the right payload; switching to Gemini disables effort but keeps model; switching to Antigravity disables both; untouched pickers submit nothing; **an empty/whitespace custom model submits nothing, not `""`**.
- Both pickers are standalone components (reuse-ready for M6).
- `make check` green.

---

## Milestone 6 — Frontend: change model/effort, surface intent vs. history, and document limitations

### Goal & Outcome
For a capable agent the user can change its model/effort after creation; the sidebar shows the selected values (immediate), the transcript shows per-turn values (history), and the user-facing harness limitations are documented.

- The actions menu offers "Change model" (Claude/Codex/Gemini) and "Change effort" (Claude/Codex), each absent where unsupported.
- Changing/clearing calls the backend and the **sidebar reflects the selection immediately** — before any turn runs.
- The **transcript footer shows each turn's model and effort** next to its timestamp.
- The README's user-facing limitations section names the Gemini and Antigravity gaps.

### Implementation Outline
- Add "Change model" and "Change effort" items to `AgentActionsMenu.svelte`, each gated on its capability map (so an Antigravity agent shows neither; a Gemini agent shows only "Change model"). Each opens an editor reusing the M5 picker, pre-filled with the current value; submit calls `setAgentModel(agentId, model?)` / `setAgentEffort(agentId, effort?)`.
- **Sidebar shows the selected values.** Change the sidebar model line (`Sidebar.svelte`, currently `runtime.meta.model`) to read `AgentRecord.model`, and add a sibling line for `AgentRecord.effort`. These are intent, so they update the moment the change lands — the immediate confirmation. Clean-hide each when `None` per the existing absent-field convention.
- **Transcript footer shows per-turn model + effort.** In the per-message footer that already renders the timestamp (`UnifiedTranscript.svelte`'s `messageMeta`), render `Turn.model` and `Turn.effort` for each agent turn; omit each when `None`. Subtle styling consistent with the timestamp.
- A model/effort change applies on the next send (dispatch reads fresh each turn — M2); no in-flight handling.

### Definition of Done
- Component tests: each action present/absent per capability; submitting a new model/effort invokes the right command; **the sidebar reflects the selection on an agent that has already run a turn** (the real post-turn state); clearing hides the line.
- Transcript test: an agent turn renders its `model`/`effort`; `None` renders nothing.
- **`SessionMeta.model` retirement audit** (the sidebar now reads `AgentRecord.model`, so its old source may be dead): confirm `SessionMeta.model` has no consumer other than per-turn population; if clean, remove the field for a clean architecture; if a harness reuses it as a per-turn-population intermediate, keep it only as that. Record the decision. (Done here, not M4, because the predicate — the sidebar no longer reading it — is this milestone.)
- **Docs (required):**
  - `docs/research/harness-behavior.md` §3.3/§3.4 status notes (model & effort selection ship; Antigravity per-turn model is carry-forward; Claude effort per-turn may be intent-only).
  - **`README.md` "Harness support and limitations"** — update/extend the user-facing bullets to name the gaps this feature exposes, in product terms: **Gemini** — model is selectable, but **reasoning effort can't be set from Switchboard** (Gemini exposes it only through Gemini's own config). **Antigravity** — **model can't be selected from Switchboard** (set it inside Antigravity), and effort is part of the Antigravity model you pick there. (This is the canonical example of bubbling a user-facing harness limitation up to the README — keep it short and in product terms, mechanism stays in the research doc.)
  - `docs/system-design.md` §9 capability matrix if model/effort belong there.
- `make check` green.

---

## Out of scope (explicitly)

- Enumerating or validating model values, or detecting plan-gated Codex models — reactive failure is the design.
- Any mechanism to set Antigravity's model or Gemini's effort (both are harness-owned config we never touch; per-`HOME`/config-dir isolation was tested and declined — §3.3). These are read-only/unavailable here and documented as user-facing limitations.
- Changing model/effort mid-turn / interrupting an in-flight turn.
- A global or project-level default for either — selection is strictly per-agent.
- "Show both values on the agent card" reconciliation — obviated by the sidebar-intent / transcript-history split.
