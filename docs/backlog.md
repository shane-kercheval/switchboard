# Deferred improvement ideas

This file lists improvements to Switchboard that were deferred on purpose, each with enough evidence that you can pick one up later without re-deriving it. Nothing listed is scheduled.

Two kinds of item go elsewhere:

- A capability one harness offers that Switchboard hasn't built goes in the capability watchlist, `docs/harness-behavior.md` §8, with the signal a new harness release would have to show. The rows waiting there include reading Claude's account usage while no turn is running, and a Tools line on Codex agents' Environment card.
- A known bug in how Switchboard handles a harness goes in the gap register, `docs/harness-behavior.md` §4.

Each entry has the same five parts: what a user would get, what we know and how we know it, why it waits, the approach recorded so far, and where the detail is. When an entry is built or dropped, delete it.

## Show a pending state wherever the window waits on slow work

**What a user would get.** Every action that waits on slow work shows that it is working, with a spinner or a control disabled without an error, rather than looking frozen or showing an error before the work finishes. Slow work means a call to an MCP prompt server (a prompt provider added in Settings) or any other round trip from the window to the Rust side that can take noticeable time. Whether any action looks frozen today is not established, because no one has listed or timed these paths.

**What we know.**

- The owner asked for this on 2026-06-21, while walking through the workflows refactor plan.
- One case was built in that refactor: picking a workflow whose prompt comes from an MCP server. If that prompt is not cached yet, the form shows a spinner and "Resolving prompts…" in place of its fields, while the form's fetch (`describe_workflow_form`) waits for, or starts, one prompt sync. If the sync ends without the prompt, the form says whether the server failed, needs sign-in, or lacks the prompt. Read from source on 2026-10-29.
- The prompt layer's latency, read from `PromptService` in `crates/prompts/src/service.rs` on 2026-10-29:
  - `list()` and `get()` read an in-memory cache and return at once.
  - `sync()` and `sync_if_generation()` rebuild the cache by asking every MCP prompt server for its prompts, each server bounded by a timeout.
  - `render()` calls an MCP server's `prompts/get` live.
  - `sign_in_mcp_provider`, `test_mcp_connection` and `test_saved_mcp_provider` also wait on the network.
- The audit has not started, and nothing has been measured.

**Why it waits.** It touches every part of the window, so it was kept out of the workflows refactor to keep that change bounded.

**Approach.** List every action that waits on the Rust side or an MCP server, time the slow ones, and give each a pending state that is not an error, as the workflow form has.

**Where the detail is.**

- `docs/implementation_plans/2026-06-21-workflows-refactor.md`, the bullet "Pending vs. error for the unresolved state (latency UX)", for why the workflow form's wait is not an error. The plan describes an earlier version of the form's fetch.
- `desktop/src/lib/components/WorkflowComposer.svelte` (the `workflow-resolving` state) and `describe_workflow_form` in `desktop/src-tauri/src/lib.rs`.
- `PromptService` in `crates/prompts/src/service.rs`.

## Free the memory held by transcripts of projects not on screen

**What a user would get.** Switchboard's memory stops growing with each project opened during a session, so a user who keeps the app running for days doesn't need to restart it to get memory back. This assumes transcripts are what grows, which is unmeasured.

**What we know.**

- On 2026-10-03, the installed app's WebKit content process (the macOS process that runs Switchboard's window) used about 2.4 GB of resident memory after two days running. That is one reading, not a trend.
- The window keeps every agent's turns in memory in `transcripts` (`desktop/src/lib/state/index.svelte.ts`), keyed by agent id. Only `unregisterAgents` and the test-only `_testing.reset()` delete entries. Switching projects (`activateProject` in `desktop/src/lib/state/workspace.svelte.ts`) calls neither, so every project opened in a session keeps its transcripts in memory until the app quits. Read from source on 2026-10-03 and again on 2026-10-29.
- `unregisterAgents` runs when an agent is removed (in this window or by another client), when a project is deleted, and when a project is removed while it is loading. Read from source on 2026-10-29.
- It is not shown to be a leak. No heap snapshot has been taken, so how much of the 2.4 GB is transcripts is unknown.

**Why it waits.** It came up during the fix for slow re-rendering of large pastes, and on 2026-10-03 the owner agreed to keep it out of that fix as separate work.

**Approach.** Measure first: take a heap snapshot after each project open and each switch, and see what grows. If transcripts are the cause, drop the transcripts of projects not on screen and reload them from disk when the project is opened again, through the path that loads a project the first time (`hydrateProject` in `workspace.svelte.ts`). Switching back to a project whose load already ran skips that path (`loadStarted`), so dropping a project's transcripts must reset that too (read from source on 2026-10-29).

**Where the detail is.** `transcripts` and `unregisterAgents` in `desktop/src/lib/state/index.svelte.ts`; `activateProject`, `hydrateProject` and `loadStarted` in `desktop/src/lib/state/workspace.svelte.ts`.

## Move the forward lifecycle into the project store, so a failed forward always comes back

A forward sends other agents' latest replies to the agents you pick, with any text you type. If a source agent is still working, Switchboard holds the forward until that reply is done. A prompt forward does the same through a saved prompt, filling the prompt's arguments with the replies.

**What a user would get.** A forward that fails while it is held never loses the text you typed or its error message, and never adds its sources to a different message you started meanwhile.

**What we know.**

- When a held forward fails, the compose bar puts it back so you can edit and resend it: `restoreForward` does this for a plain forward and `restoreForwardPrompt` for a prompt forward, both in `desktop/src/lib/components/ComposeBar.svelte`.
- Every failure reaches them: you cancel the forward, a source fails or has nothing to forward, the call to the Rust side errors, or the composed message is over the 768 KB limit agents accept (`MAX_PROMPT_BYTES` in `desktop/src/lib/promptSize.ts`).
- Both functions run inside the compose bar that sent the forward, and the window destroys and rebuilds the compose bar when you switch projects (`{#key selection.activeProjectId}` in `desktop/src/App.svelte`). The held forward itself survives the switch, because `heldForwards` (`desktop/src/lib/state/heldForwards.svelte.ts`) keeps it in a store keyed by project.
- Three cases lose something:
  1. You switch to another project and back while the forward is held. The restore runs in the destroyed compose bar, so the compose bar you return to shows neither your text nor the error.
  2. Without switching, you start a new message while the forward is held. The restore leaves the new message alone, so none of the forward's text comes back. A plain forward still adds its source chips to the new message.
  3. After a switch away and back, the destroyed compose bar's final save (`persistComposeNow`) writes the out-of-date contents it held into the saved compose draft.
- The comment above `restoreForward` (`ComposeBar.svelte` lines 2629 to 2659 on 2026-10-29) documents all three. Read from source on 2026-10-03 and again on 2026-10-29.

**Why it waits.** Fixing one case at a time would leave the others, and the fix belongs to every forward failure at once. Reviewers and the owner agreed on 2026-10-03 to defer it.

**Approach.** Move the forward's dispatch, hold and restore out of the compose bar into the project-scoped store layer, which survives the rebuild as `heldForwards` does, so nothing depends on the compose bar that sent the forward still existing. Cover all three cases with tests for both plain and prompt forwards.

**Where the detail is.** The comment above `restoreForward` in `desktop/src/lib/components/ComposeBar.svelte`, and `desktop/src/lib/state/heldForwards.svelte.ts`.

## Read Claude's `model_usage_since_last_result` instead of subtracting the saved session total

Since Claude Code 2.1.277, the cost and token totals on a resumed turn's `result` event cover the whole session so far, not the turn. The Claude adapter gets the turn's share by subtracting the total Claude saved in the session file at the end of the previous turn (the last `cost-state` record), which it reads before the turn starts. That is gap G39 in `docs/harness-behavior.md` §4, closed on 2026-09-22. Claude Code 2.1.312 adds `model_usage_since_last_result` to the `result` event. Its name suggests usage since the previous `result`.

**What a user would get.** Nothing they can see: a turn's cost and tokens stay the same. The adapter would read one field instead of reading the session file before each resumed turn.

**What we know.**

- On turns with one `init`-to-`result` cycle, `model_usage_since_last_result` equalled the share the adapter computes by subtraction. Source: the Claude Code 2.1.312 review (2026-10-28), PR #148, section "Not probed".
- It has not been probed on a turn with several cycles. A turn has several when a subagent Claude started in the background finishes: the same `claude -p` process then runs one more cycle per completion (G22 in `docs/harness-behavior.md` §4). The subtraction applies to the final `result`, and whether the new field there covers the last cycle or the whole turn is unknown.
- Anthropic does not document the field (PR #148).
- A CLI older than 2.1.312 sends no such field, so adopting it would raise the version Switchboard assumes from 2.1.277 to 2.1.312, or keep the subtraction as a fallback.

**Why it waits.** It is a simplification with no user-visible gain, and the field is undocumented and unprobed on turns with several cycles.

**Approach.** Probe a turn in which a background subagent finishes, and compare the field on every `result` with the adapter's subtraction. If they agree, replace the subtraction with the field, and keep the live test `live_claude_resumed_turn_reports_its_own_usage_not_the_sessions` (`crates/harness/tests/live.rs`), which checks that two completed turns' costs and output tokens add up to the session's saved total.

**Where the detail is.**

- G39 in `docs/harness-behavior.md` §4: the cumulative totals, the subtraction, and its edge cases (a cancelled turn, a fork's first turn, a `/context` report, a session continued on an older CLI).
- [PR #148, the Claude Code 2.1.312 review](https://github.com/shane-kercheval/switchboard/pull/148), section "Not probed".
- `UsageSeed` in `crates/harness/src/parser.rs`, `read_usage_seed` in `crates/harness/src/claude_code/session_file.rs`, and `read_usage_seed_for` in `crates/harness/src/claude_code/mod.rs`.
