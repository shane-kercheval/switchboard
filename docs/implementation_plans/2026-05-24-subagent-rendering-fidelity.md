# Follow-up: subagent rendering fidelity (`parent_tool_use_id`)

**Status: SUPERSEDED 2026-05-27** by [`2026-05-27-claude-session-file-parser-fidelity.md`](2026-05-27-claude-session-file-parser-fidelity.md), which consolidates this subagent-attribution fix with a sibling Claude session-file parser bug (out-of-order `tool_use` / `tool_result` records, observed on Claude 2.1.150) and a warning-surface UX cleanup. **Implementing agent: work from the consolidated plan, not this one.** This doc is preserved unedited for investigation provenance — the probe shapes, cross-harness verification (2026-05-24), and reasoning behind the v1 collapse decision live here.

## Problem

Switchboard runs each harness in default (non-`--bare`) mode, so a harness agent can **delegate to a subagent** mid-turn (Claude's `Agent` tool — built-in `Explore`/`Plan`/`general-purpose` or user-defined `.claude/agents/*.md`; Gemini's `invoke_agent`). This is auto-invoked behavior the model chooses — common in normal use, not an edge case.

A hands-on probe of `claude -p` (2.1.149/2.1.150) confirmed that **a subagent's *internal* tool calls are mis-attributed to the parent turn on the live stream, and the live view diverges from the rehydrated one.** It is a rendering-fidelity + consistency bug, not a crash or data loss — but it fires whenever a Claude agent delegates, and Switchboard's whole value is faithful per-agent attribution, so it matters more here than in a generic client.

Full ground truth (event shapes, on-disk layout, parser analysis): [`../research/claude-code-cli-observed.md` §"Subagent (`Agent` tool) representation"](../research/claude-code-cli-observed.md).

## What was verified (Claude Code, claude-code 2.1.149/2.1.150)

**The tool is named `Agent`** (not `Task` — system-design §9 says "`Task` tool"; update it). It appears as a normal `tool_use`: `{ name: "Agent", input: { description, subagent_type, prompt } }`.

**Live stream tags subagent-produced events with `parent_tool_use_id`** = the `Agent` tool_use's id (`null` for the parent's own events). A tool-using subagent (`Agent` call id `toolu_017e…`):

```
assistant  parent_tool_use_id=null         tool_use  name=Agent  (toolu_017e…)   ← parent
assistant  parent_tool_use_id=toolu_017e…  tool_use  name=Bash   (toolu_01UE…)   ← SUBAGENT's own call
user       parent_tool_use_id=toolu_017e…  tool_result          (Bash output)    ← SUBAGENT's own result
user       parent_tool_use_id=null         tool_result          (Agent aggregate)← subagent's report to parent
```

New `system` subtypes during a subagent run — `task_started`, `task_notification`, `status` — are skipped gracefully today (`parse_system_event` only acts on `subtype == "init"`).

**On disk, the subagent's internals are in a separate sidecar file, not inline:** the main `<session>.jsonl` has only the parent's `Agent` `tool_use` + its aggregate `tool_result`; the subagent's `Bash` call lives at `~/.claude/projects/<encoded-cwd>/<session-id>/subagents/agent-<id>.jsonl`. No `isSidechain:true` records in the main file. New top-level main-file record types this era: `ai-title`, `attachment`, `last-prompt`, `queue-operation`.

**The bug:** `crates/harness/src/parser.rs` never reads `parent_tool_use_id`.

- **Live:** `parse_assistant_envelope` / `parse_user_envelope` emit `ToolStarted{name:"Bash"}` / `ToolCompleted` with the **parent's `turn_id`** → the subagent's work renders as the parent agent's own tool calls, interleaved between the `Agent` `ToolStarted`/`ToolCompleted`. N subagent calls → N mis-attributed calls in the parent turn.
- **Disk/rehydration:** `crates/harness/src/claude_code/session_file.rs` reads only the main file → the rehydrated turn shows just `Agent` call + aggregate result; the nested calls are absent.
- **Net:** same turn renders differently live vs. after restart. Disk is the *correct* abstraction ("a delegation is one tool call from the parent's view").

## Recommended fix (v1)

**Honor `parent_tool_use_id` in the stream parser: suppress parent-tagged tool events at the parent turn level** — emit only the `Agent` `ToolStarted`/`ToolCompleted` pair (its aggregate result is the output), dropping subagent-internal `tool_use`/`tool_result` blocks whose event carries a non-null `parent_tool_use_id`. This:

- removes the live mis-attribution,
- makes **live == rehydrated** (both show "one `Agent` tool call + summary"),
- matches **Gemini's `invoke_agent`** treatment (one `ToolStarted`/`ToolCompleted` pair) → uniform cross-harness semantics: *from the parent's view, a delegation is a single tool call.*

`parent_tool_use_id` is a top-level field on the stream record (sibling of `type`), so the parser can read it in `parse_line` before dispatching to the envelope parsers and short-circuit (Skip) any envelope whose record is parent-tagged. Keep the dispatcher/`commands.rs` harness-agnostic — this is adapter/parser-layer logic.

**Deferred to v2 (data supports it, don't build now):** rendering the subagent's internal work *nested/labeled* under the `Agent` call — the live stream has `parent_tool_use_id` and the disk side has `subagents/*.jsonl`, so a future "expand this delegation" UI is feasible. v1 deliberately collapses a delegation to one tool call.

## Cross-harness verification — DONE (2026-05-24): the gap is **Claude-only**

All four harnesses were probed (tool-using subagents where the feature exists). **Only Claude leaks subagent internals; the fix is Claude-only.** Details in each harness's research doc.

1. **Gemini (`invoke_agent`) — CLEAN, no change.** A tool-using subagent runs **opaquely**: the stream shows only the `invoke_agent` `tool_use`/`tool_result` pair (plus the parent's final message); the subagent's internal `run_shell_command` never appears. Already the target shape. See `gemini-cli-observed.md` §"`invoke_agent` subagents are OPAQUE in the stream".
2. **Antigravity (`invoke_subagent`) — CLEAN, no change.** The subagent runs as its **own separate `brain/<uuid>` conversation** that Switchboard never tails. The parent transcript surfaces only the delegation (`define_subagent`/`invoke_subagent` tool calls, which pair correctly) + the answer; the subagent's internal `run_command` is in the separate conversation. See `antigravity-cli-observed.md` §"Subagent (`invoke_subagent`) representation". (Aside found there: `agy -p` hangs forever on an open stdin — `Stdio::null()` is load-bearing.)
3. **Codex — confirmed non-feature.** `codex exec` has no `Task`/delegation tool; nothing to suppress.

**Implication:** scope this work item to the **Claude parser only** (`crates/harness/src/parser.rs` honoring `parent_tool_use_id`). No Gemini/Antigravity/Codex parser changes. The DoD's Gemini/Codex/Antigravity verification items below are satisfied by these probes — keep them as the recorded result, not pending work.

## Definition of Done

- Parser reads `parent_tool_use_id`; subagent-internal tool events are suppressed at the parent turn (Claude), so the live transcript of a delegating turn shows exactly the `Agent` `ToolStarted`/`ToolCompleted` pair — matching the rehydrated view.
- **Live test** (`make test-live`, `#[ignore]`-gated, named `live_claude_*`): run a prompt that triggers a **tool-using** subagent; assert the parent turn emits one `ToolStarted{Agent}` + one `ToolCompleted`, and **no** `ToolStarted{Bash}` (or other subagent-internal tool) attributed to the parent turn.
- **Fixture test:** a recorded stream fixture with parent-tagged subagent events; assert the parser suppresses them. (Capture from the probe below.)
- Gemini probed (item 1); fix applied iff it leaks; result recorded in `gemini-cli-observed.md`.
- Codex negative result + Antigravity finding recorded in their research docs.
- `session_file.rs` confirmed to skip the new main-file record types (`ai-title`, `attachment`, `last-prompt`, `queue-operation`) without erroring.
- system-design §9 updated: `Task` → `Agent`; "spawn as expected" qualified with the representation note (or a pointer to this item).

## Reproducible probe (Claude)

Faithful to the adapter's flags; fixed `--session-id` to locate the session file; throwaway cwd:

```sh
PROBE=$(mktemp -d); cd "$PROBE"; SID=$(uuidgen | tr 'A-Z' 'a-z')
claude -p "Use the Agent tool to launch exactly one general-purpose subagent whose instruction is: run the bash command 'echo hello-from-subagent' and report its exact output. After it returns, reply with the single word done." \
  --output-format stream-json --include-partial-messages --verbose --dangerously-skip-permissions \
  --session-id "$SID" > stream.jsonl
# Inspect parent tagging:
jq -c 'select(.parent_tool_use_id != null) | {t:.type, parent:.parent_tool_use_id[0:14], content:[.message.content[]? | {type, name:(.name//null)}]}' stream.jsonl
# Locate the subagent sidecar file on disk:
find ~/.claude/projects -path "*/$SID/subagents/*.jsonl"
```
