# Fork agent (Claude Code) — 2026-08-10

> Revised 2026-08-10 after two-agent review + adjudication. Material changes from
> the first draft: the no-busy-parent-gate decision is replaced by a co-send
> validation rule (no dispatch-layer gate); inherited history appears via a
> one-shot project-merge refresh at first materialization (not only on reopen);
> fork placement gets an explicit pane rule; the roster-update flow is corrected
> to the real frontend-driven sequence; the eligibility gate routes through
> `resolve_session_file`; a raw fork-file parser fixture and a cancel-path live
> test are added; two probes are now prerequisites.

## What this is

Add a **Fork** action to the agent `…` menu: forking agent "X" creates a new, equally
first-class agent in the same project whose conversation branches from X's current
state. New turns on the fork don't affect X; new turns on X don't affect the fork.
Claude Code only in v1 — this ships the capability that system-design §9 and
harness-behavior §3.5 specified long ago but no milestone ever built.

**Scope guard.** Codex fork via `codex app-server` `thread/fork` and Gemini's lossy
`--session-file` import are *documented* (harness-behavior §3.5, re-probed
2026-08-10) but **out of scope** — do not build adapter plumbing, trait methods, or
UI states "for when Codex lands." The only cross-harness artifacts are the
capability gate in M1 and the disabled-with-explanation menu treatment in M3.

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

### Prerequisite probes (run before or during M2; record results in harness-behavior §3.5)

- **Probe A — read-during-write.** Fork while the parent is mid-turn (give the
  parent a deliberately long-running prompt). Inspect both files: does the fork's
  copy end cleanly at the last complete record, does Claude error on a torn
  trailing line, or something else? This documents the failure mode of the
  snapshot-boundary race that remains after the co-send rule (see Design
  decisions) — it does not gate the design, it bounds the residual.
- **Probe B — interrupted materialization.** Kill (`killpg`, mirroring cancel) the
  fork's first dispatch mid-turn on a *long* parent transcript. Inspect the child
  file: absent, complete copy, or truncated copy? This decides whether the
  self-healing claim below needs a mitigation. If a truncated child file is
  reachable, **stop and escalate** the mitigation choice (accept-and-document vs.
  guard) with the probe evidence — do not pick one unilaterally.

## Design decisions (settled in discussion + review — do not re-derive)

- **Lazy fork.** The Fork action only *registers* the new agent. The fork itself
  executes on the user's **first send** to the forked agent — that dispatch carries
  `--resume <parent-session> --session-id <own-session> --fork-session`. Chosen over
  an eager fork-at-click (which would need a synthetic prompt, costing quota and
  polluting both transcripts with a throwaway turn). The forked agent's transcript
  is empty until its first send; when the first turn terminates and the session
  file has materialized, the inherited history is loaded by the one-shot refresh
  (M3) — **not** by waiting for a project reopen.
- **Fork-vs-resume is derived from session-file existence, not a consumed flag.**
  The Claude adapter already picks `--session-id` (file absent) vs `--resume` (file
  present) by checking the session file on disk. The fork path extends that same
  derivation: fork-source present + own file absent → fork args; own file present →
  plain `--resume <own>`. If Claude died before creating the new file, the next
  send retries the fork; nothing to persist or roll back. This rationale must
  survive into a code comment at the arg-building site. **Scope of the
  self-healing claim:** it covers the file-absent case only. A *truncated* child
  file (killed mid-copy) would be "present" and resume silently shortened — whether
  that state is reachable is exactly Probe B; do not extend the claim until the
  probe answers.
- **The fork source is permanent provenance on the registry record**, never cleared
  after the first dispatch (it becomes inert once the file exists). Field semantics:
  "the parent *session* UUID to `--resume` from if this agent's own session file
  doesn't exist yet." It stores the parent's **session** UUID, not the parent's
  agent id — self-contained, and survives the parent agent being deleted before the
  fork's first send (Switchboard never deletes harness session files, so the fork
  still works; if the file is somehow gone, the dispatch fails as a normal
  self-explaining failed turn).
- **No dispatch-layer parent gate; instead, a co-send rule.** An earlier draft
  gated the fork's materializing dispatch on the parent being idle. Rejected in
  review, on the merits: (a) it silently changes fan-out semantics — the fork
  would inherit the parent's *answer* to the very prompt it's about to be asked;
  (b) it would be the first cross-agent blocking dependency in a dispatcher whose
  concurrency model is strictly per-agent; (c) it still left a spawn-window race
  open. (A stronger whole-turn session reservation was also rejected: it blocks
  the parent for the fork's entire first turn.) The replacement:
  - **Validation rule (unconditional):** one send must not target both an agent
    and its still-**unmaterialized** fork. Generic pairwise form: recipients X, Y
    where `Y.forked_from_session == X`'s locator UUID and Y's own session file is
    absent → refuse the send with a clear, educational error (the
    workflow-collision posture, not queueing). Enforced at the dispatch-command
    boundary so every source (compose, workflow, forward) hits it; the compose UI
    additionally prevents the co-selection upstream with the same explanation.
    Once the fork is materialized, fan-out to both is unrestricted. Rationale:
    without this, the parent's copy of the shared prompt lands in the fork's
    snapshot on a coin flip, and the user sees their own prompt twice in the
    fork's transcript.
  - **Residual (accepted, documented):** separate rapid-fire sends (fork, then
    parent — or forking while a parent turn is already in flight) can still
    overlap the parent's writes with the fork's spawn-time read. Semantics:
    "fork captures the parent's history as of the first send to the forked
    agent"; an in-flight parent turn contributes only whatever completed records
    are on disk. Torn-line behavior is documented by Probe A. The compound merge
    edge (a copied parent prompt timestamped ≥ the fork's `journal_start` while
    the fork's first turn fails before its `TurnLink`) is pinned by a
    characterization test filed alongside the existing positional-edge tests in
    `classify_turns_by_count`'s suite — the same treatment as the two documented
    positional edges.
  Record this rationale (both the rejection of the gate and the co-send rule) in
  a comment at the validation site.
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
- **Placement: the fork gets its own visible track.** The default pane layout puts
  the whole roster in one pane, and the merge emits one globally sorted stream —
  so with no placement rule, the fork's inherited history (parent timestamps
  preserved) interleaves with the parent's originals as adjacent duplicates; in a
  customized layout the fork would instead land pane-less and invisible. On fork:
  try `assignAgentToFirstVisibleEmptyPane` first (reuse a pane the user prepared),
  fall back to `moveAgentToNewPane` (which already degrades to starting minimized
  when the row is full). Roster position: append to the end — adjacency to the
  parent isn't worth new ordering machinery; manual reorder exists. Same-pane
  duplication remains reachable only by the user explicitly co-paning parent and
  fork — a Known limitation, not the default experience.

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
  (The one boundary case where a copied record can be in-window — the co-send /
  rapid-fire race — is handled by the validation rule and the characterization
  test above.)
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
  - **Happy path:** seed a parent session with a secret-word prompt,
    fork-dispatch through the adapter's real arg path, assert (a) the reply proves
    inherited context, (b) the stream's `session_id` equals the pre-generated fork
    UUID, (c) the parent session file's record count is unchanged, and (d) loading
    the child JSONL shows inherited turns with preserved timestamps, provenance,
    and hydration keys (the parser-boundary half of the M3 fixture, against the
    live CLI).
  - **Cancel path:** cancel a fork's first dispatch after the child file appears;
    assert the next dispatch resumes and the inherited context survives (exact
    assertion depends on Probe B's answer — full context if the copy is atomic,
    the documented degraded behavior otherwise).
- Probes A and B run; results written into harness-behavior §3.5.

## M3 — App command, send validation, UI, one-shot refresh, tests, docs

### Goal & Outcome

The feature is usable end to end and its transcript behavior is pinned.

- Agent `…` menu on a Claude agent with a resumable session shows **Fork**;
  clicking it creates `<name>-fork` in the roster immediately, placed in an empty
  pane if one is visible, otherwise its own new pane.
- Non-Claude agents see the action with an explanation of the gap (tooltip if the
  primitive supports it, otherwise a muted sublabel — see below); a Claude agent
  with no session file cannot trigger it.
- Sending to the forked agent produces a reply with inherited context, and the
  inherited history appears in the fork's pane **when that first turn terminates**
  — no project reopen required.
- A send targeting both a parent and its unmaterialized fork is refused with a
  clear explanation (and prevented upstream in the compose UI).
- Users can find the Claude-only limitation explained in the README.

### Implementation Outline

- **Backend command** per the thin-shim convention: a `fork_agent_impl(agent_id)`
  free function that resolves the **agent's own project** via lookup under the
  `registry_write` lock (deliberately *not* `active_project_id` — the fork command
  receives an agent id and must not assume the active project), validates
  (`supports_session_fork`; `resolve_session_file(...).is_some()`), calls
  `Project::fork_agent`, updates the `agents_by_id` cache, and returns the record
  (the same contract as `create_agent_impl`: no event is emitted — roster updates
  are frontend-driven, see below). Typed errors; the no-session case must tell the
  user *why* in gate-accurate copy (see Design decisions). Record the
  no-dispatch-gate + co-send-rule rationale at the validation site.
- **Co-send validation** at the dispatch-command boundary (the choke point every
  send source flows through): reject a send whose recipient set contains a pair
  (X, Y) with `Y.forked_from_session == X`'s locator UUID and Y's session file
  absent. Educational error message. The compose UI prevents the co-selection
  upstream with the same explanation.
- **Frontend creation flow** (mirrors the real create/attach sequence — there is
  no backend roster event to reuse): `api.forkAgent` + a workspace-store
  `forkAgent` operation that awaits the command, calls `registerAgent(record)`
  (which initializes the runtime before subscribing — that ordering is
  load-bearing), appends the record to its `project_id`'s roster, then applies
  the placement rule (`assignAgentToFirstVisibleEmptyPane`, falling back to
  `moveAgentToNewPane`).
- **TS mirror:** add `forked_from_session` to the frontend `AgentRecord` type
  (wire-format mirror per the project's serde↔TS convention). The refresh trigger
  and the co-send UI gate both need the provenance frontend-side.
- **Menu item** in the Sidebar agent actions menu. Enabled for Claude agents with
  `sessionInfo?.session_file` present (the same existence-filtered signal the
  Open-session-file item uses — no new IPC). For non-Claude harnesses the item is
  disabled with an explanation (system-design §9's educational treatment;
  precedent for disabled-not-hidden exists in this menu — the Move up/down items).
  **Verify the primitive first**: disabled menu items in the bits-ui/Radix model
  typically suppress pointer events, so a tooltip wrapper may never fire. If it
  doesn't, the sanctioned fallback is the explanation as a muted sublabel in the
  item itself (e.g. "Claude Code only"), and system-design §9's wording must be
  synced to describe the shipped treatment. For a Claude agent with no session
  file, follow the menu's existing availability convention.
- **One-shot inherited-history refresh.** Contract: exactly one successful
  inherited-history load per fork — triggered from the fork's terminal event once
  its session file has materialized; never spent on a terminal that didn't
  materialize the file; never generalized into refresh-on-every-terminal (the
  refresh filter in `hydrateProject`'s docs and the project's transcript-dup
  history are why). **Seam (load-bearing):** the refresh must go through the
  project conversation merge — re-run `load_project_conversation`, replace the
  journal overlay wholesale (it is dup-safe by design), and apply the fork's
  agent turns through the existing project-scoped application path
  (`applyAgentHydrate`), where keyed dedup collapses the live first reply against
  its disk copy by `hydration_key`. Do **not** route through `retryAgentHydration`
  / the per-agent loader: the hydrate reducer's scope comment is explicit that
  user turns are journal-overlay-owned and must not be re-merged into a per-agent
  slice — a per-agent reload would duplicate the copied prompts and the fork's own
  live prompt. The enabling ordering contract — the dispatcher writes the
  `TurnLink` before the terminal event propagates, so a terminal-triggered refresh
  sees the durable join — must be pinned by a test, not assumed.
- Extend the "load-bearing axiom" comment in the `TurnLink` merge arm
  (`commands.rs` ~4678) for cross-agent key duplication, per "Why the transcript
  side is safe" above.

### Definition of Done

- Rust tests for `fork_agent_impl`: success; parent-file-missing rejection
  (asserted through `resolve_session_file`); non-Claude rejection; missing
  agent/project errors; project resolved from the agent, not the active project.
- Rust tests for the co-send validation: parent + unmaterialized fork refused
  (compose-shaped and workflow-shaped sends); parent + materialized fork allowed;
  unrelated agents unaffected.
- **Parser-boundary fixture test**: a sanitized raw forked-session JSONL pair
  (source: the 2026-08-10 probe artifacts) driven through `load_claude_transcript`
  — asserts copied records keep original timestamps and `promptSource:"sdk"`,
  hydration keys duplicate the parent's, the leading `mode` record is skipped, and
  imported-vs-own turn boundaries land correctly. Re-point at least one
  merge-safety test at this loader output instead of hand-built turns.
- **Merge-safety tests** (app-layer): forked-shape transcript + the forked agent's
  journal (first send + `TurnLink`):
  - copied prompts render imported — they consume no send;
  - the fork's first send pairs with its own reply via key-join;
  - the parent agent's own rendering is unaffected by the fork's presence;
  - pre-first-send (no session file, no sends): forked agent contributes nothing;
  - **characterization test** for the compound edge (copied in-window `sdk`
    prompt + fork's first turn failed pre-`TurnLink`), filed with the existing
    positional-edge tests — documents the accepted degraded behavior.
- **Refresh tests** (component-level, mock `invoke`/`listen` per testing
  conventions): after the fork's first terminal, inherited imported turns appear
  and the live reply renders exactly once (the hydration-key dedup pin); both
  orderings of terminal-vs-refresh resolution; the trigger fires at most once and
  not on a non-materializing terminal; `TurnLink`-visible-at-refresh pinned.
- **Creation-flow tests** (store-level, not only a mocked Sidebar click): API →
  `registerAgent` → roster append sequence, including the fast-first-event
  ordering (an event arriving immediately after the command resolves finds a
  runtime and listener); placement lands in an empty pane when one exists,
  otherwise a new pane, parent's pane unchanged. Optional: a characterization
  test of co-pane duplication (documents the accepted user-chosen state).
- Frontend component tests: menu gating for all three states (Claude-with-session,
  Claude-without, non-Claude explanation), successful fork updating the roster,
  and the command-error path surfacing the message.
- Docs: README "Harness support and limitations" entry (user-facing, symptom-first:
  fork exists on Claude Code agents only; one or two plain lines); harness-behavior
  §3.5 "Switchboard implication" refreshed if the implemented command shape
  deviates; system-design §9 tooltip wording synced if the sublabel fallback was
  used; Probe A/B results recorded in harness-behavior §3.5.
- `make check` green; `make test-live-claude` run before the PR (adapter-touching
  change, per AGENTS.md live-test policy).

## Known limitations (accepted, not deferred bugs)

- Forked agent's transcript is empty until its first send (lazy fork). Inherited
  history appears when the first turn terminates (one-shot refresh), not at
  fork-click.
- Fork snapshot is "parent history on disk at first send," not at fork-click. A
  parent turn completing in between is included. Forking while a parent turn is
  in flight (or rapid-fire sends to fork then parent) snapshots only the parent's
  completed records, with the boundary behavior documented by Probe A; the
  fan-out case is prevented by the co-send rule until the fork materializes.
- Co-paning a fork with its parent shows the shared history twice (interleaved at
  identical timestamps) — reachable only by explicit user layout choice; the
  default placement avoids it.
- Codex/Gemini/Antigravity have no fork action in v1 (Codex app-server route and
  Gemini `--session-file` documented in harness-behavior §3.5 for the future).
