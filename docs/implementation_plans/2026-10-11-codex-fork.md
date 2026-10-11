# Codex fork

Plan date 2026-10-11. Branch `codex-fork`. Read this whole document before starting.

## What this builds, and why it differs from Claude's fork

Switchboard can fork Claude Code agents today: the Fork half of the send button (⇧⌘↵) creates `<name>-fork`, a new agent that inherits the conversation so far, and sends your message to it. This plan adds fork for **Codex** agents in two places:

1. **The compose box, exactly as for Claude.** With one idle Codex agent selected, the Fork half sends your message to a new `<name>-fork`.
2. **A "Fork" item in a Codex agent card's "…" menu.** It creates `<name>-fork` in its own pane, idle, with the inherited conversation visible, and sends nothing. Claude cannot offer this: Claude refuses to fork without a prompt (probe 4 in `docs/implementation_plans/2026-08-10-claude-fork.md`).

**A Codex fork blocks the original agent for about a second, not for the fork's whole first turn.** That is the user-visible difference from Claude, and it follows from how each CLI forks:

- **Claude forks lazily.** The fork comes into existence during its first turn, and that turn reads the parent's session file. Switchboard cannot tell when Claude has finished reading, so it locks the parent's session file until the fork's first turn completes. A message to the parent in that time is refused: Switchboard retries the lock for about 0.15 seconds, then fails the turn with "Wait for that turn to finish, then resend."
- **Codex can fork eagerly.** `codex exec fork <parent>` with no prompt creates the fork in under a second and runs no turn. At creation Codex records a cut-off point in the parent's file. On every later fork turn it re-reads the parent's file up to that point and ignores the rest. Codex only ever appends to a session file, so the bytes before the cut-off never change, and only choosing the cut-off needs the parent idle. Switchboard locks the parent for that second, registers the fork, and then sends your message as the fork's first ordinary turn, which locks only the fork's own session.

## Terms

- **Harness**: an agent CLI Switchboard drives (Claude Code, Codex, Antigravity's `agy`).
- **Rollout**: Codex's session file, `~/.codex/sessions/YYYY/MM/DD/rollout-<timestamp>-<id>.jsonl`, one JSON record per line.
- **Session locator**: what Switchboard stores on an agent record to find its session. For Codex it is `SessionLocator::Codex { thread_id, partition_date }`, where `partition_date` is the `YYYY/MM/DD` folder (`crates/core/src/agent.rs`).
- **Lazy fork / eager fork**: a lazy fork exists only after its first turn (Claude). An eager fork exists as soon as it is created, before any turn (Codex, through a promptless `codex exec fork`).
- **Materialized**: Switchboard's word for a lazy fork whose own session file exists. The gates keyed on it (`busy_fork_source`, `unmaterialized_fork_parent`, the parent key in `session_lock_keys`) are lazy-fork machinery. An eager fork is materialized from the moment it is registered.
- **`history_base`**: the pointer a Codex fork's rollout stores instead of copying the parent's history (see the next section).
- **Session lock**: Switchboard's cross-process advisory lock on a harness session (`desktop/src-tauri/src/session_lock.rs`), so two Switchboard instances never run turns on one session at once.

## What the probes established

All verified on Codex 0.162.1 on 2026-10-11 and recorded in `docs/harness-behavior.md` §3.5 ("Codex: a fork is a pointer into the parent's file"). In short:

1. **A fork sees the parent's conversation.** The parent was told the code word `PELICAN-42`; the fork answered `PELICAN-42`.
2. **Forking never modifies the parent's file.**
3. **A fork needs no prompt.** `codex exec --json --skip-git-repo-check --dangerously-bypass-approvals-and-sandbox -C <dir> -m <model> fork <parent-id>` with stdin closed exits 0 in under a second and prints one event, `thread.started`, carrying the new id. Its rollout holds a `session_meta` and one `thread_settings_applied`, and no turn. A later ordinary `codex exec resume <fork-id> "<prompt>"` sees the parent's conversation.
4. **The parent's later turns never reach the fork.** The fork's `session_meta.history_base` is `{"thread_id": "<parent>", "end_ordinal_exclusive": 16, "end_byte_offset": 67325}`. Codex re-reads the parent's first 67,325 bytes (16 records) every time the fork runs, and ignores the rest.
5. **A fork of a fork points at its immediate parent.** `end_byte_offset` is how much of the immediate parent's own file to read. `end_ordinal_exclusive` counts records across the whole chain (42 = 16 inherited + 26 of the parent's own). The grandchild still answered `PELICAN-42`.

Codex's source defines the pointer (`HistoryPosition` and `SessionMeta.history_base` in `codex-rs/protocol/src/protocol.rs`). Two details from it matter here. `history_base.thread_id` is a **rollout id**, which equals the thread id except for a thread rolled back with Codex's "revert", whose file carries a distinct id. And Codex calls the prefix file immutable.

## Decisions

These are settled. Each is the planner's call, made in the 2026-10-11 discussion, and its reason must survive into code comments or commit messages.

- **Both entry points for Codex, the menu item for Codex only.** Milestone 4 (the menu item) is separable: if it is dropped, milestones 1 to 3 still ship the compose fork.
- **The compose fork creates the fork first, then sends.** It does not use `codex exec fork <parent> "<prompt>"` in one step. In the one-step form Codex picks the cut-off inside a turn, and Switchboard holds session locks for whole turns, so the parent would stay locked for the fork's whole first turn, which is exactly Claude's blocking behavior.
- **Claude's fork is unchanged.** It stays lazy and keeps blocking its parent during the fork's first turn. Shortening that would need a "Claude has finished reading the parent" signal nobody has found, and is out of scope.
- **No new field on `AgentRecord`.** A Codex fork is registered with an ordinary Codex locator and `forked_from_session: None`. The fork's own rollout records its parent (`forked_from_id`, `history_base`), and nothing in Switchboard needs lineage for an eager fork. The existing doc comments on `forked_from_session` and `HarnessKind::supports_session_fork` anticipated this ("an eager fork does not belong here"). Because the registry schema does not change, there is no stored-data compatibility question.
- **The fork shows its inherited history, as a Claude fork does.** A Claude fork's file contains copied parent records, which render as imported history in the fork's pane. A Codex fork's file does not, so the Codex loader follows `history_base` into the parent's file. Without this, a Codex fork's pane would look empty until its first reply.
- **A working agent cannot be forked.** The existing rule (refuse while the parent is mid-turn) applies to Codex. A fork created mid-turn would capture a half-finished turn, as the Claude probe showed.
- **Naming, settings and placement match Claude's fork.** The new agent is named `<parent>-fork` (then `-fork-2`, …) by the existing `derive_fork_name`, copies the parent's model and reasoning-effort selection, lives in the parent's project, and opens in its own pane.

## Open decisions

None that change the milestones.

## Required reading

Read these before writing code:

- `docs/harness-behavior.md` §3.5 (Claude's fork probes, then the Codex fork probes).
- `docs/implementation_plans/2026-08-10-claude-fork.md`: the Claude fork design, including why it is lazy, its gates and its known limitations. It is a historical plan: trust the code where they differ.
- `docs/system-design.md` §7 ("Sends and turns") and the Fork bullets in §9, plus resolved item 10.14 in §10.
- Codex's fork pointer: <https://github.com/openai/codex/blob/rust-v0.162.1/codex-rs/protocol/src/protocol.rs> (search `HistoryPosition` and `history_base`).
- The PR that added `codex exec fork`: <https://github.com/openai/codex/pull/37367>.
- `crates/harness/src/codex/account_usage.rs` module doc: the precedent for a Codex operation that is not a turn, and why it is a free function rather than a `HarnessAdapter` method.

## Milestone 1: create a Codex fork without a turn

### Goal and outcome

The harness crate can create a Codex fork of a given session in a given folder and return the new session's locator.

- Given a Codex parent session and a working folder, the harness crate runs a promptless `codex exec fork` and returns the new `thread_id` and `partition_date`.
- A failure (Codex missing, non-zero exit, no `thread.started`, no rollout written, a hang) comes back as a typed error that names what went wrong, and leaves no process running.
- The missing-parent probe is answered and recorded before milestone 2 words its marker.

### Implementation outline

1. **Run one prerequisite probe and record the result in `docs/harness-behavior.md` §3.5 first**, before writing code: **resume a fork whose parent file is missing.** Copy a throwaway parent's rollout aside, delete the original, resume the fork with a tiny prompt, then restore the file. Record whether Codex errors, or answers without the inherited context. The result decides what the user is told (milestone 2's marker and the README limitation):
   - If Codex errors, the fork's turns fail with Codex's error. Check that the error reads clearly as a failed turn in Switchboard.
   - If Codex answers without the context, the marker and the README must say the agent no longer remembers its earlier conversation, in those words.

   **Forking a legacy-mode parent is not probed, and does not need to be.** No local rollout declares `history_mode: "legacy"` (1,640 of 1,640 declare `paginated`, checked 2026-10-11), so there is nothing to fork. Milestone 2 handles both possible outcomes without knowing which happens: if Codex copies a legacy parent's records into the fork, the fork's own file shows them; if it writes a `history_base` instead, milestone 2 reads that segment through the legacy path. `docs/harness-behavior.md` §3.5 records this.
2. **Add a free function in `crates/harness/src/codex/`, not a `HarnessAdapter` method.** Creating a fork is not a turn: it holds no turn slot in the dispatcher, streams no content and writes no journal entry. That matches `account_usage.rs`, whose module doc explains the boundary; write the same reason into this module's doc.

   The contract (illustrative names):

   ```rust
   /// Create a fork of `parent` with no turn. This call is where Codex picks
   /// the fork's cut-off in the parent's rollout (`history_base`). Every later
   /// fork turn re-reads the parent up to that cut-off, but rollouts are
   /// append-only, so those bytes never change (harness-behavior §3.5). Only
   /// this call needs the parent idle, which is why the caller holds the
   /// parent's session lock for exactly this long.
   pub async fn fork_codex_session(
       binary: &Path,
       home_dir: &Path,
       cwd: &Path,
       parent: &CodexLocator,
       selection: &AgentSelection,
   ) -> Result<CodexLocator, CodexForkError>;
   ```

   - **Command shape (probed):** `exec --json --skip-git-repo-check --dangerously-bypass-approvals-and-sandbox -C <cwd> <selection flags> fork <parent-thread-id>`, with the flags **before** `fork` and stdin closed. Pass the same model and effort flags a dispatch would (reuse `push_selection_flags`), so the fork's recorded settings match the agent Switchboard registers.
   - **Result:** read stdout as JSON lines and take `thread.started.thread_id`. Reuse the existing parser for that event rather than a second one. Before returning, find the rollout with `find_codex_session_file_for_attach`, which scans every date folder and returns the file's date. Use that date as the locator's `partition_date`. Don't stamp today's date and look only there: a creation that crosses midnight would report "rollout not found" for a fork Codex did write. The scan also means Switchboard never registers a fork Codex did not write. The existing `-<id>.jsonl` match is enough here, because a new fork's thread id is also its rollout id, so its filename always has the single-id form (milestone 2 explains the other form).
   - **Bounded wait:** the probe took under a second. Give the call a fixed timeout (account_usage has a precedent) and kill the process on timeout or on drop.
   - **Errors:** a `thiserror` enum covering spawn failure, timeout, non-zero exit (with stderr), no `thread.started`, and rollout not found.
3. **Teach `fake_codex` the fork shape** so the app-layer tests in milestone 3 can run without a real Codex. A promptless fork passes no fixture path as its last argument, so use the `sh`-shim pattern `tests/codex_account_usage.rs` already uses. The fake must also write the rollout file the function checks for, into a test home directory.

### Definition of done

- Unit tests: `thread.started` extraction, including a malformed line and its absence.
- Fixture-driven integration tests with the shim: success returns the locator; the fake writes the rollout into a date folder other than today's, and the returned `partition_date` is that folder's; non-zero exit; exit 0 with no `thread.started`; exit 0 with no rollout written; timeout (the fake sleeps), with the child process gone afterwards.
- Live test `live_codex_fork_without_a_prompt_creates_an_idle_session_that_inherits_context` in `crates/harness/tests/live.rs`:
  1. Start a parent session told a code word.
  2. Call the new function, and assert the fork's rollout holds no `task_started`.
  3. Dispatch one turn to the fork through `CodexAdapter`, and assert the reply contains the code word.
- **Prove it:** run the live test, then open the fork's rollout and read its `session_meta.history_base`. Then run each failure test once against a deliberately broken shim and watch it fail.
- `docs/harness-behavior.md` §3.5 holds the missing-parent probe result, and the plan's marker wording (milestone 2) and README limitation (milestone 4) are updated to match it.

## Milestone 2: load a Codex fork's inherited history

### Goal and outcome

Loading a Codex fork's transcript shows the parent's conversation up to the fork point, then the fork's own turns.

- A Codex fork's pane shows the parent's history from before the fork, as a Claude fork's pane does.
- A fork of a fork shows the whole chain's history.
- The parent's messages after the fork never appear in the fork.
- A missing, shortened or circular parent chain shows the fork's own turns plus a line in the transcript saying the earlier conversation is unavailable and why. It never shows a partial or wrong history, and never fails the load.

### Implementation outline

1. **Follow `history_base` in the transcript loader** (`load_codex_transcript` / `parse_codex_transcript_content` in `crates/harness/src/codex/session_file.rs`). Today nothing in the codebase reads `history_base` or `forked_from_id`.
   - **Find the prefix file by rollout id, in both filename forms.** `history_base.thread_id` is a rollout id, and the fork's record does not say which date folder it is in. Codex names an ordinary rollout `rollout-<time>-<id>.jsonl`, where the one id is both thread id and rollout id. A thread rolled back with Codex's "revert" gets `rollout-<time>-<thread-id>_<rollout-id>.jsonl` (`codex-rs/rollout/src/rollout_file_name.rs` at `rust-v0.162.1`). The existing `-<id>.jsonl` suffix match misses the second form, so add a lookup that parses each name, splits the ids on `_`, and compares the rollout id. Keep the existing rule that more than one match is an error. Try the fork's own partition first, since most forks share their parent's day.
   - **Read exactly `end_byte_offset` bytes of the prefix file.** If the file is shorter, or the offset does not end on a line break, the prefix is not the one Codex recorded (Codex calls it immutable, so this means it was rewritten). Skip the inherited history and emit the marker in step 4. Don't render a guess.
   - **Recurse** when the prefix file's own `session_meta` carries a `history_base`, so a fork of a fork rebuilds the whole chain. Guard against cycles by rollout id, and stop at a fixed depth (for example 32) with the marker.
   - **Reconstruct each file's segment with that file's own `session_meta`**, then concatenate the turns: the oldest ancestor's first, the fork's own last. Don't splice raw records into one stream. Each file declares its own `history_mode`, and the reconstruction picks its ingest path from it, so a chain whose ancestor is a legacy-mode file reads that segment through the legacy path (see milestone 1 for why that case is unprobed).
2. **Leave enrichment alone.** `parse_session_content` (model, usage, context window, settings, environment) describes the agent's current state, so it reads only the fork's own file. A promptless fork's file has no `world_state` until its first turn, so its Environment popover is sparse until then, like any new agent.
3. **Inherited turns keep the parent's hydration keys**, as a Claude fork's copied records do. The project merge already keeps that safe: link maps are per agent, and `classify_agent_turns` declines a link to a send this agent never received. Inherited user prompts carry the parent's timestamps, which fall before the fork's first send, so they render as imported history (`UserImported`) in the fork's pane. Confirm this with a merge test, but no merge code should need to change.
4. **Show a missing inherited history as a marker in the transcript, not as a `LoadedTranscript` warning.** The frontend deliberately never renders those warnings: `workspace.svelte.ts` lets them "ride along inert", and `index.test.ts` ("does not promote aggregate parse warnings onto agent runtime") protects that. A warning there would leave the fork's earlier conversation silently missing, the failure gap G40 calls unacceptable. Instead:
   - Add an additive variant to `SystemMarker` (`crates/harness/src/transcript.rs`; the enum is `#[non_exhaustive]`), for example `InheritedHistoryUnavailable { reason }`. The loader emits it as a `Turn::System` stamped with the fork's own `session_meta` timestamp, so the project merge sorts it just before the fork's own turns. The compaction marker already travels this path and survives a reopen, so no new IPC field or frontend state is needed.
   - The reason tells apart: the parent file was not found; it was found but compressed (`rollout-*.jsonl.zst`, gap G40), which Codex can still read but Switchboard cannot; it is shorter than the recorded offset, or the offset does not end on a line break (rewritten); a cycle; the depth cap.
   - Mirror the variant in `desktop/src/lib/types.ts` and render it in the transcript. Word it from milestone 1's missing-parent probe, which found that Codex **refuses to run** a fork whose parent file is missing (exit 1, no events, `missing source rollout` on stderr) and runs it again once the file is restored. So for the "not found" reason the marker must say both halves: the earlier conversation can't be shown, and the agent can't continue until the original's Codex session file is back. For example: "Earlier conversation unavailable: `<parent>`'s Codex session file is missing. This agent can't reply until that file is restored." The fork's turns meanwhile fail with Codex's own error, verbatim, through the adapter's existing "stream ended without a terminal event" path; the marker above them is what explains it in plain words.
   - Keep `LoadedTranscript.warnings` for parser diagnostics, as today.

### Definition of done

- Fixture tests in `session_file.rs`. Build fixtures by trimming real probe rollouts, so the `history_base` shape is real. Cover:
  - a fork with a parent prefix: the parent's turns come first, then the fork's;
  - a parent file with turns past the offset: they are excluded;
  - a fork of a fork: three segments, in order;
  - a missing parent: own turns plus the marker with the "not found" reason;
  - a compressed parent (`.jsonl.zst` present): own turns plus the marker with the "compressed" reason;
  - a parent shorter than the offset: own turns plus the marker;
  - an offset that splits a line: own turns plus the marker;
  - a cycle, and a chain past the depth cap: own turns plus the marker;
  - a parent in the rolled-back filename form, whose filename's rollout id differs from its `session_meta.id`: found, and its turns come first;
  - a chain whose ancestor declares `history_mode: "legacy"`: that segment is read through the legacy path;
  - a promptless fork with no turns of its own: only inherited turns.
- A frontend component test: a fork whose transcript carries the marker renders its own turns plus the explanation.
- A test asserting that enrichment ignores the inherited records, for example the context window and model come from the fork's own records.
- A merge test in `commands.rs` beside `merge_forked_session_renders_inherited_history_as_imported`: a Codex fork's inherited prompts render as imported, and the parent and fork in one project don't claim each other's sends.
- Live test `live_codex_fork_loads_the_parents_history_up_to_the_fork_point`:
  1. Give a parent two turns, fork it, and give the fork one turn.
  2. Give the parent a third turn.
  3. Load the fork's transcript, and assert it shows the parent's first two prompts, then the fork's turn, and never the parent's third.
- **Prove it:** after the live test, print the loaded turns' user text in order and compare it with the three rollouts on disk.

## Milestone 3: fork a Codex agent from the compose box

### Goal and outcome

With one idle Codex agent selected, the Fork half of the send button creates `<name>-fork` and sends the message to it, and the original agent is free again within about a second.

- The Fork half (and ⇧⌘↵) is offered for a single idle Codex recipient with a session, exactly as for Claude. Antigravity is still refused, with new copy: today's message says Switchboard "can only branch Claude Code sessions", which becomes false.
- The new agent opens in its own pane showing the inherited conversation before its first reply arrives.
- While the fork's first turn runs, a message to the original agent starts at once instead of being refused.
- During the second the fork is being created, the compose bar is disabled (the compose fork already holds the project's compose claim). Any other message to the original in that second, such as a workflow step or a second Switchboard instance, is refused with "Wait for that turn to finish, then resend", the same policy Claude's fork applies for a whole turn.
- Forking a working Codex agent is refused, as for Claude.
- A failed fork creation registers no agent and shows the error. If creation succeeds and the first turn fails, the fork stays as an idle agent with the inherited history. That matches Claude, where a failed first turn also leaves the registered fork in place.

### Implementation outline

1. **Capabilities.** Keep `HarnessKind::supports_session_fork` meaning what it means today, a lazy fork at first turn (Claude only). Add a separate capability for "a fork can be created without a turn" (Codex only). The frontend needs two answers in `lib/harnessCapabilities.ts`: is the compose fork offered (Claude or Codex), and is the menu fork offered (Codex). Replace the hardcoded `candidate.harness !== "claude_code"` check in `ComposeBar.svelte`'s `forkShapeBlock` with the capability. Keep `AgentRecord::validate` and `register_agent_inner` rejecting `forked_from_session` on any harness that is not lazy-fork capable, so a Codex record never carries it.
2. **Core registration.** Add a `Project` method that registers a fork of a Codex source from a locator milestone 1 already created. It derives the name with `derive_fork_name`, copies the source's selection, writes the Codex locator, and leaves `forked_from_session` empty. Route it through `register_agent_inner`, like every other write. `register_attached_codex_agent` is the nearest precedent.
3. **The app command.** Give `fork_agent_impl` (`desktop/src-tauri/src/commands.rs`) a Codex arm, keeping the response shape the frontend already consumes.

   **Pass the Codex program path in.** `fork_agent_impl` takes `(state, source_agent_id, home_dir)` today, and every test `AppState` has `spawns_real_harnesses: false`. Add a `binary: &Path` parameter (to `fork_agent_impl`, or to a Codex-only helper it calls), as `read_codex_account_usage_impl` takes one. The Tauri command passes the production path; app tests pass milestone 1's shim with `with_real_harnesses(true)`. Follow `read_codex_account_usage_with`'s pattern for PATH readiness: production goes through a wrapper that waits on `ensure_path_settled`, and only tests inject a ready future. In mock mode (`spawns_real_harnesses` false), refuse the Codex fork with a plain message; a mock Codex agent has no rollout, so the session-file check would refuse it anyway.

   In this order:
   1. Read the project, the source and the project's lifecycle generation together with `capture_dispatch_snapshot`, not `lookup_agent`.
   2. Wait for PATH readiness (Codex arm only), **before** the checks and the lock. A slow login-shell PATH capture at app launch can take seconds. Waiting first means it never lengthens the time the parent is locked, and the idle check in step 3 reflects the parent's state just before locking rather than seconds earlier.
   3. Run the existing checks: the source's session file exists (`resolve_session_file`), and the source is idle (`ensure_fork_source_free`).
   4. Acquire the **parent's** session lock with `acquire_session_locks`, using the parent's Codex key. If it fails with `SessionInUse`, return `ForkSourceBusy { name }` instead. `SessionInUse`'s text ends "Wait for that turn to finish, then resend", which is written for a message and makes no sense for a fork; `ForkSourceBusy` names the busy agent and says it must stop changing before you can branch. This can happen when a workflow step or a second Switchboard instance starts a parent turn between step 3 and step 4.
   5. Call milestone 1's function.
   6. Release the lock.
   7. Under the `registry_write` lock: refuse if the project is under maintenance (`reject_if_under_maintenance`) or its generation changed (`reject_if_generation_changed`), and refuse if the source is no longer in the registry. Then register and update the agent cache, both inside that guarded section. Add a `TestSeam` variant (for example `ForkRegistration`, in `state.rs`) that pauses just before this step, with the existing two-way handshake, so tests can start a project deletion at exactly this point for either harness.

   Step 7 matters because creation awaits a subprocess. Deleting the project, or changing its folder, during that wait evicts the project and bumps its generation (`begin_maintenance`). Without the re-check, the fork would be written into a project being deleted and reported as created, then vanish, or linger in the agent cache for a project no longer loaded. Ordinary sends already guard the same pattern this way. Apply steps 1 and 7 to the whole function, so Claude's fork, whose wait is shorter but has the same shape, gets the fix too.

   **Why the lock covers only the creation (write this at the call site):** creation is where Codex picks the fork's cut-off in the parent's rollout (`history_base.end_byte_offset`). Every later fork turn re-reads the parent up to that cut-off (§3.5, probe 4), but rollouts are append-only, so those bytes never change, and only choosing the cut-off needs the parent idle. A parent turn that starts during creation is refused with `SessionInUse` after the lock's retries (about 155 ms), in this process or in another Switchboard instance, instead of racing the read. For Codex that makes the busy check an actual lock rather than only a check.

   Create the fork before registering it, so a failed creation leaves nothing in Switchboard. If registration fails after a successful creation, including the refusals in step 7, the stray Codex rollout is harmless: log a warning and return the error.
4. **The frontend flow stays the same**: fork, then send to the new agent through `dispatchToRecipients`, in `ComposeBar.svelte`'s `dispatchForkSend` and `dispatchPromptForkSend`. What changes is in `forkAgentIntoOwnPane` (`lib/state/workspace.svelte.ts`), branching explicitly on the "fork without a turn" capability:
   - **Don't arm the pending notice.** `forkAgentIntoOwnPane` arms `forkHistoryPending` ("this is a branch, its earlier conversation is still coming") for every fork today, unconditionally. Only the one-time refresh, `loadForkInheritedHistory`, checks `forked_from_session`. Without the explicit branch, a Codex fork would show "still coming" forever.
   - **Load the inherited history through the project merge, and finish before the first send.** Await `chainProjectLoad(projectId, () => hydrateProject(projectId, new Set([fork.id])))`, the same serialized path `loadForkInheritedHistory` uses (clear `hydrationStarted` for the project first, as it does). Not `hydrateAgent`: the per-agent loader returns raw user turns, which the project merge owns, so it would show every inherited message twice (`loadForkInheritedHistory`'s doc comment explains this). Awaiting it costs the first send one project read; not awaiting it would let that read overlap the fork's live first turn, which no existing path does.
   - **A failed load is not a failed fork.** Any outcome other than `"applied"` still returns the fork, so the compose flow sends as usual and the user never retries into a second fork. To retry the load, widen `loadForkInheritedHistory`'s gate (which returns early when `forked_from_session` is null) to also accept forks created eagerly this session, tracked in a set; the existing terminal hook then retries after the fork's first completed turn. After a restart the ordinary project load already includes the inherited history.
   - **A failed load is shown, with a Retry that uses the project merge.** The filtered `hydrateProject` call counts as a refresh, and a failed refresh only logs and keeps the old view (`workspace.svelte.ts`, the `catch` in `hydrateProject`). Left like that, the fork's pane is silently empty until the hook's retry, and a menu fork (milestone 4) sends no turn, so for it the retry never comes. That breaks the rule set when gap G19 closed: every history-loading failure is shown and can be retried. So when the fork's load returns anything other than `"applied"`, set the fork's runtime to `hydration_status: "failed"` with the error. The existing per-agent failure display ("Couldn't load `<agent>`'s history", Retry, Details) then renders. Retry for a fork in the eager-fork set must run the same serialized `chainProjectLoad` → `hydrateProject` load, not `retryAgentHydration` (`index.svelte.ts`), which goes through the per-agent loader and would show inherited messages twice. A later successful load, from Retry or from the first-turn hook, clears the failure.
5. **Update the docs this milestone changes:**
   - `docs/harness-behavior.md` §3.5: the status line and Codex row move from unwired to built.
   - The capability watchlist row "Fork | Codex" in `docs/harness-behavior.md` §8: mark it built.
   - `docs/system-design.md`: the §7 Fork paragraph (it says Fork is hidden for non-Claude recipients and not wired for other harnesses, and describes the parent-lock rule for Claude only), the §9 Fork bullet and matrix row, and resolved item 10.14. In the §7 paragraph, the sentence saying the parent lock "costs the parent one turn's wait" becomes: a message to the parent is refused, for the fork's whole first turn with Claude and only during creation with Codex.

### Definition of done

- Core tests:
  - registering a Codex fork gets the derived name and copies the selection;
  - it carries no provenance;
  - it refuses a non-Codex source.
- App tests in `commands.rs`, using the milestone 1 fake. Cover:
  - a Codex fork registers and returns the new agent;
  - forking a working Codex agent is refused;
  - forking a source with no session file is refused;
  - a failed creation registers nothing;
  - while the parent's session lock is held by a contender, the fork is refused with `ForkSourceBusy` naming the parent, not `SessionInUse`;
  - the reverse: while creation is held open (the shim sleeps), a parent turn is refused with `SessionInUse`; after creation returns, a parent turn succeeds while the fork's first turn is still running;
  - project deletion that begins at the `ForkRegistration` seam, once for a Codex fork and once for a Claude fork (step 7 changes both): the fork is refused (`ProjectUnderMaintenance` or `ProjectViewStale`), and nothing is registered or cached;
  - mock mode refuses a Codex fork with the plain message;
  - the source's own project is used, not the active one;
  - a later turn to the fork locks only the fork's own session key.
- Frontend tests:
  - `ComposeBar.test.ts`: the Fork half is offered for one idle Codex recipient and refused for Antigravity, with the new copy naming Claude Code and Codex; ⇧⌘↵ works for Codex.
  - `workspace.test.ts`: an eager fork loads its history through the project path before the send, and arms no pending notice; two overlapping fork creations followed by a project refresh show each inherited message once; a failed history load leaves the fork registered and marked failed, and the compose flow still sends its message; Retry on that failure goes through the project load, shows each inherited message once, and calls the fork command no second time.
- Live test in `desktop/src-tauri` (named `live_codex_…`):
  1. Fork a Codex agent through `fork_agent_impl`.
  2. Send the fork a turn, and assert it knows the parent's code word.
  3. While that turn is still running, start a turn on the parent, and assert it is not refused for a held session.
- **Prove it in the app** (`make dev`):
  1. Fork a Codex agent from the compose box with a prompt that takes a while.
  2. Immediately send the original agent a message, and confirm both stream at the same time.
  3. Confirm the fork's pane showed the inherited conversation before its reply.
  4. Reopen the project, and confirm the fork still shows it.

## Milestone 4: a "Fork" item on Codex agent cards

### Goal and outcome

A Codex agent's "…" menu has a Fork item that creates an idle fork without sending anything.

- The item appears on Codex agent cards only, not on Claude or Antigravity cards.
- Choosing it creates `<name>-fork` in its own pane, idle, showing the inherited conversation. The original agent is not blocked afterwards.
- It is unavailable while the agent is working or has no session, and says why, following how the menu's existing items handle the same states.
- It is also unavailable, and says why, while another compose operation in the project holds the compose claim (below). A send with a saved prompt can hold that claim indefinitely while it waits for a browser sign-in.
- If the fork is created but its earlier conversation fails to load, the fork shows the existing history-load error with Retry (milestone 3, step 4).
- A failure shows the error and creates no agent.

### Implementation outline

Small milestone. Add the item to the agent card menu in `lib/components/Sidebar.svelte`, gated by the "fork without a turn" capability from milestone 3. Place it beside "Compact context", and follow that item's handling of a busy agent and of errors. It calls the same `fork_agent` command and `forkAgentIntoOwnPane` as the compose path, minus the send.

**Take the project's compose claim while the fork is being created**, as the compose fork does (`beginOperation` in `lib/state/composeOperations.svelte.ts`; add a kind for it). Without the claim, a compose send to the original typed during that second is refused with "resend" (milestone 3 explains the lock); with it, the compose bar is disabled for that second instead.

**When the claim is already held, disable the item with a reason.** `beginOperation` returns null while any compose operation is in flight in the project, and a send waiting on a browser sign-in holds it with no time limit until the user clicks "Stop waiting". Without this rule, the obvious code returns on null and the click does nothing, with no message. While `operationFor(projectId)` is set, show the item disabled with a tooltip such as "Another send in this project is still in progress." Keep the null check after `beginOperation` as a backstop for a claim taken between render and click, showing the same message.

Update `docs/system-design.md` §7, whose agent-row paragraph says Fork is "deliberately **not** a row action" because a Claude fork can only exist as a turn. State that Codex has the row action, and why Claude does not.

### Definition of done

- Frontend tests in `Sidebar.test.ts`:
  - the item shows for a Codex agent and not for Claude or Antigravity;
  - it is unavailable while the agent is working;
  - choosing it creates the fork and its pane, and dispatches no turn;
  - compose sends are disabled while the fork is being created, and enabled again afterwards, on success and on failure;
  - with another compose operation in flight, the item is disabled with its reason, and clicking it calls no fork command;
  - a failed fork creation shows the error;
  - a fork whose history load fails shows the history-load error; Retry succeeds without sending a turn, inherited messages appear once, and the fork command was called exactly once.
- **Prove it in the app:** fork a Codex agent from the menu, confirm the new agent is idle with the inherited conversation, then send the original agent a message and confirm it runs at once.
- `README.md`: update the "Fork a conversation" feature bullet, the Fork how-to section, and the "Fork is currently Claude Code only" limitation, so they describe both harnesses and both entry points.

## Known limitations to record

Record these in `docs/harness-behavior.md` §3.5 and, where a user would notice, in the README's "Harness support and limitations".

- **A Codex fork depends on its parent's rollout for as long as it exists.** Codex re-reads the parent up to the fork's cut-off on every fork turn. If that file is unavailable to Switchboard, Switchboard shows the marker from milestone 2 instead of the inherited history. What Codex does depends on why, and only one case is proven:
  - **Deleted (probed):** Codex refuses to run the fork. Every turn fails until the file is restored, and works again afterwards.
  - **Compressed (`rollout-*.jsonl.zst`, gap G40):** Codex can still read it, so the fork keeps working. Only Switchboard can't show the inherited history.
  - **Moved or rewritten in place (`codex migrate-rollouts --apply`):** unprobed.

  The README limitation covers the proven case only, in those words: a Codex fork stops working if its original's Codex session file is deleted, and works again if the file is put back. Switchboard never deletes harness files: deleting an agent leaves its rollout in place (`delete_agent_sidecars`), so deleting the parent agent is safe.
- **A parent and its fork in the same pane show the shared history twice**, as with Claude. The fork opens in its own pane to avoid it.
- **A message to the parent during the second a fork is being created is refused**, for example a workflow step or a send from a second Switchboard instance. The user resends after the fork exists. Claude's fork refuses the same messages for its whole first turn.
