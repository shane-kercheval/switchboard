# Reading mode — watch a project without being "in" it

## Goal

Let the user read a project's live transcript while telling Switchboard "treat me as
though I'm not here" — the compose box hides, and completions in *this* project notify
the same way a background project's would. The mode ends by itself when the project goes
quiet, which is when the user actually has something to act on.

Getting there requires settling a pre-existing inconsistency: Switchboard has three
signals that all mean "this project finished," and today they mean three different
things. Reading mode is the third consumer of that judgment, so the judgment gets a
single definition first.

## Background: three signals, three meanings

| Signal | Today's condition |
|---|---|
| Sidebar row spinner | `liveProjectSends(id).size > 0 \|\| workflowRunning` — project-wide |
| Green checkmark | The project's live *send pairs* went from some to none, **and** it wasn't the active project. Not workflow-aware. |
| Completion notification | One **activity batch** drained — sends transitively merged over shared recipients. Not project-wide. Workflow runs notify separately from the backend, at run terminal. |

The notification's batch scope is not a considered semantic choice — it is the residue of
a bug fix. The original design (`2026-08-06-notifications.md`, "What the user asked for"
item 1) was per-*send*: "for a fan-out send, once when *all* recipients finish." Commit
`7fe9b23` widened it to connected batches to stop notifications firing *between queued
turns* on one agent. That merge rule is exactly the minimum that fixes that symptom, and
the module's own rationale argues for going at least that wide, not for stopping there:

> notifying for one part while a **connected** queued turn remains live would recreate the
> intermediate notification this module avoids

Project scope is a strict superset satisfying the same rationale. The trajectory has been
monotone — per-send → per-connected-batch → per-project — each step widening for one
reason: don't tell the user it's done when it isn't.

## Required reading before implementing

- `AGENTS.md` — cross-cutting invariants, test vocabulary, the `make` targets rule.
- `docs/implementation_plans/2026-08-06-notifications.md` — **especially D3 and D4**. D3
  owns the suppression policy and the `visible_project` mechanism this feature works
  through; D4 owns the structural exclusion of workflow *steps*. Both are load-bearing
  here and one of them is amended by M2 (see below).
- `docs/system-design.md` §7 "Inside a project" and "Composing and dispatching messages".
- `docs/ui-conventions.md` — for the toggle affordance (semantic tokens, reach for a
  primitive, `Tooltip` not `title`).
- `crates/app/src/notification.rs` module docs — the gate, and why it lives there.

No new external dependencies or platform APIs. macOS delivery (`mac-usernotifications`,
the ad-hoc signing precondition, the Do Not Disturb limitation) is untouched by this plan.

## Decisions carried from planning

These cannot be recovered from the code. Each must survive into a module doc or comment
where noted.

1. **Reading mode works *through* the existing suppression gate, not around it.** The
   frontend already derives `visibleProjectId` and pushes it to the backend
   (`App.svelte`); the Rust gate reads it. Reading mode adds a clause to that derivation
   so the project reads as "not on screen." The gate is not modified, and the frontend
   does not gain a second gate. This preserves D3's core invariant.

2. **Reading mode is transient and in-memory — not persisted.** It is a posture ("I'm
   watching this run"), not a preference; auto-off already says it is meant to be
   short-lived. Persisting it means opening the app tomorrow to a hidden compose box with
   no memory of why. Record this rationale in the state module's doc.

3. **Auto-off fires on project-idle, not on a batch finishing.** The user's intent is
   "tell me when I can proceed," which is when nothing is left running in the project.

4. **The notification moves to project scope, and the backend workflow notification is
   deleted rather than suppressed.** Only the frontend can see every outcome: a
   pre-dispatch IPC rejection never reaches the backend event stream (`sendCompletion.ts`
   module doc), while the workflow run's outcome *is* fully visible to the frontend via
   the progress payload. A suppression rule would instead require a second project-idle
   predicate in Rust that must agree with the TypeScript one forever — the drift D3 warns
   about, relocated from the gate to the trigger. This does not weaken D3: `api.notify`
   is built for frontend-composed *text* with backend-owned *policy*.

5. **D4 is amended, not violated.** D4 excludes workflow *steps* from notifying, and that
   still holds — steps are never registered. What changes is that the workflow *run's*
   terminal is now reported by the frontend accumulator instead of the backend. Amend D4
   in `2026-08-06-notifications.md` with a dated note pointing at this plan, rather than
   silently leaving it stale. (Per `AGENTS.md`, implementation plans are historical; a
   pointer note is the exception that keeps a superseded decision honest, not a rewrite.)

6. **The green checkmark is deliberately left alone.** Considered and rejected: the
   checkmark's active-project guard only bites in the one scenario reading mode covers
   (you are *in* the project), and auto-off fires at the same instant the checkmark would
   be set — so it would either flash invisibly or sit on the row of a project the user is
   looking at, claiming an unread completion for something on screen. The case that
   matters (reading mode on for project A while looking at B) already works, because A is
   not active. Record this in the plan only; no code comment needed for a change not made.

7. **Reading mode is allowed on a project that is already quiet**, where it stays on until
   the user turns it off or until later activity starts and settles. Chosen over disabling
   the toggle for idle projects. Reading a long finished transcript without the compose box
   in the way is a legitimate use that the feature's name promises, and the confusion the
   restriction would prevent is already answered by a visibly latched toggle. Restricting
   would also couple two more surfaces (the toggle's enabled state and the palette entry)
   to the idle predicate for no functional gain. Record the resulting behavior in the
   toggle's own doc or comment so it reads as intentional rather than as a missing guard.

8. **Settlement stays event-driven; only the scope and the completeness test change.**
   `sendCompletion.ts` deliberately settles on authoritative dispatcher events rather than
   inferring completion from liveness, and its module doc gives three reasons. Project
   scope changes *what* accumulates and *when the accumulation is complete* — it does not
   change *what triggers* settlement. An earlier draft of this plan replaced the flush
   trigger with a liveness edge and reintroduced the second of those three hazards
   verbatim; see M2 for the corrected trigger shapes. This sentence must survive into the
   rewritten module doc.

## Milestone order

M1 → M2 → M3, and the order is load-bearing. M3 (reading mode) is the requested feature,
but it lands last: with M2 done there is exactly **one** notification per project-idle
transition, which turns reading mode's auto-off from a race-prone coordination across N
batch flushes into a single ordered step. Doing M3 first would mean building that
coordination and then deleting it.

---

## M1 — One project-idle predicate

### Goal & Outcome

Give "this project has nothing left running" a single definition, and point the existing
consumers at it.

- The sidebar row, the background-completed observer, and (later) reading mode and the
  notification all judge idleness identically.
- The green checkmark becomes workflow-aware: a project whose workflow is between steps
  no longer records as completed. (Today the *rendered* checkmark is correct because its
  branch sits after `busy || workflowRunning`, but the recorded transition is not — a
  latent inconsistency this closes.)
- No user-visible behavior change other than that fix.

### Implementation Outline

Add `projectIsIdle(projectId)` to `workspace.svelte.ts`, beside `liveProjectSends`. Its
definition is the sidebar row's existing spinner condition, inverted:

```
projectIsIdle(id) === liveProjectSends(id).size === 0
                   && no run in workflowRuns[id] with status "running"
```

The workflow clause is not optional. `liveProjectSends` does not see a workflow between
steps, before its first dispatch, or in the failed-held state — the reason is already
documented at the sidebar row's `{@const workflowRunning}` and must not be re-derived.

Two consumers change:

- **The sidebar row** keeps its separate `busy` / `workflowRunning` locals — it needs them
  to choose the cancel action and its label ("Stop workflow" vs "Cancel all running
  agents"). But the *idle judgment* it renders must route through the shared predicate, so
  the row and the predicate cannot drift.

- **`startProjectActivityObserver`** does two distinct jobs today; only one moves. Keep the
  per-pair disappearance tracking that feeds `recordProjectsActivityLocally` — that drives
  `last_activity` ordering and is a different concern. Redirect only the
  `backgroundCompletedProjectIds` determination onto the not-idle → idle transition of the
  predicate. This means the observer's tracked snapshot needs a per-project idle flag, not
  just live send pairs, so a workflow terminal alone can complete the transition.

  The observer stays **edge-triggered**, and deliberately so — do not harmonize it with
  M2's level-checked flush. It answers "did this project just finish while you weren't
  looking," which is inherently an event, and it never inspects outcomes, so the
  late-settlement hazard that forces M2 to be level-checked does not apply to it.

Leave the observer's `id !== selection.activeProjectId` guard exactly as-is (decision 6).

### Definition of Done

- Unit tests in `workspace.test.ts`:
  - A project with a running workflow but no live sends is **not** idle (the case
    `liveProjectSends` alone gets wrong).
  - A project with a failed-held workflow run and no live sends **is** idle.
  - The background-completed transition fires when the last live send drains *and* fires
    when a workflow terminalizes with no live sends — and does **not** fire when only one
    of the two clears.
  - `last_activity` recording is unaffected by the above (guards against conflating the
    observer's two jobs).
- Existing `ProjectsSidebar` component tests still pass unchanged; add one asserting the
  spinner shows for a workflow between steps with no live sends.

### Known limitation recorded during M1

**A wedged workflow run leaves its project marked busy until the app restarts.**
`cancel_workflow_run` only *requests* cancellation — it fires the run's token and returns
`Ok` without waiting — so a run that never honours the request keeps a `running` entry in
`workflowRuns` indefinitely, and the row keeps spinning with no error to surface (Stop
genuinely succeeded). Restarting clears it: the interpreter is an in-process task, so the
run file loses its live registry entry and `classify_run_file` reclassifies it as
`interrupted`, which is not `running`.

This is why **Delete is deliberately not gated on the idle predicate** while Archive is.
`delete_project` cancels the project's runs under `RUN_TEARDOWN_DEADLINE` and proceeds
regardless, so deleting a project with a stuck run is a supported backend operation;
gating the UI on it made the app stricter than its own guarantee and — since there is no
directory-removal affordance anywhere in the UI — left such a project with no disposal
path at all. An earlier revision of M1 did gate it, on the reasoning that stopping the
run first is one click away; that reasoning fails precisely when Stop cannot help.

Not closed further (e.g. a force-delete affordance surfaced after a pending cancellation):
an ordinary confirmed delete already performs the operation correctly, so the extra
surface would buy nothing but an alarming control for a case most users never hit.

---

## M2 — Notification at project scope

### Goal & Outcome

One notification per project, when the project goes quiet, describing everything that
happened.

- Finishing one send while another agent in the same project is still working produces
  **no** notification; the notification arrives when the project is idle.
- A workflow run's terminal no longer notifies on its own — it is reported at the same
  project-idle boundary as manual sends, so a workflow finishing into a quiet project
  produces exactly one notification instead of two.
- Cancelled and interrupted outcomes stay silent, exactly as both paths already treat them.
- The suppression gate (master preference, window focus, viewed project) is unchanged.

### Implementation Outline

**Widen the accumulator from batch to project.** `sendCompletion.ts` currently maintains
per-batch state whose only purpose is computing connectivity: `mergeBatches`,
`activeBatchByAgent`, `abandonBatch`, and the damaged-batch recovery paths. Project scope
makes connectivity trivial — one accumulator per project — so that machinery should be
**deleted**, not adapted. Retain and re-home:

- per-recipient outcome tracking (`completed` / `failed` / `cancelled`),
- registration of the full recipient set *before* any IPC call (so a pre-dispatch
  rejection cannot erase a recipient that was supposed to be there),
- settlement via the dispatcher's authoritative `agent_idle`, turn, and cancellation
  events — do not substitute a liveness diff. The module doc explains at length why
  watching `buildLiveSendsMap` is the wrong trigger; that reasoning survives the rescope
  intact and must survive into the rewritten doc.

**`sendCompletion` stays a leaf module.** It imports only `$lib/api` and types today, which
is what lets its tests exercise it through a bare `await import("./sendCompletion")` with no
workspace graph behind it. Preserve that: **it must not import workspace state**, and in
particular must not import `projectIsIdle` — dynamically or otherwise. That direction is
also a cycle (`workspace` → `index` → `sendCompletion`), but the cycle is the symptom; the
leaf property is the rule, and stating it the other way invites someone to "fix" it with a
dynamic import while keeping the wrong dependency direction.

Both of the flush's triggers therefore arrive from outside. Settlement events already come
in from `index` below. The predicate-transition trigger is **pushed in from above** — the
activity observer (or a thin coordinator alongside it) calls an exported entry point on
`sendCompletion` when a project's idle state changes. M1 shaped `projectIsIdle` to make this
easy: it takes an optional pre-computed live-send map, so the observer's existing single
pass can supply the transition without a second scan.

**The flush is level-checked, not edge-triggered.** This is the single most important
contract in the milestone and the easiest to get wrong. The flush condition is:

```
projectIsIdle(projectId)                    // M1's predicate — workflow-aware
  AND every tracked outcome for the project is settled
  AND every started agent has drained (agent_idle)
```

evaluated on **both** every settlement event (as the current code does) **and** every
predicate transition. Both triggers are required, and neither alone is sufficient:

- Settlement-only misses the case where a workflow terminalizes as the last activity — the
  condition becomes true with no send event to re-evaluate it.
- Transition-only misses every outcome that settles *after* the project already looks idle.
  Cancelling a queued send clears it from `buildLiveSendsMap` immediately, before the
  backend's `message_cancelled` carries the outcome — so the transition fires while
  outcomes are still unsettled, correctly declines to flush, and no later edge ever occurs.
  A send whose dispatch IPC rejects has the same shape: it never makes the project
  non-idle, so no edge exists at all. These are hazards #3 and #2 in the module's own
  "why not liveness" list; do not reintroduce them.

Two rules that fall out and must be stated in the code:

- **Never flush an empty accumulator.** A flush with nothing tracked has nothing to report,
  and this property is load-bearing for M3 (decision 7) — without it, enabling reading mode
  on a quiet project would immediately silent-flush and clear itself.
- **Keep a per-project in-flight marker — as a signal, never as a guard.** *(Corrected
  during M2 implementation; the original text here required guarding flush re-entrancy with
  it, and that was wrong.)* Re-entrancy needs no guard: the flush deletes the project's
  accumulator synchronously *before* the delivery promise, so a second evaluation finds
  nothing to flush — the guarantee is structural. Using the marker as a guard is actively
  harmful: activity registered and completed while a previous notification is still in
  flight is a genuinely new quiet-down, and suppressing it drops that notification
  permanently, since nothing retries once the promise settles (a test pins this case).
  The marker survives for exactly one consumer: M3's fallback watcher reads it so it
  cannot clear reading mode — and with it restore the project's visibility — while the
  notification that clearing would suppress is still travelling to the gate.

**Fold the workflow run terminal into the accumulator.** Delete `notify_run_terminal` and
its call site in `crates/app/src/workflow_commands.rs`. The frontend already receives the
run's terminal status and reason in the progress payload (`workflows.svelte.ts`
`handleProgress`, "no re-query" — the payload is authoritative). Carry these semantics
across verbatim; they are currently documented on the deleted Rust function and must not
be lost:

- `complete` and `failed` are worth reporting.
- `cancelled` is silent — the user just asked for it and was present to ask. This already
  matches `describe()`'s existing rule for an all-cancelled send, so the two converge
  rather than conflict.
- `interrupted` is silent, and note it never arrives as a progress *event* at all (a
  crashed run has no live process; it surfaces only via the seed-on-subscribe query). So
  there is no event path to handle — record why, or the next reader will look for one.

Two ordering constraints on that hook, both load-bearing:

- **Record the outcome before mutating `workflowRuns`.** The drop is what flips
  `projectIsIdle`, and today it reaches the flush only through the activity observer's
  *scheduled* effect — so the reverse order would also work, and a test cannot distinguish
  them. State the contract rather than softening it to taste: if the idle push ever becomes
  synchronous (calling `noteProjectStates` directly from `handleProgress` for promptness is
  the obvious optimization, and M3 cares about notification timing), recording after the
  drop would let the flush run without the workflow's outcome and silently omit it.
- **Do not require the run to be previously known.** A terminal can arrive for a run the
  frontend never held in `workflowRuns` (background-start; noted in that function's own
  comment), so treat the payload as self-contained. It is — except for the project *name*
  the notification body needs, which the payload does not carry and which must come from
  `projects.list`. The deleted Rust function received it as a parameter; nothing replaces
  that automatically.

**Notification text.** The existing shape — a title classifying the outcome mix, a body
leading with the project name — is right and should be preserved; the body says *which*
project whenever that name is available, since one notification can arrive while the user
is in another. (It is a graceful fallback, not a guarantee: the tracker learns names from
`registerSend` and the observer's push, so a notification composed before either has run
drops the prefix rather than being suppressed. Narrow — a startup race — but real, and
closing it would require the import cycle the leaf rule forbids.) Widening the scope means
one notification may now describe both agent outcomes and a workflow outcome. Keep it
terse and outcome-first; do not enumerate every agent when the list is long.

**Keep the gate untouched.** `should_deliver` in `crates/app/src/notification.rs` and the
`visible_project` mechanism are not modified by this milestone.

### Definition of Done

- `sendCompletion.test.ts` reworked to the new boundary. The existing 26 cases encode real
  bugs — port their intent rather than deleting them. Specifically must cover:
  - Two sends to **disjoint** agents produce **one** notification after both drain (the
    behavior change this milestone exists for).
  - A second send queued onto a busy agent still produces one notification after the queue
    drains (the `7fe9b23` regression must not return).
  - A pre-dispatch IPC rejection for every recipient still notifies.
  - An all-cancelled project is silent; one survivor notifies.
  - Agent removal mid-flight settles that agent without blocking the others.
  - A workflow completing into a project with live manual sends does **not** notify until
    the sends drain — one notification, not two.
  - A workflow `cancelled` / a run that only ever appears as `interrupted` via seed is
    silent.
  - **Late-settling outcomes** (the level-check regression guard): cancel the last live
    sends in a project after one agent completed, and assert the survivor still notifies
    once `message_cancelled` arrives. This test fails against an edge-triggered flush.
  - **No-edge outcomes**: a send whose dispatch IPC rejects for every recipient, in an
    otherwise-quiet project, still notifies.
  - **Workflow-only edge**: a workflow terminalizing as the last activity flushes with no
    accompanying send event.
  - **Unknown-run terminal**: a workflow terminal for a run never present in
    `workflowRuns` still contributes its outcome.
  - **Mixed content**: a quiet-down covering both agent outcomes and a workflow outcome
    produces one notification naming both.
- Rust: delete `notify_run_terminal` and any test asserting it fires. Confirm no other
  `notifier.notify` call site remains in `workflow_commands.rs`.
- Amend D4 in `docs/implementation_plans/2026-08-06-notifications.md` with a dated pointer
  to this plan (decision 5).
- Record the **starvation tradeoff** as a known limitation in the `sendCompletion.ts`
  module doc: a long-running agent now delays notification of a quick answer from an
  unrelated agent in the same project. Accepted deliberately — the contract becomes "the
  project is quiet, come back," which is what the feature is for. If it proves painful the
  fix is a separate per-agent notification option, **not** a revert to batch scope.
- Record the second known limitation: with the backend notification deleted, a webview
  crash during a long unattended run loses that run's notification. Accepted because
  manual-send notifications already have exactly this exposure — this removes an accidental
  asymmetry rather than introducing a new failure class.

---

## M3 — Reading mode

### Goal & Outcome

- The user can turn reading mode on for the project they are viewing. The compose box
  disappears; the transcript, panes, pinned messages, sidebars, and live streaming all keep
  working normally.
- While reading mode is on, a completion in *that* project notifies exactly as a background
  project's would — i.e. it obeys the user's "Also notify me about other projects while I'm
  using Switchboard" preference.
- If a workflow is running, the workflow progress view still occupies the compose slot. It
  is not hidden.
- Reading mode turns itself off when the project goes idle, after that project's
  notification has been decided.
- Turning reading mode on for a project that is *already* quiet is allowed; it stays on
  until the user turns it off or until later activity starts and settles (decision 7).
- Reading mode is per-project and does not survive a restart.
- Settings copy explains the reading-mode case of the "other projects" preference.

### Implementation Outline

**State.** A small dedicated module holding per-project reading-mode flags in memory. No
`localStorage`, no backend persistence (decision 2 — record the rationale in the module
doc). Clear a project's flag when the project is removed, alongside the other per-project
teardown in `workspace.svelte.ts`.

**Notification behavior.** Add a reading-mode clause to `App.svelte`'s `visibleProjectId`
derivation, which already returns `null` for Settings / Git / not-yet-loaded. That is the
entire notification half — the Rust gate needs no change. With `visible_project` null,
`should_deliver` falls through to `!is_viewed_project && prefs.while_focused`, which is
precisely the requested semantics. The derived-not-mirrored property of that expression is
deliberate (its comment explains why); preserve it.

**Hiding the compose box.** `ComposeBar`'s top-level template is already
`{#if activeWorkflowRun} <live progress> {:else} <compose box> {/if}`, so the workflow
progress requirement is satisfied by construction — extend the `{:else}` rather than
gating the component from `App.svelte`. Keep `ComposeBar` **mounted**: not rendering it
would also remove the workflow progress view (it lives inside) and would push the draft
through the `onDestroy` flush for no reason.

Three details the branch alone does not cover:

- The strip's outer wrapper carries its own padding, so the reading-mode condition belongs
  outside that wrapper — otherwise a bare padded strip remains.
- `ComposeBar` registers a **`window` keydown listener** outside that branch. ⌘1–9
  recipient toggles, ⇧A select-all, and ⌘Enter send would stay live against an invisible
  box. Gate the handler.
- The transcript grows into the vacated space. `UnifiedTranscript` already re-anchors on a
  `ResizeObserver`, so the mechanism exists — but it is unverified for this transition and
  needs a browser test (below).

**Affordance.** A pane-header toggle (gated on the same condition as the other pane header
controls) plus a command-palette entry — `paletteCommands` in `App.svelte` is already a
derived list. Follow `docs/ui-conventions.md`: reach for an existing primitive, use
`Tooltip` rather than `title`.

Because decision 7 lets reading mode persist on a quiet project, the affordance carries the
explanation: the toggle must show a clear latched on-state, and the palette entry must
reflect the current state so exiting is discoverable rather than requiring the user to
remember what they turned on. This is what makes "allow" safe — a hidden control would turn
a deliberate mode into a lost compose box.

Because `notify_while_focused` **defaults off**, a user on defaults who enables reading
mode gets no notifications at all — which reads as broken. Surface the dependency at the
point of use: when reading mode is on and that preference is off, say so where the user
just acted (tooltip or an inline line where the compose box was), pointing at the setting.
Do **not** have reading mode override the preference — silently ignoring a setting the user
chose is worse than explaining it.

**Auto-off, and the ordering hazard.** Turning reading mode off restores
`visible_project` to the real project id. If that write reaches Rust *before* the notify
call, the gate sees the project as visible and suppresses the notification for the very
event that ended reading mode. `set_visible_project` and `notify` are independent async
IPC calls; the existing sequence guard only orders `set_visible_project` against itself.

After M2 there is exactly one notify per project quiet-down, so the fix is a single ordered
step: **await the notify call, then clear reading mode.** The gate's decision runs
synchronously inside the Rust command handler before it returns, so awaiting is a genuine
happens-before. This is not obvious and the natural implementation — an effect watching
idle state — gets it wrong intermittently; state it in a comment at the sequencing point.

**The flush is the sole owner of clearing reading mode.** It already runs on the silent
path (`describe()` returning null short-circuits the notify, but the flush ran), so it
clears in both branches: after the awaited notify when there is a message, immediately when
silent. Do **not** add an independent watcher that also clears — one that fires on every
idle transition cannot tell whether a notify is in flight, and if its effect flushes first
it restores `visible_project` mid-flight and suppresses the very notification this
sequencing exists to protect. That failure is intermittent and worst under real IPC latency,
so a jsdom test with mocked `invoke` can pass while production loses it.

One narrowly-scoped fallback is still required, for a project going idle with **nothing
tracked** — e.g. turns observed after a webview reload that were never registered. It has a
**deliberately different trigger shape from the flush**, and the difference is not an
inconsistency to be tidied away later:

- The **flush is level-checked**: it asks "is everything settled yet?", a condition that
  becomes true when the last piece of information arrives.
- The **fallback is edge-triggered** on the not-idle → idle transition: it asks "did
  something I wasn't tracking just finish?", which is an event. A level-checked fallback
  would see "idle + nothing tracked" the instant reading mode is enabled on a quiet project
  and clear it immediately — a click that visibly does nothing, and a direct contradiction
  of decision 7.

The fallback additionally requires **both** an empty accumulator for the project **and**
M2's per-project flush-in-flight marker to be clear. The marker is what stops the fallback
recreating the suppression race through its own branch: when the last outcome settles, the
flush starts and awaits notify, and the idle transition then fires on the next effect flush
with the accumulator already drained. Gating on emptiness alone would make correctness
depend on whether the flush clears its state before or after the await; the marker makes it
order-independent. Record that reason — it is the kind of guard a later reader removes as
redundant.

**Settings copy.** The "Also notify me about other projects while I'm using Switchboard"
preference now also governs the project you are in while reading mode is on. Update the
Settings copy, and the `notify_while_focused` doc comment in `crates/app/src/preferences.rs`
plus `should_deliver`'s doc comment in `notification.rs` — both currently assert that the
project on screen never notifies, which stops being true. Per the user's copy preference,
the Settings text should *explain* the case, not merely get shorter.

### Definition of Done

- Component tests (`ComposeBar`): compose box hidden with reading mode on; workflow
  progress view still rendered with reading mode on and a run active; ⌘Enter / ⌘1–9 / ⇧A
  are inert while hidden; no residual padded strip.
- State tests: reading mode is per-project (enabling for A does not affect B); cleared on
  project removal; not restored after a simulated reload.
- Ordering test — the one that matters most: with reading mode on and the app focused, a
  project going idle **delivers** the notification and *then* clears reading mode. Assert
  the observed **sequence** — that the `notify` IPC is issued before the
  `set_visible_project` write restoring the real id. A test that only asserts both happened
  passes against the broken ordering and is worthless here.
- Auto-off on the silent path: an all-cancelled project (no notification) still clears
  reading mode, via the flush rather than the fallback.
- Auto-off via the fallback: a project whose only activity was never registered (simulating
  post-reload turns) clears reading mode on the idle transition.
- Fallback does **not** fire while a flush is in flight — the guard that prevents the
  suppression race re-entering through that branch.
- Decision 7: enabling reading mode on an already-quiet project leaves it on (it does not
  self-clear), and the toggle renders its latched state.
- Browser test (`tests/browser/`, WebKit): the transcript holds its scroll anchor when the
  compose strip is removed and restored. Poll measured geometry per the existing
  convention — never a fixed sleep.
- `App.svelte` test: `visibleProjectId` resolves to `null` when reading mode is on for the
  active project, and the existing Settings / Git / loading cases still resolve to `null`.
- Settings copy, `preferences.rs` and `notification.rs` doc comments updated.
- `docs/system-design.md` §7: add reading mode to the user-facing model, and correct the
  notification description where it says the viewed project never notifies.
- Record the **entry race** as a known limitation, with its pre-existing framing so a later
  reader does not attribute it to this feature: enabling reading mode pushes the
  null-visibility write fire-and-forget, so a completion landing inside that single IPC
  round-trip is suppressed. Reviewed and deliberately not closed — the identical window
  already exists on every navigation into and out of a project (`App.svelte`'s
  `visibleProjectId` effect; the monotonic `seq` guard orders visibility writes against each
  other but not against `notify`). An arming protocol for reading mode alone would fix one
  instance of a general property of the visibility mechanism and leave the others. If it is
  ever worth closing, the fix belongs at that mechanism for all navigation paths. Practical
  cost is near zero: the user is holding the pointer and the result is on screen.

---

## Out of scope

Named so they are not quietly absorbed:

- Marking reading-mode projects in the projects sidebar. Possibly a good follow-up; not
  discussed as part of this.
- Any change to the green checkmark's active-project guard (decision 6).
- A per-agent notification option (the escape valve if the starvation tradeoff proves
  painful — deliberately not pre-built).
- Closing the visibility-sync entry race generally (see M3's known limitations). It is a
  property of `visible_project`'s fire-and-forget push, not of reading mode.
- Persisting reading mode, and any keyboard shortcut beyond the command-palette entry.
- Anything touching macOS delivery, signing, or the authorization gate.

## Verification

`make check` before opening a PR — it runs fmt, lint, the Rust and jsdom suites, and the
WebKit browser suite. Per `AGENTS.md`, run it in the **foreground** and block on it; if it
cannot fit one turn, split by package with every other flag byte-identical, and never
report a killed run as a passing one.
