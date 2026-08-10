# Fork agent (Claude Code) — 2026-08-10

> Revised 2026-08-10 after two-agent review + adjudication. Material changes from
> the first draft: the no-busy-parent-gate decision is replaced by a co-send
> validation rule (no dispatch-layer gate); inherited history appears via a
> one-shot project-merge refresh at first materialization (not only on reopen);
> fork placement gets an explicit pane rule; the roster-update flow is corrected
> to the real frontend-driven sequence; the eligibility gate routes through
> `resolve_session_file`; a raw fork-file parser fixture and a cancel-path live
> test are added; two probes are now prerequisites.
>
> **Revised again 2026-08-10 (post-M2), redesigning the trigger: Fork is a
> send-time compose option (a chip), not an agent-menu action.** Driven by a new
> probe result — Claude has no copy-a-session operation and refuses a promptless
> fork, so a branch can only come into existence *as a turn*. Fork-on-send makes
> the UI isomorphic to that: the send that carries the option registers the
> branch and dispatches its first turn as one action. This deletes the co-send
> validation rule (the hazardous state is now unrepresentable), collapses the
> busy-parent gate to the chip's enabled state, and removes the `…`-menu item
> and its disabled-tooltip primitive question. M1/M2 (core + adapter) are
> untouched by the redesign — they were built against record state, not the
> trigger. Superseded text below is edited in place; this note is the changelog.

## What this is

Add a **Fork** option to the compose bar: with a single Claude agent "X" selected
as recipient, enabling the Fork chip makes that send branch X's conversation into
a new, equally first-class agent (`X-fork`) — the message is dispatched as the
branch's first turn, carrying X's full history. New turns on the fork don't
affect X; new turns on X don't affect the fork. Claude Code only in v1 — this
ships the capability that system-design §9 and harness-behavior §3.5 specified
long ago but no milestone ever built.

**Scope guard.** Codex fork via `codex app-server` `thread/fork` and Gemini's lossy
`--session-file` import are *documented* (harness-behavior §3.5, re-probed
2026-08-10) but **out of scope** — do not build adapter plumbing, trait methods, or
UI states "for when Codex lands." The only cross-harness artifacts are the
capability gate in M1 and the chip's disabled-with-tooltip treatment in M3.

## Required reading (before implementing)

- `docs/harness-behavior.md` §3.5 — fork mechanics per harness, incl. the 2026-08-10
  re-probe results this plan builds on, and the `/fork` vs `/branch` vs
  `--fork-session` naming trap.
- `docs/system-design.md` §9 ("Fork from checkpoint" matrix row + note) and resolved
  question 10.14.
- `docs/research/same-session-parallel-invocation.md` — why session-id uniqueness is
  app-enforced; needed to reason about the fork dispatch reading the parent's session.
- `docs/implementation_plans/2026-05-31-session-identity-into-registry.md` — why
  session identity lives on `AgentRecord` and how Claude pre-generates it.
- `docs/implementation_plans/2026-07-06-durable-send-turn-correlation.md` and
  `2026-06-21-transcript-merge-prompt-provenance.md` — the key-join + positional
  merge the forked transcript's rendering depends on.
- `docs/implementation_plans/2026-05-29-agent-autocreate-rename-remove.md` — existing
  registration/name-derivation conventions.
- The hydrate-merge scope comment in `src/lib/state/reducers.ts` (search "Scope:
  keyed AGENT turns only") — user turns are journal-overlay-owned and must never be
  re-merged into a per-agent slice. The M3 refresh design depends on this.
- Claude Code CLI reference: https://code.claude.com/docs/en/cli-reference (the
  `--fork-session`, `--resume`, `--session-id` flags).

## Probe evidence (2026-08-10, claude 2.1.226)

These findings are load-bearing for the design; they supersede parts of the
2026-06-17 (@2.1.172) probe recorded in harness-behavior §3.5 (that doc was updated
alongside this plan).

1. **The forked session's ID is caller-controlled.**
   `claude -p --resume <parent-uuid> --session-id <new-uuid> --fork-session -- "<prompt>"`
   completes the turn in a **new** session with exactly `<new-uuid>`, carrying the
   parent's full context (verified with a remember-the-secret-word probe). The CLI
   enforces the trio: `--session-id` with `--resume` but *without* `--fork-session`
   errors with "`--session-id can only be used with --continue or --resume if
   --fork-session is also specified`". Consequence: **no session-capture machinery is
   needed** — a fork uses the same pre-generated-UUID path as normal Claude
   registration (`Project::register_agent`).
2. **The forked session file is a full structural copy of the parent** at
   `~/.claude/projects/<encoded-cwd>/<new-uuid>.jsonl`: every parent record is
   copied with its record type, message `uuid`/`parentUuid`, `promptSource: "sdk"`,
   and **original timestamps** preserved (only the per-record `sessionId` field is
   rewritten to the new UUID), then the fork's own new turn is appended. The forked
   file also gains a leading `mode` record the parent lacks (skipped by
   `ingest_record`'s type dispatch — pinned by the M3 fixture). The parent file is
   not modified by the fork.
3. **The parent file was only read** by the fork dispatch in this probe — parent
   record count and bytes unchanged. **Scope caveat:** the probe ran against an
   *idle* parent. It says nothing about reading the file while another Claude
   process is appending to it — that is Probe A below, and no design claim in this
   plan may lean on #3 beyond the idle case.
4. **A fork cannot be materialized without a real prompt.** `--fork-session` is a
   modifier on a resume, not a copy command: both `-- ""` and omitting the prompt
   are refused ("`Error: … Provide a prompt to continue the conversation.`") and
   **no child file is created**. There is no copy-a-session operation in the CLI.
   This is the ground truth behind the fork-on-send UI (see the second revision
   note): a branch can only come into existence *as* a turn, so the honest UI
   couples forking to a send rather than presenting a create-then-message flow.

### Prerequisite probes (run before or during M2; record results in harness-behavior §3.5)

**Both probes are done (2026-08-10 @ 2.1.226); results recorded in
harness-behavior.md §3.5.** Summary, because two design claims rest on them:

- **Probe A — read-during-write: files stay intact; the snapshot gains a
  synthesized in-window reply.** Forking while the parent is mid-turn succeeds,
  inherits context correctly, and corrupts neither file (no unparseable lines in
  either). For the parent's in-flight turn — prompt on disk, answer still
  streaming — Claude copies the prompt (parent's timestamp preserved) and
  **synthesizes an assistant record reading `"No response requested."`**. Two
  measured properties of that record drive the merge analysis (artifact:
  `9bfebaf7…` in the probe project): it is **stamped at copy time** (so it lands
  *inside* the fork's own live window, not its pre-journal history), and its
  `message.id` is a **bare UUID** — a hydration key that never existed in the
  parent and never gets a `TurnLink`. The user-level harm: the branch
  permanently lacks the parent's answer to its last question, with a truthful
  "unanswered" stub in its place, in both the transcript and the model's
  context. The merge-level hazard (send-stealing) needs **three** conditions at
  once: the parent's turn starts *after* the fork's `Send` is journaled (so the
  copied prompt is in-window and consumes), **and** the fork's first turn fails
  before its `TurnLink` lands — the artifact itself is the *benign* ordering
  (copied prompt out-of-window → renders imported → clears `pending_send`, so
  the synthesized turn pairs with nothing). With fork-on-send, the fork's send
  and dispatch are one action, so a parent turn cannot start between them
  through Switchboard; the chip is disabled while the parent is mid-turn, and
  the sub-second spawn-window residual is documented under Known limitations.
- **Probe B — interrupted materialization: a partial child file was never
  observed.** Across 22 `killpg` runs (0.5 s–8 s) against parents of 40 records
  / 35 KB and 31 records / 160 KB, on APFS at CLI 2.1.226, the child file was
  only ever **absent** or **content-complete** — never partial, never a torn
  line, with the transition observed as a single step. **This is a bounded
  observation, not a guarantee** — it cannot distinguish one atomic write from
  a create-then-append faster than the 0.5 s sampling, and says nothing about
  I/O-error conditions. The decision: **no partial-file guard is built**; retry
  self-healing is a documented dependency on this upstream behavior
  (harness-behavior §3.5 records the falsifier for re-probes: a child that
  parses cleanly but whose *conversation content* is a strict prefix of the
  parent's — raw record counts are a false signal, since a complete child
  legitimately carries fewer records than its parent).

## Design decisions (settled in discussion + review — do not re-derive)

- **Fork is a send, because a fork *is* a send (probe #4).** The Fork chip in the
  compose bar, enabled with exactly one Claude recipient selected, makes that
  send the branch point: on submit, Switchboard registers `X-fork`
  (`Project::fork_agent`), places it, moves the compose selection to it, and
  dispatches the message as its first turn — which carries
  `--resume <parent-session> --session-id <own-session> --fork-session`. There
  is no create-then-message flow and no "unmaterialized fork" the user can
  observe: creation and materialization are one action. (An earlier draft made
  Fork an agent-menu action that registered an empty agent whose first later
  send forked — that separation is what created the co-send hazard and the
  two-place busy gate, both deleted by this design.) If the fork's first turn
  *fails* before Claude writes the file, the agent legitimately exists with no
  session; the next send to it re-forks automatically (see the next bullet), so
  the registered-but-unmaterialized state still exists *transiently on
  failure* — just never as a resting state the user is expected to act on.
  Inherited history is loaded by the one-shot refresh (M3) when the first turn
  terminates — **not** by waiting for a project reopen.
- **Fork-vs-resume is derived from session-file existence, not a consumed flag.**
  The Claude adapter already picks `--session-id` (file absent) vs `--resume` (file
  present) by checking the session file on disk. The fork path extends that same
  derivation: fork-source present + own file absent → fork args; own file present →
  plain `--resume <own>`. If Claude died before creating the new file, the next
  send retries the fork; nothing to persist or roll back. This rationale must
  survive into a code comment at the arg-building site. **Scope of the
  self-healing claim:** Probe B settled this — a killed first dispatch leaves the
  child file either absent (re-forks) or content-complete (resumes with full
  inherited history), never partial, so the claim holds for every reachable
  state.
- **The fork source is permanent provenance on the registry record**, never cleared
  after the first dispatch (it becomes inert once the file exists). Field semantics:
  "the parent *session* UUID to `--resume` from if this agent's own session file
  doesn't exist yet." It stores the parent's **session** UUID, not the parent's
  agent id — self-contained, and survives the parent agent being deleted before the
  fork's first send (Switchboard never deletes harness session files, so the fork
  still works; if the file is somehow gone, the dispatch fails as a normal
  self-explaining failed turn).
- **The busy-parent gate is the chip's enabled state — no dispatch-layer
  machinery, no co-send rule.** History, so nobody re-litigates it: an earlier
  draft gated the fork's materializing dispatch on the parent being idle
  (rejected: unbounded queue starvation behind a backlogged parent; delivers the
  parent's answer to the very turn the user was branching away from); a
  session-reservation subsystem was rejected as heavier than the harm; a co-send
  validation rule ("one send must not target a parent and its unmaterialized
  fork") existed solely because fork-creation and first-send were separate
  actions. **Fork-on-send deletes the co-send hazard outright** — the chip
  requires a single recipient and the branch doesn't exist until the send
  resolves, so the pair can never be co-recipients. What remains:
  - **Chip disabled while the selected parent is mid-turn**, with the tooltip
    saying why in probe-measured terms ("X is working — its current answer
    wouldn't be included; wait or cancel first"). Re-validated at the fork-send
    command (compose-then-send window; also covers non-compose callers), which
    refuses with the same explanation — the workflow-collision posture, not
    queueing.
  - **Failure-retry sends** to a fork whose first turn failed are ordinary sends
    to that agent and get the same treatment: the materializing dispatch is
    refused while the parent is mid-turn, at the same command-boundary check.
  - **Residual (accepted, documented):** the parent can start a turn from a
    non-Switchboard writer (bare CLI — already a discouraged pattern) or within
    the sub-second window between the busy check and claude's file read.
    Consequence per Probe A: no corruption; the branch inherits the in-flight
    prompt with a synthesized `"No response requested."` stub. The three-condition
    merge edge is pinned by a characterization test filed alongside the existing
    positional-edge tests in `classify_turns_by_count`'s suite.
  Record this rationale at the validation site.
- **Naming.** Agent names must match `^[A-Za-z0-9_-]+$` (`core/src/name.rs`), so
  "X (forked)" from the original discussion is invalid. Derive `<parent>-fork`,
  disambiguating collisions with a numeric suffix (`<parent>-fork-2`, `-fork-3`, …)
  against the project's canonicalized-uniqueness rule. The user can rename after.
- **Precondition: the parent has a resumable session file** —
  `resolve_session_file(...)` returns `Some` (this is deliberately *not* "at least
  one completed turn": a cancelled first turn also leaves a resumable file, and
  forking it is well-defined — the fork inherits whatever is on disk). UI copy must
  say what the gate actually is, e.g. "this agent has no session to branch from
  yet," not "needs a completed turn." The check lives at the **app layer**
  (`resolve_session_file` is the documented per-harness path authority — do not
  add a third copy of path-resolution logic), and the sidebar already carries the
  same signal as `sessionInfo.session_file` (existence-filtered), so the frontend
  gate needs no new IPC.
- **Model/effort are copied from the parent** at fork time (they're per-agent
  selections the fork should inherit; user can change them after, as with any agent).
- **Placement: the fork gets its own visible track; the parent stays put.** The
  default pane layout puts the whole roster in one pane, and the merge emits one
  globally sorted stream — so with no placement rule, the fork's inherited
  history (parent timestamps preserved) interleaves with the parent's originals
  as adjacent duplicates; in a customized layout the fork would instead land
  pane-less and invisible. On fork-send: try
  `assignAgentToFirstVisibleEmptyPane` first (reuse a pane the user prepared),
  fall back to `moveAgentToNewPane` (which already degrades to starting
  minimized when the row is full). **X is neither moved nor replaced** — it may
  be mid-someone-else's-turn-of-thought; only the *compose selection* swaps (X
  deselected, `X-fork` selected, chip reset to off), so the next message goes to
  the branch. Roster position: append to the end — adjacency to the parent isn't
  worth new ordering machinery; manual reorder exists. Same-pane duplication
  remains reachable only by the user explicitly co-paning parent and fork — a
  Known limitation, not the default experience.
- **First-turn placeholder.** Until the first turn terminates and the refresh
  lands, the fork's pane shows only the new message and streaming reply; then
  the inherited history fills in above. Show a small notice in the interim
  ("Branched from X — inherited history appears when this turn completes") so
  the fill-in reads as designed behavior, not a glitch. Copy educates per
  convention.

## Why the transcript side is (verifiably) safe

No merge/rendering changes are needed, but this is subtle enough that M3 must pin
it with tests rather than trust it:

- The copied history's records keep the **parent's timestamps** and
  `promptSource: "sdk"`. In `merge_project_conversation`, sends, links, and
  `journal_start` are all **per-agent** (`agent_sends`, `agent_links`,
  `journal_start` maps keyed by `AgentId` — see `crates/app/src/commands.rs`). The
  forked agent's `journal_start` is its own first send's timestamp, which is
  later than every copied record — so copied `sdk` prompts fail the
  `started_at >= journal_start` window and render as `UserImported` (the existing
  "adopted an external session" path), never consuming the fork's own sends.
  (The one boundary case where a copied record can be in-window — the parent's
  turn starting inside the busy-gate residual — is bounded by the chip gate and
  pinned by the characterization test above. The mid-turn *synthesized* turn is
  its own shape: in-window, keyed, link-less — covered by the dedicated fixture
  in M3's DoD.)
- Second, independent defense: the fork's own sends are key-joined to their turns
  via `TurnLink`s, so positional consumption never reaches them.
- **Cross-agent hydration-key duplication is new but safe.** Copied turns carry the
  same `hydration_key`s (first assistant `message.id`) as the parent's turns. The
  link maps are per-agent, so the parent's `TurnLink`s can't claim the fork's
  copies. The "load-bearing axiom" comment in the `TurnLink` merge arm
  (`commands.rs`, ~line 4678) currently states keys are unique *per agent* —
  forking makes cross-agent duplication a normal occurrence; extend that comment to
  say the per-agent scoping is what makes this safe.
- **Pins are unaffected.** Pin keys are agent-scoped
  (`agent:hydration:<agent_id>:<key>`, `src/lib/messageIdentity.ts`), so a copied
  record can't collide with a parent pin; imported prompts are unpinnable by
  design.

---

## M1 — Core: fork capability gate, registry field, `Project::fork_agent`

### Goal & Outcome

Core can represent and create a forked agent.

- `AgentRecord` carries optional fork provenance; old registry files load unchanged.
- `Project::fork_agent(<source>)` produces a valid new Claude agent record: fresh
  UUID-v7 session locator, derived unique name, parent's model/effort, fork source
  set to the parent's session UUID.
- Non-Claude harnesses are rejected by a capability gate that a future harness
  cannot silently inherit.

### Implementation Outline

- Add `HarnessKind::supports_session_fork()` following the exact pattern of
  `supports_model_selection` (exhaustive match, no `_` arm, rationale comment) —
  `true` only for `ClaudeCode`. Name it to avoid the `/fork`-vs-branch trap
  documented in harness-behavior §3.5.
- Add the fork-source field to `AgentRecord` with wire key
  `forked_from_session: Option<Uuid>` (pinned — it's the registry.jsonl format).
  Serde posture: plain `Option` like `model`/`effort` (missing key → `None`,
  backward compatible), **not** the fail-loud custom deserializer used for
  `session_locator` — a pre-fork record legitimately lacks the concept. Serialize
  explicitly as `null` when unset, matching the record's existing self-describing
  posture. Document on the field: semantics (see Design decisions), permanence
  (never cleared; inert once the session file exists), and that only
  `supports_session_fork` harnesses ever carry `Some`.
- Add `Project::fork_agent(source_agent_id) -> Result<AgentRecord>`: load the
  source record; reject if its harness fails `supports_session_fork` or it has no
  session locator (typed errors via the crate's existing error enum); derive the
  `-fork`/`-fork-N` name against `check_name_unique`; append a record with a fresh
  `Uuid::now_v7()` locator (same rationale as `register_agent`'s Claude arm),
  copied `model`/`effort`, and `forked_from_session = source.session_locator`'s
  UUID. Reuse the registration flow's structure rather than inventing a parallel
  append path.

### Definition of Done

Unit tests in core:

- Round-trip serialization with `forked_from_session` set and unset; a record
  missing the key deserializes to `None` (backward compat); unset serializes as
  explicit `null`.
- `fork_agent` happy path: new distinct id + locator, name derived, model/effort
  copied, fork source = parent session UUID, record appended after existing rows.
- Name collision walks to `-fork-2` (and again to `-fork-3`).
- Fork of a non-Claude agent and of a missing agent id → typed errors.
- `supports_session_fork` truth table (exhaustive-match compile guard is the real
  protection; the test documents intent).

## M2 — Claude adapter: fork-aware first dispatch (+ probes + live tests)

### Goal & Outcome

Dispatching to a forked agent executes the fork on the first turn and behaves like
a normal agent thereafter.

- First send to a forked agent spawns
  `claude -p … --resume <parent> --session-id <own> --fork-session … -- <prompt>`.
- Every later send (own session file exists) is an ordinary `--resume <own>` —
  fork flags never reappear.
- A first dispatch that died before the file was created retries the fork on the
  next send, automatically.
- Probes A and B are run and their results recorded in harness-behavior §3.5
  (Probe B's outcome may escalate a mitigation decision — see Prerequisite probes).

### Implementation Outline

- Extend `claude_code::build_args`: when the agent has `forked_from_session:
  Some(parent)` **and** the existing session-file existence check says the agent's
  own file is absent, emit `--resume <parent> --session-id <own> --fork-session`
  instead of the current absent-file branch's bare `--session-id <own>`. The
  present-file branch is unchanged (fork source ignored). Defensive: locator absent
  + fork source set shouldn't be constructible (M1 always sets the locator), but if
  hit, behave as today (no session flags) rather than panicking.
- Respect the existing flag-ordering constraint (everything before the `--`
  end-of-options separator) — the comment in `build_args` already warns about this.
- Carry the design-decision rationale (file-existence-derived fork-vs-resume ⇒
  idempotent retry scoped to the file-absent case; CLI-enforced flag trio, with the
  exact error string from the probe) into the comment at this branch.

### Definition of Done

- Unit tests alongside the existing `build_args` suite: fork args on first turn
  (file absent); plain `--resume <own>` once the file exists even with fork source
  set (the "retry-safety + never-refork" contract); fork flags absent for a
  non-forked agent; model/effort still ride the fork dispatch.
- Live tests in `crates/harness/tests/live.rs`, named per the load-bearing
  convention (`live_claude_fork_…`), `#[ignore]`-gated for `make test-live-claude`,
  tiny constrained prompts per the cost discipline in AGENTS.md:
  - **Happy path:** seed a parent session with a secret-word prompt (asserting
    the seed completed, so auth/quota failures surface at the seed), fork-dispatch
    through the adapter's real arg path, assert (a) the reply proves inherited
    context, (b) the session file exists at the pre-generated fork UUID — the
    file-at-our-id check, which proves the operational contract more directly
    than a raw-stream `session_id` assertion, (c) the parent session file is
    byte-identical before/after, and (d) loading **both** parent and child
    through `load_claude_transcript` shows the parent's prompts *and* its keyed
    agent turns present in the child with identical `hydration_key` and
    timestamps. Raw-record lineage (`parentUuid` chains, `promptSource`) is
    normalized away by the loader and is deliberately **assigned to M3's raw
    fixture**, not this test.
  - **Cancel path:** one live test that (1) proves the cancel genuinely landed —
    the cancelled stream must contain **no `TurnEnd`** (the adapter's cancel
    contract is to end without a terminal; a completed turn means the test
    proved nothing and must fail loudly, with the fix being a longer prompt) —
    and (2) asserts the state-independent recovery invariant: the next send
    lands a fork carrying the parent's context, whichever of the two states
    (absent → re-fork, complete → resume) the cancel produced. Deterministic
    per-state coverage lives in the `build_args` unit tests; a tiny parent
    cannot exercise the size-sensitive copy window (that was Probe B's job).
    The long cancellable prompt is a documented exception to the cost
    discipline.
- Probes A and B run; results written into harness-behavior §3.5 (bounded
  observations, with falsifiers — not guarantees; see Probe evidence above).
- Fail-closed dispatch guard: fork provenance without a `Uuid` locator is
  refused before spawning (`DispatchError::InvalidAgentState`) — spawning would
  let claude mint an untracked session id and silently break continuity.
  Unreachable via core APIs; guards a corrupted registry.

## M3 — Fork-on-send: compose chip, command, one-shot refresh, tests, docs

### Goal & Outcome

The feature is usable end to end and its transcript behavior is pinned.

- With a single Claude agent selected in compose, a **Fork chip** (left of
  Forward, `git-branch`-style icon) can be enabled; sending then creates
  `<name>-fork` in the roster, places it (empty pane, else new pane; the parent
  stays put), moves the compose selection to the fork, and dispatches the
  message as the branch's first turn.
- The chip explains itself via tooltip in every disabled state: non-Claude
  recipient, multiple recipients, no session to branch from yet, parent
  mid-turn.
- The fork's reply arrives with inherited context, and the inherited history
  appears in the fork's pane **when that first turn terminates** — no project
  reopen required. Until then the pane carries the "Branched from X" notice.
- Users can find the Claude-only limitation explained in the README.

### Implementation Outline

- **Backend command** per the thin-shim convention: a `fork_agent_impl(agent_id)`
  free function that resolves the **agent's own project** via lookup under the
  `registry_write` lock (deliberately *not* `active_project_id`), validates
  (`supports_session_fork`; `resolve_session_file(...).is_some()`; **parent not
  mid-turn** — the send-time half of the busy gate, same busy signal the
  dispatcher already tracks), calls `Project::fork_agent`, updates the
  `agents_by_id` cache, and returns the record (like `create_agent_impl`: no
  event emitted — roster updates are frontend-driven). Typed errors with
  gate-accurate copy (see Design decisions); record the busy-gate rationale at
  the validation site.
- **Frontend fork-send flow** (mirrors the real create/attach sequence — there
  is no backend roster event to reuse): on submit with the chip enabled, the
  workspace-store operation awaits `api.forkAgent`, calls `registerAgent(record)`
  (runtime initialized before subscribing — that ordering is load-bearing),
  appends the record to its `project_id`'s roster, applies the placement rule
  (`assignAgentToFirstVisibleEmptyPane`, else `moveAgentToNewPane`), swaps the
  compose selection (parent out, fork in, chip off), then routes the message
  through the **normal send path** to the new agent — reusing journaling,
  queueing, and dispatch unchanged. No fused backend fork-and-send command: the
  ms-scale window between registration and dispatch is inside the documented
  spawn-window residual, and the normal send path is where every send invariant
  already lives. A failed `forkAgent` leaves compose untouched with the error
  surfaced; a failed *dispatch* leaves a registered fork with no session, and
  the next send to it retries the fork through the same command-boundary busy
  check.
- **Chip enablement** (compose): visible for a single-recipient selection;
  enabled only when that recipient is Claude (`harness`), has
  `sessionInfo?.session_file` (the existing existence-filtered signal — no new
  IPC), and is not mid-turn (the same busy state the compose bar already knows
  for queueing). Disabled states carry educational tooltips per the copy
  conventions — the non-Claude tooltip is derived from
  `SessionForkUnsupported`'s wording (Switchboard's support, not vendor
  capability). Tooltip-on-disabled-chip is the standard wrapper pattern; no
  primitive verification task (that concern was specific to Radix menu items).
- **TS mirror** (landed in M1 follow-up): `forked_from_session` on
  `AgentRecord` — the refresh trigger needs the provenance frontend-side.
- **One-shot inherited-history refresh.** Contract: exactly one successful
  inherited-history load per fork — triggered from the fork's terminal event
  once its session file has materialized; never spent on a terminal that didn't
  materialize the file; never generalized into refresh-on-every-terminal (the
  refresh filter in `hydrateProject`'s docs and the project's transcript-dup
  history are why). **Seam (load-bearing):** the refresh must go through the
  project conversation merge — re-run `load_project_conversation`, replace the
  journal overlay wholesale (dup-safe by design), and apply the fork's agent
  turns through the existing project-scoped application path
  (`applyAgentHydrate`), where keyed dedup collapses the live first reply
  against its disk copy by `hydration_key`. Do **not** route through
  `retryAgentHydration` / the per-agent loader: user turns are
  journal-overlay-owned (the hydrate reducer's scope comment) and a per-agent
  reload would duplicate the copied prompts and the fork's own live prompt. The
  enabling ordering contract — the dispatcher writes the `TurnLink` before the
  terminal event propagates — must be pinned by a test, not assumed.
- **First-turn placeholder** in the fork's pane per Design decisions.
- Extend the "load-bearing axiom" comment in the `TurnLink` merge arm
  (`commands.rs` ~4678) for cross-agent key duplication, per "Why the transcript
  side is safe" above.

### Definition of Done

- Rust tests for `fork_agent_impl`: success; parent-file-missing rejection
  (asserted through `resolve_session_file`); **parent-mid-turn rejection**;
  non-Claude rejection; missing agent/project errors; project resolved from the
  agent, not the active project.
- **Parser-boundary fixture test**: a sanitized raw forked-session JSONL pair
  (source: the 2026-08-10 probe artifacts) driven through
  `load_claude_transcript` — asserts copied records keep original timestamps and
  `promptSource:"sdk"`, hydration keys duplicate the parent's, the leading
  `mode` record is skipped, **raw-record lineage is preserved** (`parentUuid`
  chains — the loader normalizes this away, so only the raw fixture can assert
  it), and imported-vs-own turn boundaries land correctly. Re-point at least one
  merge-safety test at this loader output instead of hand-built turns.
- **Merge-safety tests** (app-layer): forked-shape transcript + the forked
  agent's journal (first send + `TurnLink`):
  - copied prompts render imported — they consume no send;
  - the fork's first send pairs with its own reply via key-join;
  - the parent agent's own rendering is unaffected by the fork's presence;
  - failed-first-turn shape (send + `Outcome`, no file contribution) renders
    the outcome marker only;
  - **mid-turn-fork fixture — must synthesize the *hazardous* ordering, not
    copy the probe artifact** (the artifact is the benign ordering: its copied
    prompt is out-of-window and clears `pending_send`). Shape: copied history
    + a copied parent prompt timestamped *after* the fork's `Send` + the
    synthesized in-window/keyed/link-less agent turn + the fork's own linked
    reply. Asserts **desired** behavior, not current: the fork's send pairs
    with its real reply; the synthesized turn never claims it — including in
    the failed-pre-`TurnLink` variant, where the user's failed send must still
    render correctly. If the merge fails this today, that is a real bug to fix
    in M3, not a characterization to enshrine.
- **Refresh tests** (component-level, mock `invoke`/`listen` per testing
  conventions): after the fork's first terminal, inherited imported turns appear
  and the live reply renders exactly once (the hydration-key dedup pin); both
  orderings of terminal-vs-refresh resolution; the trigger fires at most once
  and not on a non-materializing terminal; `TurnLink`-visible-at-refresh pinned.
- **Fork-send flow tests** (store-level, not only a mocked compose click): API →
  `registerAgent` → roster append → placement → selection swap → dispatch
  sequence, including the fast-first-event ordering; placement lands in an empty
  pane when one exists, otherwise a new pane, parent's pane unchanged; failed
  `forkAgent` leaves compose state untouched; failed first dispatch leaves a
  registered fork whose next send retries the fork.
- Frontend component tests: chip gating for all states (single Claude with
  session → enabled; non-Claude / multi-recipient / no-session / parent-busy →
  disabled with the right tooltip), and the command-error path surfacing the
  message.
- Docs: README "Harness support and limitations" entry (user-facing,
  symptom-first: fork exists on Claude Code agents only, via the compose Fork
  option; one or two plain lines); harness-behavior §3.5 "Switchboard
  implication" refreshed if the implemented shape deviates; memory/plan docs
  already carry the redesign.
- `make check` green; `make test-live-claude` run before the PR (adapter-touching
  change, per AGENTS.md live-test policy).

## Known limitations (accepted, not deferred bugs)

- The fork's inherited history appears when its first turn terminates (one-shot
  refresh), not instantly at send — the interim shows the "Branched from X"
  notice. A CLI constraint, not a choice: a branch can only materialize as a
  turn (probe #4).
- You cannot branch an agent while it is mid-turn (chip disabled; command
  refuses). Escape hatch: cancel the parent, then fork — cancelling leaves a
  clean session to branch from. Rationale: a mid-turn snapshot inherits a
  synthesized `"No response requested."` stub in place of the parent's in-flight
  answer (Probe A), permanently, in both transcript and model context.
- The busy gate has a sub-second residual (non-Switchboard writers; the window
  between the busy check and claude's file read). Consequence per Probe A: no
  corruption; the stub turn above. The merge's behavior under the worst ordering
  is pinned by the M3 mid-turn fixture.
- You cannot fan-out one message to X and a not-yet-created branch of X in a
  single send (the chip takes a single recipient); send to the branch first,
  then fan-out freely.
- Retry self-healing after an interrupted first dispatch depends on Claude's
  observed absent-or-complete child-file behavior — a bounded observation at
  2.1.226, not a guarantee (falsifier documented in harness-behavior §3.5;
  re-probe on CLI bumps).
- Co-paning a fork with its parent shows the shared history twice (interleaved at
  identical timestamps) — reachable only by explicit user layout choice; the
  default placement avoids it.
- Codex/Gemini/Antigravity have no fork option in v1 (Codex app-server route and
  Gemini `--session-file` documented in harness-behavior §3.5 for the future).
