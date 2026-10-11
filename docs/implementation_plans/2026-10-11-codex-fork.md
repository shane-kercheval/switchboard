# Codex fork

Plan date 2026-10-11. Branch `codex-fork`. Read this whole document before starting.

## What this builds, and why it differs from Claude's fork

Switchboard can fork Claude Code agents today: the Fork half of the send button (⇧⌘↵) creates `<name>-fork`, a new agent that inherits the conversation so far, and sends your message to it. This plan adds fork for **Codex** agents in two places:

1. **The compose box, exactly as for Claude.** With one idle Codex agent selected, the Fork half sends your message to a new `<name>-fork`.
2. **A "Fork" item in a Codex agent card's "…" menu.** It creates `<name>-fork` in its own pane, idle, with the inherited conversation visible, and sends nothing. Claude cannot offer this: Claude refuses to fork without a prompt (probe 4 in `docs/implementation_plans/2026-08-10-claude-fork.md`).

**A Codex fork blocks the original agent for about a second, not for the fork's whole first turn.** That is the user-visible difference from Claude, and it follows from how each CLI forks:

- **Claude forks lazily.** The fork comes into existence during its first turn, and that turn reads the parent's session file. Switchboard cannot tell when Claude has finished reading, so it locks the parent's session file until the fork's first turn completes. A message to the parent waits until then.
- **Codex can fork eagerly.** `codex exec fork <parent>` with no prompt creates the fork in under a second and runs no turn. Codex reads the parent's file only while creating the fork. Switchboard locks the parent for that second, registers the fork, and then sends your message as the fork's first ordinary turn, which locks only the fork's own session.

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
- **The compose fork creates the fork first, then sends.** It does not use `codex exec fork <parent> "<prompt>"` in one step. The one-step form reads the parent's file during a turn, so Switchboard would have to lock the parent for that whole turn, which is exactly Claude's blocking behavior.
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
- The open questions from the probes are answered and recorded before milestone 2 depends on them.

### Implementation outline

1. **Run three prerequisite probes and record the results in `docs/harness-behavior.md` §3.5 first**, before writing code. Each costs nothing or one tiny turn on a throwaway session.
   - **Fork a legacy-mode rollout.** `history_base` is documented for paginated rollouts. Fork one of the local July 2026 rollouts written with `history_mode: "legacy"` (promptless, no model call). Record whether the fork gets a `history_base` or copies the parent's records. Milestone 2 has to handle whichever it is.
   - **Resume a fork whose parent file is missing.** Copy a throwaway parent's rollout aside, delete the original, resume the fork, then restore the file. Record whether Codex errors, or answers without the inherited context. This decides how milestone 2 words its warning.
   - **Check where a promptless fork's rollout lands.** Confirm its partition folder is the local date at creation, so stamping `chrono::Local::now().date_naive()` is right. That is the same rule the first-turn capture uses in `codex/mod.rs::run_producer`, which accepts the same edge at midnight.
2. **Add a free function in `crates/harness/src/codex/`, not a `HarnessAdapter` method.** Creating a fork is not a turn: it holds no turn slot in the dispatcher, streams no content and writes no journal entry. That matches `account_usage.rs`, whose module doc explains the boundary; write the same reason into this module's doc.

   The contract (illustrative names):

   ```rust
   /// Create a fork of `parent` with no turn. Codex reads the parent's rollout
   /// only during this call (harness-behavior §3.5), which is why the caller
   /// holds the parent's session lock for exactly this long.
   pub async fn fork_codex_session(
       binary: &Path,
       cwd: &Path,
       parent: &CodexLocator,
       selection: &AgentSelection,
   ) -> Result<CodexLocator, CodexForkError>;
   ```

   - **Command shape (probed):** `exec --json --skip-git-repo-check --dangerously-bypass-approvals-and-sandbox -C <cwd> <selection flags> fork <parent-thread-id>`, with the flags **before** `fork` and stdin closed. Pass the same model and effort flags a dispatch would (reuse `push_selection_flags`), so the fork's recorded settings match the agent Switchboard registers.
   - **Result:** read stdout as JSON lines and take `thread.started.thread_id`. Reuse the existing parser for that event rather than a second one. Before returning, confirm the rollout exists with `locate_session_file`, so Switchboard never registers a fork Codex did not write.
   - **Bounded wait:** the probe took under a second. Give the call a fixed timeout (account_usage has a precedent) and kill the process on timeout or on drop.
   - **Errors:** a `thiserror` enum covering spawn failure, timeout, non-zero exit (with stderr), no `thread.started`, and rollout not found.
3. **Teach `fake_codex` the fork shape** so the app-layer tests in milestone 3 can run without a real Codex. A promptless fork passes no fixture path as its last argument, so use the `sh`-shim pattern `tests/codex_account_usage.rs` already uses. The fake must also write the rollout file the function checks for, into a test home directory.

### Definition of done

- Unit tests: `thread.started` extraction, including a malformed line and its absence.
- Fixture-driven integration tests with the shim: success returns the locator; non-zero exit; exit 0 with no `thread.started`; exit 0 with no rollout written; timeout (the fake sleeps), with the child process gone afterwards.
- Live test `live_codex_fork_without_a_prompt_creates_an_idle_session_that_inherits_context` in `crates/harness/tests/live.rs`:
  1. Start a parent session told a code word.
  2. Call the new function, and assert the fork's rollout holds no `task_started`.
  3. Dispatch one turn to the fork through `CodexAdapter`, and assert the reply contains the code word.
- **Prove it:** run the live test, then open the fork's rollout and read its `session_meta.history_base`. Then run each failure test once against a deliberately broken shim and watch it fail.
- `docs/harness-behavior.md` §3.5 holds the three probe results.

## Milestone 2: load a Codex fork's inherited history

### Goal and outcome

Loading a Codex fork's transcript shows the parent's conversation up to the fork point, then the fork's own turns.

- A Codex fork's pane shows the parent's history from before the fork, as a Claude fork's pane does.
- A fork of a fork shows the whole chain's history.
- The parent's messages after the fork never appear in the fork.
- A missing, shortened or circular parent chain shows the fork's own turns plus a warning. It never shows a partial or wrong history, and never fails the load.

### Implementation outline

1. **Follow `history_base` in the transcript loader** (`load_codex_transcript` / `parse_codex_transcript_content` in `crates/harness/src/codex/session_file.rs`). Today nothing in the codebase reads `history_base` or `forked_from_id`.
   - **Find the prefix file by rollout id.** `history_base.thread_id` is a rollout id, and the fork's record does not say which date folder it is in. Search the partitions for `rollout-*-<id>.jsonl`, as `find_codex_session_file_for_attach` already does. Try the fork's own partition first, since most forks share their parent's day.
   - **Read exactly `end_byte_offset` bytes of the prefix file.** If the file is shorter, or the offset does not end on a line break, the prefix is not the one Codex recorded (Codex calls it immutable, so this means it was rewritten). Skip the inherited history and warn. Don't render a guess.
   - **Recurse** when the prefix file's own `session_meta` carries a `history_base`, so a fork of a fork rebuilds the whole chain. Guard against cycles by rollout id, and stop at a fixed depth (for example 32) with a warning.
   - **Reconstruct each file's segment with that file's own `session_meta`**, then concatenate the turns: the oldest ancestor's first, the fork's own last. Don't splice raw records into one stream. Each file declares its own `history_mode`, and the reconstruction picks its ingest path from it. Milestone 1's legacy-mode probe tells you whether a chain can mix modes.
2. **Leave enrichment alone.** `parse_session_content` (model, usage, context window, settings, environment) describes the agent's current state, so it reads only the fork's own file. A promptless fork's file has no `world_state` until its first turn, so its Environment popover is sparse until then, like any new agent.
3. **Inherited turns keep the parent's hydration keys**, as a Claude fork's copied records do. The project merge already keeps that safe: link maps are per agent, and `classify_agent_turns` declines a link to a send this agent never received. Inherited user prompts carry the parent's timestamps, which fall before the fork's first send, so they render as imported history (`UserImported`) in the fork's pane. Confirm this with a merge test, but no merge code should need to change.
4. **Surface warnings through the existing `LoadedTranscript` warnings**, worded from milestone 1's missing-parent probe.

### Definition of done

- Fixture tests in `session_file.rs`. Build fixtures by trimming real probe rollouts, so the `history_base` shape is real. Cover:
  - a fork with a parent prefix: the parent's turns come first, then the fork's;
  - a parent file with turns past the offset: they are excluded;
  - a fork of a fork: three segments, in order;
  - a missing parent: own turns plus a warning;
  - a parent shorter than the offset: own turns plus a warning;
  - an offset that splits a line: own turns plus a warning;
  - a cycle, and a chain past the depth cap: own turns plus a warning;
  - a promptless fork with no turns of its own: only inherited turns.
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

- The Fork half (and ⇧⌘↵) is offered for a single idle Codex recipient with a session, exactly as for Claude. Antigravity is still refused, with its current message.
- The new agent opens in its own pane showing the inherited conversation before its first reply arrives.
- While the fork's first turn runs, a message to the original agent starts at once instead of waiting.
- Forking a working Codex agent is refused, as for Claude.
- A failed fork creation registers no agent and shows the error. If creation succeeds and the first turn fails, the fork stays as an idle agent with the inherited history. That matches Claude, where a failed first turn also leaves the registered fork in place.

### Implementation outline

1. **Capabilities.** Keep `HarnessKind::supports_session_fork` meaning what it means today, a lazy fork at first turn (Claude only). Add a separate capability for "a fork can be created without a turn" (Codex only). The frontend needs two answers in `lib/harnessCapabilities.ts`: is the compose fork offered (Claude or Codex), and is the menu fork offered (Codex). Replace the hardcoded `candidate.harness !== "claude_code"` check in `ComposeBar.svelte`'s `forkShapeBlock` with the capability. Keep `AgentRecord::validate` and `register_agent_inner` rejecting `forked_from_session` on any harness that is not lazy-fork capable, so a Codex record never carries it.
2. **Core registration.** Add a `Project` method that registers a fork of a Codex source from a locator milestone 1 already created. It derives the name with `derive_fork_name`, copies the source's selection, writes the Codex locator, and leaves `forked_from_session` empty. Route it through `register_agent_inner`, like every other write. `register_attached_codex_agent` is the nearest precedent.
3. **The app command.** Give `fork_agent_impl` (`desktop/src-tauri/src/commands.rs`) a Codex arm, keeping the response shape the frontend already consumes. In this order:
   1. Run the existing checks: the source's own project, the source's session file exists (`resolve_session_file`), and the source is idle (`ensure_fork_source_free`).
   2. Acquire the **parent's** session lock with `acquire_session_locks`, using the parent's Codex key.
   3. Call milestone 1's function. Honor mock mode (`state.spawns_real_harnesses`) and Codex PATH readiness, as `read_codex_account_usage_impl` does.
   4. Release the lock.
   5. Register under the `registry_write` lock, and update the agent cache.

   **Why the lock covers only the creation (write this at the call site):** Codex reads the parent's rollout only while creating the fork (§3.5, probe 4). A parent turn that starts in that second waits on the lock instead of racing the read, in this process or in another Switchboard instance. For Codex that makes the busy check an actual lock rather than only a check.

   Create the fork before registering it, so a failed creation leaves nothing in Switchboard. If registration fails after a successful creation, the stray Codex rollout is harmless: log a warning and return the error.
4. **The frontend flow stays the same**: fork, then send to the new agent through `dispatchToRecipients`, in `ComposeBar.svelte`'s `dispatchForkSend` and `dispatchPromptForkSend`. What changes is in `forkAgentIntoOwnPane` (`lib/state/workspace.svelte.ts`). For an eager fork, hydrate the new agent at once, because its inherited history already exists. Skip the lazy-fork machinery: the pending "branched from" notice (`forkHistoryPending`) and the one-time refresh (`installForkHistoryRefresh`). Both are keyed on `forked_from_session`, which a Codex fork never has. Make that branch explicit on the capability, not an accident of the missing field.
5. **Update the docs this milestone changes:**
   - `docs/harness-behavior.md` §3.5: the status line and Codex row move from unwired to built.
   - The capability watchlist row "Fork | Codex" in `docs/harness-behavior.md` §8: mark it built.
   - `docs/system-design.md`: the §9 Fork bullet and matrix row, and resolved item 10.14.

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
  - while the parent's session lock is held by a contender, the fork is refused rather than racing;
  - the source's own project is used, not the active one;
  - a later turn to the fork locks only the fork's own session key.
- Frontend tests:
  - `ComposeBar.test.ts`: the Fork half is offered for one idle Codex recipient and refused for Antigravity, with today's copy; ⇧⌘↵ works for Codex.
  - `workspace.test.ts`: an eager fork hydrates at once and arms no pending notice.
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
- A failure shows the error and creates no agent.

### Implementation outline

Small milestone. Add the item to the agent card menu in `lib/components/Sidebar.svelte`, gated by the "fork without a turn" capability from milestone 3. Place it beside "Compact context", and follow that item's handling of a busy agent and of errors. It calls the same `fork_agent` command and `forkAgentIntoOwnPane` as the compose path, minus the send.

Update `docs/system-design.md`, whose §2 says Fork is "deliberately not a row action" because a Claude fork can only exist as a turn. State that Codex has the row action, and why Claude does not.

### Definition of done

- Frontend tests in `Sidebar.test.ts`:
  - the item shows for a Codex agent and not for Claude or Antigravity;
  - it is unavailable while the agent is working;
  - choosing it creates the fork and its pane, and dispatches no turn;
  - a failure shows the error.
- **Prove it in the app:** fork a Codex agent from the menu, confirm the new agent is idle with the inherited conversation, then send the original agent a message and confirm it runs at once.
- `README.md`: update the "Fork a conversation" feature bullet, the Fork how-to section, and the "Fork is currently Claude Code only" limitation, so they describe both harnesses and both entry points.

## Known limitations to record

Record these in `docs/harness-behavior.md` §3.5 and, where a user would notice, in the README's "Harness support and limitations".

- **A Codex fork depends on its parent's rollout for as long as it exists.** If that file is deleted, moved, compressed (gap G40) or rewritten in place (`codex migrate-rollouts --apply`), the fork's inherited history is lost from Switchboard's view. Milestone 1's probe records what Codex itself does. Switchboard never deletes harness files: deleting an agent leaves its rollout in place (`delete_agent_sidecars`), so deleting the parent agent is safe.
- **A parent and its fork in the same pane show the shared history twice**, as with Claude. The fork opens in its own pane to avoid it.
- **The partition date can be wrong at midnight.** It is the local date when the fork is captured, the same accepted edge as every Codex first turn.
