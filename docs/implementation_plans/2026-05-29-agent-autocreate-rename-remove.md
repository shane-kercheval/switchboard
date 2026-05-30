# Auto-create, rename, and remove agents

**Status:** Planned — not started.
**Branch:** lands as commits on the current M4 branch (`m4-dispatcher-contention-cancel`); per the "all of M4 is one PR" convention, sub-features here are commits, not separate review units.

## Problem & motivation

Switchboard's whole premise is multi-harness use inside one project. Today, creating a project gives you an empty roster — the user must open the "Add an agent" dialog and hand-create an agent for every harness, every time, even though that's what they'll want the overwhelming majority of the time.

We flip the default: **a newly created project is auto-populated with one agent per installed harness**, named after the harness (`claude-code`, `codex`, `gemini`, `antigravity`). The counterweight to over-population is making agents trivially easy to **remove** (a user with all four harnesses installed who only wants two should prune in two clicks) and to **rename** (the auto-generated name is a starting point, not a fixed label).

Two hard scope boundaries from the discussion:

- **New projects only.** Existing projects are never retroactively populated. This is purely a creation-time behavior.
- **Installed harnesses only.** We reuse the existing binary-presence detection; a harness whose CLI isn't on PATH gets no agent.

This plan also closes a pre-existing gap surfaced during design: **agent-name validation (format + uniqueness) is enforced only on the backend today**, surfaced reactively as a post-submit error string. We add live frontend validation so both the create form and the new rename editor can show an inline error and disable their commit action before a round-trip.

## What we are NOT doing

- Not touching existing projects (no migration, no backfill).
- Not adding a "create agents for harnesses installed *later*" reconciliation. Auto-create is a one-shot at project-creation time.
- Not adding bulk/multi-select remove. One agent at a time.
- Not deleting harness-native session files (`~/.claude/projects/...`, `~/.codex/sessions/...`, etc.) on remove. Those are harness-owned (system-design §3); we only remove Switchboard's own state.
- Not allowing duplicate agent names. The backend already forbids them; we mirror that on the frontend.

## Required reading before implementing

Read these before writing any code — several decisions below are only correct in light of how these already work:

- `docs/system-design.md` §3 (filesystem layout + split source-of-truth) and §4 (name uniqueness / canonicalization rules). The canonicalization rule is load-bearing for validation (below).
- `crates/core/src/name.rs` — `validate_name` and `canonicalize_for_uniqueness`. The frontend validation must mirror these exactly.
- `crates/core/src/project.rs` — `register_agent_inner_with_id` (validation + uniqueness loop), `list_agents`, `create_on_disk`, and how `registry.jsonl` is written. Note it is **append-only today** (`append_jsonl`); remove/rename need a rewrite path.
- `crates/app/src/commands.rs` — `create_agent_impl`, `create_project_impl`, `get_harness_install_status_impl`, and the `AppError` → string mapping at the command boundary.
- `crates/harness/src/meta_sidecar.rs` (`meta_sidecar_path`), `crates/harness/src/codex/sidecar.rs` (`sidecar_path`), `crates/harness/src/antigravity/sidecar.rs` (`sidecar_path`) — the three per-agent sidecar path helpers remove must clean up.
- `crates/dispatcher/src/lib.rs` — `Dispatcher` holds a `Mutex<HashMap<AgentId, AgentSlot>>` with `Active`/`Closing` states; understand the teardown path before deciding what remove does to a live slot.
- `src/lib/state/workspace.svelte.ts` — `createProjectAndActivate`, `agentsByProject`, `activateProject`, roster reload.
- `src/lib/components/CreateAgentForm.svelte`, `src/lib/components/Sidebar.svelte` (agent card, lines ~283–318), `src/lib/components/AgentActionsMenu.svelte`, `src/lib/components/ui/Input.svelte`.

## Cross-cutting conventions established here

These are introduced in the earliest milestone that needs them and **reused, not reinvented**, by later milestones:

1. **Frontend name validation mirrors backend canonicalization (M1).** Any frontend code that needs to validate an agent name — the create form (M2) and the rename editor (M7) — consumes the single helper from M1. No second implementation.
2. **Registry mutation beyond append lives in `core` (M5).** `remove_agent` and `rename_agent` both need to rewrite `registry.jsonl` (not append). The rewrite helper(s) are added to `Project` in `crates/core` once, and both commands use them. The app/dispatcher layer does not hand-roll JSONL rewriting.
3. **Harness availability is read from one reactive store (M3).** After M3, the blank state, the create form, and the auto-create flow all read availability from the same store rather than each issuing their own `get_harness_install_status` calls.

## Decisions locked in discussion (do not re-litigate)

- **No duplicate names**, on both create and rename, matching the backend's canonicalized uniqueness (hyphens→underscores, case-insensitive).
- **Validation UX** = red border on the input + the error message in a hover/focus tooltip + the commit action disabled. Identical treatment in create and rename.
- **Rename is inline, never a dialog.** Triggered from a "Rename" menu item *and* by double-clicking the name. In edit mode the name becomes an input and the harness icon swaps to a save (check) icon.
- **Rename commit:** Enter or the save icon. **Rename cancel:** Escape *or* clicking anywhere outside the input that isn't the save icon (blur cancels — never persist on blur).
- **Remove is inline-confirmed**, not a dialog (consistent with the no-dialog philosophy). Disabled while the agent is active.

---

## Milestone 1 — Frontend agent-name validation helper

### Goal & Outcome

A single, tested frontend module that validates an agent name the same way the backend does, so any form can show a live error before submitting.

- Given a candidate name and the existing roster, callers get back a discriminated result: valid, or invalid with a specific reason (empty, bad characters, duplicate).
- The duplicate check uses the same canonicalization as the backend, so the frontend and backend never disagree about whether a name is taken.
- The duplicate check can **exclude one agent** (by id), so renaming an agent to its own current name is not flagged as a self-collision.

### Implementation Outline

Add a small validation module under `src/lib/` (colocate near other shared logic; the implementing agent picks the exact path to match conventions). It must mirror `crates/core/src/name.rs`:

- **Format/empty:** non-empty after trim, and every character in `[A-Za-z0-9_-]`. (No leading-character constraint — digit/hyphen/underscore-first are all valid, matching the Rust rule.)
- **Canonicalization for uniqueness:** hyphens → underscores, then lowercase. Two names collide iff their canonical forms are equal.

Contract (shape, not prescribing names):

```ts
type NameValidation =
  | { ok: true }
  | { ok: false; reason: "empty"; message: string }
  | { ok: false; reason: "invalid_chars"; message: string }
  | { ok: false; reason: "duplicate"; message: string; collidesWith: string };

// roster excludes self by passing the editing agent's id (undefined when creating)
function validateAgentName(candidate: string, roster: AgentRecord[], excludeAgentId?: AgentId): NameValidation;

// the single normalization chokepoint — both validation and every submit site run input through this
function normalizeAgentName(name: string): string;
```

Per-reason variants (not a single `ok:false` with optional `collidesWith`) so `collidesWith` is required on the duplicate case and machine-checked.

**Cross-milestone invariant (M2 + M7):** the value handed to `createAgent` / `renameAgent` must be `normalizeAgentName(value)`, never the raw input. Validate and submit through the same normalization so "what we validated" equals "what we submit" — the backend's `validate_name` does not trim, so a raw, untrimmed submit would be rejected even when the form showed "valid." Do **not** normalize the bound input field mid-keystroke (it fights the cursor); normalize at validate-time and at submit-time only. Component tests for those milestones assert on the value that reached the mocked `invoke`, not on field contents.

- The `message` strings are the user-facing copy for the tooltip; keep them short and consistent with the discussion ("Name can't be empty" / "Use only letters, numbers, hyphens, and underscores" / "An agent named '{collidesWith}' already exists").
- For `duplicate`, `collidesWith` carries the *existing* agent's verbatim name (so the message can name it), found by canonical match.

Rationale to preserve in a module-level comment: **this is a deliberate duplication of the backend rule for live UX; the backend remains authoritative.** Note the canonicalization mirror explicitly so a future reader doesn't "simplify" it to a literal string compare.

### Definition of Done

Unit tests (this module is pure and cheap to test thoroughly — do so):

- Empty / whitespace-only → `empty`.
- Disallowed characters (space, slash, unicode, `.`) → `invalid_chars`.
- Allowed edge forms (`1agent`, `-agent`, `_agent`, mixed case) → `ok`.
- Duplicate detection across the canonicalization boundary: `claude-code` vs `claude_code` vs `CLAUDE-CODE` all collide; `collidesWith` returns the existing verbatim name.
- `excludeAgentId` lets a name match the excluded agent's own current name without flagging duplicate, but still flags a *different* agent with that name.
- Parity check: a short table of cases asserted to match the documented backend behavior (mirror the cases in `name.rs`'s own tests so drift is visible).

---

## Milestone 2 — Live validation in the create-agent form

### Goal & Outcome

The "Add an agent" form validates the name as the user types, using M1, instead of only catching problems after a failed backend round-trip.

- A bad or duplicate name shows the inline error treatment (border + tooltip) and disables the Create/Attach button.
- The backend error path remains as the authoritative fallback (unchanged), but in normal use the user never reaches it for name problems.

### Implementation Outline

- Thread the active project's roster into `CreateAgentForm` (currently it receives none). Source: `agentsByProject[activeProjectId]` in `workspace.svelte.ts`. Pass it from the parent (`AddAgentModal` / `App.svelte`) the same way availability props are passed today.
- Fold `validateAgentName(name, roster)` (no `excludeAgentId` — this is a create) into the existing `canSubmit` derived value, alongside the current non-empty/harness checks.
- Render the validation message via the shared inline treatment. Reuse the existing `Input` component; add the error-border + tooltip affordance. **This border+tooltip treatment is the same one M7 will use** — if it's worth extracting a tiny wrapper or shared class for the errored-input look, do it here so M7 reuses it. (Judgment call for the implementing agent against the actual markup; don't over-abstract a two-line class.)
- Leave the existing backend-error display (`error` prop) in place untouched.

### Definition of Done

- Component-level tests (per the project's component-test convention — mock nothing backend-side needed here since validation is client-side, but exercise the real reducer/derived state): typing a duplicate name disables Create and shows the message; fixing it re-enables; bad characters behave likewise; attach-mode UUID validation still works alongside name validation.
- Manual check: create form in a project that already has the auto-created agents shows a live duplicate error if you type `codex` when a `codex` agent exists.

---

## Milestone 3 — Harness-availability reactive store

### Goal & Outcome

A single reactive source of truth for which harness binaries are installed (and their version), fetched once at startup and refreshed at natural moments, so no surface re-probes independently. **Full-consolidation scope** (chosen over a narrower refactor): every install/version consumer reads this store, so they agree by construction.

- On app load, install status for all four harnesses is fetched and held in reactive state.
- Every surface that needs "is harness X installed / what version" reads the store: the binary-missing banner stack, the create-form gating, the Settings + blank-state "Supported CLIs" list, and (M4) auto-create.
- The store is refreshed when the user is likely to have changed their install set (at minimum: when the settings panel opens).

### Context: the two pre-existing mechanisms this replaces

Before this milestone there are **two** backend availability commands and **three** frontend consumers, which is exactly the duplication this store removes:

- `check_*_binary` (probe-only → `Result<()>`): `App.svelte` runs four of these into `BinaryState` (`checking`→`available`/`missing`), feeding **both** the binary-missing banner stack **and** the `CreateAgentForm`/`AddAgentModal` gating props.
- `get_harness_install_status` (probe **+ version** → `{installed, version}`): called directly by `HarnessStatusList.svelte` (rendered in **Settings** and the **blank-state** `GettingStarted` surface), which **interleaves auth probes** alongside.

`get_harness_install_status` is a superset of `check_*_binary` (adds version), so the store wraps it and serves all install consumers.

### Implementation Outline

- Add a dedicated `harnessAvailability.svelte.ts` store module (alongside the existing pure `harnessAvailability.ts` helpers, which stay). It holds `HarnessInstallStatus | null` per harness (**`null` = not yet probed = "checking"**, so the create form's existing fail-closed-while-checking behavior is preserved exactly) and a `refresh()` that calls `get_harness_install_status` for each harness.
- Derive `HarnessAvailability` (`binary`: `null`→`checking`, `installed`→`available`, else `missing`) from the store for the banner + gating path; expose the `installed` boolean for M4 and `version` for the status list.
- **No new backend command.** Purely a frontend caching layer over the existing per-harness command; detection is unchanged.
- Populate at startup, refresh on settings-open. No polling.
- **Full consolidation — all install/version consumers read the store:**
  - `App.svelte`: derive banners + `CreateAgentForm` gating props from the store; **remove the four `check_*_binary` probes** in `onMount`. The now-unused `check*Binary` **frontend wrappers** in `api.ts` are deleted. Leave the backend `check_*_binary` commands (tested, cheap; removing them is out of scope).
  - `HarnessStatusList.svelte`: read `installed`/`version` from the store instead of its own `getHarnessInstallStatus` fetch. **Keep its local auth probe** — auth is deliberately reactive in v1 and not part of this store (this store is install/availability only, never an auth store). Install and auth are genuinely different axes with different lifecycles; sourcing them separately is correct, not a wart.
- **Behavior note:** `App.svelte` startup moves from `check_*_binary` (probe-only) to `get_harness_install_status` (probe + version) — one extra `version()` call per harness, negligible (same CLI spawn), and version is then available everywhere.

Rationale to preserve in a module comment: the store exists so **auto-create (M4) has availability in hand at project-creation time without a round-trip**, and so **one place answers "is this harness installed," with every surface agreeing by construction**.

### Definition of Done

- Store unit tests with `invoke` mocked: populates all four entries; not-installed reflected; `refresh()` updates a previously-cached value; pre-probe state derives to `checking` (fail-closed).
- The derived `HarnessAvailability` mapping is tested (`null`→`checking`, installed→`available`, missing→`missing`).
- `CreateAgentForm` / banner / `HarnessStatusList` behavior unchanged from the user's perspective (existing tests pass, adjusted only for the new data source). `HarnessStatusList`'s auth column still works off its local probe.

---

## Milestone 4 — Auto-create agents on new-project creation

### Goal & Outcome

Creating a new project automatically creates one agent per installed harness; opening an existing project does not.

- After a user creates a project via the New Project dialog, its roster already contains an agent for each installed harness, named `claude-code` / `codex` / `gemini` / `antigravity`.
- If no harnesses are installed, the project is created empty (no error) — the existing blank/no-agent surface handles that case as it does today.
- Existing projects, when activated, are never auto-populated.

### Implementation Outline

- Hook into `createProjectAndActivate` in `workspace.svelte.ts` (the function the New Project dialog calls). **Only this path** auto-creates — `activateProject` for existing projects must remain untouched, which is what guarantees the "new projects only" boundary.
- Sequence: after the project is created and activated, read the M3 availability store, and for each installed harness call the existing `create_agent` command with the harness-derived name. Then reload the roster **once** (not once per agent).
  - **Await a fresh availability probe before reading `installed()` (race guard, mandatory):** the M3 store's startup probe is fired un-awaited, and `harnessAvailability.installed()` returns `[]` until it resolves. If the user reaches project creation inside that startup window, reading `installed()` directly would silently yield zero agents. So auto-create must `await refreshHarnessAvailability()` (idempotent + cheap — a few PATH lookups, already done by the time the create round-trip completes in the common case) **then** read `installed()`, guaranteeing a definitive answer regardless of startup timing.
  - **Ordering is load-bearing:** activation must precede the auto-creates, because `create_agent_impl` writes to the *active* project (it reads `state.active_project_id`). That means `activateProject`'s own roster fetch (`api.listAgents` → `agentsByProject[projectId]`) ran on an empty project, so it will not reflect the new agents.
  - **Per-agent register, not a bulk roster re-fetch (corrected from the original draft):** mirror the canonical create path `createOrAttachAndRegister` — for each installed harness, `api.createAgent` → `registerAgent(agent)` → `addAgentToActiveProject(agent)`. The earlier "re-fetch via `api.listAgents` → `agentsByProject`" idea was an *incomplete* quote of `activateProject`'s load: it omits `registerAgent` (the call that wires up live per-agent transcript/dispatch state), so it would render sidebar cards that can't show a transcript or accept a send. `createAgent` already returns the `AgentRecord`, so `addAgentToActiveProject` appends it reactively — no `listAgents` round-trip and no ordering subtlety. Do **not** `hydrateAgent` (create-mode agents have no history; hydrate is attach-only).
  - Naming: use the shared `HARNESS_DEFAULT_AGENT_NAME` map in `harnessDisplay.ts` (extracted from `CreateAgentForm`'s local slug helper, which both the form and this path now consume). It's a **direct** slug map (`claude_code → "claude-code"`, …), not a regex over a display label — `HARNESS_LABEL` is the short label (`"Claude"`) and would slug wrong. These names are distinct under the backend's canonicalization, so no self-collision.
- Creation order: deterministic — iterate `harnessAvailability.installed()`, which returns in `HARNESSES` order (claude → codex → gemini → antigravity). Sequential; `create_agent` serializes on `registry_write` anyway.
- **Active-project coupling (race + durable fix).** Both `create_agent` (reads `state.active_project_id`) and `addAgentToActiveProject` (reads `selection.activeProjectId`) bind to the *currently active* project, not the one being created. The multi-create seed window is interruptible (the user could dismiss the dialog and switch projects mid-seed), which would scatter agents into the wrong project. Mitigations applied: (1) **capture-id guard** — `seedAgentsForInstalledHarnesses` captures the project id and bails if it changes (also gates the failure-push so a banner never strands on a project the user left); (2) **dialog belt** — the New Project `Dialog` is `dismissible={!newProjectBusy}`, so it can't be dismissed (escape/outside/✕ all suppressed via bits-ui) while seeding runs. **Durable fix (deferred):** `create_agent`/`attach_agent` taking an explicit `project_id` instead of reading active state removes the coupling entirely — **M5's `remove_agent`/`rename_agent` face the same "which project is active" question**, so consider doing the `project_id` threading there.
- **Partial-failure policy — user-visible, not console-only:** if one `create_agent` fails, catch it per-iteration (so it never throws out of `createProjectAndActivate` and wedges the New Project dialog), keep the agents that succeeded, and record `{harness, error}` in an `agentCreationFailures` reactive list. Surface it as a **dismissible banner** in the project view (reuse the existing `Banner` component, with an added optional `onDismiss`) — a console log would go unnoticed and the user would silently get a short roster. The banner clears on dismiss and on project switch (`activateProject` resets the list). The project still opens with whatever succeeded; the user adds any missing agent via "+ Add agent".

### Definition of Done

- Tests (frontend, `invoke` mocked): creating a project with 2 of 4 harnesses available creates exactly those 2 agents with the expected names and reloads the roster once; with 0 available creates none and still lands in the project; activating an *existing* project creates nothing.
- **Race-guard test:** simulate the availability probe still *pending* at project-creation time (a `get_harness_install_status` mock that defers resolution rather than resolving synchronously) and assert auto-create still sees the right harnesses — i.e. the `await refreshHarnessAvailability()` actually closes the window. Confirm the mock genuinely defers, or the test passes without exercising the race.
- A test asserting the new-project path and the existing-project activation path diverge (the latter never calls `create_agent`).
- Manual: create a project with multiple harnesses installed; confirm the roster and names; confirm reopening an existing project adds nothing.

---

## Milestone 5 — Backend `remove_agent` and `rename_agent` commands

### Goal & Outcome

Two new Tauri commands that mutate an existing agent's registry record, plus the core-level registry-rewrite support they require. This milestone is backend-only; the UI that calls them lands in M6/M7.

- `remove_agent(agent_id)` removes the agent from `registry.jsonl` and deletes Switchboard's per-agent sidecar files for it; harness-native session files are left intact.
- `rename_agent(agent_id, new_name)` changes the stored name after re-validating format and uniqueness (excluding the agent itself), returning a clear error on collision.
- Both reflect the change in in-memory `AppState` so the next roster read is correct.

### Implementation Outline

**Core (`crates/core/src/project.rs`) — establish convention #2 here.** `registry.jsonl` is append-only today; add the rewrite path both commands need:

- A method to **remove** an agent by id: read all records, drop the matching one, rewrite the file. Return whether a record was removed (so a stale/double remove is detectable, not a silent no-op).
- A method to **rename** an agent by id: read all records, validate the new name (`validate_name`) and check canonicalized uniqueness against the *other* records (reuse the existing uniqueness logic — factor it so create and rename share it rather than copy it), update the matching record's `name`, rewrite the file. Return the updated `AgentRecord` or the appropriate `CoreError` (`DuplicateAgentName` / `InvalidName` / not-found).
- Rewrite must be crash-safe to the same degree as the rest of core's writes — write-temp-then-rename if that's the existing pattern, or match whatever `append_jsonl`/`write_yaml` do. Don't invent a weaker scheme. Note in a comment *why* this is a rewrite and not an append (remove/rename can't be expressed as an append to an append-only log without a compaction story we're not building).
- Add a `CoreError` variant for "agent not found" if one doesn't already fit.

**App (`crates/app/src/commands.rs` + `lib.rs`):**

- `remove_agent_impl` — **two phases, and the split is mandatory, not stylistic.** `lock()` returns a `std::sync::Mutex` guard (held synchronously everywhere in `commands.rs`), but `dispatcher.shutdown_agent` is `async`. Holding a `std::sync` guard across an `.await` is a non-`Send` compile error under the multi-threaded Tokio runtime. So the shutdown **must not** happen inside the locked section.
  - **Phase (a) — no lock held:** check agent state; if a turn is in flight, reject with a clear error *before mutating anything* (no partial state change on the reject path). Otherwise `await dispatcher.shutdown_agent(...)` to tear down any live/idle actor slot (reuse the existing teardown/`Closing` path — read the dispatcher before choosing). Do **not** orphan a live actor.
  - **Phase (b) — under `registry_write`, fully synchronous, no `.await`:** remove the registry record (core method above); delete the per-agent sidecars if present, best-effort (a missing file is fine; a failed delete logs at `warn` and does not fail the command — the registry removal is the authoritative effect): `meta_sidecar_path`, codex `sidecar_path`, antigravity `sidecar_path` (use the existing path helpers; do not hardcode the filename layout); drop the agent from `state.agents_by_id`.
  - The "same lock discipline as `create_agent_impl`" applies **only to phase (b)** — the registry mutation — not to the whole function.
- `rename_agent_impl`: under the `registry_write` lock, call the core rename, update `state.agents_by_id`, return the updated record. The duplicate/invalid errors propagate to the frontend as strings via the existing `AppError` mapping (M7 also pre-checks, but the backend stays authoritative).
- Wire both as `#[tauri::command]` thin shims and add frontend `api.ts` wrappers.

### Definition of Done

- Core unit tests: remove drops exactly the target record and leaves others; remove of a non-existent id reports not-removed/error; rename changes the name and rewrites; rename to a canonicalized-duplicate of another agent errors with `DuplicateAgentName`; rename to the agent's *own* name (or a case/hyphen variant of it) succeeds (excludes self); rename to an invalid name errors.
- App tests (free-function `*_impl` level, per the crate's convention): remove deletes present sidecars and tolerates absent ones; remove updates `agents_by_id`; remove of an active agent is rejected **before any registry mutation or sidecar delete** (assert no partial state change on the reject path); rename updates `agents_by_id` and returns the record.
- Confirm harness-native session files are untouched by remove (assert only the `.switchboard/.../sessions/<id>.*` sidecars are deleted).
- A live test is **not** required (no change to how we talk to a real CLI); fixture-level is sufficient. Note that explicitly so the next reader doesn't think coverage was skipped.

---

## Milestone 6 — Remove action in the agent actions menu

### Goal & Outcome

Users can remove an agent from the per-agent three-dots menu, with an inline confirmation and no dialog.

- A "Remove" item appears in `AgentActionsMenu`, disabled (with explanatory tooltip) while the agent is active — same gating pattern as "Stop".
- Choosing it shows an inline confirm/cancel affordance within the menu; confirming calls `remove_agent` and the agent disappears from the roster.
- Errors surface without losing the user's place.

### Implementation Outline

- Add the menu item to `AgentActionsMenu.svelte` following the existing item pattern (Stop / Resume / Open session file). Gate `disabled` on the same `active` signal the component already receives.
- **`DropdownMenuItem` auto-closes the menu on select** (bits-ui behavior — the sibling "Resume" item only tolerates this because it hands off to a separate `Dialog`; inline confirm has no such handoff). For the inline confirm to render, the Remove item must call `e.preventDefault()` in its `onSelect` to suppress the auto-close, then manage `menuOpen` manually while the confirm affordance shows. (The reviewer's alternative — an anchored popover — also satisfies "no dialog," but the inline row most literally matches the intended UX; use it.)
- Inline confirm: on first click, suppress the close (above) and swap the item (or reveal an adjacent row) to a "Remove? Confirm / Cancel" affordance inside the still-open menu — no `Dialog`. This matches the no-dialog philosophy used for rename. Confirm calls the `api.ts` `removeAgent` wrapper; Cancel reverts to the normal item.
- On success: trigger a roster reload (reuse the existing reload path used after create) and close the menu. The removed agent's card disappears. If the removed agent was the one expanded/selected in any UI state, ensure that state degrades gracefully (no dangling selection of a gone agent).
- On error: show the error near the menu (reuse the component's existing `loadError`-style surface) and keep the agent.

### Definition of Done

- Component tests (mock `invoke`/`listen` per the component-test convention): Remove disabled when active with tooltip; **the first Remove click keeps the menu open and renders the Confirm/Cancel row** (not just that `removeAgent` eventually fires); confirm flow calls `removeAgent` and triggers roster reload; cancel aborts without calling and reverts the row; error path shows the message and keeps the agent.
- Manual: remove an idle agent; confirm its card and any expanded state clear; confirm the menu closes.

---

## Milestone 7 — Inline rename editor in the agent card

### Goal & Outcome

Users rename an agent inline in its sidebar card — no dialog — with live validation.

- A "Rename" item in the three-dots menu, and a double-click on the name, both put the card's name into edit mode.
- In edit mode the name becomes a text input and the harness icon is replaced by a save (check) icon; Enter or the save icon commits, Escape or clicking away cancels and reverts.
- Live validation (M1) shows the inline border+tooltip and disables the save action for empty/invalid/duplicate names (excluding the agent's own current name).

### Implementation Outline

- **Structural prerequisite (read the card first):** in `Sidebar.svelte` the name `<span>` currently lives *inside* the collapse-toggle `<button>`. An `<input>` cannot be nested in a button, and clicks would toggle collapse. In edit mode, render the input **in place of** the toggle button (swap the whole left side), not inside it. Outside edit mode the card is unchanged.
- Edit-mode state is per-card (which agent is being edited, the draft value). On entering edit mode: seed the draft with the current name, focus the input, select-all so typing replaces.
- Triggers: the menu "Rename" item sets edit mode (it does **not** open a dialog — that's the whole point); double-click on the name span does the same. Reuse the same edit-mode entry for both.
- Icon swap: while editing, replace `HarnessIcon` with a save (check) icon button. Save is disabled when validation fails (mirror the create-form treatment from M2 — reuse the shared errored-input look if one was extracted).
- Validation: `validateAgentName(draft, roster, agent.id)` — note the `excludeAgentId` so re-saving the unchanged name (or a case/hyphen variant) isn't a false duplicate.
- **A11y wiring (inherited from M2 — keep identical):** `aria-invalid={!validation.ok}` (tracks validity, decoupled from the visible border, which stays on the suppressed-for-empty message), and `aria-describedby` pointing at the message element only when a message is actually shown (gate on the message being present, not on `!ok`, or you get a dangling reference on the empty case). In the cramped card the message surfaces via the `title` tooltip rather than an inline span.
- Commit: Enter or save icon → call `api.ts` `renameAgent`, on success exit edit mode and reload/refresh the roster (or update the local record). On backend error (authoritative fallback), surface it and stay in edit mode.
- Cancel: Escape, or blur/click-away on anything that is not the save icon → discard draft, exit edit mode, revert to the original name. **Never persist on blur.**
- **Blur-vs-save-click race (the one fiddly interaction):** the save icon is itself "clicking away," so a naive implementation fires the input's blur-cancel before the save click and the rename never commits. Standard fix: call `e.preventDefault()` in the save icon's **`mousedown`** handler — this prevents the input from losing focus, so blur never fires; do the actual commit in the icon's **`click`** handler. Escape and genuine click-away still cancel.

### Definition of Done

- Component tests (mock `invoke`): menu "Rename" and double-click both enter edit mode; Enter commits and calls `renameAgent`; save icon commits; Escape reverts without calling; blur/click-away reverts without calling; clicking the save icon commits and does **not** get pre-empted by blur-cancel; invalid/duplicate name disables save and shows the message; renaming to the agent's own name is allowed (exclude-self).
- A test asserting the input is not rendered inside the collapse toggle (no nested-interactive regression) — or at least that toggling collapse and editing don't interfere.
- Manual: rename an agent two ways (menu + double-click); confirm Enter, save-icon, Escape, and click-away each behave per spec; confirm a duplicate is blocked live.

---

## Suggested commit sequence

One commit per milestone, in order (M1→M7). Each is independently reviewable; M5 (backend) precedes its UI consumers (M6/M7), M1 precedes its consumers (M2/M7), M3 precedes M4. Stop for human review after each milestone; do not commit until approved.
