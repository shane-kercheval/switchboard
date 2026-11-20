# Deferred improvement ideas

This file lists improvements to Switchboard that were deferred on purpose, each with enough evidence that you can pick one up later without re-deriving it. Nothing listed is scheduled, and no approach in it is decided.

Each kind of item has one home:

- A bug in a harness, or in how Switchboard handles one (its adapter, stream parser, or session-file reader), goes in the gap register, `docs/harness-behavior.md` §4.
- A harness capability Switchboard hasn't built goes in the capability watchlist, `docs/harness-behavior.md` §8, including one whose availability on that harness is unprobed. The rows waiting there include reading Claude's account usage while no turn is running, and a Tools line on Codex agents' Environment card.
- Everything else goes here, including app bugs outside harness handling.

Each entry has the same five parts: what a user would get, what we know and how we know it, why it waits, a candidate approach, and where the detail is. When an entry is built or dropped, delete it.

## Slow actions have not been checked for a pending state

**What a user would get.** Every action that waits on slow work shows that it is working, with a spinner or a control disabled without an error, rather than looking frozen or showing an error before the work finishes. Slow work means a call to an MCP prompt server (a prompt provider added in Settings) or any other round trip from the window to the Rust side that can take noticeable time. Whether any action looks frozen today is not established.

**What we know.**

- The owner requested this on 2026-06-21 and kept it out of the workflows refactor being planned then. Source: the notes the orchestrating agent (the agent coordinating that work) kept of that conversation, which are outside the repo.
- That refactor built one case: picking a workflow whose prompt comes from an MCP server. If that prompt is not cached yet, the form shows a spinner and "Resolving prompts…" in place of its fields, while the form's fetch (`describe_workflow_form`) waits for, or starts, one prompt sync. If the sync ends without the prompt, the form says whether the server failed, needs sign-in, or lacks the prompt. Read from source on 2026-10-29.
- Where `PromptService` (`crates/prompts/src/service.rs`) can wait, read from source on 2026-10-29:
  - `list()` reads an in-memory cache.
  - `get()` reads the cache for an MCP prompt, and reads every file in the local prompt folders for a local prompt.
  - `sync()` asks every MCP prompt server for its prompts, each bounded by a timeout. `sync_if_generation()` reuses a finished sync instead of rebuilding only when a sync has finished since the caller last checked and that sync covered the current provider configuration. A provider change after that sync forces another rebuild.
  - `render()` calls the server's `prompts/get` only for an MCP prompt, and renders local and built-in prompts in the app.
  - `sign_in_mcp_provider`, `test_mcp_connection` and `test_saved_mcp_provider` wait on the network.

**Why it waits.** It is a sweep across every part of the window rather than one fix, and no slow path has been listed or timed, so its size is unknown.

**A candidate approach.** This is recorded, not decided. One option is to list every action that waits on the Rust side or an MCP server, time the slow ones, and give each a pending state that is not an error, as the workflow form has.

**Where the detail is.**

- `docs/implementation_plans/2026-06-21-workflows-refactor.md`, the bullet "Pending vs. error for the unresolved state (latency UX)", for why the workflow form's wait is not an error. The plan describes an earlier version of the form's fetch.
- `desktop/src/lib/components/WorkflowComposer.svelte` (the `workflow-resolving` state) and `describe_workflow_form` in `desktop/src-tauri/src/lib.rs`.
- `PromptService` in `crates/prompts/src/service.rs`.

## Switching projects keeps every opened project's transcripts in memory

**What a user would get.** Switchboard's memory stops growing with each project opened during a session, so a user who keeps the app running for days doesn't need to restart it to get memory back. This assumes transcripts are what grows, which is unmeasured.

**What we know.**

- The owner reported on 2026-10-03 that the installed app's WebKit content process (the macOS process that runs Switchboard's window) was using about 2.4 GB of resident memory after two days running. No capture was kept, and it is one reading, not a trend.
- The window keeps every agent's turns in memory in `transcripts` (`desktop/src/lib/state/index.svelte.ts`), keyed by agent id. Only `unregisterAgents` and the test-only `_testing.reset()` delete entries. Switching projects (`activateProject` in `desktop/src/lib/state/workspace.svelte.ts`) calls neither, so every project opened in a session keeps its transcripts in memory until the app quits. Read from source on 2026-10-29.
- `unregisterAgents` runs when an agent is removed (in this window or by another client), when a project is deleted, and when a project is removed while it is loading. Read from source on 2026-10-29.
- It is not shown to be a leak. No heap snapshot has been taken, so how much of the 2.4 GB is transcripts is unknown.
- Two guards in `desktop/src/lib/state/workspace.svelte.ts` decide whether a project's transcripts load. `loadStarted` marks a project whose opening path (open the project, list and register its agents, load its transcripts) has run, and switching back to such a project skips that path. `hydrationStarted` makes `hydrateProject`, which loads the transcripts, return `"skipped"` for a project it has already loaded. `maybeRefreshProject` already re-reads a project's transcripts on switch-back when a session file changed, by clearing only `hydrationStarted` and calling `hydrateProject`. Read from source on 2026-10-29.

**Why it waits.** No measurement shows what grows, so there is no evidence yet that freeing transcripts would reduce memory.

**A candidate approach.** This is recorded, not decided. It starts with a measurement: a heap snapshot after each project open and each switch, to see what grows. If transcripts are the cause, one option is to drop the transcripts of projects not on screen and reload them from disk when the project is opened again, through `hydrateProject`.

**Where the detail is.**

- `transcripts` and `unregisterAgents` in `desktop/src/lib/state/index.svelte.ts`.
- `activateProject`, `hydrateProject`, `maybeRefreshProject`, `loadStarted` and `hydrationStarted` in `desktop/src/lib/state/workspace.svelte.ts`.
- The deferral was agreed in review on 2026-10-03. Source: the orchestrating agent's notes of that conversation, outside the repo.

## A forward that fails while held can lose the user's text

A forward sends other agents' latest replies to the agents you pick, with any text you type. If a source agent is still working, Switchboard holds the forward until that reply is done. A prompt forward does the same through a saved prompt, filling the prompt's arguments with the replies.

**What a user would get.** A forward that fails while it is held never loses the text you typed or its error message, and never adds its sources to a different message you started meanwhile.

**What we know.**

- When a held forward fails, the compose bar puts it back so you can edit and resend it: `restoreForward` does this for a plain forward and `restoreForwardPrompt` for a prompt forward, both in `desktop/src/lib/components/ComposeBar.svelte`.
- Every failure reaches them: you cancel the forward, a source fails or has nothing to forward, the call to the Rust side errors, or the composed message is over 768 KiB. Switchboard chose that ceiling to leave room under macOS's limit on a command's arguments and environment (`MAX_PROMPT_BYTES` in `crates/harness/src/adapter.rs`, copied in `desktop/src/lib/promptSize.ts`).
- Both functions run inside the compose bar that sent the forward, and the window destroys and rebuilds the compose bar when you switch projects (`{#key selection.activeProjectId}` in `desktop/src/App.svelte`). The held forward itself survives the switch, because `heldForwards` (`desktop/src/lib/state/heldForwards.svelte.ts`) keeps it in a store keyed by project.
- Three cases lose something:
  1. You switch to another project and back while the forward is held. The restore runs in the destroyed compose bar, so the compose bar you return to shows neither your text nor the error.
  2. Without switching, you start a new message while the forward is held. The restore leaves the new message alone, so none of the forward's text comes back. A plain forward still adds its source chips to the new message.
  3. After a switch away and back, the destroyed compose bar's final save (`persistComposeNow`) writes the out-of-date contents it held into the saved compose draft.
- The comment above `restoreForward` documents all three. Read from source on 2026-10-29.

**Why it waits.** A patch for one case would leave the other two, and the one option recorded for closing all three, below, changes the whole forward lifecycle rather than one failure path.

**A candidate approach.** This is recorded, not decided. One option is to move the forward's dispatch, hold and restore out of the compose bar into the project-scoped store layer, which survives the rebuild as `heldForwards` does, so that nothing depends on the compose bar that sent the forward still existing. Tests for it would cover all three cases, for both plain and prompt forwards.

**Where the detail is.**

- The comment above `restoreForward` in `desktop/src/lib/components/ComposeBar.svelte`, which records the deferral.
- `desktop/src/lib/state/heldForwards.svelte.ts`.
- The agreement to defer all three cases together was made in review on 2026-10-03. Source: the orchestrating agent's notes of that conversation, outside the repo.

## A Claude turn's cost relies on a session-file read that a new field might replace

Since Claude Code 2.1.277, the cost and token totals on a resumed turn's `result` event cover the whole session so far, not the turn. The Claude adapter gets the turn's share by subtracting the total Claude saved in the session file at the end of the previous turn (the last `cost-state` record), which it reads before the turn starts. That is gap G39 in `docs/harness-behavior.md` §4, closed on 2026-09-22. On Claude Code 2.1.312, the `result` event also carries `model_usage_since_last_result`. Its name suggests usage since the previous `result`.

**What a user would get.** Nothing they can see: a turn's cost and tokens stay the same. The adapter would read one field instead of reading the session file before each resumed turn.

**What we know.**

- On turns with one `init`-to-`result` cycle, `model_usage_since_last_result` equalled the share the adapter computes by subtraction. Source: the Claude Code 2.1.312 review (2026-10-28), PR #148, section "Not probed".
- The field is present on 2.1.312 and absent on 2.1.302: PR #148 diffed the keys of every stream event between those two versions. The first release from 2.1.303 to 2.1.312 that carries it was not identified.
- It has not been probed on a turn with several cycles. A turn has several when a subagent Claude started in the background finishes: the same `claude -p` process then runs one more cycle per completion (G22 in `docs/harness-behavior.md` §4). The subtraction applies to the final `result`, and whether the new field there covers the last cycle or the whole turn is unknown.
- Anthropic does not document the field (PR #148).
- A CLI without the field, such as 2.1.302, would need the subtraction as a fallback, or Switchboard would have to require a version that carries it.

**Why it waits.** It would change nothing a user sees, and the field is undocumented and unprobed on turns with several cycles.

**A candidate approach.** This is recorded, not decided. One option is to probe a turn in which a background subagent finishes, compare the field on every `result` with the adapter's subtraction, and, if they agree, replace the subtraction with the field. The live test `live_claude_resumed_turn_reports_its_own_usage_not_the_sessions` (`crates/harness/tests/live.rs`) checks that two completed turns' costs and output tokens add up to the session's saved total, so it would catch a replacement that got a turn's share wrong.

**Where the detail is.**

- G39 in `docs/harness-behavior.md` §4: the cumulative totals, the subtraction, and its edge cases (a cancelled turn, a fork's first turn, a `/context` report, a session continued on an older CLI).
- [PR #148, the Claude Code 2.1.312 review](https://github.com/shane-kercheval/switchboard/pull/148), section "Not probed", where the review declined to adopt the field.
- `UsageSeed` in `crates/harness/src/parser.rs`, `read_usage_seed` in `crates/harness/src/claude_code/session_file.rs`, and `read_usage_seed_for` in `crates/harness/src/claude_code/mod.rs`.

## An agent's card lists Switchboard's own MCP server, which Switchboard turns off

**What a user would get.** An agent's Environment card marks Switchboard's own MCP server (agent access) as off for that agent, rather than listing it among the servers the agent has. Today a user who checks the card after reading in Settings that the agents Switchboard starts don't get the server sees it listed and can conclude the block failed.

**What we know.**

- The setup copy registers the server for every folder: Claude Code with `--scope user`, Codex in `~/.codex/config.toml`. Each Claude Code and Codex run then gets the server turned off (`docs/harness-behavior.md` §3.13).
- The card's MCP list comes from the harness's config files. Codex: `crates/harness/src/codex/config.rs` reads `~/.codex/config.toml` and the folder's `.codex/config.toml`, called from `codex/mod.rs` when the session's details are emitted. Claude Code: when a project opens, `crates/harness/src/claude_code/session_file.rs` calls `claude_code/config.rs`, which reads the user, local, and project scopes. Read from source on 2026-11-14.
- After a live Claude turn, the list comes from Claude Code's startup event instead, and that event leaves a denied server out (observed 2026-11-14 at 2.1.321 with entries at local scope). So a Claude card lists `switchboard` after a project reopens and drops it after the agent's next turn. A Codex card always lists it.
- A server read from a config file is shown by name only, with no status label or dot (`AgentEnvironment.test.ts`, "draws no status dot for config-file servers").
- The Settings copy and the README say that an agent's card lists the MCP servers in its configuration, so it can still show `switchboard` there.

**Why it waits.** The mismatch misleads but changes nothing an agent can do, and a correct label needs the block's own matching to reach the display code, including the transcript-load path, which doesn't have the agent access port today.

**A candidate approach.** This is recorded, not decided. A label must come from the same code that builds the block, so the card never says "off" for an entry the run left on. Two examples: a Codex entry the lookup skips, such as one in a file it can't parse, and a Claude Code `mcp-remote` bridge under another name, which `deniedMcpServers` doesn't match. For Codex that means calling the lookup in `crates/harness/src/codex/own_server.rs`. For Claude Code it means the name and URL patterns in `own_server_settings` (`crates/harness/src/claude_code/mod.rs`), never the bridge's arguments. The card would also need a neutral tone and a label for the new status (`desktop/src/lib/agentEnvironment.ts`).

**Where the detail is.**

- `docs/harness-behavior.md` §3.13, for what each harness turns off and what it misses.
- `desktop/src/lib/agentEnvironment.ts`, for the card's rule of showing what is configured rather than hiding it.

## Agents created or changed over MCP skip the window's effort rules

**What a user would get.** An Antigravity agent created or changed over MCP gets an effort level its model accepts, as one set up in the window does, instead of failing every turn on a level `agy` rejects.

**What we know.**

- `create_agent` in `desktop/src-tauri/src/ops/change.rs` gives a new agent the caller's effort or else the Settings default, whatever the model. `set_agent_model_and_effort` keeps the agent's effort when the caller changes only the model. Read from source on 2026-11-19.
- The window applies two rules that MCP skips, both in `desktop/src/lib/agentSelection.ts`: `selectionForNewAgent` clears the effort for a model that takes none, and `resolveModelChange` clears it or picks a quick choice the new model accepts.
- Three pairs MCP can create today:
  - An orchestrator model with the default `medium`. Probed @ 1.5.12: `agy -p ... --model gemini-pro-orchestrator-neon --effort medium` exits 1 within 4 seconds with `--effort is not supported for model "gemini-pro-orchestrator-neon"`. The sidebar hides the effort control for a model that takes no effort, so the user can't see the setting that fails.
  - GPT-OSS 120B with a default of `high`. `agy` accepts only `medium` for it (`harness-behavior.md` §3.4).
  - Gemini 4 Argon with a default of `xhigh`. `agy models` lists Argon at `low`, `medium` and `high` only. Not probed.
- To recover today, call `set_agent_model_and_effort` with `effort: null`, or switch the agent to another model in the window. The window offers the curated models plus the agent's quick choices, and the orchestrators are not curated. If the orchestrator is among the agent's quick choices, switching back to it clears the effort (`resolveModelChange`); if not, the window can't put the agent back on it. `create_agent` copies the quick choices from the Settings defaults.

**Why it waits.** Reaching any of these needs an MCP caller to pass a model the pickers leave out, or a Settings default the model lacks, and no agent is known to be in this state. A fix touches only the app's ops layer: `create_agent` and `set_agent_model_and_effort` would load `antigravityModels.json`, which in Rust only a test module reads today, and apply the window's two rules. `switchboard-core` stays unchanged.

**A candidate approach.** This is recorded, not decided. Apply `selectionForNewAgent` and `resolveModelChange` in the ops layer, from the same `antigravityModels.json`. Separately, the agent's selection editor could offer to clear an effort its model doesn't take, which would also repair agents already in this state.

**Where the detail is.**

- `desktop/src-tauri/src/ops/change.rs` and `desktop/src/lib/agentSelection.ts`.
- `docs/harness-behavior.md` §3.4, for the per-model levels and the catalog decision.

## Quit does not wait for an agent another action is already stopping

**What a user would get.** Quitting Switchboard always waits until every running agent has stopped, including one that is being removed at that moment, so neither `agy` nor a command it started keeps running after the app exits.

**What we know.**

- Quit (`finish_orderly_quit` in `desktop/src-tauri/src/lib.rs`) builds its lists of agents and projects from `agents_by_id` and `projects`, then runs `drain_agents_then_release_locks`, which calls `Dispatcher::shutdown_agent` for each listed agent. Read from source on 2026-11-20.
- Removing an agent (`remove_agent_impl` in `desktop/src-tauri/src/commands.rs`) stops its actor through `Dispatcher::remove_agent`, which marks the slot `Closing` while it waits, and removes the agent from `agents_by_id` only once the actor has stopped. So quit lists the agent, but its `shutdown_agent` returns at once because the slot is already `Closing` (`begin_close` returns `None`), by design: the other caller owns the drain.
- Deleting a project (`delete_project_impl`) and changing a project's folder (`set_project_directory_impl`) start with `begin_maintenance`, which removes the project from `projects` and its agents from `agents_by_id` before stopping them. So quit does not list those agents at all.
- Either way, a quit that starts while one of those actions is still stopping an agent does not wait for that agent. It can release the project's lock and exit while the agent's turn is still being cancelled, and exiting ends the other action's wait too. What that leaves running is not checked; the expectation is that `agy`, and the commands G52's cleanup would have ended, can keep running.
- The trigger is narrow: an agent with a running turn is removed, or its project deleted or moved, and the user quits within the seconds stopping it takes. Found in review of the G52 fix on 2026-11-20; it predates that change.

**Why it waits.** The fix changes how the dispatcher stops agents for every harness and for each path above, which is outside the change that found it.

**A candidate approach.** This is recorded, not decided. The fix has two parts. First, quit finds every agent the dispatcher is still running or stopping, from the dispatcher's own slots, rather than from `agents_by_id`; alternatively it waits for any project change in progress to finish. Second, a caller that finds a slot already `Closing` waits for a completion signal the first caller fires once the actor has stopped, instead of returning at once. A shared completion signal alone fixes only the removal case. A test would overlap each of the three actions with a quit while the adapter is paused in its kill path, and assert that the quit's drain returns only after the actor has stopped.

**Where the detail is.**

- `Dispatcher::shutdown_agent`, `Dispatcher::remove_agent` and `begin_close` in `crates/dispatcher/src/lib.rs`.
- `begin_maintenance` in `desktop/src-tauri/src/commands.rs`, and `finish_orderly_quit` in `desktop/src-tauri/src/lib.rs`.
- G52 in `docs/harness-behavior.md`, whose quit claim names this exception.

## An agent card does not say it is waiting on a forward an MCP client made

**What a user would get.** When an agent such as an orchestrator calls Switchboard's MCP `forward` tool to send one agent's answer to another, the recipient's card reads "Waiting for" and the source agent's name while the forward waits for that source to finish, as it already does for a forward the user holds in the compose bar.

**What we know.** Read from source on 2026-11-20.

- The card's waiting state (`deriveAgentActivity` in `desktop/src/lib/state/agentActivity.svelte.ts`) reads two things the window holds: forwards held by its own compose bar (`heldForwards.svelte.ts`) and the steps of running workflows (`workflows.svelte.ts`).
- An MCP forward waits inside `ops::send` (`desktop/src-tauri/src/ops/send.rs`): `render_body` composes the body, waiting on a source that is mid-turn, and only after that does the send call `desktop_sync.ensure_ready` and dispatch. No event reaches the window during that wait, so the window has nothing to show until the recipient's turn starts.

**Why it waits.** It needs a new event from the Rust side, and its lifecycle (start, release, cancel, timeout, the window reloading meanwhile) is the larger part of the work. The card already shows the recipient as working once the forward is sent.

**A candidate approach.** This is recorded, not decided. One option is for `compose_forward` to emit a "forward held" event on the project's channel with the forward id, sources and recipients, and a matching release event on every exit path, which the window would merge with its own held forwards.

**Where the detail is.**

- `render_body` and `compose_forward` in `desktop/src-tauri/src/ops/send.rs`.
- `heldForwardTargets` in `desktop/src/lib/state/agentActivity.svelte.ts`.
