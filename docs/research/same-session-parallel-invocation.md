# Research: Same-session parallel invocation (Claude Code and Codex)

**Captured:** 2026-05-10
**Versions:** Claude Code 2.1.138, Codex CLI 0.130.0
**Affects system-design sections:** §7 (Walking away / workflow execution), §9 (Process model), §12 open question 10.2 (concurrent agent use across workflows).

## Question

When Switchboard invokes a harness twice in parallel against the **same session ID** — e.g., a workflow step targeting agent X is in flight when a second workflow step (or manual user action) also targets X — what happens? Does the harness error? Block? Corrupt the session file?

(Distinct from the §9 concurrency probe, which tested parallel `claude -p` against *different* session IDs and confirmed no contention.)

## Method

For each harness:

1. Spawn invocation 1 with a known session/thread ID, asking the model to run a long-blocking shell command (`sleep 25 && echo done`) and then reply.
2. Three seconds in (while invocation 1 is mid-tool), spawn invocation 2 against the same session ID with a quick prompt ("reply with: pong").
3. Wait for both to exit. Inspect exit codes, logs, and the on-disk session file.
4. Resume the session afterward and ask the model to summarize what it sees.

## Results

### Claude Code

- Both invocations exited 0; both reported the same `session_id` in their final `result` events.
- Invocation 2 returned its answer and completed before invocation 1 (which was still sleeping).
- The session JSONL file at `~/.claude/projects/<encoded-cwd>/<session>.jsonl` is structured as a **tree** — every event has a `uuid` and a `parentUuid`. Both invocations wrote events into the same file, but with different parent chains, producing two parallel branches from a common ancestor (the last completed event before invocation 2 spawned).
- Subsequent `claude --resume <sid>` followed the **most recently written branch** (invocation 1, which finished after invocation 2 because of the sleep) and was unaware of invocation 2's branch. Invocation 2's "pong" exchange is now an orphan branch in the file — recoverable by inspection but invisible to normal resume.

**Behaviour summary:** soft-corruption. No errors, no file damage, but one of the two parallel conversations becomes unreachable through the normal CLI.

### Codex

- Both invocations exited 0; both reported the same `thread_id`.
- Invocation 2 ("pong") completed in ~6s; invocation 1 (sleep + ack) completed in ~33s.
- The session rollout file at `~/.codex/sessions/YYYY/MM/DD/rollout-...jsonl` is **flat append-only**. Events from both invocations were written into the same file in chronological order, distinguishable only by their `turn_id` field. Each invocation's user message, agent messages, function calls, and `task_complete` events are all present, intermixed.
- Subsequent `codex exec resume <thread-id> "..."` treated the **entire interleaved transcript as one linear history**. When asked to list every user message it had received, the model returned all four messages (invocation 1, invocation 2, the resume probe, and the listing probe) in chronological order, as if they were sequential turns from the same user.

**Behaviour summary:** hard-conflation. Two concurrent independent conversations are silently merged into one transcript, with no application-layer signal that anything unusual happened.

## Implications for Switchboard

1. **Neither harness rejects same-session parallel invocation.** Both accept it, both succeed, both write to the session file. There is no error code or stream event to detect this collision at the harness layer.
2. **The on-disk effects diverge by harness:**
   - Claude Code: tree-with-orphan-branch. Recoverable in principle; invisible by default.
   - Codex: flat interleaved transcript. Two threads merged into one; the conflation is permanent and invisible.
3. **Switchboard MUST enforce single-in-flight-turn-per-agent at the application layer.** The harnesses will not protect us. A dispatcher that tracks each agent's "in-flight turn" status and refuses or queues subsequent dispatches against the same agent is the correct place for this rule.
4. **No reservation needed across workflow steps.** Whether a future workflow step "owns" an agent is irrelevant — the rule is purely "is this agent currently mid-turn?" If yes, refuse or queue. This composes cleanly with both autonomous workflow execution and ad-hoc manual sends, and avoids surprising "agent locked" UX.
5. **UI gating is the natural enforcement point.** A user in the UI can't manually dispatch to an agent that's currently busy because the input affordance is disabled (or the dispatch shows a clear "agent busy" error). Workflow-step dispatch goes through the same dispatcher and gets the same gate. The collision scenario simply can't occur if the dispatcher is the single chokepoint.

## Resolution to system-design open question 10.2

**Working assumption** (now grounded in the probe):

> Switchboard enforces one in-flight turn per agent at the application layer. A dispatch (whether from a workflow step or a manual user send) against an agent that's already mid-turn refuses with a clear error ("agent X is busy, currently running step N of workflow P"). The user can switch focus to inspect the busy agent. Queueing is not implemented in v1; refuse-on-collision is the v1 default.

This works because:
- Workflows within a single session are serialized by the orchestrator; workflow-internal collisions don't occur by construction.
- Cross-workflow collisions (two workflows invoked concurrently both targeting agent X) are caught by the dispatcher.
- Manual-during-workflow collisions (user typing to X while workflow uses X) are caught by the dispatcher.

Two workflows invoked simultaneously that target *disjoint* agents both run normally. The constraint is per-agent, not per-workflow.

## Enforcement design

The probe established that the harnesses won't reject parallel same-session invocation, so Switchboard's enforcement lives in the application layer. The design:

1. **Single dispatcher chokepoint.** All turn dispatches — workflow steps, manual user sends from the UI, anything that talks to a harness — go through one function in the Rust core. No code path bypasses it. The dispatcher is the only place that knows how to spawn `claude -p` / `codex exec`, so any new send surface has to call into it.
2. **Per-agent in-memory state.** A `HashMap<AgentId, AgentState>` behind a Tokio `Mutex` (or `RwLock`) holds each agent's status: `Idle` or `InFlight { workflow_id, step_idx, started_at }`. Pre-flight check on dispatch: if non-idle, refuse with a structured error the UI renders as "agent X is busy, currently running step N of workflow P." Flip back to `Idle` on the terminal event (`result` / `turn.completed` / `turn.failed`) or when the cancellation path runs.
3. **Project-level instance lock.** Take an exclusive `flock` on `<directory>/.switchboard/projects/<project-id>/instance.lock` at project open. A second Switchboard process trying to open the same project sees "this project is open in another Switchboard process" and refuses. ~10 lines of Rust; closes the multi-process-same-project hole that in-memory state alone can't cover. (Different projects in the same working directory can be open in the same Switchboard window concurrently — flock is per-project, not per-directory; see system-design §3 for the multi-project-per-directory model.)
4. **Crash recovery: none needed.** Harness subprocesses are spawned in Switchboard's process tree (per §9 Process model — own process group, but parented to Switchboard). When Switchboard exits — clean or crash — those subprocesses die. On next start, every agent is legitimately `Idle`; there is no in-flight state to reconcile.

### What we deliberately don't do

- **On-disk per-agent lock files.** Stale-after-crash failure mode; OS-specific edge cases; redundant with the in-memory dispatcher gate.
- **Harness-level locks (lsof, fcntl on the session JSONL).** Fragile, OS-dependent, and the dispatcher gate makes them unnecessary.
- **Cross-workflow reservations** (e.g., "workflow A reserves agent X for steps 1-5"). The per-agent in-flight check is sufficient: if workflow A holds X mid-turn-of-step-3 and workflow B's step-1 wants X, B's step-1 is refused at dispatch time. Reservations would add lifecycle complexity (release-on-error, release-on-cancel, partial-reservation-on-skip) for no behavioral gain.

## What this does **not** answer

- Whether two parallel invocations against *different* session IDs in the same `cwd` step on each other in any way (already tested in §9: no, they don't).
- Whether either harness exposes a lock file or other detection mechanism we could use as a belt-and-suspenders check (didn't probe; the application-layer rule is sufficient).
- What recovery looks like if a Switchboard bug *did* let two in-flight turns happen — at minimum, the affected session would carry orphan-branch (Claude Code) or interleaved (Codex) artifacts. Codex's case is worse and harder to detect; this is the strongest reason not to rely on harness behavior for safety.

## Reproduction

The probe scripts live in `/tmp/switchboard-probes/` from the original run; the two key commands were:

```bash
# Claude Code
claude --session-id <uuid> -p "<long task>" --output-format stream-json --verbose --dangerously-skip-permissions &
sleep 5
claude --resume <same-uuid> -p "<short task>" --output-format stream-json --verbose --dangerously-skip-permissions

# Codex
codex exec --json --skip-git-repo-check --dangerously-bypass-approvals-and-sandbox "<long task>" &
# capture thread_id from first stream event, then:
codex exec resume <thread-id> --json --skip-git-repo-check --dangerously-bypass-approvals-and-sandbox "<short task>"
```

Both confirmed exit 0 and reproduced the on-disk patterns described above.
