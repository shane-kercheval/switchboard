# Workflow progress view + compose lockout

Status: planned. Follows `2026-06-21-workflows-refactor.md`. One change/review, built in the milestone order below.

## What and why

While a workflow runs in a project today, the compose box stays a free-text queue and the only run feedback is a global count chip in the app header. Two problems:

- **Queueing during a run is incoherent.** A message sent to a *participating* agent while a workflow runs interleaves with the workflow's own sends and effectively interrupts it. Messaging an *idle, non-participating* agent in the same project is a coherent intent — but per-agent compose during a run is deferred (see decisions below), so v1 locks the whole project's compose.
- **The run is opaque.** The header chip ("2 workflows") doesn't say which step you're on, what's left, or which agent is being invoked.

This plan:

1. Makes **step labels required**, so steps have human-readable names.
2. Adds a **`WorkflowSteps` view** with two modes — a **preview** in the `+ Workflow` composer (so you can see what a workflow does and who it invokes before running) and a **live** view that **replaces the compose box** during a run (so the project's input is locked to the running workflow).
3. **Removes the global header indicator** and replaces its one unique job — surfacing a *background* run that failed — with a **per-project failure badge in the sidebar** that persists until you open the project and dismiss the held failed view.

### Decisions settled in discussion (do not re-litigate)

- **One workflow per project** (multiple projects may run concurrently). The live view is therefore **singular per project**; no stacking.
- **Step labels are required** on *every* step type, including the capability-gated `pause_for_user` / `for_each` (not runnable in v1 but will be by end of v1 — requiring labels now avoids a later migration).
- **Failed runs hold** the live view (failed step + reason) until the user **dismisses**; dismiss restores the compose box. Complete/cancelled restore compose immediately.
- **Whole-project compose lockout is the accepted v1 mechanism.** Replacing the entire project compose box (not merely disabling sends to participants) is the simplest airtight anti-queue guarantee. It also blocks messaging idle non-participant agents for the run's duration — an accepted, recorded v1 limitation (per-agent compose during a run is deferred), chosen for structural simplicity over partial-compose + recipient filtering.
- **No global header indicator.** The app-header workflow chip (`WorkflowRunIndicator`) is removed. Running/success are covered by the sidebar project row + the per-project live view; a background failure is covered by the sidebar failure badge. Cross-project *stop* is preserved by making the sidebar card's existing stop control workflow-run-aware (cancels the run), so removing the header chip loses no capability.
- **Layout is a vertical step list** (chosen over horizontal milestone-dots: legible with long labels and many steps, not width-constrained in the compose area, consistent with existing checklist/tool-call visuals).

### Required reading before implementing

- Svelte 5 runes — `$derived` (the dynamic preview resolution depends on this): https://svelte.dev/docs/svelte/$derived ; `$state`/`$bindable`: https://svelte.dev/docs/svelte/$state
- `serde_norway` (the YAML parser used for workflow files): https://docs.rs/serde_norway
- Internal: `docs/ui-conventions.md` (token model, `ui/` primitives, `Spinner`/`StatusDot`), `docs/system-design.md` §7 (sends/turns) and §6 (workflows), and the predecessor plan `docs/implementation_plans/2026-06-21-workflows-refactor.md`.
- Internal: `AGENTS.md` → Testing (the component-test rule for IPC+event+state wrappers; `await tick()` for presence assertions).

## Shared conventions (established here, reused by later milestones)

- **`WorkflowStepInfo`** (M1/M2) is the single shape describing a step for display. It is exposed in two places — the form descriptor (declared, for preview) and `WorkflowRunInfo` (resolved at invoke, for the live view). M3 and M4 both render it; neither invents a parallel shape.
- **Per-step status is derived, not stored** (M3): given the run's current `step` index, `total`, and `failed_step`, a step at `index < step` is *done*, `== step` is *active* (or *failed* if `failed_step == index`), `> step` is *pending*. The `WorkflowSteps` component owns this derivation; the backend adds no per-step status field.
- **Seed-then-update run state** (existing, in `workflows.svelte.ts`): `workflowRuns[projectId]` is seeded from `list_workflow_runs` on subscribe and updated by `workflow:<project-id>` progress events; failed/interrupted runs persist until **abandon**. M4 and M5 reuse this — the live view, the failure hold, and the sidebar badge all read `workflowRuns[projectId]`, and "dismiss" is the existing abandon command.

---

## M1 — Required step labels (grammar, model, parser)

### Goal & Outcome
Every workflow step carries a required, human-readable `label`. After this milestone:
- A workflow file whose step is missing a label (or has a blank/non-string label) fails to parse with a clear, typed error naming the offending step.
- A step with a label and exactly one step-type key parses; the label is available on the parsed model for every step type, including `pause_for_user` / `for_each`.
- All built-in workflows and all test fixtures carry labels; the authoring doc documents `label` as required.

### Implementation Outline
- **Grammar:** `label` is a **reserved sibling key** alongside the single step-type key, e.g.
  ```yaml
  steps:
    - label: Gather reviews
      send: { to: reviewers, prompt: code-review }
  ```
  Chosen over nesting `label` inside each step body (`send: { label, to, ... }`) because the label belongs to the step wrapper, not the step's parameters — keeping it out of `parse_send`/`parse_wait_for`/etc.
- **Parser** (`crates/workflow/src/parse.rs`, `parse_step` at the `map.len() == 1` check): extract `label` from the step mapping first (required; error via the existing `WorkflowError` validation path if absent, non-string, or blank, with the step's `ctx` in the message), then enforce **exactly one remaining key** as the step type as today. Add `label` to the reserved-key set so it can't be read as a step type. The "exactly one step-type key" guarantee must continue to hold for the non-label remainder.
- **Model** (`crates/workflow/src/model.rs`): carry `label` such that it is **uniformly readable for every step** (the M2 exposer must iterate steps and read each label without matching per-variant). A wrapper that pairs a label with the step kind, or a label field on each variant — implementer's call against the code, but uniform access is the contract.
- **Rationale must survive into code:** a comment at the parser/model change stating *why* `label` is a reserved sibling key and *why* it's required (drives both the preview and live progress views; agents author workflows so it's no burden). No milestone/plan references in the comment per `AGENTS.md`.
- **Fixtures & built-ins:** add labels to the built-in workflows (`crates/workflow/src/builtin.rs`) and to every workflow YAML fixture across the `workflow`, `dispatcher`, and `app` crates — the parser now rejects label-less YAML, so this is mechanical but must be complete or those crates won't compile their tests.

### Definition of Done
- **Unit tests** (`workflow` crate): label absent → typed error; label blank → error; label non-string → error; valid label + one step-type key → ok; label + two step-type keys → the existing "exactly one step-type key" error; a label present and readable on each step type including `pause_for_user` and `for_each`.
- Existing built-in/parse tests pass with labels added (no behavior regressions).
- **Docs:** the workflow-authoring doc states `label` is required, with a corrected example. Note as a known constraint that any externally-authored label-less workflow will now fail to parse (intended).

---

## M2 — Expose step metadata (`WorkflowStepInfo`)

### Goal & Outcome
The step list is available to the frontend in both the places it's needed.
- The workflow **form descriptor** carries each step's label, **declared** recipients (literal agent names and/or input-slot names), and an optional data-flow hint — enough to render a preview.
- A running workflow's **`WorkflowRunInfo`** (from `list_workflow_runs`) carries the same per-step list with recipients **resolved** against the bindings the user submitted at invoke, so the live view can name the actual agents.

### Implementation Outline
- **Shape:** introduce `WorkflowStepInfo { label, recipients, feeds_from }` where `recipients` is an ordered list of references — each either a literal agent name or an input-slot name (a small two-variant enum; `SendStep.to` is a `Templated` that may be a single value or a list, so model recipients as a list). `feeds_from` is optional, derived from `forward_from`, used only as a display hint. Keep this in the app crate alongside the other wire types (`workflow_commands.rs`) and mirror it in `src/lib/types.ts` as a snake_case discriminated shape, consistent with existing wire types.
- **Descriptor (preview side):** add `steps: Vec<WorkflowStepInfo>` to `WorkflowFormDescriptor` (`workflow_commands.rs:527`), populated where the descriptor is built (`describe_workflow_form_impl`). Recipients here are **declared** — slot names stay as slot names; the frontend resolves them (M3).
- **Run info (live side):** add the same `steps` to `WorkflowRunInfo` (`:248`). For a **live** run (in the in-memory registry), recipients are **resolved**: slot references map to the agent names the user bound at invoke; a recipient that can't be statically resolved at invoke (a runtime template — uncommon in v1) falls back to its raw reference string.
- **Durable steps for disk-sourced runs (load-bearing — fixes a real break):** the in-memory registry entry is pruned on terminal, so any `failed` **or** `interrupted` run surfaces from the disk scan (`classify_run_file`) after a reload — and the held-failure view + the badge's "open to dismiss" path are *primary* ways to reach it post-restart. The run file today stores only workflow name + step count (`RunRecord::Started`, `crates/workflow/src/run.rs:73`), so there is nothing to render steps from. **Do not** reconstruct by re-parsing the workflow file by name — that's fragile (a built-in vs user copy share a name; the file may have been edited or deleted since launch), so it can show the *wrong* steps. Instead, **persist a compact declared-step snapshot** (labels + declared recipients — i.e. the declared `WorkflowStepInfo[]`) into the run file at start (extend `Started` or add an additive `#[non_exhaustive]` record). `list_workflow_runs` reconstructs `steps` for any disk-sourced run from that snapshot — no re-parse, no name collision. Accepted asymmetry: a failed run shows *resolved* recipients live and *declared* recipients after a reload (fine — it exists only to be read and dismissed). **§3 note:** labels + declared recipients are workflow-*definition* metadata, not agent output, so persisting them does not touch the "Switchboard stores no agent content" invariant — but update the rationale comment in `run.rs` (and the `started_record_holds_only_name_count_and_time` test, whose asserted shape legitimately changes) to say *why* display metadata is now durable and that it is still not replay state. **Pre-release migration:** a run file written before this change has no snapshot, so a legacy failed/interrupted run reconstructs with empty steps; acceptable since workflows are pre-release (no run files in the wild) and the record is additive — degrade gracefully (render the run with no step rows), don't error.
- **Enforce one run per project (the singular-view invariant the UI relies on):** `invoke_workflow_impl` inserts `ActiveRun` keyed by `run_id` with no project-level guard, so a double-click / stale UI / concurrent invoke can start two runs in one project — breaking the live view and interleaving sends. Two parts:
  - *Active runs (atomic):* the check and the insert must be **atomic under a single `workflow_runs` lock acquisition** — while holding the lock, scan for an existing active run with the same `project_id`, reject with `WorkflowAlreadyRunning` if found, otherwise insert the new `ActiveRun`, and spawn the run task only *after* that insert returns. Do not check, release, then insert (two concurrent invokes would both pass).
  - *Held runs (occupancy):* a **retained failed/interrupted** run also occupies the project — it replaces the compose box until dismissed (M4), so launching requires dismissing it first. Before registering, scan the project's run files for a surfaced failed/interrupted run (excluding live runs, whose not-yet-terminal file classifies as `interrupted`) and reject with a **distinct** `WorkflowRunRequiresDismissal` (so the UI prompts "dismiss the failed run first" rather than "wait for the running one"). Do this disk scan **outside** the `workflow_runs` lock — `read_run_files` does blocking I/O and the mutex is sync; it is race-free because a new held file can only appear when an active run terminalizes, and the atomic active-run check already rejects a second active run. *(This corrects an earlier version of this plan that said a held run "must not block a new launch" — that predated the M4/M5 decision that a held failure replaces compose, and contradicts it.)*
- **Do NOT** put the step list on the per-event `WorkflowProgressPayload` (`:116`). That payload fires every step transition; bloating it with the full list each time is wasteful and breaks the seed-then-update split. The list belongs on `WorkflowRunInfo` (seeded once on subscribe / refreshed on invoke — see M4); progress events keep updating only `step`/`status`/`reason` as today.
- **Rationale into code:** comment on `WorkflowRunInfo.steps` explaining the declared-vs-resolved split and why the list rides the run info (not the progress event), and on the run-file snapshot explaining the durable-display-metadata decision.

### Definition of Done
- **Unit tests** (`app` crate): the descriptor exposer yields steps in order with correct labels, declared recipients (literal *and* slot cases), and `feeds_from` when `forward_from` is present / `None` otherwise. The live registry yields resolved recipients for a representative invoke (slot → bound agent names; multi-recipient list; the runtime-template fallback). A disk-sourced **failed** run **and** a disk-sourced **interrupted** run each reconstruct `steps` from the persisted snapshot (declared recipients). One-run-per-project: a sequential duplicate invoke returns `WorkflowAlreadyRunning`; a held **failed** *and* a held **interrupted** run each make invoke return `WorkflowRunRequiresDismissal`, and a subsequent `abandon` frees the project so the same invoke succeeds.
- **Wire-shape tests:** `WorkflowStepInfo` serializes snake_case and round-trips against the TS type; the run-file snapshot round-trips (follow the `run.rs` round-trip pattern). The `started_record_holds_only_name_count_and_time` test is *updated* (not removed) to the new record shape.
- **Docs:** none beyond code comments unless a system-design wire-shape table needs the new field.

---

## M3 — `WorkflowSteps` component (preview + live) and composer preview

### Goal & Outcome
A single component renders the step list in either mode, and the `+ Workflow` composer shows a live-updating preview.
- In the composer, below the description, the user sees every step: its label, the agent(s) it will invoke, and any "feeds from" hint.
- A step targeting an unbound input slot shows the **slot name**; as the user toggles panes/agents in the form, that row **updates live** to the bound agent name(s) — and back to the slot name if cleared.
- In live mode, the same rows show per-step state (done / active / pending / failed), with a spinner on the active step.

### Implementation Outline
- **Component:** a `WorkflowSteps` component taking the `WorkflowStepInfo[]` plus a mode discriminator. **Preview mode** input: the steps + the current form `inputs` (to resolve slot recipients). **Live mode** input: the steps + `step`/`total`/`failedStep`/`status`/`reason` (per-step status derived per the shared convention). It owns the status derivation; callers pass raw run fields, not pre-computed per-step states.
- **Dynamic preview resolution (the load-bearing decision):** the preview's displayed recipients are a `$derived` over the form's `inputs` record. `WorkflowComposer` already holds `inputs` as a `$bindable` rune-backed record that the pane/agent chips mutate on click; reading it inside a `$derived` makes the rows re-resolve automatically on every toggle — no event wiring, no backend round-trip. A single `agent` slot resolves to one name (or the slot name when empty); an `[agent]` slot resolves to the live list. Reuse the existing input/slot accessors in `WorkflowComposer` rather than re-deriving binding logic.
- **Wire preview** into `WorkflowComposer.svelte` below the description block, reading `descriptor.steps`. Preview rows carry no status glyphs and no stop control.
- **Glyphs & styling:** reuse `Spinner.svelte` (active) and `StatusDot` (pending/failed); hand-roll the check SVG for done (no shared checkbox primitive exists — consistent with the codebase). Follow `docs/ui-conventions.md` tokens; the failed state uses `status-failed`. Vertical list, one row per step: glyph, label, resolved recipients, optional "feeds from" line; long lists scroll within the available height.

### Definition of Done
- **Component tests** (jsdom): preview renders rows in order from a descriptor; a slot recipient shows the slot name when `inputs` is empty and the resolved agent name(s) once bound; toggling a binding updates the row (assert with `await tick()` for the presence change); single-slot vs list-slot rendering; `feeds_from` hint shown when present. Live mode: for a given `step`/`total`/`failedStep`, the correct rows render done/active/pending and the active row shows the spinner; a `failed` run marks the failed row with the reason. These are pure-prop renders (no IPC), so jsdom is sufficient — no browser test needed (no layout measurement involved).
- **Docs:** none beyond code comments.

---

## M4 — Compose lockout, live view, and failure hold

### Goal & Outcome
A project running a workflow shows the live progress view instead of compose, and queueing is impossible until the run resolves.
- When a workflow starts in the viewed project, the compose box is replaced by the live `WorkflowSteps` view with a working **stop** control.
- On **complete** or **cancelled**, the compose box returns.
- On **failed**, the view **stays** showing the failed step + reason until the user **dismisses** it; dismiss returns the compose box.
- While the live view is up there is no textarea and no send path — queueing cannot happen.

### Implementation Outline
- **Swap point:** in `ComposeBar.svelte`, gate the compose UI on the viewed project's run state from `workflowRuns[projectId]` (the existing `$state`). A `running` run → render `WorkflowSteps` (live) in place of compose. This is the *replacement* that makes queueing structurally impossible — do not merely disable the textarea.
- **The run must obtain its `steps` on the start path (load-bearing — the normal launch path breaks without it):** today `handleProgress` (`workflows.svelte.ts:106`) builds the run row from the lean progress payload, which has **no** `steps`. Two defects to fix: (1) a freshly-invoked run whose first `running` event arrives before any seed gets appended *step-less*; (2) the in-place update path (`current.map(r => row)`) *replaces* the whole row, **wiping `steps`** even on a run that was seeded with them. Fix both: after `invokeWorkflow` resolves (and before tearing down the composer), call `refreshRuns(projectId)` so the row is seeded from `list_workflow_runs` (which now carries `steps`); and change the known-run update in `handleProgress` to **merge** — preserve existing `steps` (and any field the payload lacks) rather than replacing the object. For an unknown `running` run arriving before the refresh, trigger a `refreshRuns` instead of appending a skeletal row. Compose must not become usable in the gap. *(Defect (2), the merge-preserve, already landed in M2 — the type change to `WorkflowRunInfo` forced the touch; M4 owns defect (1), the refresh-on-invoke and unknown-event refresh, plus the start-path tests below, which must be written before M4 is considered done.)*
- **Singular per project:** rely on the backend one-run-per-project guard (M2); render the single run, no stacking.
- **Stop:** wire the live view's stop control to the existing `cancelRun` command (reuse, don't add a new command).
- **Failure hold + dismiss:** when the run's status is `failed` (or `interrupted`), keep rendering the live view (failed state) instead of restoring compose. **Dismiss reuses the existing abandon command** — abandoning drops the run from the registry/`workflowRuns`, which both clears the held view (compose returns) and clears the sidebar badge (M5). Do not invent a separate dismiss state; the persistence of failed/interrupted runs in `workflowRuns` *is* the hold, and abandon *is* the dismiss.
- **Constraint:** preserve the established listener-before-seed ordering in `workflows.svelte.ts` so a run that terminalizes between subscribe and seed is handled (existing behavior — don't regress it). The swap must react correctly to a progress event arriving before the initial `list_workflow_runs` seed resolves.

### Definition of Done
- **Component tests** (`ComposeBar`, jsdom, mocking `invoke`/`listen` and driving `workflow:<project-id>` events per the `AGENTS.md` rule): compose swaps to the live view on a `running` run; restores on `complete` and on `cancelled`; **holds** on `failed` and restores only after abandon; the stop control invokes `cancelRun`; no send path is reachable while the live view is up. **Start-path tests:** invoke emits a `running` event *before* the refresh resolves → the live view eventually renders labeled steps and compose never becomes usable in the gap; a progress update on a known seeded run **preserves** its `steps`. **Disk-seeded failure-hold test:** the held failure view renders correctly when the failed run is seeded from disk (declared steps), not only from a live transition. Include the ordering race and re-delivery idempotency.
- **Docs:** none beyond code comments.

## M5 — Remove global indicator; per-project sidebar failure badge

### Goal & Outcome
The global header indicator is gone; the per-project sidebar row carries both the running and failed signals, including a workflow-aware stop.
- The app header no longer shows the workflow-run chip (`WorkflowRunIndicator`).
- A project actively running a workflow shows a running indicator on its sidebar row, and its existing stop control **cancels the workflow run** (not just live sends) — preserving cross-project stop now that the header chip is gone.
- A project whose workflow ended in failure (or was found interrupted at startup) shows a persistent failure mark (a `status-failed` dot/badge) on its sidebar row.
- The badge persists across navigation and clears when the user opens the project and dismisses the held failed view (the same abandon action from M4).

### Implementation Outline
- **Remove** the `WorkflowRunIndicator` mount from `App.svelte` and delete the component; drop any now-unused global-only helpers in `workflows.svelte.ts` (e.g. `allRuns`) if nothing else consumes them (verify against the code before deleting).
- **Workflow-derived row state (correctness — the existing spinner does NOT cover this):** the sidebar row's `busy` is `liveProjectSends(project.id).size > 0` — live agent *sends*, not workflow state. A workflow between steps, before its first dispatch, or in the failed-held state has no live send and would look idle. So derive explicit row state from `workflowRuns[project.id]`: `workflowRunning` and `workflowFailedOrInterrupted`. Do **not** describe this as the unchanged existing spinner.
- **Workflow-aware stop (resolves the lost-cross-project-cancel concern):** when `workflowRunning`, the row's stop control calls `cancelRun(runId)` for that project's run (which stops the run and its sends), instead of the live-sends cancel. When there is no workflow run, the control keeps its current "cancel all running agents" behavior. This relocates cross-project stop from the removed header chip to the sidebar row, where the user already expects it — so no capability is lost.
- **Badge derivation:** derive the badge from `workflowFailedOrInterrupted`. Reuses the seed-then-update state — no new backend signal.
- **Clear on dismiss:** because dismiss = abandon removes the run from `workflowRuns` (M4), the badge clears automatically; no extra wiring. Confirm the sidebar reads the same reactive state the abandon path mutates.
- **Rationale into code:** comment that the badge is the *only* surviving signal for a *background* workflow failure (the global indicator having been removed) and persists until abandon, and that the stop control is workflow-run-aware when a run is active.

### Definition of Done
- **Component/state tests:** a `running` run with **no** live sends still renders the running indicator (the bug this fixes); the row's stop calls `cancelRun` when a workflow is running and the live-sends cancel otherwise; a `failed`/`interrupted` run renders the badge while a `running`/absent run does not; abandoning clears the badge; a normal agent turn (live send, no workflow) still spins as before. Assert the header no longer mounts the indicator (and that removing `allRuns`/global helpers didn't break a remaining consumer — search first).
- **Docs:** if `README.md`'s harness/limitations or a UI doc references the global indicator, update it.

## M6 — Verification (manual, with induced failure)

### Goal & Outcome
Confirm the full path works in the real app, including the failure surfaces, which are otherwise hard to reach. After this milestone the user has personally seen: a live progress run, the held failure state in the progress component, and the persistent sidebar failure badge clearing on dismiss.

### Implementation Outline
- Run the app (`make dev`) and exercise a real workflow end-to-end: preview in the composer (recipients resolving live as agents are bound), launch, the compose→live swap, step progression, and a clean completion restoring compose.
- **Induced failure (temporary, must be reverted):** introduce a clearly-marked **temporary** hack that forces a running workflow to fail at a step — e.g. make a step's execution return an error in the interpreter, gated so it's obviously a debug stub (a `// TEMP: induce failure` marker). This is so the user can **see** the failure state in two places:
  1. the **live progress component** — the failed step marked with `status-failed` + the reason, the view *held* (compose not restored);
  2. the **sidebar failure badge** — present on the project row, persisting while the user navigates to another project and back.
  Then have the user **dismiss** (abandon) and confirm both the held view clears (compose returns) and the badge clears.
- **Revert the hack** before the milestone is complete; confirm via `git diff` that no temporary code remains. The induced-failure change is never committed.
- Run `make check` (fmt, lint, test, type-check, browser suite) and confirm green.

### Definition of Done
- The user has visually confirmed: live progress, the held failed state in the progress component, the sidebar badge, and that dismiss clears both.
- The temporary failure hack is fully reverted (`git diff` clean of it).
- `make check` passes.
- **Known limitations recorded:** per-agent compose during a run (messaging a non-participating agent) remains out of scope; horizontal milestone-dot layout deferred; multiple concurrent workflows per project is not a v1 capability.
