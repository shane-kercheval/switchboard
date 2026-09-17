# Manual context compaction for Claude agents

**Status:** proposed · **Created:** 2026-09-16 · **Revised:** 2026-09-16 after two review rounds

Add a "Compact context" action to the agent card menu for Claude agents. Selecting it asks Claude
Code to summarize the agent's conversation so far and continue from the summary — the same thing
`/compact` does in a Claude Code terminal session, and the same thing the Claude desktop app's
compact button does. The action runs as a turn: it queues if the agent is busy, it can be cancelled,
and its outcome shows in the transcript and the sidebar context bar.

The research behind this plan is in [`docs/harness-behavior.md`](../harness-behavior.md) §3.9. The
short version: the Claude CLI's `/compact` slash command works in headless (`-p`) mode, which
Switchboard already uses for every turn, so this needs no new transport. Codex can also be compacted
but only through a protocol Switchboard does not speak, and Antigravity cannot be compacted at all
today — so **this feature is Claude-only**, gated by a per-harness capability predicate like fork and
model selection are.

The whole plan is one feature PR. The milestones are dependency-ordered implementation units.

## Decisions

These cannot be recovered from the code; treat them as settled. Decisions 1–5 came out of the design
discussion; 6–9 are the planner's, made because the code forces them, and reviewed. Where a decision
has a rationale, that rationale must survive into the code (doc comments on the branch that
implements it), not only here.

1. **A compaction behaves like a send.** It is a work item on the agent's per-agent queue: if the
   agent is idle it starts immediately, if busy it queues in FIFO order behind the in-flight turn and
   any earlier queued sends, and it is cancellable through the same Stop / cancel-send affordances.
   No special "disabled while busy" state. The dispatcher's per-agent actor already serializes work
   items, so this is the *cheaper* option, not the more elaborate one.
2. **No confirmation dialog.** Opening the menu and choosing the action is the confirmation.
3. **Default compaction only.** Claude accepts `/compact <custom instructions>`; that is deferred.
   The action sends exactly `/compact`.
4. **The autocompact threshold is not built.** `--autocompact` / `autoCompactWindow` is a separate
   knob (an absolute token cap, not a "compact at N%" setting). Switchboard does not restrict Claude's
   settings sources, so a user who sets `autoCompactWindow` in `~/.claude/settings.json` already
   gets that behavior on every Switchboard dispatch. Nothing to build.
5. **The compact dispatch bypasses the slash-escape by construction, not by exception.** Every prompt
   that reaches Claude passes through `claude_transport_prompt`, which prefixes a space to slash-leading
   text so the CLI does not intercept it as a command. That rule is load-bearing (harness-behavior §0)
   and is also what currently prevents the cross-harness fake-success hazard in §3.9. Do not add a
   "unless it's `/compact`" exception to it. A compaction is a **distinct adapter operation** with
   its own argument builder; the escaping function is never on its path.
6. **A compaction writes nothing to the journal.** No `Send` (there is no user prompt — and the merge
   correlates journaled sends to on-disk prompts by exact text, so a text-less `Send` would be a
   correlation hazard; this is why `claude_transport_prompt` is `pub`), no `TurnLink` (no assistant
   message, so no hydration key), and **no `Outcome` marker on failure or cancel**. The journal's
   outcome markers exist so a user's send is never silently lost from history; a failed or cancelled
   compaction leaves the conversation exactly as it was, so there is nothing to mark. A *successful*
   compaction is durable through the harness session file, which owns the `compact_boundary` and
   recap records and already hydrates as a `Compaction` system marker. Consequence: a failed
   compaction is visible live and gone after restart — the same live-only status queued-but-unstarted
   sends already have.
7. **The compaction turn enters the transcript when it starts, in execution order, and a completed
   one stays as a small row carrying the before/after token counts.** While queued it renders from
   the agent's pending list, like a queued send; the transcript row is created by `turn_start`, never
   at click time (a click-time row lands *before* any send queued ahead of it and would make the
   sidebar read the wrong turn). The completed row is what feeds the sidebar context bar, which reads
   the most recent completed turn's `usage`. The harness-owned recap marker arrives beside it through
   the existing Claude-only refresh path. The two rows are the same split every send has —
   Switchboard-owned event beside harness-owned content — and the row's absence after restart is the
   same live-only status as decision 6.
8. **A queued compaction that is cancelled before it starts disappears without a row.** Nothing ran,
   and there is no user message for a "cancelled" row to sit under. This differs from a cancelled
   queued send, which renders a cancelled row under its prompt; the difference is deliberate.
9. **A compaction is invisible to forwarding.** Forwarding from an agent that is compacting waits for
   the compaction to finish (keeping "one thing at a time" simple) and then resolves exactly as if the
   agent were idle — the latest real answer, read from disk. A failed or cancelled compaction resolves
   the same way, because the conversation is unchanged. A compaction never invalidates a forward.

## Required reading before implementing

- [`AGENTS.md`](../../AGENTS.md) — `make` targets, foreground execution of long commands, the
  test-type vocabulary, the live-test naming convention (`live_claude_…`), and the component-test
  requirement for Svelte components that wrap IPC + events.
- [`docs/harness-behavior.md`](../harness-behavior.md) **§3.9** (the verified event protocol and the
  fake-success hazard — the single most important input to this plan), §0 (the slash-escape rule),
  §1.1 (turn-completion detection, which a compaction must *not* rely on), §3 and G29 (how the
  context bar is derived and why window selection never guesses).
- [`docs/system-design.md`](../system-design.md) §3 (the split source of truth — why a compaction is
  not journaled), §7 (sends, turns, forwarding), §9 (the capability matrix — add a row).
- `crates/core/src/harness.rs` — `supports_session_fork` and its siblings. The doc comment on
  `supports_session_fork` already explains why Codex's app-server operations are a different shape;
  the new predicate makes the same argument.
- `crates/dispatcher/src/lib.rs` — `WorkItem`, `Command`, `run_turn`, `TurnAwaiters`, and the
  terminal handling. Understand where the journal `Send` is written (fail-closed, before spawn), where
  `Outcome` / `TurnLink` are written at the terminal, and where current-turn waiters are fired (at
  the terminal event, with post-terminal arrivals answered from a stash) — a compaction changes all
  three.
- `crates/harness/src/parser.rs` — `ParserState`, `parse_result` (note: a successful `result` is
  **folded** into `pending_completed_terminal` and emitted by the adapter at EOF via
  `take_final_turn_end`; only failures fail fast), `select_context_window`,
  `extract_usage_from_result`, `parse_assistant_envelope` (auth stash, id tracking, synthetic text),
  and `parse_stream_event` (the delta layer).
- `crates/app/src/commands.rs` — `send_message_impl`, `fork_agent_impl`, `busy_fork_source`, and
  `resolve_source` (the forward path: a completed current turn's live text is authoritative and an
  empty source invalidates the forward; an idle source reads disk).
- `src/lib/state/index.svelte.ts` (`dispatchUserTurn`, `recordSendAccepted` — pending-send entries
  are registered **before** IPC and receive their receipt after), `src/lib/state/reducers.ts` (the
  `turn_start` / `message_failed` / `message_cancelled` arms and `pickPendingIndex`'s front-entry
  fallback), `src/lib/components/UnifiedTranscript.svelte` (`queuedSendIds`, `queuedRow`),
  `src/lib/components/Sidebar.svelte` (the agent-card menu and `contextUtilization`), and
  `src/lib/components/CompactionMarker.svelte` (the existing harness-owned marker; the new turn row is
  a sibling, not a replacement).

The Claude Code product docs do not document `/compact`'s headless behavior; harness-behavior §3.9 is
the reference, and the captured fixtures below are its evidence.

## Captured fixtures (prerequisite — already done)

Both streams were captured on 2026-09-16 @ Claude 2.1.270 with **Switchboard's exact argv** (the
flags `build_args` emits, including `--include-partial-messages`, which is what turns streaming deltas
on):

```
claude -p --output-format stream-json --verbose --include-partial-messages \
  --dangerously-skip-permissions --no-chrome --add-dir / --resume <sid> -- /compact
```

Raw captures: `/tmp/cc_compact_probe/compaction-success.jsonl` and
`/tmp/cc_compact_probe/compaction-too-small.jsonl`, plus the resulting session file under
`~/.claude/projects/-private-tmp-cc-compact-probe/`. Sanitize per the existing fixture conventions
and land them under `crates/harness/tests/fixtures/claude/`. If they have been cleaned up, re-capture
with the argv above (a one-turn session; the failure shape is produced by compacting a session that
has already been compacted once — a fresh one-turn session compacts successfully).

What they show, and what the parser design below depends on:

| | success | failure ("Not enough messages to compact.") |
|---|---|---|
| verdict | `system/status {status:null, compact_result:"success"}` | `… compact_result:"failed", compact_error:"…"` |
| `system/compact_boundary` | present, `compact_metadata {trigger:"manual", pre_tokens, post_tokens, cumulative_dropped_tokens, duration_ms, …}` | absent |
| assistant envelope | **none** | one, `model:"<synthetic>"`, **with a `message.id`**, text = the error |
| streaming deltas | **none** | **none** |
| `result` | `subtype:"success"`, `is_error:false`, `result:""`, `num_turns:0`, `total_cost_usd` real (0.10–0.20 on a 1–2 turn session), `modelUsage` **one entry** with `contextWindow`, `result.model` **absent**, `result.usage` all zeros | `subtype:"success"`, `is_error:false`, `result:"Not enough messages to compact."`, `total_cost_usd:0`, `modelUsage` **empty** `{}`, `result.usage` all zeros |
| after `result` | `system/init` re-emitted (carries `model`), then the recap `user` records | `system/init`, then the envelope, then `result` |

Two consequences worth stating up front: `result` reports success in both cases (the verdict is the
status pair, never `result`); and on failure the empty `modelUsage` falls through to the zero-valued
`result.usage`, which is *schema-present*, so `extract_usage_from_result` returns a `Some` with no
window — the shape that would blank the context bar if emitted (see milestone 1).

## Milestone 1 — Capability predicate and the Claude adapter operation

### Goal & Outcome

Give the harness layer a first-class "compact this session" operation that the Claude adapter
implements against the verified protocol, with the capability declared where every other per-harness
capability lives.

- `HarnessKind` answers `supports_manual_compaction()`: Claude `true`, Codex and Antigravity `false`,
  with the reasons in the doc comment.
- `HarnessAdapter` exposes a compaction operation distinct from `dispatch`. The Claude adapter spawns
  `claude` with the agent's normal per-dispatch flags and the bare, **unescaped** `/compact` positional.
  Codex and Antigravity refuse with a typed error.
- The stream a compaction produces is parsed into the same `AdapterEvent` vocabulary a send produces,
  ending in exactly one `TurnEnd` whose outcome is **read from the compaction verdict**, never from the
  CLI's `result` record; an auth failure still classifies as `AuthFailure` with the authored message.
- A successful compaction's `TurnEnd.usage` carries the before/after occupancy and, when it can be
  resolved without guessing, the context window; a failed one carries no `usage` at all.
- A live test proves the real CLI still emits this shape.

### Implementation Outline

**Capability predicate.** Add `supports_manual_compaction` to `HarnessKind` beside
`supports_session_fork`, same authority-and-exhaustiveness role. The doc comment must say why each
`false` is false: Codex's compaction exists only as the experimental app-server
`thread/compact/start` RPC, a transport the Codex adapter does not run (the same reasoning the fork
predicate gives for `thread/fork`); Antigravity's `/compact` is present in the binary but behind a
remote feature flag that is off; and a `/compact` *prompt* to either harness is answered by the model
claiming success while nothing compacts. Point at harness-behavior §3.9.

**Adapter contract.** Add a compaction method to `HarnessAdapter` alongside `dispatch`, taking the
agent, cwd, turn id, and `DispatchOptions` — no prompt, no attachments. It returns the same
`EventStream` with the same contract (exactly one terminal `TurnEnd`; the adapter synthesizes
`AdapterFailure` if the process dies without one). Give it a default implementation that returns a
new `DispatchError` variant meaning "this harness has no such operation", so Codex, Antigravity, and
the mock adapter get the refusal for free; the app layer's predicate gate is the real guard and this
is defense in depth. Chosen over threading a "kind" through `dispatch`'s `prompt` parameter: `dispatch`
means "a user-initiated turn with a prompt" everywhere it is read, and a compaction is not that.

**Claude argument builder.** The compaction's argv must be **identical to a send's** — resume flag,
model, effort, chrome, `--add-dir`, permissions, `--include-partial-messages` — differing only in the
positional, which is the literal string `/compact` with no leading space. Build it by sharing the
base-flag construction with the send path rather than duplicating the flag list; the test in
Definition of Done pins the "identical except the positional" property so the two cannot drift.
Running under the agent's selected model is deliberate: the summarization call is billed and the user
chose that model for this agent.

A compaction is never a session's first dispatch (there would be nothing to compact), so the builder
always takes the `--resume` branch. The app layer (milestone 3) refuses before spawn if the agent has
no established session; the adapter additionally fails closed with `InvalidAgentState` if asked to
compact a session that does not exist, rather than letting the CLI mint a fresh session containing
only a failed compaction.

**Parser: a compaction mode.** The stream parser must know it is reading a compaction. `ParserState`
gains a mode flag set by the compaction operation, and the following rules apply only in that mode;
an ordinary send's handling of the same records is unchanged and must be pinned by tests.

1. *The verdict is the `system/status` pair, and it decides the terminal at `result` — within the
   parser's existing terminal shape.* Record `compact_result` / `compact_error` from the status
   record. When `result` arrives, **after** the existing auth-stash check (so an auth-failed
   compaction stays `AuthFailure` — it has no verdict, and must not fall into the branch below):
   - `"failed"` verdict → the existing fail-fast return, `Failed { HarnessError, message: compact_error }`.
   - **no verdict seen** → also fail fast, `Failed { HarnessError, message: "compaction did not run: <result text>" }`.
     Fail-closed by construction: this covers `DISABLE_COMPACT` and any future shape where the
     command did not run. It is a *named* harness error rather than letting the stream end with
     nothing pending and the adapter's EOF truncation synthesis produce an `AdapterFailure` —
     `FailureKind` drives the frontend's recovery copy, and "adapter failure" would tell the user to
     file a bug for something Claude declined.
   - `"success"` verdict → the normal fold into `pending_completed_terminal`, so the adapter's EOF
     emission and exit-status gating apply exactly as for a send.
   Outside compaction mode these status records remain `Liveness`, as today.
2. *Envelope handling keeps diagnostics and nothing else.* The failure path emits a `<synthetic>`
   assistant envelope that carries a `message.id`, text equal to the error, and a zero-valued `usage`
   object. In compaction mode `parse_assistant_envelope` still runs the auth-failure stash and the
   model keep-last (so `AuthFailure` classification and `TurnEnd.model` are preserved), and **skips**:
   synthetic text emission (the verdict already carries the message — emitting it too double-renders),
   the `TurnIdentity` push **and the id tracking behind it** (`first_assistant_message_id` /
   `last_assistant_message_id` stay `None`, so `TurnEnd.first_message_id` / `stable_message_id` are
   `None` on every terminal path — a synthetic id must never become a hydration key or a sidecar join
   key), tool extraction, and `track_assistant_context_usage` (the zero-valued `usage` would set
   occupancy to `Some(0)`, and a window resolving beside it would render a confident 0%). The delta
   layer (`parse_stream_event` text deltas) is gated too; the captures show no deltas on either path,
   so this is drift protection, and cheap.
3. *Usage is verdict-gated.* On **success**, build `TurnUsage` with `context_input_tokens` ←
   `compact_metadata.pre_tokens` and `context_tokens_after_turn` ← `post_tokens` (the fields' real
   meanings: occupancy fed in, occupancy after; `context_tokens_after_turn` is what the sidebar bar
   reads), token totals and `total_cost_usd` from `result` as usual, and `spend` stamped as usual (an
   overage compaction is real spend). On **failure**, emit `usage: None`. Rationale, which must be on
   the branch: the failure shape yields a schema-present zero usage with no window, and the sidebar
   treats the newest usage-bearing turn as authoritative — emitting it would *hide the bar after a
   no-op*, when the previous turn's number is still exactly right because nothing was compacted.
   (Cancelled terminals are synthesized by the dispatcher with no usage already.)
4. *Window resolution never guesses (G29).* A compaction has no parent assistant envelope, and
   `result.model` was absent in both captures, so the existing chain would reach the sole-entry
   fallback. Add one exact-key source ahead of it: the model announced by the same dispatch's
   post-compaction `system/init`. `parse_system_event` currently takes no state and discards that
   model after building `SessionMeta`; give it `&mut ParserState` and stash the init model. Resolution
   order in compaction mode: init model exact key (with the existing `[1m]` key normalization) →
   `result.model` exact key → sole entry **only when neither identifier exists** → none. An identifier
   that names model A while the sole entry is model B resolves to **none** — fail closed, never bind
   the wrong window. When no window resolves on success, keep `usage` (the row's numbers come from
   `compact_metadata`, not the window) with `context_window: None`; the bar clean-hides until the next
   turn, which is honest — showing the pre-compaction percentage after a successful compaction would
   be a wrong number.

`SessionMeta` from the post-compaction `system/init` and the `RateLimitEvent` flow through unchanged.

### Definition of Done

- Unit tests (harness crate):
  - `HarnessKind::supports_manual_compaction` per variant (Claude only), matching the sibling tests.
  - The compaction argv equals the send argv for the same agent except the positional, and the
    positional is exactly `/compact` — proving the escape is not on the path. Cover the
    no-session case failing closed.
  - Parser, from the fixtures: success → folds, emits `Completed` at EOF with occupancy from
    `compact_metadata` and `context_window` resolved (via init model), `first_message_id` /
    `stable_message_id` `None`; a folded success still fails on non-zero exit; failure → fail-fast
    `Failed { HarnessError }` with the CLI's message, `usage: None`, **no** `ContentChunk`, **no**
    `TurnIdentity`, both id fields `None`; `result` with no verdict → fail-fast named `HarnessError`;
    an auth-failed compaction (synthesize from the existing `auth-failure.jsonl` shape if it cannot be
    produced live) → `AuthFailure` with the authored message; a two-entry `modelUsage` with no
    `result.model` resolves via the init model; init model naming a model absent from `modelUsage` →
    no window, `usage` kept; outside compaction mode the same status records are `Liveness` (pin the
    existing behavior).
  - Session-file parser: the manual-compaction session file hydrates one `Compaction` marker (note
    the manual shape writes no bare `/compact` `SlashCommand` record, unlike the auto-compaction
    fixture — verify both hydrate).
- Live test `live_claude_compact_…` in `crates/harness/tests/live.rs`: two minimal turns, then a
  compaction; assert `Completed`, `context_tokens_after_turn` present and below
  `context_input_tokens`, a resolved `context_window`, and a `compact_boundary` in the session file
  afterwards. A second `live_claude_compact_…` compacts the already-compacted session and asserts the
  too-small failure surfaces the CLI's message as `Failed` with no `usage`. Note in the test that a
  compaction is a full-context summarization call — $0.10–0.20 on a one-to-two-turn session at probe
  time — so it is the most expensive single live test and must stay on a minimal session.
- `make check` green.

## Milestone 2 — Dispatcher: a compaction work item

### Goal & Outcome

Let the dispatcher run a compaction through the same per-agent actor as a send, so queueing,
cancellation, liveness, and terminal handling all come for free — while it writes nothing to the
journal and never presents itself to a forward as a conversational turn.

- A compaction can be accepted for an agent; if the agent is busy it queues FIFO behind existing
  work; if idle it starts at once. The accept returns a `MessageId` like a send does.
- It emits `TurnStart` → stream events → `TurnEnd` on the agent's channel, and `AgentIdle` after.
- Cancel-send (by its `send_id`), cancel-agent, and shutdown all drain it exactly as they drain a send,
  including a queued compaction being dropped with the existing `MessageCancelled` signal.
- No journal record of any kind is written for a compaction — started, completed, failed, or cancelled.
- A forward that waits on a compacting agent resolves as if the agent were idle, only once the
  compaction's process is fully gone (decision 9).

### Implementation Outline

Make the work item's payload an enum — a prompt-with-attachments send, or a compaction — rather than
adding a boolean beside `prompt`. A compaction carries the fields the actor needs in common
(`message_id`, minted `send_id`, `selection` snapshot so model/effort match the agent's current
choice, no completion channel, `emit_user_message: false`) and no prompt. The enum makes every place
`run_turn` must branch explicit; there are five:

1. **Journal `Send`** — skipped. Put decision 6's rationale on the branch.
2. **User-message emission and attachment rendering** — skipped (nothing to render).
3. **The adapter call** — `compact(...)` instead of `dispatch(...)`, with the same `DispatchOptions`
   (cancel token, chrome preference). `is_first_dispatch_after_attach` may be passed through; the
   Claude adapter ignores it.
4. **Terminal handling** — `Outcome` and `TurnLink` journal writes skipped; the early `TurnLink` on
   `TurnIdentity` cannot fire (milestone 1 emits no identity), but guard it anyway so a future parser
   change cannot journal a link for a compaction. The metadata-sidecar persistence of the context
   snapshot and of per-turn spend both key on `stable_message_id`, which is `None`, so neither
   persists — leave that as is; the consequences are corollaries of the row being live-only (see
   "Known limitations").
5. **Current-turn waiters** — the forward path's `WaitForCurrentTurn`. Today waiters fire at the
   terminal event with the turn's captured text, and a wait arriving after the terminal is answered at
   once from a stash; that is correct for a send because its text was captured live. A compaction has
   no text, and resolving it as `CurrentTurnWait::Terminal { Completed, text: "" }` makes the forward
   path invalidate the forward as an empty source. Instead, for a compaction item, every current-turn
   waiter — registered mid-turn *or* arriving after the terminal — is answered
   `CurrentTurnWait::Idle`, **at stream drain** (after the `select!` loop exits, before the idle guard
   drops), never at the terminal event: the app's `Idle` path reads the session file, and the Claude
   adapter ends its stream only when it has reaped the process, so drain is the first moment the file
   is settled. This holds for every outcome (completed, failed, cancelled). Document on
   `CurrentTurnWait::Idle` that it means "no conversational turn to capture; read disk," which a
   compaction satisfies by definition. No app-layer change: `resolve_source` already handles `Idle`,
   and its journal-derived failure note cannot fire for a compaction (nothing is journaled).

Everything else in the actor — liveness/heartbeat, `PeekCurrentTurn`, the `IdleAfter`/`TurnAfter`
state machine, backlog drops on cancel/shutdown, the session-lock permit — must treat a compaction
identically to a send.

Expose a `compact_agent` entry point on `Dispatcher` mirroring `send_message`'s signature minus prompt
and attachments, always `OnBusy::Enqueue`. No awaitable variant.

The `TurnStart` wire event does not need a new field: the frontend originated the compaction and
registered a pending entry keyed by the same `message_id` `TurnStart` carries. Do not add a wire
`kind`.

### Definition of Done

- Dispatcher tests with `MockHarnessAdapter` (extend the mock to script a compaction stream — a
  success with usage, a failure, and a hang for the cancel case):
  - Idle agent: compaction starts immediately; `TurnStart`/`TurnEnd` carry the accepted `message_id`
    and the compaction's `send_id`; `AgentIdle` follows.
  - Busy agent: the compaction queues behind the running turn and behind an earlier queued send, and
    starts only after both terminate.
  - Journal: after a completed, a failed, and a cancelled compaction, the journal has **zero** new
    records. This is the test that protects decision 6; make the assertion exact, not "no `Send`".
  - Cancel-send on the compaction's `send_id` while running → synthesized `Cancelled` terminal; while
    queued → dropped with `MessageCancelled`, no journal. Cancel-agent drains it like a send.
  - Waiters: a `WaitForCurrentTurn` registered mid-compaction resolves `Idle` and does so only after
    the stream has drained (assert ordering against `AgentIdle` or the mock's teardown), for success,
    failure, and cancel; a wait arriving between the terminal event and drain also resolves `Idle` at
    drain, not immediately; a send queued behind the compaction is unaffected by the waiter.
  - The mock adapter's default compaction (unsupported) surfaces the way a `DispatchError` from
    `dispatch` does today — pin it.
- `make check` green.

## Milestone 3 — App command and the agent-card action

### Goal & Outcome

The user can compact a Claude agent from its card menu, watch it run, and see the result.

- The agent card menu shows "Compact context" for Claude agents only. It is enabled whether the agent
  is idle or busy (busy → queues). No confirmation.
- Choosing it shows a queued/compacting row in the transcript at the position the compaction will run
  (after everything already queued for that agent), then a compacted row with `before → after` token
  counts, or a failed row with the CLI's message. Queued and running compactions are cancellable
  through the same controls as a send; a queued one that is cancelled disappears (decision 8).
- On success, the transcript refreshes from disk so the harness's recap marker ("Conversation
  compacted", collapsible) appears beside the row, and the sidebar context bar drops to the
  post-compaction occupancy. A failed compaction leaves the bar unchanged.
- Forwarding from an agent while it compacts delivers that agent's latest real answer (decision 9).
- Invoking it on a non-Claude agent, an agent with no session yet, or a fork still awaiting
  materialization is refused with a typed error (the frontend never offers the first, and surfaces the
  others as an ordinary pre-start failure).

### Implementation Outline

**Backend command.** Add a `compact_agent` Tauri command as a thin shim over a `compact_agent_impl`
free function shaped like `send_message_impl`: capture the dispatch snapshot, build the same
`DispatchContextFactory`, check the generation, and call the dispatcher's compaction entry point.
Three gates before that, each a typed `AppError`:

- `agent.harness.supports_manual_compaction()` — the authority; mirrors `fork_agent_impl`'s gate.
- The agent has an established session (a resolvable session file). A never-dispatched agent has
  nothing to compact; refusing here is what keeps the adapter's `--resume` branch the only branch.
- The agent is not a fork still awaiting materialization. Forks are created by a fork-send from the
  compose bar, so this state is normally unreachable — it exists only as a failure residue: if that
  first fork-send fails to launch, the agent record carries fork provenance but has no session, and
  the *next ordinary send* re-forks (see the doc on `ensure_materializing_fork_may_dispatch`). A
  compaction must not be the dispatch that performs the fork: it would create the branch as a side
  effect of a maintenance action, and Claude's fork is a turn that needs a prompt. There is **no
  existing backend error for this** — the promptless-fork rule lives on the frontend, and the backend's
  fork errors describe the *source* agent — so add a new `AppError` variant ("fork not yet
  materialized; send a message first"). The condition is the predicate `busy_fork_source` already
  computes in its first lines (`forked_from_session.is_some()` and no resolvable session file);
  factor it so both call sites share it.

**Frontend: capability and menu.** Mirror the predicate the way the frontend mirrors other per-harness
capabilities (see how `agentSelection.ts` / `messageIdentity.ts` key on `harness`). Add the menu item
to the agent-card `DropdownMenu` in `Sidebar.svelte`, rendered only when the capability holds, never
disabled for busy. Copy should say what it does in product terms — see `docs/ui-conventions.md`: a
label "Compact context" with a description line such as "Summarize the conversation so far to free up
context. Runs as a turn; queues if the agent is busy." Reuse existing `DropdownMenuItem` conventions
for the two-line item.

**Frontend: lifecycle — the pending list, exactly as sends use it.** An optimistic send is a *user*
turn in the transcript plus a `pending_sends` entry registered **before** IPC (`dispatchUserTurn`),
with the receipt recorded after (`recordSendAccepted`); `turn_start` *appends* a fresh agent turn and
consumes the pending entry by `message_id`, falling back to the receipt-less front entry when the
event beats the IPC reply. That list is also what `message_failed` / `message_cancelled` prune and
what makes cancel-while-queued work. A compaction must live in that same list — an entry outside it
would let a compaction's `turn_start` consume a concurrent user send's slot in the pre-receipt race,
mis-attributing that send's reply. Concretely:

1. A `dispatchCompaction` state action, parallel to `dispatchUserTurn`: append a `pending_sends`
   entry `{ send_id, user_turn_id: <synthetic id>, kind: "compaction" }` (the `PendingSend` type gains
   an optional `kind`), flip `idle → starting` like a send (busy → entry only), **then** invoke
   `compact_agent`, and on resolve record the receipt via `recordSendAccepted`. No user turn is
   appended. An IPC rejection routes through the same pre-start failure path a rejected send uses.
2. **Queued rendering** comes from the pending list, not from a turn: a new standalone row derived
   from compaction-flagged pending entries (parallel to `queuedSendIds`), rendered at the end of that
   agent's blocks — a compaction has no user block to nest under. It carries the cancel affordance
   that maps to `cancel_send` with the entry's `send_id`, exactly like `queuedRow`.
3. `turn_start` keeps appending an agent turn (this is what puts it in execution order — decision
   7). The transcript reducer receives the popped entry's `kind` alongside its `send_id` and stamps
   `kind: "compaction"` onto the new `AgentTurn` (a new optional discriminator on the agent-turn state
   type; `status` values are unchanged — "queued" lives in the pending list, never on a turn).
4. `turn_end`: on `completed`, the row keeps its `usage` and the state triggers the Claude-only
   project refresh so the disk-sourced `Compaction` marker lands beside it. On `failed`, the row shows
   the outcome's message (`usage` is absent by milestone 1, so `contextUtilization` skips it and the
   bar keeps the previous, still-correct number). On `cancelled`, the row shows cancelled.
5. `message_failed` (pre-start failure: spawn failure, journal-free launch failure, IPC rejection)
   for a compaction appends a failed compaction row — carry `kind` through `appendFailedTurnImpl` so it
   does not render as an empty send. `message_cancelled` for a queued compaction prunes the pending
   entry and appends **nothing** (decision 8) — the existing arm must branch on the entry's kind.

The hydrate merge must **keep** a completed compaction row: it has no `hydration_key` and no on-disk
agent turn, so under the existing collision rules a resident with no disk counterpart is retained. Pin
this with a reducer test; decision 7 depends on it.

**Frontend: the row.** A new transcript row component for the compaction turn, a sibling of
`CompactionMarker.svelte` in visual language (borderless, icon, one-line label, muted detail) but
distinct in content: it is the *action* ("Compacting…", "Compacted · 23.4k → 4.2k tokens", "Compaction
failed: Not enough messages to compact."), not the harness's recap. It has a success/failure state
where the marker has none. Per-turn cost/overage footer behavior applies to it as to any turn (an
overage compaction shows its cost). It renders ungrouped — no user row above it.

**Context bar.** No change to `contextUtilization` should be needed once the compaction row is a
completed agent turn carrying `usage`; verify rather than assume, and add the sidebar tests.

### Definition of Done

- App tests (free functions): `compact_agent_impl` refuses a Codex and an Antigravity agent, an
  agent with no session, and a fork still awaiting materialization (new variant), each with the
  intended typed error; accepts a Claude agent with a materialized session and returns the
  dispatcher's `MessageId`. Forwarding: a forward whose source is mid-compaction resolves to the
  source's previous completed answer and is not invalidated; the same after a failed compaction.
- Reducer tests: `dispatchCompaction` registers the pending entry and flips `starting` only when idle;
  `turn_start` appends a `kind: "compaction"` turn and consumes the right entry — including the
  pre-receipt race with a user send registered *after* the compaction (the compaction's `turn_start`
  must not consume the send's entry, and vice versa); `turn_end` completed keeps `usage` and marks
  the refresh; failed carries the message; `message_failed` renders a failed compaction row;
  `message_cancelled` for a queued compaction removes the entry and appends nothing; hydrate keeps the
  completed row when the disk has no counterpart and the `Compaction` marker arrives beside it.
- Component tests (mock `invoke` + `listen`, per AGENTS.md): the menu item is present for a Claude
  agent and absent for Codex/Antigravity; selecting it invokes `compact_agent` and the queued row
  appears with a working cancel; the realistic sequence `turn_start → liveness → turn_end(completed)`
  renders the compacted row and requests the refresh; the failure sequence renders the message; the
  **whole** sequence `turn_start → turn_end` arriving before the IPC reply resolves still yields one
  row in the right state (no duplicate, nothing stranded on queued); `message_failed` before the IPC
  reply yields one failed row; the ordering scenario **running send → queued send → queued
  compaction** asserts the sidebar bar shows the compaction's occupancy after it completes, and again
  after a hydrate; a failed compaction leaves the bar unchanged.
- Copy reviewed against `docs/ui-conventions.md`.
- `make check` green, including the browser suite.

## Milestone 4 — Documentation

Small; compress accordingly.

- `docs/harness-behavior.md` §3.9: flip the status line for Claude to shipped, name the tests, and
  record the residuals below in the section rather than as new gaps. Do not touch G34 (the Codex
  boundary-hydration defect is independent and remains open).
- `docs/system-design.md` §7: one line that a compaction is invisible to forwarding (decision 9); §9
  capability matrix: add a "Manual compaction" row (Claude native via `/compact` in `-p`; Codex
  headless via `codex app-server` `thread/compact/start` only, not wired; Antigravity unavailable,
  flag-gated).
- `README.md` "Harness support and limitations": one user-facing entry — compaction from the agent
  menu is available for Claude Code agents only; Codex and Antigravity conversations cannot be
  compacted from Switchboard.
- Confirm the `supports_manual_compaction` doc comment carries decision 5, the dispatcher's
  "no journal" branch carries decision 6, the waiter branch carries decision 9, and the parser's
  failure-path `usage: None` branch carries its rationale.

## Out of scope (deliberately)

- Custom compaction instructions (decision 3).
- Any autocompact threshold UI (decision 4).
- Codex compaction — a transport decision (running `codex app-server`), to be costed on its own.
- Antigravity — re-probe when the upstream flag opens; nothing to build until then.
- Hiding the live compaction row when the disk marker lands beside it, or de-duplicating the two.

## Known limitations to record (harness-behavior §3.9)

- **Reopen shows the pre-compaction context bar until the next completed turn.** The context
  snapshot the sidecar persists is keyed on the final assistant `message.id`; a compaction has none,
  so its post-compaction occupancy is live-only. After a restart, the bar reads the last persisted
  snapshot, which predates the compaction, and self-corrects on the agent's next turn.
- **Failed and cancelled compactions are live-only** (decision 6). **A queued compaction cancelled
  before it starts leaves no trace at all** (decision 8).
- **The compaction row is live-only** (decision 7); after reopen only the harness's recap marker
  remains, which is the durable record. A corollary: the inline overage-cost marker Switchboard shows
  on a turn that ran on extra-usage credits (the only case a Claude cost is rendered at all) goes with
  the row — per-turn spend persists through the same `stable_message_id` gate, which a compaction
  cannot satisfy. This is not a cost the user would see, avoid, or recover by compacting in the CLI or
  desktop app instead; the compaction costs the same tokens everywhere, and neither keeps a
  per-compaction cost line in history. Developer-facing only.
- **`DISABLE_COMPACT`** in the user's environment makes every compaction fail with "compaction did
  not run: …"; surfaced as an ordinary failed compaction.
- **Window resolution rests on the post-compaction `system/init` model.** If a future CLI stops
  re-emitting `init` after a compaction and `result.model` stays absent, the bar clean-hides after a
  successful compaction until the next turn. The row's token counts are unaffected.
