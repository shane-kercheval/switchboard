# Codex compaction: show it in the transcript, then add "Compact context" (2026-10-11)

Codex compacts long conversations on its own, and Switchboard shows nothing when it does. This plan first makes Switchboard's transcript show a marker wherever a Codex conversation was compacted. It then adds the "Compact context" action that Claude Code agents already have, for Codex agents.

"Compaction" means the harness replaces the conversation history it sends to the model with a summary, so a long conversation fits the model's context window again. Switchboard shipped manual compaction for Claude Code in `docs/implementation_plans/2026-09-16-claude-compaction.md`. Codex supports it too, but only through `codex app-server`, Codex's experimental JSON-RPC server, not through `codex exec`, which is how Switchboard runs every Codex turn.

## Decided, and why

These decisions came out of the investigation that preceded this plan. Each one states a reason that must survive into the code, as a comment next to the code it governs or in the commit message.

- **The `compacted` record is the only source of a Codex compaction marker.** Codex writes two things per compaction: a top-level `compacted` record, and an `event_msg` → `item_completed` whose `item.type` is `ContextCompaction`. Codex's persistence policy writes `compacted` in every history mode, but writes the `item_completed` record only in "paginated" session files. Reading both would show two markers for one compaction. Reading only the item would miss compactions in older files.
- **A marker never splits a Codex agent turn.** The marker goes in after the agent turn in which the compaction happened, even when the compaction happened partway through that turn. Splitting one Codex task into two agent turns on disk would break the one-to-one match between a live turn and its disk turn. The merge pairs them by the turn's hydration key (`turn_context.turn_id`). This repo has already shipped exactly that duplicated-transcript bug once: Claude Code's mid-turn records split one live turn into several disk turns, and refreshing the project then showed the extra fragments as duplicates. The cost is that a marker for a mid-turn compaction sits after the turn's content rather than at the exact point.
- **The Codex marker has no summary.** Codex encrypts the summary it writes: `compacted.message` is `""`, and the real summary is an `encrypted_content` blob inside `replacement_history`. So `SystemMarker::Compaction` makes its summary optional, and the frontend renders a summary-less marker with no preview, no expand control and no "Copy summary" action. If a future Codex writes a non-empty `message`, the marker shows it.
- **A Codex compaction doesn't move the sidebar's context bar. The bar updates on the agent's next reply.** The token figure Codex reports for the compaction isn't what the next turn uses. In one probe, the compaction reported 9,705 tokens and the next turn used 17,572. Codex's fixed instructions and tool definitions (about 17,000 tokens) aren't counted in the compaction's figure. A bar that dropped to 9.7k and jumped back on the next reply would mislead. So the live compaction reports no usage, and on reopen a compaction-only turn contributes no usage. Either way, the bar keeps the last real turn's reading until the next reply.
- **Manual compaction drives a short-lived `codex app-server`, the same pattern as the quota read.** `crates/harness/src/codex/account_usage.rs` already starts a bare `codex app-server` over stdio, writes its requests, keeps stdin open until the answer, and kills the process group. Compaction needs the same lifecycle plus two more requests and a longer wait. Running a long-lived server was the reason this was declined before, and it isn't needed. Milestone 2 extracts the shared lifecycle into one module, so the quota read and compaction use one implementation.
- **The compaction runs on the agent's selected model and effort.** Claude Code's compaction does the same: the dispatcher snapshots the selection when the user asks. `thread/resume` accepts a `model` override and a `config` map, so the Codex compaction passes the same model and effort `codex exec` would get for the agent's next turn.
- **A compaction Codex runs on its own during a live Switchboard turn shows its marker on the next project reload, not when the turn ends.** The marker comes from reading Codex's session file, which Switchboard does when a project is opened or refreshed (for example after switching away and back). Showing it at turn end would need a new signal on the turn-end event telling the frontend to reload the agent's history, and that signal touches every adapter's turn-end path. A manual "Compact context" (Milestone 3) shows its marker immediately, because the frontend already reloads the project after a completed compaction.
- **Switchboard never sends `/compact` to Codex as a prompt.** `codex exec` hands `/compact` to the model, which replies "Compacted." while nothing compacts (`docs/harness-behavior.md` §3.9, "The fake-success hazard").
- **A Codex reply that a history reread caught half-written collapses into the live reply when the reply ends.** After a completed compaction, Switchboard rereads the project's history at once so the marker appears, and Milestone 3 keeps that. With Codex, a message queued behind the compaction starts the moment the compaction ends, so that reread routinely runs while Codex is writing the next reply. A Codex reply gets the id that matches it to its saved copy (`turn_context.turn_id`) only when it ends. So the reread adds the half-written saved copy as a second reply, and nothing removes it until the project is next loaded fresh (gap G46 in `docs/harness-behavior.md`, verified with a reducer test). Claude Code is safe, because its reply gets the id at its first message and the `turn_identity` step collapses any copy carrying it. Milestone 3 makes `turn_end` collapse copies the same way. This was chosen over delaying the reread until the agent is idle, for two reasons: it fixes the cause for every reread, including the existing ones after switching back to a project and after a Claude Code compaction, and it leaves the shipped reread unchanged. A reply that ends without its id (gap G44) can still show twice after a reread.

## Read before starting

Read these before writing code:

- `docs/harness-behavior.md` §3.9 ("Manual context compaction"), especially the 2026-10-11 Codex re-probe (items 1 to 11), and gap **G34**. They hold the verified protocol: request order, the notification sequence, cancellation, and the session-file records.
- `docs/implementation_plans/2026-09-16-claude-compaction.md`. Milestone 3 reuses its dispatcher queue, app gate, frontend row and refresh-on-completion without changing them.
- The module doc of `crates/harness/src/codex/inherited_history.rs`. A Codex fork's rollout points into its original's rollout instead of copying it, and this module rebuilds the fork's earlier conversation by running each ancestor's file through the same reader Milestone 1 changes. Background: `docs/implementation_plans/2026-10-11-codex-fork.md` and `docs/harness-behavior.md` §3.5.
- Gaps **G44** and **G46** in `docs/harness-behavior.md`, and the `turn_identity` and `turn_end` cases of `transcriptReducer` in `desktop/src/lib/state/reducers.ts`, before Milestone 3's reducer step.
- The module doc and comments of `crates/harness/src/codex/account_usage.rs`. Every behavioral claim there (stdin must stay open, the measured latency, the interleaved notifications) was measured, and the shared module in Milestone 2 inherits them.
- `AGENTS.md` sections "Live testing against real harnesses" and "Test-type vocabulary".
- Codex's own sources at the version this was probed against (codex-cli 0.162.1):
  - <https://github.com/openai/codex/blob/rust-v0.162.1/codex-rs/rollout/src/policy.rs>: which records are persisted in which history mode (`compacted` always, `item_completed` only when paginated).
  - <https://github.com/openai/codex/blob/rust-v0.162.1/codex-rs/app-server-protocol/src/protocol/common.rs>: the `thread/compact/start` and `thread/resume` request definitions.
- Codex's generated protocol schema, which is the authority for the app-server's request and notification shapes. Run `codex app-server generate-json-schema --out <dir>`, then read `v2/ThreadResumeParams.json`, `v2/ThreadCompactStartParams.json` and `ServerNotification.json`.

## Milestone 1: Codex transcripts show where Codex compacted

**Goal.** Reading a Codex session file produces a compaction marker for every compaction in it, and stops producing empty agent turns for compactions and cancelled compactions. This closes gap G34 in `docs/harness-behavior.md`. Nothing about dispatch changes in this milestone.

**Outcomes:**

- A user who reopens a project with a Codex agent whose conversation Codex compacted sees a "Conversation compacted" marker right after the turn in which it happened. This covers automatic compactions partway through a turn, which are the common case: 387 of the 504 compactions counted on the developer's machine. It also covers compactions run as a turn of their own.
- A compaction run as its own turn shows only the marker, not an empty agent turn before or after it.
- A Codex turn that recorded no user prompt and no content shows nothing. Today such a turn shows as an empty agent turn with status `failed`. A cancelled compaction is the main source.
- The Codex marker shows the label only: no summary preview, no expand control, no "Copy summary". Claude Code's marker looks exactly as it does today.
- After reopening a project whose Codex agent's last turn was a compaction, the sidebar's context bar shows the last real turn's reading, not the compaction's figure.
- Opening a project with a compacted Codex session doesn't duplicate any turn, and a live turn still pairs with its own disk turn.
- A Codex fork of a compacted conversation shows the original's marker among its inherited turns, because the fork's earlier conversation goes through the same reader.

**Implementation outline:**

1. **Make the compaction summary optional.** In `crates/harness/src/transcript.rs`, `SystemMarker::Compaction { summary: String }` becomes `summary: Option<String>`. The Claude Code producer in `crates/harness/src/claude_code/session_file.rs` sets `Some(text)`. The type crosses IPC as `ConversationItem::SystemMarker` (`desktop/src-tauri/src/commands.rs`) and is never persisted: pins store only a message key, and no sidecar or journal stores markers. So this is a wire change with no stored-data impact, and backend and frontend ship together. Update the TypeScript `SystemMarker` union in `desktop/src/lib/types.ts`. Update `desktop/src/lib/components/CompactionMarker.svelte` and, in `UnifiedTranscript.svelte`, the marker row and its "Copy summary" action, so a marker whose summary is absent or blank renders the label only and offers no copy. Those are the summary's only frontend readers (checked 2026-10-12): `unified.ts` reads only the marker's kind, and `transcriptIndex.ts` skips markers. Only the `Compaction` variant changes. The `InheritedHistoryUnavailable` variant that the fork work added stays as it is.
2. **Emit a marker per `compacted` record in the Codex reader.** In `crates/harness/src/codex/session_file.rs`, the reconstruction path (`CodexAgentBuilder` and `handle_event_msg`) currently drops top-level `compacted` records on its `_ => {}` arm. For each one, produce a `Turn::System { marker: Compaction { summary } }`:
   - `started_at` is the record's timestamp.
   - `summary` is `compacted.message` when it is non-empty, otherwise `None`.
   - Ignore `item_completed` records whose `item.type` is `ContextCompaction`, with a comment saying why: the `compacted` record is the single source.
3. **Place the marker after its agent turn without splitting it.** Hold the pending marker while the task is open, and push it into the turn list when the task's agent turn closes, right after that turn. Put the reason in a comment where the marker is held: one Codex task is one disk agent turn with one hydration key, and splitting it reintroduces the duplicate-on-refresh bug. If a task holds more than one compaction (the developer's machine has a session with 27), each gets its own marker, in order.
4. **Drop agent turns with no prompt and no content.** When a task closes, if it recorded no user prompt and produced no items, don't emit an agent turn for it. If it held a compaction, emit only the marker. If it didn't, emit nothing. This applies however the task closed: `task_complete`, an unhandled `turn_aborted`, the next `task_started`, or end of file. That task's token usage must not become any turn's usage, which is what keeps the reopened context bar on the last real turn. A normal turn always records its user prompt inside its task (`task_started` comes before `user_message`), so a cancelled normal turn still renders as it does today.
5. **Leave the live path alone.** `parse_session_content`, which the adapter runs at the end of every live turn, keeps its current behavior. Taking its per-turn usage from the last `token_count` already reflects a mid-turn compaction correctly, because that count comes from a real model call made after the compaction.
6. **Forks need no code of their own.** `load_inherited_history` (`codex/inherited_history.rs`) rebuilds each ancestor's segment with `parse_codex_transcript_content`, so steps 2 to 4 apply to every segment of a fork's chain. If a segment's cut-off falls inside a task, step 4's end-of-file close applies to it. Confirm this with the fork test below rather than assuming it.

**Definition of done:**

- Fixture-driven unit tests in `codex/session_file.rs`, using records copied from real 0.162.1 rollouts (capture fresh ones with the probe in "Prove it works" below, and sanitize them):
  1. A mid-turn compaction yields one agent turn with all its items, then one `Compaction` marker.
  2. A compaction-only task yields one marker and no agent turn, and the following turn's usage is unaffected.
  3. A task with no prompt and no content, ending in `turn_aborted`, yields nothing.
  4. A paginated file containing both `compacted` and the `ContextCompaction` item yields one marker.
  5. A legacy-mode file containing only `compacted` yields one marker.
  6. Two compactions in one task yield two markers, in order.
  7. A non-empty `compacted.message` becomes the summary, and an empty one becomes `None`.
  8. A cancelled normal turn (with a user prompt) renders as it does today.
- A fork test in `codex/inherited_history.rs`, using that module's staged-rollout helpers: a fork whose original holds a compaction shows the original's turns, then its marker, then the fork's own turns, with no empty agent turn.
- Merge tests in `desktop/src-tauri/src/commands.rs`, through the real Codex parser, in the style of `merge_compaction_session_through_real_parser_does_not_duplicate_prompt`:
  - A compacted Codex session merges without duplicating any turn.
  - A live turn with a mid-turn compaction pairs with its single disk turn by hydration key.
  - The marker doesn't shift provenance correlation, mirroring `merge_compaction_marker_renders_without_shifting_provenance_correlation`.
- Frontend tests:
  - `CompactionMarker` renders the label alone, with no expand control and no copy action, when the summary is `null`.
  - The existing Claude marker tests pass unchanged.
- Update the Claude Code tests that construct `Compaction { summary }` to the new type. Their assertions don't change.
- Docs:
  - In `docs/harness-behavior.md`, mark G34 closed and state the placement rule and the summary-less marker.
  - Update §3.9's "Verified adjacent defect" paragraph to describe the current behavior, not the defect.
  - Add a README "Harness support and limitations" line: a Codex compaction marker shows no summary because Codex encrypts it.

## Milestone 2: the Codex adapter can compact a session through a short-lived app-server

**Goal.** `CodexAdapter::compact` compacts an agent's Codex session by driving a bare `codex app-server`, and returns an event stream with the same contract as Claude Code's compaction. Nothing user-visible changes yet, because the app still refuses Codex compaction until Milestone 3 turns it on.

**Outcomes:**

- Calling `compact` on a Codex agent that has a session compacts it. The session file gains one compaction turn, and the next `codex exec resume` turn continues the conversation with its history summarized.
- The stream ends in exactly one `TurnEnd`:
  - `Completed` when Codex reports the compaction done;
  - `Failed`, carrying Codex's own error text, when Codex refuses or fails;
  - none at all when cancelled, matching Claude Code, where the dispatcher synthesizes `Cancelled`.
- Cancelling a running compaction stops it cleanly. Codex records an interrupted turn, writes no `compacted` record, and leaves no server process behind.
- The quota read behaves exactly as before, on the shared module.

**Implementation outline:**

1. **Extract the app-server lifecycle into one module, and make the quota read use it.** Create a module under `crates/harness/src/codex/` (for example `app_server.rs`) that owns everything `account_usage.rs` does that isn't specific to quota:
   - spawning a bare `codex app-server` with piped stdio, `kill_on_drop`, `process_group(0)` and the PATH environment;
   - writing newline-delimited JSON-RPC requests in one write and keeping stdin open until the caller is done (the module doc must keep the measured reason: closing stdin makes the server exit without finishing, 0 of 20 runs closed against 20 of 20 open, and for compaction it aborts the work);
   - reading stdout line by line, matching replies by JSON-RPC id and passing notifications through, so an interleaved notification never shifts the match;
   - draining stderr into a tail for error messages;
   - teardown through `terminate_then_kill` on every path, and the cancellation-safe task structure.

   `account_usage.rs` keeps its request, response parsing and error types, and becomes a client of the module. This is a behavior-preserving refactor, so its tests in `crates/harness/tests/codex_account_usage.rs` and its unit tests must pass unchanged. Any future app-server feature uses this module instead of starting its own server code.
2. **Implement `CodexAdapter::compact`** in `crates/harness/src/codex/mod.rs`, on the shared module:
   - **Refuse before spawning** with `DispatchError::InvalidAgentState` when the agent has no `SessionLocator::Codex`, or when `locate_session_file` finds no rollout for it. This mirrors `ClaudeCodeAdapter::compact`. Codex itself answers `thread not found` for an unknown session.
   - **Send one write of three requests, then keep stdin open:**
     1. `initialize`, with the same `clientInfo` the quota read sends.
     2. `thread/resume` with `threadId`, the agent's `cwd`, `excludeTurns: true` (without it the reply carries the whole history, and Codex says that mode is deprecated), and the agent's `model`. Pass the effort through `config`, mirroring the `-c model_reasoning_effort=…` that `build_args` passes to `codex exec`.
     3. `thread/compact/start` with `threadId`.
   - **Bound only the startup.** All three requests must be answered within a startup bound. Size it from the quota read's measured latency, which `ACCOUNT_USAGE_TIMEOUT` (30s) is based on. The reply to `thread/compact/start` is `{}` and means only "accepted". After it, the compaction runs as a turn, and like any Switchboard turn it has no overall bound. Compaction latency on a long real conversation is unmeasured (probes took 3 to 4 seconds on small sessions), and the user can always press Stop.
   - **Follow the compaction turn.** The compaction turn is the first `turn/started` after `thread/compact/start` is accepted. Success requires an `item/completed` whose `item.type` is `contextCompaction` within that turn, followed by `turn/completed` with `status: "completed"`. Emit `AdapterEvent::Liveness` for that turn's notifications, so the row shows activity. Ignore every other notification, including `thread/tokenUsage/updated` (its figure is the one the context-bar decision rejects), `mcpServer/startupStatus/updated`, `account/updated`, `remoteControl/status/changed` and `deprecationNotice`.
   - **Map the outcome to the terminal `TurnEnd`** as follows. On every terminal, `usage` is `None`, and `model`, `effort`, `stable_message_id` and `first_message_id` are `None`.

     | What happened | `TurnEnd` outcome |
     |---|---|
     | `contextCompaction` completed, then `turn/completed` `completed` | `Completed` |
     | `turn/completed` `failed` | `Failed`, classified from the turn error's `codexErrorInfo`: `usageLimitExceeded` is `UsageLimit` and `unauthorized` is `AuthFailure` (both variants exist in the 0.162.1 schema's `CodexErrorInfo`). Anything else is `HarnessError`. The message is Codex's own text, verbatim, as on the `codex exec` path. |
     | `turn/completed` `completed` with no `contextCompaction` completed | `Failed{HarnessError}`, "compaction did not run". Fail closed, as Claude Code's no-verdict case does. |
     | JSON-RPC error reply to `thread/resume` or `thread/compact/start` | `Failed{HarnessError}` with Codex's message |
     | Error reply to `initialize` | `Failed{AdapterFailure}` |
     | stdout ends or the server exits before the compaction turn completes | `Failed{AdapterFailure}` |
     | Startup bound exceeded | `Failed{AdapterFailure}` |
     | `options.cancel_token` fires | Tear the process group down and end the stream with no terminal |

   - **Never touch the shared background daemon.** Switchboard must not use `codex app-server proxy`, the control socket, or `--managed-daemon`. A bare stdio `codex app-server` is its own process, and the quota read already relies on that.
3. **Keep `context_report` refused for Codex.** Only `compact` changes.

**Definition of done:**

- Fixture-driven tests through `fake_codex` in `app-server` mode, which replays a fixture line by line while the client matches by id. Put them in a new `crates/harness/tests/codex_compaction.rs` or in `codex_adapter.rs`:
  - The success sequence ends in one `TurnEnd{Completed}` with `usage: None`, preceded by `Liveness`.
  - `turn/completed` `failed` with an error message gives `Failed{HarnessError}` with that message verbatim.
  - A usage-limit turn error gives `Failed{UsageLimit}`.
  - A `thread not found` error reply gives `Failed{HarnessError}` naming it.
  - A completed turn with no `contextCompaction` item gives `Failed`, "compaction did not run".
  - The server exiting mid-compaction gives `Failed{AdapterFailure}`.
  - With `// hang`, firing the cancel token ends the stream with no terminal and leaves no process in the group (the existing `// pgid_to:` directive supports that check).
  - An unanswered startup gives `Failed{AdapterFailure}` within the bound. Make the bound injectable in tests, as the quota read's timeout is.
  - Interleaved notifications before and between the replies don't break id matching.
- A test that the requests carry `excludeTurns: true`, the agent's model and effort, and the right `threadId`. If `fake_codex` can't record its stdin, add a directive that writes stdin to a file.
- Refusal tests mirroring `compact_refuses_an_agent_with_no_session_locator` and `compact_refuses_an_agent_with_no_session_file`.
- The quota read's tests pass unchanged, and `live_codex_account_usage_read_returns_named_buckets` passes.
- `compact_is_refused_as_an_unsupported_operation` in `crates/harness/tests/codex_adapter.rs` asserts the old behavior for Codex, which this milestone deliberately changes. Replace its Codex case with the tests above, and keep the Antigravity refusal test.

## Milestone 3: Codex agents offer "Compact context"

**Goal.** A user can compact a Codex agent's conversation from Switchboard the same way as a Claude Code agent's. The dispatcher queue, app gate, live row and post-compaction refresh are reused unchanged. One step of the frontend's transcript reducer changes, so that the refresh can't show a Codex reply twice.

**Outcomes:**

- A Codex agent's menu, and the button on its context bar, offer "Compact context", exactly as for a Claude Code agent. Antigravity agents still don't offer it.
- The action queues like a message: it runs at once on an idle agent, behind the running turn and earlier queued messages on a busy one, and Stop cancels it.
- The live row shows "Compacting context…", then "Context compacted". It shows no before/after token counts, because Codex reports none that mean anything. A failure shows "Compaction failed" with Codex's reason.
- When the compaction completes, the project refreshes and the Milestone 1 marker appears without reopening.
- A message queued behind a Codex compaction gets a reply that shows once, even though the refresh runs while Codex is writing it. The same fix covers any Codex reply a refresh catches half-written, such as switching back to a project mid-reply (gap G46).
- The context bar doesn't move until the agent's next reply.
- The agent's next turn continues the conversation normally. It still runs commands without asking for approval, so the compaction's `thread/resume` didn't change the session's sandbox or approval settings.

**Implementation outline:**

1. Make `HarnessKind::supports_manual_compaction` (`crates/core/src/harness.rs`) true for `ClaudeCode` and `Codex`, and update its doc comment to say why Codex qualifies (a mechanism-verified compaction, not a prompt). Make the frontend mirror `supportsManualCompaction` (`desktop/src/lib/harnessCapabilities.ts`) match.
2. Nothing else in the dispatcher or app is expected to change. `compact_agent_impl` gates on the predicate, `ProjectDispatchContextFactory::preflight` takes the Codex session lock for a compaction exactly as for a send, and `drain_turn` journals nothing for a compaction. Confirm each against the code rather than assuming.
3. The frontend row already renders the label alone when the counts are absent (`CompactionTurn.svelte`), and the terminal hook already refreshes the project after a completed compaction. Confirm both for a Codex agent.
4. **Make `turn_end` collapse other copies of the reply it ends, as `turn_identity` already does.** In `transcriptReducer` (`desktop/src/lib/state/reducers.ts`), after stamping the hydration key, collapse every other agent turn carrying that key into the ending turn. Use `resolveTurnCollision` with the ending turn protected, so it keeps its `turn_id` and content and absorbs the copy's live-only fields, exactly as the `turn_identity` case does. Put the reason in a comment: a Codex reply has no key until it ends, so a refresh while it was being written inserted its saved copy as a separate turn (G46). Collapse on the key the turn ends with, whether it came with `turn_end` or earlier. For a Claude Code reply and a cancelled Codex reply, `turn_identity` has already collapsed the copies, so this step only catches a copy a refresh added after that. A rough version of this step (filtering out other turns with the key) passed the whole jsdom suite on 2026-10-12, so no existing test depends on the duplicate.
5. Update the docs to describe what ships:
   - `docs/harness-behavior.md` §3.9: the status line, and "What Switchboard ships", with a Codex part covering the protocol, the outcome mapping and its residuals.
   - Gap G46: closed for replies that end with their id, and still open for those that end without one (G44).
   - The §8 watchlist row for Codex manual compaction: now shipped.
   - The README's "Compacting a conversation" line: Claude Code and Codex. For Codex: no before/after counts, no summary in the marker, and the context bar updates on the next reply.
   - `docs/system-design.md` item 10.11: the resolved compaction strategy now covers Codex.

**Definition of done:**

- Update the tests that pin the old Claude-Code-only behavior to the new behavior. Each still asserts Antigravity is refused.
  - `supports_manual_compaction_is_claude_only` in `crates/core/src/harness.rs`.
  - `compaction_is_refused_for_every_harness_that_cannot_be_driven` in `commands.rs`.
  - The Sidebar "compact context action" tests.
- New tests:
  - `compaction_is_accepted_for_a_codex_agent_with_a_session` (app gate).
  - A Sidebar test that the action and the context-bar button are offered for a Codex agent.
  - A Sidebar test that a completed Codex compaction with no usage leaves the bar unchanged.
  - A `UnifiedTranscript` test that a Codex compaction row renders "Context compacted" without counts.
  - Reducer tests for step 4, in `reducers.test.ts` next to the `turn_identity` suite:
    - A live reply, then a refresh carrying its half-written copy, then `turn_end` with the key: one agent turn, the live one, complete, with its own `turn_id` and content.
    - A second refresh, carrying the completed copy, still leaves one.
    - A `turn_end` with no key collapses nothing. This pins the G44 residual.
  - A component-level test through the real stores, per `AGENTS.md`'s rule for IPC-wrapping components: a Codex compaction completes, and a queued message's reply starts. The post-compaction refresh resolves while that reply streams, with a mocked `load_project_conversation` that returns the reply's half-written copy. Then the reply ends. The transcript shows one reply and one compaction marker.
- Live tests in `crates/harness/tests/live.rs`, named `live_codex_…` so `make test-live-codex` runs them:
  - `live_codex_compact_succeeds_and_the_conversation_continues`:
    1. Dispatch a turn that asks the model to remember a code word.
    2. Compact through the adapter, and assert `TurnEnd{Completed}`.
    3. Assert the raw rollout gained a `compacted` record, and that the compaction turn's `turn_context.model` is the requested model.
    4. Assert `load_codex_transcript` yields one `Compaction` marker and no empty agent turn.
    5. Dispatch a turn that asks for the code word and runs a shell command. Assert the answer is right and the command ran without an approval stop.
  - `live_codex_compact_cancelled_leaves_no_compaction`:
    1. Start a compaction and cancel it on the first `Liveness`.
    2. Assert the stream ends with no terminal, the rollout has a `turn_aborted` and no new `compacted` record, and `load_codex_transcript` shows no empty or failed turn.
    3. Assert the next turn works.
  - `live_codex_compact_of_a_fork_keeps_its_inherited_context`. Compacting a fork is unprobed. Codex continues a fork by re-reading its original's rollout up to the fork point, and the compaction reaches the fork through `thread/resume` rather than `codex exec`. Follow `live_codex_fork_loads_the_parents_history_up_to_the_fork_point` for setup:
    1. Run a turn on a parent that asks the model to remember a code word.
    2. Fork it with `fork_codex_session`, without a prompt.
    3. Compact the fork through the adapter, and assert `TurnEnd{Completed}`.
    4. Assert the fork's rollout gained a `compacted` record and the parent's rollout is byte-for-byte unchanged.
    5. Assert `load_codex_transcript` for the fork yields the parent's turns, then one `Compaction` marker, and no empty agent turn.
    6. Ask the fork for the code word, and assert the answer is right.
- `make check` and `make test-live-codex` pass.

## Prove it works

The tests above aren't enough on their own. Before calling each milestone done, look at the real result.

1. **Milestone 1.** Create a compacted Codex session to look at:
   1. Run two `codex exec` turns in a scratch folder.
   2. Compact the session with a short script that drives `codex app-server`: `initialize`, `thread/resume` with `excludeTurns: true`, `thread/compact/start`, keeping stdin open until `turn/completed`.
   3. Kill a second compaction right after its `item/started`.
   4. Keep the script and its output with the milestone's work.

   Then load the session through `load_codex_transcript` and print the turns. Expect the two prompt-and-answer pairs, one marker and nothing else. Copy these records, sanitized, as the Milestone 1 fixtures. Then run the app (`make dev`), add a project for that scratch folder with a Codex agent attached to that session, and look at the transcript: one "Conversation compacted" marker with no expand control, and no empty turns.
2. **Milestone 2.** Run the live test from Milestone 3 against the adapter as soon as `compact` exists. Then read the rollout yourself:
   - it has one new compaction turn;
   - its `turn_context` names the requested model;
   - after a cancel, it has a `turn_aborted` and no `compacted` record.

   After each run, check with `ps` that no `codex app-server` process is left.
3. **Milestone 3.** In `make dev`, create a Codex agent and send it a few turns. Then:
   1. Use "Compact context" from its menu, and watch the row go from "Compacting context…" to "Context compacted".
   2. Watch the marker appear after the refresh, and confirm the context bar didn't move.
   3. Send another message, and confirm the bar updates and the agent still runs a shell command without asking.
   4. Start a compaction, and while it runs, send the agent a message that asks for a long answer, so it queues behind the compaction. Confirm the reply shows once when it ends, then switch to another project and back, and confirm it still shows once.
   5. Queue a compaction behind a running turn, then cancel it.
   6. Start another and press Stop while it runs. Reopen the project, and confirm no failed or empty turn appears.

## Known limitations to record, not fix

Record each of these in `docs/harness-behavior.md` §3.9, and the user-facing ones in the README:

- The Codex marker shows no summary, because Codex encrypts it.
- A Codex compaction row shows no before/after token counts, and the context bar updates only on the next reply.
- A compaction's cost appears in no figure Codex reports, so Switchboard can't show it.
- A marker for a compaction partway through a turn appears after the turn's content, not at the exact point.
- A compaction Codex runs on its own during a live turn shows its marker on the next project refresh or reopen, not when the turn ends.
- A Codex reply that ends abnormally carries no id (gap G44), so any later refresh, including the one after a compaction, can show it twice. This plan doesn't change that.
- The app-server protocol is marked experimental by OpenAI. The live tests are the drift detector, as they already are for the quota read.
- Compaction latency on a long real conversation is unmeasured.
