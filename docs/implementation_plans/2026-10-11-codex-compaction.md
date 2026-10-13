# Codex compaction: read compaction turns correctly, then add "Compact context" (2026-10-11)

Switchboard can compact a Claude Code agent's conversation on request, but not a Codex agent's. This plan adds the "Compact context" action for Codex agents. First it fixes how Switchboard reads compactions from Codex's session files: today a compaction run as its own turn shows as an empty agent reply, and after this plan it shows as a one-line marker where it ran.

"Compaction" means the harness replaces the conversation history it sends to the model with a summary, so a long conversation fits the model's context window again. Switchboard shipped manual compaction for Claude Code in `docs/implementation_plans/2026-09-16-claude-compaction.md`. Codex supports it too, but only through `codex app-server`, Codex's experimental JSON-RPC server, not through `codex exec`, which is how Switchboard runs every Codex turn.

## Decided, and why

These decisions came out of the investigation that preceded this plan and the plan's review. Each one states a reason that must survive into the code, as a comment next to the code it governs or in the commit message.

- **The `compacted` record is the only source of a Codex compaction marker.** Codex writes two things per compaction: a top-level `compacted` record, and an `event_msg` → `item_completed` whose `item.type` is `ContextCompaction`. Codex's persistence policy writes `compacted` in every history mode, but writes the `item_completed` record only in "paginated" session files. Reading both would show two markers for one compaction. Reading only the item would miss compactions in older files.
- **Only a compaction that runs as its own turn gets a marker. A compaction Codex runs on its own, inside a reply, shows nothing, as today.** A compaction runs as its own turn when Codex records it in a task of its own, with no prompt and no reply content (Milestone 1, step 2, has the exact rule). Switchboard's "Compact context" (Milestone 3) and `/compact` in Codex's own interface both run that way. The file doesn't record who asked for a compaction, so the rule is about where it ran, not who started it. Codex's automatic compactions happen inside a reply when the context fills, and they are the common kind. Of the 529 compactions in the developer's Codex rollouts (counted 2026-10-12), 408 came partway through a reply and 40 at its start, before any content. Only 81 ran as a turn of their own. The developer decided on 2026-10-12 not to mark them, for three reasons:
  - A Codex marker can only be a label, because Codex encrypts its summary (next decision). Claude Code's marker is worth showing mainly for its summary.
  - It would sit in the wrong place. A mid-reply marker would have to go after the whole reply, because splitting one Codex task into two agent turns on disk breaks the one-to-one match between a live turn and its disk turn, which the merge pairs by hydration key (`turn_context.turn_id`). This repo has already shipped that duplicated-transcript bug once, when Claude Code's mid-turn records split one live turn into several disk turns. Placed after the whole reply, a compaction followed by an hour of work would look as if it had just happened.
  - The sidebar's context bar already shows how full the context is.

  A compaction run as its own turn has none of these problems: it isn't part of any reply, so its marker sits exactly where it ran. Claude Code keeps its markers for automatic compactions, which carry its summary.
- **The Codex marker has no summary.** Codex encrypts the summary it writes: `compacted.message` is `""`, and the real summary is an `encrypted_content` blob inside `replacement_history`. So `SystemMarker::Compaction` makes its summary optional, and the frontend renders a summary-less marker with no preview, no expand control and no "Copy summary" action. If a future Codex writes a non-empty `message`, the marker shows it.
- **A Codex compaction doesn't move the sidebar's context bar. The bar updates on the agent's next reply.** The token figure Codex reports for the compaction isn't what the next turn uses. In one probe, the compaction reported 9,705 tokens and the next turn used 17,572. Codex's fixed instructions and tool definitions (about 17,000 tokens) aren't counted in the compaction's figure. A bar that dropped to 9.7k and jumped back on the next reply would mislead. So the live compaction reports no usage, and on reopen a compaction-only turn contributes no usage. Either way, the bar keeps the last real turn's reading until the next reply.
- **Manual compaction drives a short-lived `codex app-server`, the same pattern as the quota read.** `crates/harness/src/codex/account_usage.rs` already starts a bare `codex app-server` over stdio, writes its requests, keeps stdin open until the answer, and kills the process group. Compaction needs the same lifecycle plus two more requests and a longer wait. Running a long-lived server was the reason this was declined before, and it isn't needed. Milestone 2 extracts the shared lifecycle into one module, so the quota read and compaction use one implementation.
- **The compaction runs with the agent's selected model and effort, and with the permissions `codex exec` gives it.** Claude Code's compaction does the same with model and effort: the dispatcher snapshots the selection when the user asks. `thread/resume` accepts a `model` override and a `config` map, so the Codex compaction passes the same model and effort `codex exec` would get for the agent's next turn. It also passes the sandbox and approval policy `codex exec` runs with, because Codex records each turn's settings and the agent card shows the latest turn's (Milestone 2, step 2).
- **Switchboard never sends `/compact` to Codex as a prompt.** `codex exec` hands `/compact` to the model, which replies "Compacted." while nothing compacts (`docs/harness-behavior.md` §3.9, "The fake-success hazard").
- **After a manual Codex compaction, its row is the record until the project is next loaded in full.** When a Claude Code compaction completes, the frontend rereads the project, so the harness's recap appears under the row. For Codex that reread does nothing in a project with only Codex agents, because it runs only when a Claude Code agent's session file changed. That is fine, because a Codex marker would only repeat the row's "Context compacted". Don't widen the reread to Codex agents. It would add a second "compacted" line under the row. It must also never merge a Codex agent's replies: a Codex reply gets the id that matches it to its saved copy only when it ends, so a reread while the next queued reply is being written would show that reply twice (gap G46). After a restart, the marker takes the row's place. In a project that also has Claude Code agents, a later reread can show the marker under the row, as Claude Code's recap does.

## Read before starting

Read these before writing code:

- `docs/harness-behavior.md` §3.9 ("Manual context compaction"), especially the 2026-10-11 Codex re-probe (items 1 to 11), and gap **G34**. They hold the verified protocol: request order, the notification sequence, cancellation, and the session-file records.
- `docs/implementation_plans/2026-09-16-claude-compaction.md`. Milestone 3 reuses its dispatcher queue, app gate, frontend row and post-compaction hook without changing them.
- The module doc of `crates/harness/src/codex/inherited_history.rs`. A Codex fork's rollout points into its original's rollout instead of copying it, and this module rebuilds the fork's earlier conversation by running each ancestor's file through the same reader Milestone 1 changes. Background: `docs/implementation_plans/2026-10-11-codex-fork.md` and `docs/harness-behavior.md` §3.5.
- Gap **G46** in `docs/harness-behavior.md`, before changing anything that rereads a Codex agent's history.
- The module doc and comments of `crates/harness/src/codex/account_usage.rs`. Every behavioral claim there (stdin must stay open, the measured latency, the interleaved notifications) was measured, and the shared module in Milestone 2 inherits them.
- `AGENTS.md` sections "Live testing against real harnesses" and "Test-type vocabulary".
- Codex's own sources at the version this was probed against (codex-cli 0.162.1):
  - <https://github.com/openai/codex/blob/rust-v0.162.1/codex-rs/rollout/src/policy.rs>: which records are persisted in which history mode (`compacted` always, `item_completed` only when paginated).
  - <https://github.com/openai/codex/blob/rust-v0.162.1/codex-rs/app-server-protocol/src/protocol/common.rs>: the `thread/compact/start` and `thread/resume` request definitions, and their `serialization` key, which makes Codex run requests about one thread one at a time.
  - <https://github.com/openai/codex/blob/rust-v0.162.1/codex-rs/app-server/src/request_processors/thread_processor.rs> (`thread_compact_start_inner`): Codex queues the compaction before it sends the request's reply.
  - <https://github.com/openai/codex/blob/rust-v0.162.1/codex-rs/core/src/session/turn.rs> (`run_turn`): an automatic compaction at the start of a reply runs before the prompt is recorded.
- Codex's generated protocol schema, which is the authority for the app-server's request and notification shapes. Run `codex app-server generate-json-schema --out <dir>`, then read `v2/ThreadResumeParams.json`, `v2/ThreadCompactStartParams.json`, `v2/TurnCompletedNotification.json` and `ServerNotification.json`.

## Milestone 1: Codex transcripts read compaction turns correctly

**Goal.** Reading a Codex session file turns a compaction run as its own turn into a compaction marker, and stops producing empty agent turns for compactions and cancelled compactions. A compaction partway through a turn stays invisible, as today. This closes gap G34 in `docs/harness-behavior.md`, with the scope decided above. Nothing about dispatch changes in this milestone.

**Outcomes:**

- A user who reopens a project whose Codex agent ran a compaction as its own turn sees one "Conversation compacted" marker where it ran, and no empty agent turn before or after it. That includes any `/compact` run in Codex's own interface. On the developer's machine it is 81 of 529 compactions (counted 2026-10-12).
- A turn in which Codex compacted on its own reads exactly as it does today: its content in full, and no marker. That includes a compaction at the start of a turn, before any of its content.
- A Codex turn that recorded no user prompt and no content, and didn't finish (it was cancelled or cut off), shows nothing. Today such a turn shows as an empty agent turn with status `failed`. A cancelled compaction is the main source. One that finished normally keeps its empty row, as today.
- The Codex marker shows the label only: no summary preview, no expand control, no "Copy summary". Claude Code's marker looks exactly as it does today.
- After reopening a project whose Codex agent's last turn was a compaction, the sidebar's context bar shows the last real turn's reading, not the compaction's figure.
- Opening a project with a compacted Codex session doesn't duplicate any turn, and a live turn still pairs with its own disk turn.
- A Codex fork of a conversation that holds a compaction run as its own turn shows the original's marker among its inherited turns, because the fork's earlier conversation goes through the same reader.
- Forwarding from a Codex agent whose last turn was a compaction forwards its last real reply. Today it forwards nothing: a forward from an idle agent takes the newest finished agent turn from its session file (`latest_completed_agent_text` in `crates/harness/src/forward.rs`), and the compaction reads as an empty finished one. Milestone 3's "Compact context" depends on this, and so do workflow forwards.

**Implementation outline:**

1. **Make the compaction summary optional.** In `crates/harness/src/transcript.rs`, `SystemMarker::Compaction { summary: String }` becomes `summary: Option<String>`. The Claude Code producer in `crates/harness/src/claude_code/session_file.rs` sets `Some(text)`. The type crosses IPC as `ConversationItem::SystemMarker` (`desktop/src-tauri/src/commands.rs`) and is never persisted: pins store only a message key, and no sidecar or journal stores markers. So this is a wire change with no stored-data impact, and backend and frontend ship together. Update the TypeScript `SystemMarker` union in `desktop/src/lib/types.ts`. Update `desktop/src/lib/components/CompactionMarker.svelte` and, in `UnifiedTranscript.svelte`, the marker row and its "Copy summary" action, so a marker whose summary is absent or blank renders the label only and offers no copy. Those are the summary's only frontend readers (checked 2026-10-12): `unified.ts` reads only the marker's kind, and `transcriptIndex.ts` skips markers. Only the `Compaction` variant changes. The `InheritedHistoryUnavailable` variant that the fork work added stays as it is.
2. **Turn a compaction-only task into a marker.** In `crates/harness/src/codex/session_file.rs`, the reconstruction path (`CodexAgentBuilder` and `handle_event_msg`) currently drops top-level `compacted` records on its `_ => {}` arm. Note each one against the open task. When the task closes, it is a **compaction-only task** if it holds at least one noted record and both of these hold:
   1. It recorded no user prompt. Set the task's prompt flag where the reader already creates the user turn: the legacy `user_message` arm of `handle_event_msg`, and the paginated `UserMessage` arm of the `item_completed` handler, including an attachment-only prompt with empty text. Never set it from `response_item` messages. The reader ignores those on purpose: in paginated files they duplicate the `item_completed` records, and they include context Codex injects with role `user`.
   2. It produced no items.

   The rule is structural on purpose, so it reads every Codex version the same way. A field Codex writes only on regular reply tasks (`turn_attribution` at 0.162.1) was considered and rejected: if a later Codex wrote it on compaction tasks too, every Codex marker would disappear without any test but a live one noticing. Its one benefit was an unseen case, listed under "Known limitations".

   For a compaction-only task, emit one `Turn::System { marker: Compaction { summary } }` per noted record, in order, in place of the agent turn:
   - `started_at` is the record's timestamp.
   - `summary` is `compacted.message` when it is non-empty, otherwise `None`.

   For any other task, discard its noted records, with a comment pointing to the decision above: that compaction was Codex's own, inside a reply, and it shows nothing. A `compacted` record with no open task (three `codex exec` rollouts from 2026-09-27 have one) also shows nothing, as today. Ignore `item_completed` records whose `item.type` is `ContextCompaction`, with a comment saying why: the `compacted` record is the single source.
3. **Drop unfinished agent turns with no prompt and no content.** When a task closes having recorded no user prompt and produced no items:
   - If it is a compaction-only task, emit its markers (step 2), however it closed. One case also keeps the empty agent turn, with status `Failed` and `usage: None`, before the markers: an unfinished task whose `task_started` carries `turn_attribution`. That is a message stopped after Codex compacted at the start of its reply and before Codex recorded its prompt, and its turn holds the message's place in the merge's count-based pairing, as the next bullets explain. Its usage is the compaction's figure, which must not reach the context bar. A finished task never keeps it, so a forward after a compaction is unaffected even if Codex wrote the field on compaction tasks.
   - Otherwise, if it closed with `task_complete`, keep its empty agent turn with status `Complete`, as today. A forward from an idle agent takes the newest finished agent turn (`latest_completed_agent_text`), and an empty one means there is nothing to forward. Dropping it would make the forward send an older reply, while a forward that waited on the same turn live gets nothing. Put that reason in a comment.
   - Otherwise, if its `task_started` carries `turn_attribution`, keep its empty agent turn with status `Failed`, as today. Codex 0.162 writes that field on every regular reply task and never on a compaction task, so this is a message stopped before Codex recorded its prompt. The merge's count-based pairing (`classify_turns_by_count`) needs its turn: without it, the stopped message counts as still waiting, and a reply to a prompt typed in Codex's own interface pairs with it while that prompt is hidden. Unlike the marker rule, this may rest on the field, because if Codex ever wrote it on compaction tasks, a cancelled compaction would only show an empty failed row again, as it did before this milestone.
   - Otherwise (an unhandled `turn_aborted`, the next `task_started`, or end of file), emit nothing. The reader marks those turns `Failed`, and forwarding reads only finished turns, so dropping them can't change what a forward sends.

   A dropped task's token usage must not become any turn's usage, which is what keeps the reopened context bar on the last real turn. A normal turn records its user prompt inside its task (`task_started` comes before the prompt), so a cancelled normal turn still renders as it does today.
4. **Leave the live path alone.** `parse_session_content`, which the adapter runs at the end of every live turn, keeps its current behavior. Taking its per-turn usage from the last `token_count` already reflects a mid-turn compaction correctly, because that count comes from a real model call made after the compaction.
5. **Forks need no code of their own.** `load_inherited_history` (`codex/inherited_history.rs`) rebuilds each ancestor's segment with `parse_codex_transcript_content`, so steps 2 and 3 apply to every segment of a fork's chain. If a segment's cut-off falls inside a task, step 3's end-of-file close applies to it. Confirm this with the fork test below rather than assuming it.

**Definition of done:**

- Fixture-driven unit tests in `codex/session_file.rs`, using records copied from real 0.162.1 rollouts (capture fresh ones with the probe in "Prove it works" below, and sanitize them):
  1. A task with a user prompt yields its agent turn with all its items and no marker, wherever its compaction falls: before its first item, between items, or after its last.
  2. A compaction-only task yields one marker and no agent turn, and the following turn's usage is unaffected.
  3. A task with no prompt and no content, ending in `turn_aborted`, yields nothing.
  4. A paginated compaction-only task containing both `compacted` and the `ContextCompaction` item yields one marker.
  5. A legacy-mode compaction-only task containing only `compacted` yields one marker.
  6. A file holding both kinds yields markers only for its compaction-only tasks, in order.
  7. A non-empty `compacted.message` becomes the summary, and an empty one becomes `None`.
  8. A cancelled normal turn (with a user prompt) renders as it does today.
  9. A task that completed with no prompt, no content and no compaction yields its empty agent turn with status `Complete`, as today. The same task cut off by the end of the file yields nothing.
  10. A paginated task with a prompt, a compaction and no reply content yields the prompt and no marker, so the prompt flag alone decides. Repeat it with an attachment-only prompt (empty text).
  11. A `compacted` record before any task yields nothing.
  12. A task whose `task_started` carries `turn_attribution`, stopped or cut off with no prompt and no content, yields its empty agent turn with status `Failed`.
  13. A compaction-only task stopped after its `compacted` record, or cut off by the end of the file, still yields its marker.
  14. A task whose `task_started` carries `turn_attribution`, with a `compacted` record and a `token_count` and no prompt, yields its empty agent turn (status `Failed`, no usage, its own start time and hydration key) and then the marker when stopped, and only the marker when finished.
- Forwarding tests next to the existing one in `codex/session_file.rs`, using `latest_completed_agent_text`:
  - A rollout whose last task is a completed compaction returns the reply before it. So does one whose last task is a cancelled compaction.
  - A rollout whose last task completed with no prompt, no content and no compaction returns no forwardable text, as today.
- A fork test in `codex/inherited_history.rs`, using that module's staged-rollout helpers: a fork whose original holds a compaction-only task shows the original's turns, then its marker, then the fork's own turns, with no empty agent turn.
- Merge tests in `desktop/src-tauri/src/commands.rs`, through the real Codex parser, in the style of `merge_compaction_session_through_real_parser_does_not_duplicate_prompt`:
  - A compacted Codex session merges without duplicating any turn.
  - A live turn whose reply Codex compacted partway through pairs with its single disk turn by hydration key.
  - The marker doesn't shift provenance correlation, mirroring `merge_compaction_marker_renders_without_shifting_provenance_correlation`.
  - A message stopped before Codex recorded its prompt, followed by a prompt and reply typed in Codex's own interface: with `turn_attribution` the typed exchange renders as imported, including when Codex compacted at the start of the stopped reply, with and without a cancel-time link. Without it, a characterization test pins the known misgrouping, with and without a cancel-time link.
- Frontend tests:
  - `CompactionMarker` renders the label alone, with no expand control and no copy action, when the summary is `null`.
  - The existing Claude marker tests pass unchanged.
- Update the Claude Code tests that construct `Compaction { summary }` to the new type. Their assertions don't change.
- Docs:
  - In `docs/harness-behavior.md`, close G34 with the scope decided above: a compaction run as its own turn shows a summary-less marker, and a compaction Codex runs on its own partway through a reply shows nothing, by decision.
  - Update §3.9's "Verified adjacent defect" paragraph and the paragraph after it to describe the current behavior, not the defect. In "How common it is", split the 117 compactions that "open their own turn": that count mixed compactions run as a turn of their own with ones at the start of a reply that has a prompt and content. A 2026-10-12 recount found 81 of the first kind and 40 of the second.
  - Add a README "Harness support and limitations" line: a Codex compaction marker shows no summary because Codex encrypts it, and a compaction Codex runs on its own partway through a reply shows no marker.

## Milestone 2: the Codex adapter can compact a session through a short-lived app-server

**Goal.** `CodexAdapter::compact` compacts an agent's Codex session by driving a bare `codex app-server`, and returns an event stream with the same contract as Claude Code's compaction. Nothing user-visible changes yet, because the app still refuses Codex compaction until Milestone 3 turns it on.

**Outcomes:**

- Calling `compact` on a Codex agent that has a session compacts it. The session file gains one compaction turn, and the next `codex exec resume` turn continues the conversation with its history summarized.
- The compaction turn records the same sandbox and approval policy as the agent's normal turns.
- The stream ends in exactly one `TurnEnd`:
  - `Completed` when Codex reports the compaction done;
  - `Failed`, carrying Codex's error text, or Switchboard's own sign-in message for an auth failure, when Codex refuses or fails;
  - none at all when cancelled, matching Claude Code, where the dispatcher synthesizes `Cancelled`.
- Cancelling a running compaction stops it cleanly. Codex records an interrupted turn, writes no `compacted` record, and leaves no server process behind.
- The quota read behaves exactly as before, on the shared module.

**Implementation outline:**

1. **Extract the app-server lifecycle into one module, and make the quota read use it.** Create a module under `crates/harness/src/codex/` (for example `app_server.rs`) that owns everything `account_usage.rs` does that isn't specific to quota:
   - spawning a bare `codex app-server` with piped stdio, `kill_on_drop`, `process_group(0)` and the PATH environment;
   - writing newline-delimited JSON-RPC requests in one write and keeping stdin open until the caller is done (the module doc must keep the measured reason: closing stdin makes the server exit without finishing, 0 of 20 runs closed against 20 of 20 open, and for compaction it aborts the work);
   - reading stdout line by line, matching replies by JSON-RPC id, and passing every notification to the caller, including those that arrive while it waits for a reply. An interleaved notification must never shift the match. The quota read discards notifications (`classify_line`), and compaction can't: its turn's notifications can arrive before the reply to `thread/compact/start` (step 2);
   - draining stderr into a tail for error messages;
   - teardown through `terminate_then_kill` on every path. Each caller runs the server on a task it owns, so a dropped caller can't strand it: the quota read keeps its drop-guarded task, and the compaction runs on the adapter's producer task, like every dispatch.

   `account_usage.rs` keeps its request, response parsing and error types, and becomes a client of the module. This is a behavior-preserving refactor, so its tests in `crates/harness/tests/codex_account_usage.rs` and its unit tests must pass unchanged. Any future app-server feature uses this module instead of starting its own server code.
2. **Implement `CodexAdapter::compact`** in `crates/harness/src/codex/mod.rs`, on the shared module:
   - **Refuse before spawning** with `DispatchError::InvalidAgentState` when the agent has no `SessionLocator::Codex`, or when `locate_session_file` finds no rollout for it. This mirrors `ClaudeCodeAdapter::compact`. Codex itself answers `thread not found` for an unknown session.
   - **Send one write of three requests, then keep stdin open:**
     1. `initialize`, with the same `clientInfo` the quota read sends.
     2. `thread/resume` with:
        - `threadId` and the agent's `cwd`;
        - `excludeTurns: true` (without it the reply carries the whole history, and Codex says that mode is deprecated);
        - the agent's `model`, and its effort through `config`, mirroring the `-c model_reasoning_effort=…` that `build_args` passes to `codex exec`;
        - `sandbox: "danger-full-access"` and `approvalPolicy: "never"`, the settings `--dangerously-bypass-approvals-and-sandbox` gives `codex exec` in `build_args`. Codex records each turn's settings in its `turn_context`, and the agent card's Environment section shows the latest turn's. Without these two fields the compaction turn records sandbox `read-only` (both 2026-10-11 probe rollouts did), and after a reopen the card would say the agent is read-only until its next reply. Both fields are in 0.162.1's `ThreadResumeParams` schema.
     3. `thread/compact/start` with `threadId`.
   - **Bound only the startup.** All three requests must be answered within a 30-second startup bound, injectable in tests as the quota read's timeout is. The bound covers only work that doesn't grow with the conversation. Measured 2026-10-12 on copies of real rollouts in a scratch `CODEX_HOME`: `initialize` plus `thread/resume` with `excludeTurns: true` answered in 0.08 to 0.15s for both a 237 KB and a 13.5 MB rollout, and in 0.09s with an MCP server configured that never answers, and through the adapter on a copy of the largest rollout (214.8 MB) the compaction turn started 1.7s after launch, so resuming doesn't wait for MCP servers to start. Codex queues the compaction and answers `thread/compact/start` at once (0.24s in the probes). After that the compaction runs as a turn, and like any Switchboard turn it has no overall bound: the user can always press Stop.
   - **Follow the compaction turn.** The compaction turn is the first `turn/started` after the requests are written, whether it arrives before or after the reply to `thread/compact/start`. Codex 0.162.1 queues the compaction before it sends that reply (`thread_compact_start_inner`), so the turn's notifications, or the whole turn, can come first. The probes happened to see the reply first. A bare server resumed with `excludeTurns: true` runs no other turn. Match `item/completed` and `turn/completed` to that turn by thread id and turn id. Success needs all three, in any order: the `{}` reply, an `item/completed` whose `item.type` is `contextCompaction`, and `turn/completed` with `status: "completed"`. Emit `AdapterEvent::Liveness` for that turn's notifications, so the row shows activity. Ignore every other notification, including `thread/tokenUsage/updated` (its figure is the one the context-bar decision rejects), `mcpServer/startupStatus/updated`, `account/updated`, `remoteControl/status/changed` and `deprecationNotice`.
   - **Report the earliest request's failure.** The three requests go out in one write, so when `thread/resume` fails, `thread/compact/start` fails too, with `thread not found`. Report the error of the earliest request that failed: `initialize`, then `thread/resume`, then `thread/compact/start`. Codex runs requests about one thread one at a time, so the resume's reply comes first in practice, and this rule makes the order irrelevant. The quota read follows the same rule for its handshake (`a_rejected_handshake_reports_its_own_reason_not_the_downstream_complaint`).
   - **Map the outcome to the terminal `TurnEnd`** as follows. On every terminal, `usage` is `None`, and `model`, `effort`, `stable_message_id` and `first_message_id` are `None`. Codex's error text goes through `unwrap_error_message` (`codex/parser.rs`), as on the `codex exec` path. It leaves plain text unchanged.

     | What happened | `TurnEnd` outcome |
     |---|---|
     | The reply, a completed `contextCompaction` item, and `turn/completed` `completed`, in any order | `Completed` |
     | `turn/completed` `failed` | `Failed`, classified from the turn error's `codexErrorInfo`. `usageLimitExceeded` is `UsageLimit`, with Codex's text. `unauthorized` is `AuthFailure`, with `CODEX_AUTH_MESSAGE`, the same text a send shows for the same problem. Both variants exist in the 0.162.1 schema's `CodexErrorInfo`. Anything else is `HarnessError`, with Codex's text. |
     | `turn/completed` `interrupted`, when Switchboard didn't cancel | `Failed{HarnessError}`, with Codex's error text when the turn carries one, otherwise "compaction was interrupted". The schema allows this status, and nothing has shown when Codex sends it unprompted. |
     | `turn/completed` `completed` with no `contextCompaction` completed | `Failed{HarnessError}`, "compaction did not run". Fail closed, as Claude Code's no-verdict case does. |
     | JSON-RPC error reply to `thread/resume` or `thread/compact/start` | `Failed{HarnessError}` with Codex's message, from the earliest failing request. A fork whose original's rollout is missing lands here: `thread/resume` answers `invalid paginated history lineage for <id>: missing source rollout` (verified 2026-10-12 on a copied fork). |
     | Error reply to `initialize` | `Failed{AdapterFailure}` |
     | stdout ends or the server exits before the compaction turn completes | `Failed{AdapterFailure}` |
     | Startup bound exceeded | `Failed{AdapterFailure}` |
     | `options.cancel_token` fires | Tear the process group down and end the stream with no terminal |

   - **Never touch the shared background daemon.** Switchboard must not use `codex app-server proxy`, the control socket, or `--managed-daemon`. A bare stdio `codex app-server` is its own process, and the quota read already relies on that.
3. **Keep `context_report` refused for Codex.** Only `compact` changes.

**Definition of done:**

- Fixture-driven tests through `fake_codex` in `app-server` mode, which replays a fixture line by line while the client matches by id. Put them in a new `crates/harness/tests/codex_compaction.rs` or in `codex_adapter.rs`:
  - The success sequence ends in one `TurnEnd{Completed}` with `usage: None`, preceded by `Liveness`.
  - `turn/started` arriving before the reply to `thread/compact/start` still ends in one `TurnEnd{Completed}`.
  - The whole compaction turn arriving before that reply ends in one `TurnEnd{Completed}` once the reply arrives.
  - `turn/completed` `failed` with an error message gives `Failed{HarnessError}` with that message, unwrapped when it is JSON-wrapped.
  - A usage-limit turn error gives `Failed{UsageLimit}`.
  - An `unauthorized` turn error gives `Failed{AuthFailure}` with `CODEX_AUTH_MESSAGE`.
  - `turn/completed` `interrupted` that Switchboard didn't request gives `Failed{HarnessError}`.
  - A `thread not found` error reply gives `Failed{HarnessError}` naming it.
  - A `thread/resume` error reply followed by `thread/compact/start`'s `thread not found` gives `Failed` with the resume's message. One such fixture uses the missing-original lineage error above.
  - A completed turn with no `contextCompaction` item gives `Failed`, "compaction did not run".
  - The server exiting mid-compaction gives `Failed{AdapterFailure}`.
  - With `// hang`, firing the cancel token ends the stream with no terminal and leaves no process in the group (the existing `// pgid_to:` directive supports that check).
  - An unanswered startup gives `Failed{AdapterFailure}` within the bound.
  - Interleaved notifications before and between the replies don't break id matching.
- A test that the requests carry `excludeTurns: true`, the agent's model and effort, `sandbox: "danger-full-access"`, `approvalPolicy: "never"` and the right `threadId`. If `fake_codex` can't record its stdin, add a directive that writes stdin to a file.
- Refusal tests mirroring `compact_refuses_an_agent_with_no_session_locator` and `compact_refuses_an_agent_with_no_session_file`.
- The quota read's tests pass unchanged, and `live_codex_account_usage_read_returns_named_buckets` passes.
- `compact_is_refused_as_an_unsupported_operation` in `crates/harness/tests/codex_adapter.rs` asserts the old behavior for Codex, which this milestone deliberately changes. Replace its Codex case with the tests above, and keep the Antigravity refusal test.

## Milestone 3: Codex agents offer "Compact context"

**Goal.** A user can compact a Codex agent's conversation from Switchboard the same way as a Claude Code agent's. The dispatcher queue, app gate, live row and post-compaction hook are reused unchanged.

**Outcomes:**

- A Codex agent's menu, and the button on its context bar, offer "Compact context", exactly as for a Claude Code agent. Antigravity agents still don't offer it.
- The action queues like a message: it runs at once on an idle agent, behind the running turn and earlier queued messages on a busy one, and Stop cancels it.
- The live row shows "Compacting context…", then "Context compacted". It shows no before/after token counts, because Codex reports none that mean anything. A failure shows "Compaction failed" with Codex's reason.
- The row is the record of the compaction for the rest of the session. After a restart, the Milestone 1 marker sits where the row was.
- The context bar doesn't move until the agent's next reply.
- The agent's next turn continues the conversation normally and still runs commands without asking for approval. The agent card's Environment section keeps showing the agent's real sandbox and approval policy after the compaction.

**Implementation outline:**

1. Make `HarnessKind::supports_manual_compaction` (`crates/core/src/harness.rs`) true for `ClaudeCode` and `Codex`, and update its doc comment to say why Codex qualifies (a mechanism-verified compaction, not a prompt). Make the frontend mirror `supportsManualCompaction` (`desktop/src/lib/harnessCapabilities.ts`) match.
2. Nothing else in the dispatcher or app is expected to change. `compact_agent_impl` gates on the predicate, `ProjectDispatchContextFactory::preflight` takes the Codex session lock for a compaction exactly as for a send, and `drain_turn` journals nothing for a compaction. Confirm each against the code rather than assuming.
3. The frontend row already renders the label alone when the counts are absent (`CompactionTurn.svelte`). Confirm it for a Codex agent. Leave the post-compaction hook in `installForkHistoryRefresh` (`desktop/src/lib/state/workspace.svelte.ts`) unchanged, and don't widen it to Codex agents (see the last decision above).
4. Update the docs to describe what ships:
   - `docs/harness-behavior.md` §3.9: the status line, and "What Switchboard ships", with a Codex part covering the protocol, the outcome mapping, its residuals, and that compactions Codex runs on its own show no marker.
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
  - A component-level test through the real stores, per `AGENTS.md`'s rule for IPC-wrapping components. Use a project with only a Codex agent, and the fingerprints the backend really returns for one (`refresh_capable: false`, `fingerprint: null`). A compaction completes, and the row reads "Context compacted" with no history reread (`load_project_conversation` isn't called again). A message queued behind it then streams and ends, and its prompt and its reply each show once.
- Live tests in `crates/harness/tests/live.rs`, named `live_codex_…` so `make test-live-codex` runs them:
  - `live_codex_compact_succeeds_and_the_conversation_continues`:
    1. Dispatch a turn that asks the model to remember a code word.
    2. Compact through the adapter, and assert `TurnEnd{Completed}`.
    3. Assert the raw rollout gained a `compacted` record, and that the compaction turn's `turn_context` has the requested model and the same `sandbox_policy` and `approval_policy` as the turn before it.
    4. Assert `load_codex_transcript` yields one `Compaction` marker and no empty agent turn.
    5. Dispatch a turn that asks for the code word and runs a shell command. Assert the answer is right and the command ran without an approval stop.
  - `live_codex_compact_cancelled_leaves_no_compaction`:
    1. Start a compaction and cancel it on the first `Liveness`.
    2. Assert the stream ends with no terminal, the rollout has a `turn_aborted` and no new `compacted` record, and `load_codex_transcript` shows no empty or failed turn.
    3. Assert the next turn works.
  - `live_codex_compact_of_a_fork_keeps_its_inherited_context`. Codex continues a fork by re-reading its original's rollout up to the fork point, and the compaction reaches the fork through `thread/resume` rather than `codex exec`. Follow `live_codex_fork_loads_the_parents_history_up_to_the_fork_point` for setup:
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

   Then load the session through `load_codex_transcript` and print the turns. Expect the two prompt-and-answer pairs, one marker and nothing else. Copy these records, sanitized, as the Milestone 1 fixtures. Also load a real rollout in which Codex compacted on its own partway through a turn (the developer's machine has many), and confirm its turns read exactly as before, with no marker. Then run the app (`make dev`), add a project for the scratch folder with a Codex agent attached to that session, and look at the transcript: one "Conversation compacted" marker with no expand control, and no empty turns.
2. **Milestone 2.**
   1. Run the three live tests from Milestone 3 against the adapter as soon as `compact` exists, including the fork test. Compacting a fork is unprobed. Resuming one through a bare app-server works: verified 2026-10-12 on a copy, the fork resumed in 0.12s, and with its original's rollout removed it failed with the lineage error above. If the fork test fails, stop and take it to the developer: refusing compaction for Codex forks would remove the action for some agents, which is a product decision.
   2. Read the rollout yourself. It has one new compaction turn, whose `turn_context` names the requested model, sandbox `danger-full-access` and approval policy `never`. After a cancel, it has a `turn_aborted` and no `compacted` record.
   3. Measure compaction on a long conversation. Copy the largest real rollout (214.8 MB on the developer's machine on 2026-10-12) into a scratch `CODEX_HOME` with `auth.json` and `config.toml`, compact it through the adapter, and record the time in §3.9. Then delete the scratch home. This costs one compaction's quota and never touches the real session.
   4. After each run, check with `ps` that no `codex app-server` process is left.
3. **Milestone 3.** In `make dev`, create a Codex agent and send it a few turns. Then:
   1. Use "Compact context" from its menu, and watch the row go from "Compacting context…" to "Context compacted".
   2. Confirm the context bar didn't move, and that the agent card's Environment section still shows sandbox `danger-full-access`.
   3. Send another message, and confirm the bar updates and the agent still runs a shell command without asking.
   4. Start a compaction, and while it runs, send the agent a message that asks for a long answer, so it queues behind the compaction. Confirm the reply shows once when it ends, then switch to another project and back, and confirm it still shows once.
   5. Quit and restart the app, and confirm the compaction's row is replaced by one "Conversation compacted" marker at the same place, with no empty turn. This also covers Milestone 1's in-app check, which was deferred to here: the marker is the label alone, with no expand control and no copy button.
   6. Queue a compaction behind a running turn, then cancel it.
   7. Start another and press Stop while it runs. Reopen the project, and confirm no failed or empty turn appears.

## Known limitations to record, not fix

Record each of these in `docs/harness-behavior.md` §3.9, and the user-facing ones in the README:

- The Codex marker shows no summary, because Codex encrypts it.
- A compaction Codex runs on its own, inside a reply, shows no marker. Claude Code shows its own, with its summary.
- An automatic compaction at the start of a reply shows a marker when the reply's prompt never gets recorded: a Codex hook rejected it, a Stop landed after the compaction finished and before Codex recorded the prompt, or the file ends there. Nothing structural tells it apart from a compaction run as its own turn. After a Stop in current Codex files, the stopped message's empty failed reply shows beside the marker. The marker sits where the compaction ran. If that reply had finished, a forward from the agent afterward sends the reply before it, while a forward that waited on the reply live gets nothing. No such task appears in the developer's rollouts.
- In a file written before Codex recorded `turn_attribution`, a message stopped before Codex recorded its prompt leaves no turn, so a reply to a prompt typed into that session from Codex's own interface pairs with the stopped message and the typed prompt is hidden. A message stopped before Codex started its turn already did this, in every file.
- A Codex compaction row shows no before/after token counts, and the context bar updates only on the next reply.
- A compaction's cost appears in no figure Codex reports, so Switchboard can't show it.
- During the session, the row stands in for a manual Codex compaction. Its marker appears when the project is next loaded in full, in practice after a restart. In a project that also has Claude Code agents, a later reread can show the marker under the row.
- The app-server protocol is marked experimental by OpenAI. The live tests are the drift detector, as they already are for the quota read.
