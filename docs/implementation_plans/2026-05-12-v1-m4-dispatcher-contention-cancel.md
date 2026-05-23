# M4 — Project switcher + dispatcher hardening + contention/queueing + per-turn cancel + multi-recipient fan-out

## How to read this plan

This is the implementation-grade expansion of milestone M4 from [`2026-05-12-v1.md`](2026-05-12-v1.md). It follows the same per-sub-milestone shape as the M2/M3 plans: each sub-milestone has a **Goal & Outcome** (validate the *outcomes*, not the implementation choices), an **Implementation Outline** (a handoff to an agent that will read the code but was not in the planning conversation — it carries the decisions that can't be recovered from the codebase), and a **Definition of Done**.

Implement sub-milestones in order. Complete each fully — code, tests, docs — and stop for human review before starting the next. Do not commit until the human approves. When a load-bearing assumption is ambiguous, ask before implementing.

This is one document on purpose. M4 spans backend (M4.1–M4.5) and frontend (M4.6–M4.8) work; keeping it unified means that when implementation reveals a problem in an early sub-milestone, the fix and its downstream consequences live in a single source of truth rather than drifting across files.

## Critical premise — most of M4's *mechanism* already exists

M4 reads like five new systems; the code says otherwise. Before planning anything, internalize the current state:

- **The dispatcher is already the single chokepoint.** `crates/dispatcher/src/lib.rs` already owns per-agent state, already enforces one-in-flight-turn-per-agent via the `AgentIdleGuard` RAII pattern, already mints `TurnId`s, and already emits through the `EventEmitter` trait.
- **Contention is already enforced** (`AgentStatus::{Idle, InFlight}` + `AgentIdleGuard::acquire` returning `Busy`). The frontend already gates Send on `run_status` and has a `failSendStart()` path.
- **Multi-project backend shape already exists**: `AppState.projects`, `active_project_id`, `open_project`/`set_active_project`/`list_projects`, and the four harness adapters are all wired. **Transitional, read before M4.6:** this shape (and M4.1's `init_directory` rebind path, which clears `projects`/`project_locks`/`agents_by_id` wholesale when the bound directory changes) is the *current single-bound-directory* model. M4.6 replaces it with the flat cross-directory workspace (system-design §3): `init_directory` becomes additive add-directory (never clears other directories), rebind goes away, and lock/cache invalidation scopes to a single removed directory. So treat "clears everything on rebind" as M4.1-only — don't carry it into M4.6, and keep the M4.2 cancel-drain helper parameterized by *a set of agents* (so remove-directory can pass just one directory's agents). The interim M4.2–M4.5 window keeps M4.1's synchronous rebind-clear (the pre-existing M1.4 orphaned-drain gap) — a conscious interim, superseded at M4.6, not a regression to fix mid-cluster.
- **The frontend already supports concurrent in-flight turns** structurally — `transcripts`/`runtimes` are keyed by `agent_id`, listeners are per-agent (`agent:<id>`) and persist for the session, and the unified transcript merges by `started_at`.

So M4 is **not greenfield**. It is: (1) harden ownership/lifecycle boundaries that M1 deliberately deferred, (2) add the genuinely new mechanism — **per-turn cancellation** (nothing cancellable exists today) and **per-agent queueing** (a deliberate override of v1's original "no queueing"), and (3) add the missing product affordances (project switcher UI, multi-recipient compose).

The single biggest *new* risk is cancellation plumbing, because today the harness subprocess `Child` is owned by a detached `run_producer` task that the dispatcher/app layer cannot reach. M4.2/M4.3 fix that.

## Decisions resolved in planning (do not re-litigate; implement these)

These were settled with the maintainer. They override the original v1 outline where they conflict (v1 is a directional best-guess, not a contract). The durable user-facing semantics now live in **`docs/system-design.md` §7** — read it; this plan does not restate it.

1. **Vocabulary.** A *send* is one dispatch action (one compose-bar submit, or later one workflow step) targeting 1..N recipients. Each recipient's request→response cycle is a *turn*. A multi-recipient send creates N **independent** turns. Use this vocabulary in code, comments, and tests.
2. **Cancellation is cooperative and token-driven; the adapter owns the *kill*, the dispatcher owns the *outcome*.** The dispatcher fires a binary `CancellationToken` and remembers the `CancelSource` it fired with. Each adapter watches the token (via `select!`) and, on cancel, does only the harness-specific work — process-group kill, per-harness quirks — then ends its stream **without** emitting a terminal event. The dispatcher's drain task detects the cancelled token on stream-end and synthesizes the terminal `TurnEnd { Cancelled { source } }`, stamping the source it stored. Rationale (load-bearing — a review caught this): a binary token cannot carry intent, and the dispatcher (which initiated the cancel) is the only layer that knows *why*; making it stamp the source keeps adapters source-agnostic and unifies all four harnesses, including Codex (which exits 0 and emits nothing on SIGTERM). This is a deliberate, narrow exception to "the adapter owns the single `TurnEnd`" — it applies *only* to the cancel path; on normal completion the adapter still owns its terminal event. The orchestrator never branches on which harness it is talking to. **Cancellation is effective only before the first terminal `TurnEnd`:** the drain task records `terminal_seen` in the shared per-agent state entry (set on `TurnEnd`, read by `cancel` under the state lock); a `cancel` *after* terminal — e.g. during Codex's post-terminal enrichment window before `AgentIdle` — is a typed no-op (no kill, no synthesized `Cancelled`).
3. **`TurnOutcome::Cancelled { source }`** is a first-class terminal outcome, distinct from `Failed`. `source ∈ { user, workflow, shutdown }`. Cancellation is intent-bearing, not an error.
4. **Signal policy:** SIGTERM to the process group first, escalate to SIGKILL if the group has not exited within a short grace window. Applies to all four harnesses (they all spawn in their own process group already).
5. **Per-agent FIFO message queue** for manual compose-bar sends (overrides v1's "no queueing"). In-memory only — queued-but-undispatched messages do not survive restart. Cancelling an in-flight turn does **not** clear the queue. Removing a queued message returns its text to the compose bar (Switchboard never silently discards user-authored text). Cross-agent dependency chaining ("feed B's output to A") is explicitly **out of scope** — that is the workflow engine's job (M6).
6. **Switchboard owns the user's side of the conversation; harnesses own the agents' side.** Switchboard persists an append-only **conversation journal** per project, with two record types:
   - a *send* record written **when the recipient's turn actually starts** (co-located with `TurnStart`), one per recipient: `{ send_id, turn_id, agent_id, prompt, at }`. Writing at turn-start (not at submit) means a message **removed from the queue before it starts is never journaled** — correctly absent after restart — while an idle recipient (starts immediately) and an instantly-cancelled *started* turn are both journaled. **Durable conversation history begins when a turn starts, not when the user hits Send**: queued-but-unstarted messages are live-UI-only and intentionally absent after restart (consistent with decision 5's in-memory queue — not a new product decision, its corollary). `turn_id` is included as a free correlation key (available now that the write is at turn-start). No `queued_removed` record is needed.
   - a *turn-outcome* record on terminal **for every non-completed turn — failed or cancelled**: `{ send_id, turn_id, agent_id, outcome: Cancelled{source} | Failed{kind, message}, started_at, ended_at }`, **no agent content** (the failure reason is outcome metadata, not content). Completed turns write no outcome record — their content is in the harness file. The two sources thus **partition**: harness files supply *completed*-turn content; the journal supplies *non-completed*-turn outcomes. This needs no journal↔harness correlation or dedup (there is no shared key) and makes failures visible on reload. Tradeoff: a failed turn's *partial* output (if the harness wrote any before erroring) isn't shown after restart — only the marker + reason — the same as cancelled.

   The unified transcript is rendered as: **user turns from the journal** (grouped by `send_id` → one message attributed to the union of recipients, e.g. "User → B|C"; the group's render timestamp is the `min(at)` across its records) **+ completed-turn content from harness session files** (assistant-role content only; harness *user-role* entries filtered out of the unified view) **+ failed/cancelled outcome markers from the journal**. After restart a partially-queued fan-out shows **only the recipients whose turns started** — e.g. "User → A" if B was still queued when the app closed (B's turn never ran; this is intended and more truthful than implying B received it, *not* a hydration bug; it is a visible difference from the live UI and must be documented as such). This **refines** the earlier "harness files are the sole source of truth / Switchboard keeps no transcript store" invariant — which a review showed cannot represent the user's side: a fan-out duplicates the user prompt across N harness files, and an instantly-cancelled send appears in none. So: harness files own **completed-turn agent content**; Switchboard owns the **user-send journal + non-completed-turn outcomes**. Buffered partial *content* of a cancelled or failed turn stays in-memory only (lost on restart). See system-design §3/§7. The journal is the persistence shape M6 extends for workflow/step outcomes.
7. **Project loading:** eager registry load (cheap `AgentRecord`s for all projects in the directory at startup), lazy per-project transcript hydration on first view (with a loading indicator), cached thereafter. Background projects keep streaming within a running session (listeners stay registered across switches).
8. **Session-id uniqueness, scoped to each harness's session-file namespace (M4.1 enforces; M4.6 widens for the global harnesses).** An agent (harness + session-id) may not be registered against the same underlying harness session twice — same-session parallel invocation is the hazard (`docs/research/same-session-parallel-invocation.md`). The collision *domain* differs by how each harness names its session files: **Claude and Gemini are cwd-namespaced** (`~/.claude/projects/<encoded-cwd>/…`; Gemini under a cwd-resolved `~/.gemini/tmp/<project-name>/`), so the same id under a *different* working directory is a *different* session — uniqueness is correctly **per-directory** (M4.1's scan). **Codex and Antigravity are globally addressed** (`~/.codex/sessions/<date>/…`; `~/.gemini/antigravity-cli/brain/<uuid>/`), so the same id is the *same* session everywhere — uniqueness must be **workspace-wide**. Under single-directory bind (M4.1) directory-scoped covers all four; once M4.6 makes directories concurrent, the Codex/Antigravity scan must widen to all workspace directories (the Claude/Gemini scan stays per-directory — widening it uniformly would false-reject legitimate same-id-different-cwd attaches). Create-new can't collide (fresh/server-assigned id).
9. **Project lock = app-layer advisory file lock** (`flock`-style), per project, held for the project-loaded lifetime, OS-released on crash. Inter-process guard only; intra-process re-open is a no-op returning the existing handle.
10. **Event coalescing:** coalesce streamed text chunks per agent over a short window; never coalesce or reorder structured/terminal events. Targeted measure, not the §10 ring buffer.
11. **Context menu scope for M4:** cancel in-flight turn + open session file only. Fork-session and reset/remove are part of the v1 product (system-design §7) but are **deferred out of M4** to keep lifecycle/deletion complexity out of this milestone.
12. **Multi-recipient fan-out** = N independent dispatches with no aggregation. Idle recipients start immediately; busy recipients queue. Fan-in (aggregation) is M6.
13. **The dispatcher owns the queue + per-agent state + timing; the app provides a per-dispatch context factory** (reviews caught that the queue had nothing to dispatch *with*, and that the per-dispatch emitter + `DispatchOptions` are app-layer and computed fresh each time — they cannot be frozen at enqueue). The `Dispatcher` holds one per-agent state map whose entry is `{ status, queue, cancel_token, cancel_source, terminal_seen }`, plus a per-agent **app-provided dispatch-context factory**. The factory — evaluated **at start time** for both immediate and dequeued turns — returns the adapter, cwd, the per-dispatch-wrapped emitter (`SessionMetaObservingEmitter`), and freshly-computed `DispatchOptions` (`is_first_dispatch_after_attach` read from `needs_session_meta` *at that moment*). This keeps session-meta knowledge in the app layer (per the deliberate separation in `crates/app/src/emitter.rs`) while making queued auto-dispatch self-contained. `QueuedMessage` carries only `{ queued_message_id, send_id, prompt }` — **never a frozen `DispatchOptions` or emitter**, which would go stale before the queued turn starts. The factory is invoked **after the per-agent state lock is released** (never held across the `dispatch().await` — see decision-aligned sequencing in M4.4) and must respect `AppState`'s lock order. This per-agent state map (cancellation token decision 2, queue decision 5, `terminal_seen` decision 2/4, status) is slotted into `AppState`'s lock-order convention alongside the project-lock map.
14. **Per-agent "what this agent saw" view: enabled now, surfaced later.** The journal (decision 6) plus the raw per-agent harness files make a per-agent transcript view possible (showing exactly what one agent received, including the user message as *it* saw it — sourced from that agent's harness file as-is, distinct from the deduplicated unified view). M4 ensures the data model supports this; building the dedicated UI is a small deferred follow-up, not M4 scope.

## Documentation the implementing agent must read before coding

- `docs/system-design.md` §7 (User-facing model) — the canonical UX semantics this plan implements; §3 (filesystem layout, transcript source-of-truth); §9 (process model, terminal outcome variants).
- `docs/research/same-session-parallel-invocation.md` — why session-id uniqueness must be enforced (decision 8) and why one-in-flight-per-agent exists.
- `docs/research/codex-cli-observed.md`, `gemini-cli-observed.md`, `antigravity-cli-observed.md`, `claude-code-cli-observed.md` — per-harness subprocess/exit behavior; **Codex exits 0 on SIGTERM** (load-bearing for decision 2: detect cancellation from the token, never from exit code or terminal event).
- The current code: `crates/dispatcher/src/lib.rs`, `crates/app/src/state.rs`, `crates/app/src/commands.rs`, `crates/harness/src/subprocess.rs` (`kill_subprocess_group`), each adapter's `run_producer` (`crates/harness/src/{claude_code,codex,gemini,antigravity}/mod.rs`), and the frontend state in `src/lib/state/index.svelte.ts` + `src/lib/state/reducers.ts`.
- `tokio_util::sync::CancellationToken` — https://docs.rs/tokio-util/latest/tokio_util/sync/struct.CancellationToken.html
- `tokio::select!` — https://docs.rs/tokio/latest/tokio/macro.select.html (the concurrency primitive that makes cancellation work even when a read is parked — see M4.3)
- `nix::sys::signal` (`killpg`, `Signal`) — https://docs.rs/nix/latest/nix/sys/signal/index.html (already a dependency)

## Sub-milestone overview and sequencing

Backend hardening + new mechanism first, then the UI that sits on top of it. The eight sub-milestones group into two clusters that are natural PR/review boundaries.

**Cluster M4A — backend mechanism & hardening (M4.1–M4.5).** Suggested first PR boundary after M4.5: the backend is then complete and fully testable via the mock adapter + live harness tests, with no UI dependency.

- **M4.1 — State & lock foundations.** Project file lock, agent register-cache, JSONL durability fsync, directory-wide session-id uniqueness, core visibility tightening.
- **M4.2 — Cancellation contract (dispatcher + core).** `Cancelled { source }` outcome (dispatcher-stamped), `CancellationToken` in dispatcher per-agent state, `cancel_turn` command, conversation-journal persistence (user sends + turn outcomes), lifecycle wiring (project unload / app shutdown).
- **M4.3 — Adapter cancellation (all four harnesses).** The `select!`-driven cancel path in each `run_producer`, shared SIGTERM→SIGKILL helper, adapter kills + ends stream (dispatcher synthesizes the `Cancelled` terminal); per-harness fixture + live SIGTERM tests.
- **M4.4 — Per-agent message queue.** FIFO queue in the dispatch layer; auto-dispatch on idle; remove-queued; in-memory only.
- **M4.5 — Event coalescing + concurrency load test.** Per-agent text-chunk coalescing; multi-agent fan-out stress test.

**Cluster M4B — frontend affordances (M4.6–M4.8).** Built against the now-final backend state shapes; second PR boundary.

- **M4.6 — Project switcher (frontend + state reshape).** Always-loaded directory/project-list/active-project state; switcher UI; eager-load + lazy-hydrate; background listeners stay alive.
- **M4.7 — Multi-recipient compose + fan-out.** Multi-select recipients; N independent dispatches; queueing UX for busy recipients.
- **M4.8 — Context menu: cancel + open session file.** Per-agent actions in sidebar + transcript.

**Dependency notes:** M4.1 is foundational for all. M4.2 must precede M4.3 (contract before adapters) and M4.4 (queue keys off the turn lifecycle and idle signal) and M4.8 (the UI action calls the command). M4.7 depends on M4.4. M4.6 has no hard dependency on the cancellation work and could move earlier if convenient, but is sequenced after the backend hardening so the switcher is built against final state shapes.

## Shared patterns established here (reused by later milestones)

- **Cooperative cancellation token** (M4.2/M4.3) is the *same* mechanism M6 will use for workflow-level cancel and M8 for shutdown — `source` already distinguishes them. Do not invent a second cancellation path later.
- **Conversation journal** (M4.2 — user sends + turn outcomes) is the persistence shape M6 will extend for workflow/step outcomes. Establish it cleanly.
- **Register-cache** (`AgentId → (ProjectId, AgentRecord)`, M4.1) is the canonical agent-lookup path; later code must use it rather than re-scanning project registries.
- **Per-agent FIFO queue** (M4.4) is the contention-overflow primitive; M6 decides how workflow dispatch interacts with it, but does not replace it.
- **Concurrency & lock-order (the highest-risk surface in M4 — read before M4.1/M4.2/M4.4).** M4 adds new shared mutable state: the project-lock-handle map (app layer) and the dispatcher's per-agent state entry `{ status, queue, cancel_token, cancel_source, terminal_seen }`. Each must be slotted *explicitly* into the documented lock-order convention, and that doc comment updated to match — do not add a mutex without placing it in the order. Preserve the existing discipline: `std::sync::Mutex` only for O(1) state flips, never held across an `.await`. The trickiest path is M4.4's terminal→next-turn handoff: it must be **atomic under a single lock hold** — on a non-empty queue, transition `InFlight(old) → InFlight(next)` without passing through `Idle`; only reach `Idle` (via the guard) when the queue is empty; start the next turn *after* releasing the lock. So the `AgentIdleGuard` drop is the idle path *only when the queue is empty* — not the unconditional sole idle path. If the safe sequencing isn't obvious from the surrounding code, **stop and confirm before implementing** rather than guessing.
---

## M4.1 — State & lock foundations

### Goal & Outcome

Harden the persistence and process-ownership boundaries M1 deferred, and add the multi-instance/multi-project safety the switcher will rely on. No user-visible feature lands here on its own; this is the floor the rest of M4 stands on.

Outcomes:
- Opening the same project in a second Switchboard process is refused with a clear, typed error; the OS releases the lock automatically if the first process crashes.
- Re-opening an already-loaded project in the *same* process is a no-op that returns the existing in-memory handle (no second lock, no reload).
- Registering or attaching an agent whose (harness + session-id) is already registered in *any* project in the working directory is rejected with a clear error.
- Agent lookup on the hot path (send, switch) is an in-memory cache hit, not a disk scan + `Project` clone.
- A power loss or kernel panic immediately after a project/agent record is written cannot leave a torn JSONL line that bricks future `list_projects` / `list_agents`.

### Implementation Outline

**Project lock (app layer, not core).** This is runtime process ownership, not persistence, so it belongs in `crates/app` alongside `AppState` — core continues to *describe and load* projects; the app owns "this process has this project open." Add a lock-handle map to `AppState` (keyed by `ProjectId`) holding an advisory exclusive, non-blocking file lock on `<directory>/.switchboard/projects/<project-id>/instance.lock`. Acquire it in the project-open path *before* inserting into `AppState.projects`; `create_project` immediately loads what it creates, so it locks too. Release on directory rebind (alongside the existing `projects` / `active_project_id` / `needs_session_meta` clearing in `init_directory_impl`). The lock is the **inter-process** guard only — intra-process re-open returns the existing handle and must not attempt re-acquire. Surface a typed error (a new app-error variant, e.g. project-locked carrying the `ProjectId`); the frontend copy is "This project is already open in another Switchboard window."

- Choose the lock crate per the AGENTS.md dependency policy (use `cargo add`). Recommendation: a cross-platform file-lock crate (e.g. `fs4`) so M8's Windows packaging needs no rework; `nix`'s `flock` (unix-only) is acceptable if you prefer to lean on the existing dep, but then gate behind `cfg(unix)` and leave an explicit Windows extension point. Either way the contract is: advisory, exclusive, non-blocking (fail fast if held), held for the project-loaded lifetime, auto-released on process death.
- This also closes the concurrent-session-file race M1.3 flagged (two instances both seeing `session_file_exists() == false` for the same agent UUID): two processes can no longer hold the same project.

**Register-cache.** Add an `AgentId → (ProjectId, AgentRecord)` map to `AppState`. Populate on project open, on `list_agents`, and on register/attach; clear on directory rebind. Route `lookup_agent` (today at `commands.rs` ~939, which clones every loaded `Project` out of the mutex and scans `registry.jsonl`) through the cache. Since v1 has no agent/project deletion, invalidation is trivial (insert-only within a directory session; full clear on rebind). This is the canonical lookup path — `send_message`, the switcher, and the cancel command all use it.

**Directory-wide session-id uniqueness — verify, don't rebuild (review Topic 7).** This is largely already implemented: `attach_agent_impl` already scans every on-disk project (loaded or not, via `directory.list_projects()`) for a (harness, session-id) collision across all four harnesses (`commands.rs:~183`, including the Codex/Antigravity sidecar scan). M4's job is to **verify and preserve** that scan — do *not* replace it with a parallel cache-based check that could weaken the existing sidecar-corruption handling. The *create-new-agent* path needs **no** cross-project collision check: it mints a fresh session-id (Claude/Gemini) or gets a server-assigned one post-spawn (Codex/Antigravity), so it cannot collide by construction. (Rationale already lives in code comments: two `AgentRecord`s pointing at one harness session is the same-session-parallel-invocation hazard.)

**JSONL durability.** In `crates/core/src/io.rs::append_jsonl`, add `file.sync_data()` after the write (portable) and a parent-directory fsync **gated `cfg(unix)`** — the directory-handle fsync has no portable Windows equivalent (review Topic 7), so leave an explicit Windows extension point, the same unix/Windows posture as the project lock. Keep the existing fail-loud `CorruptJsonl` behavior — no silent skip/repair. The rationale (torn-line-bricks-the-log) goes in a comment.

**Core visibility tightening (low-priority, include if cheap).** Make `Directory.path` and `Project`'s load-bearing fields `pub(crate)` with accessors, so callers can't construct stale handles that bypass invariants. If this turns out to ripple widely across the app crate, stop and confirm scope before continuing — it is the least important item here.

### Definition of Done

- **Unit/integration tests:**
  - Second open of the same project (same process simulating a second lock acquisition, or a genuine second lock handle) fails with the typed locked error; releasing/ dropping the first lock then allows acquisition. Verify the lock is released on rebind.
  - Intra-process re-open returns the same handle and does not error.
  - **Attaching** a duplicate (harness, session-id) across two projects in the same directory is rejected (verify the existing scan still holds for all four harnesses); the same session-id in two *different* directories is allowed. (Create-new needs no such test — it cannot collide by construction.)
  - Register-cache returns the correct `(ProjectId, AgentRecord)` after open/register/attach, and is cleared on rebind.
  - `append_jsonl` round-trips after `sync_data`; a deliberately torn line still surfaces `CorruptJsonl` (fail-loud preserved).
- **Docs:** note the `instance.lock` file in the filesystem layout (system-design §3 already enumerates `.switchboard/` runtime data — confirm the lock file is mentioned or add it). Record the lock-crate choice and the unix/Windows posture in the crate source.
- **Known limitation to record:** if the lock crate chosen is unix-only, state explicitly that Windows multi-instance protection lands in M8.

---

## M4.2 — Cancellation contract (dispatcher + core)

### Goal & Outcome

Establish the cancellation mechanism end-to-end *except* the per-adapter subprocess kill (that is M4.3). After this sub-milestone the system has a complete, type-checked cancellation contract that the `MockHarnessAdapter` can exercise fully.

Outcomes:
- A turn can be cancelled by `agent_id` via a Tauri command; the agent returns to idle and is immediately re-promptable.
- A cancelled turn ends with a terminal event marked `Cancelled { source }`, distinct from `Failed` — the frontend can tell "the user stopped this" from "the harness errored."
- The user's send and every non-completed turn's outcome (failed or cancelled) are persisted in a Switchboard-owned conversation journal, so after restart the transcript shows the user's message once (even for a fan-out) and shows failed/cancelled turns as such; partial streamed *content* of a non-completed turn is not persisted.
- A reusable "cancel all of these agents' in-flight turns → await drain → release their project locks" mechanism exists, tagged with the right `source`, with the lock released only after the turns drain. M4.6 wires it to remove-directory; the app-quit handler is deferred to M8. M4.2 ships and unit-tests the mechanism itself.

### Implementation Outline

**Cancel by agent, not by turn.** Given one-in-flight-turn-per-agent, agent-keyed cancellation is unambiguous and matches the user's mental model ("stop this agent"). The command surface is `cancel_turn(agent_id)` (a `#[tauri::command]` shim over a `*_impl` free function, per the app crate's convention).

**Outcome taxonomy.** Add `Cancelled { source: CancelSource }` to the terminal `TurnOutcome` (in `crates/harness/src/events.rs`; the enum is `#[non_exhaustive]`, so this is additive). `CancelSource ∈ { User, Workflow, Shutdown }`. The TS wire union gets a corresponding additive variant; the frontend reducers' default branch must degrade gracefully on unknown discriminants (existing convention). The rationale — cancellation is intent-bearing, not failure — goes in the type's doc comment, because M6 (workflow cancel) and M8 (shutdown) both depend on this distinction.

**Token plumbing and source stamping (decisions 2 and 13).** The dispatcher's per-agent state entry is `{ status, queue, cancel_token, cancel_source, terminal_seen }` (plus the app-provided dispatch-context factory of decision 13) — the single map the queue (M4.4) and status also live in. On accepting a turn the dispatcher mints a binary `CancellationToken` (`tokio_util`), stores it, and passes a clone into `adapter.dispatch(...)` so the producer can watch it. Expose `dispatcher.cancel(agent_id, source)`: under the state lock it first checks `terminal_seen` — if a terminal `TurnEnd` was already observed (e.g. during Codex's post-terminal enrichment window before `AgentIdle`), it is a **typed no-op** (no kill, no synthesized outcome); otherwise it records `cancel_source` and fires the token. The drain task sets `terminal_seen` on the first `TurnEnd`. The **dispatcher, not the adapter, owns the cancelled terminal event** — when the drain task sees stream-end with a cancelled token, it synthesizes `TurnEnd { Cancelled { source } }` from the stored `cancel_source` (any late adapter terminal after cancel is ignored — exactly-one-terminal preserved). Token, source, and `terminal_seen` reset when the drain task ends on any path — one teardown site, alongside the idle/next-turn transition (M4.4).

- **`dispatch()` signature changes** to accept the (binary) token. This is a breaking change to the `HarnessAdapter` trait — fine and expected. In M4.2 the adapters can accept-and-ignore the token (the `MockHarnessAdapter` honors it) so the crate compiles and the contract is testable before the real subprocess work in M4.3.

**Conversation journal (decision 6).** Add a per-project append-only journal (e.g. `<directory>/.switchboard/projects/<project-id>/journal.jsonl` — confirm naming against the §3 layout). Two record types, both written through the now-fsync'd `append_jsonl`:
- *send* — written **when the turn starts** (co-located with the dispatcher's `TurnStart` emission), **one record per recipient**: `{ type: send, send_id, turn_id, agent_id, prompt, at }`. The frontend mints one `send_id` per Send action and passes it on each per-recipient call, so a fan-out's records share a `send_id` (hydration groups by it → one "User → B|C" message; group render timestamp = `min(at)` across its records). Writing at turn-start (not at submit) means a queued message **removed before it starts is never journaled** — correctly absent after restart — while idle and instantly-cancelled-*after*-start turns are journaled. `turn_id` is included as a free correlation key (available because the write is now at turn-start). One record per recipient (not a recipient list) simply because dispatch is N independent calls; **render-once-by-`send_id` assumes a shared prompt** — M6 must revisit grouping if it introduces divergent per-recipient (templated) prompts.
- *outcome* — written on terminal **for every non-completed turn (failed or cancelled)**: `{ type: outcome, send_id, turn_id, agent_id, outcome: Cancelled{source} | Failed{kind, message}, started_at, ended_at }`, **no agent content** (the failure reason is metadata). Completed turns write no outcome record (their content is in the harness file). This **partitions** cleanly with the harness files (which supply completed-turn content only) — no journal↔harness correlation/dedup, and failures stay visible on reload. The one tradeoff: a failed turn's partial output (if the harness wrote any) isn't shown after restart, only the marker + reason — same as cancelled.

The journal is Switchboard's user-side source of truth; agent content is never written here. Document in a comment that this refines the "Switchboard stores no transcript" invariant: Switchboard owns the user-send journal + non-completed-turn outcomes, harness files own completed-turn agent content. Hydration/merge is M4.6.

**Durability profile (decided in review of M4.1; confirm here).** M4.1 made `append_jsonl` fsync the file per record and (only when it *creates* the file) the parent directory. A sync *failure* is **returned as an error** (the kernel reporting durability couldn't be confirmed), so the durability guarantee is enforceable for the journal — but the caller contract is "never destructively roll back on an append error" (the record may already have committed, since the fsync happens after the write): `Directory::create_project` keeps the project directory rather than deleting it. The journal **is durable-per-record** — its records land at *human-paced* turn boundaries (one at turn-start, one at a cancelled terminal), not per token-delta, so the fsync pressure is negligible, and the journal is the user-side source of truth where losing the last send on a crash is the exact loss it exists to prevent. **Relax to OS-writeback / fsync-on-clean-shutdown only if M4.5 load testing shows fsync-per-record is a real bottleneck** (the documented fallback); if so, `append_jsonl` grows a strict-vs-relaxed durability *mode* (the journal opting into relaxed) — not a change to its error-on-sync-failure contract, which the structural logs depend on. Do not pre-relax.

- **`send_id` is introduced here** (decisions 6/13): minted per send, written on the send + outcome records and carried in the queued-message payload; M4.7 attributes a fan-out's turns to one `send_id`. Defining it now avoids a later schema migration.

**Lifecycle wiring (also resolves review Topic 4 — lock-release ordering).** Build a reusable helper that, given a set of agents, iterates them via the `AppState` register-cache (M4.1), calls `dispatcher.cancel(agent_id, Shutdown)` for each in-flight turn, **awaits drain, then releases each affected project's lock** — never releasing a lock while a turn is still live (that would reopen the double-drive race the lock prevents). The drain task still emits `AgentIdle` last on the cancel path. **M4.2 ships and unit-tests this helper standalone; it is not yet wired to a UI-triggered lifecycle event.** The teardown entry points it serves arrive later: **remove-directory** wires it in M4.6 (when a working directory is dropped from the workspace, its projects' in-flight turns drain before their locks release); the **app-quit** handler is deferred to M8. App-quit needs no cancel-and-drain for correctness — the harness subprocesses are children of the Switchboard process group and die with it, and an interrupted turn is already inferable on next launch from a journal *send* record that has no matching *outcome* marker and no completed-turn content in the harness file. This closes the M1.4 "orphaned drain tasks" deferral at the mechanism level.

**Mock-driven correctness.** `MockHarnessAdapter` should be extended so a test can: start a stream, fire the token, and assert the adapter (mock) stops and the dispatcher emits `TurnEnd { Cancelled }` then `AgentIdle`. This proves the contract without any real subprocess and without M4.3.

### Definition of Done

- **Unit/integration tests (fixture-driven, no live harness):**
  - Cancel an in-flight mock turn → terminal `Cancelled { source: User }` emitted, then `AgentIdle`; agent status returns to `Idle`; a subsequent send succeeds.
  - Cancelling an agent that is *not* in flight is a clean no-op (typed "nothing to cancel").
  - **Cancelling after a terminal `TurnEnd` but before `AgentIdle`** (post-completion enrichment window) is a no-op — no synthesized `Cancelled`, no kill; the turn stays completed.
  - Token/source/`terminal_seen` are cleared on every terminal path (completed / failed / cancelled / truncated) — no leak.
  - A send record is written at turn-start (with `TurnStart`); a queued message removed before it starts is never journaled (absent after restart); an idle send then instant cancel keeps its record. Outcome records are written for failed and cancelled turns; completed turns write no outcome record (their content comes from the harness file).
  - Re-hydration renders the user's message once (grouped by `send_id`) and a cancelled *or* failed turn as a marker carrying its outcome/reason (full merge tested in M4.6).
  - The cancel-all-and-drain helper, given a set of agents with in-flight turns, cancels each with `source: Shutdown`, awaits drain, and releases the affected project locks only after the turns drain (tested standalone — no UI lifecycle trigger in M4.2; remove-directory wiring is M4.6).
- **Docs:** `TurnOutcome::Cancelled` doc comment explains the intent-bearing rationale and that the dispatcher (not the adapter) stamps the source; the journal is documented as Switchboard's user-side source of truth (sends + outcomes, no agent content); update system-design §3/§7/§9 as needed.
- **Known limitation:** partial content is in-memory only (already true); state it where the journal schema is defined.

---

## M4.3 — Adapter cancellation (all four harnesses)

### Goal & Outcome

Make cancellation actually kill real subprocesses, uniformly across Claude Code, Codex, Gemini, and Antigravity, with the harness-specific quirks handled inside each adapter.

Outcomes:
- Firing a turn's token terminates the harness subprocess tree promptly (SIGTERM, then SIGKILL if needed) for every harness.
- Cancellation works even for a harness that buffers its whole response before emitting (no output to interrupt) — the token is observed regardless of whether the read is parked.
- Codex's "exits 0 on SIGTERM, emits no terminal event" behavior is handled: the adapter kills and ends its stream; the dispatcher (which knows the cancel was requested) synthesizes the terminal `Cancelled` event — so no per-harness terminal-synthesis is needed.
- The dispatcher/app layer remains harness-agnostic — all harness-specific *kill* logic lives in the adapters; the cancelled *outcome* is the dispatcher's (decision 2).

### Implementation Outline

**The `select!` loop is the load-bearing pattern.** For the three **stdout-draining** adapters (Claude Code, Codex, Gemini), `run_producer` currently awaits the next event from the subprocess; restructure that loop to `tokio::select!` over *both* the next-event future *and* `token.cancelled()`. This is what makes cancellation work for a buffering harness: the parked read does not block noticing the token, because `select!` polls both concurrently. **Antigravity is different** (review Topic 6): it already uses `tokio::select!` (`antigravity/mod.rs:406`) and tails a transcript *file* on a tick rather than draining stdout — so the "interrupt a parked read" justification does not apply; its cancel branch simply joins the existing `select!` (break the poll loop + kill). When the token branch wins, in every adapter:
1. Send SIGTERM to the process group immediately (do not wait for output).
2. Wait a short grace window; if the group hasn't exited, escalate to SIGKILL.
3. **End the stream — do not emit a terminal event.** The dispatcher synthesizes `TurnEnd { Cancelled { source } }` on the cancelled-token path (decision 2); the adapter owns the kill, not the outcome.
4. Return — the parked read unwinds when the process dies.

**Generalize the kill helper.** `crates/harness/src/subprocess.rs::kill_subprocess_group` currently sends a single signal (SIGKILL). Generalize it to take a `nix::sys::signal::Signal` (and/or add a `terminate_then_kill` helper that does SIGTERM → grace → SIGKILL). The three adapters that already call it (Codex `mod.rs:396`, Gemini `mod.rs:376`, Antigravity `mod.rs:495`) move to the new escalation path. Claude Code, which today relies on stdin-EOF/natural exit and never calls the helper, now wires the cancel path in too — its single process is in its own group, so the same `killpg` works.

**Per-harness quirks (handle inside each adapter, document each in a comment):**
- **Codex:** exits 0 on SIGTERM and may not emit a terminal event. Do *not* infer cancellation from exit code or terminal event — the adapter just kills and ends the stream; the dispatcher's token-driven synthesized `TurnEnd { Cancelled }` is authoritative. The existing `codex/mod.rs:420` comment about exit-0 is the anchor.
- **Gemini / Antigravity:** verify SIGTERM actually tears down the process tree (the M3 plan pre-flagged "verify Gemini SIGTERM before M4"). If a harness ignores SIGTERM, the SIGKILL escalation covers it, but record the observed behavior in the relevant `docs/research/*-cli-observed.md`.
- **Claude Code:** confirm SIGTERM-to-group leaves the session file in a resumable state with the incomplete turn absent (this is the design assumption).

**Stream-drop safety (M1.4 deferral, resolve here).** Today a consumer dropping the event stream does not kill the subprocess (the `Child` lives in the producer task — see the `claude_code/mod.rs:89` comment). With the token now plumbed, cancellation no longer depends on stream-drop; but ensure the token is also fired if the stream/consumer goes away unexpectedly, so a dropped consumer can't leave a live subprocess.

**Live tests.** Add one small-prompt live test per harness exercising real cancellation, named per the AGENTS.md convention `live_<harness>_cancel_*` (e.g. `live_codex_cancel_terminates_group`). They assert the subprocess group is gone after cancel and a `Cancelled` outcome surfaces. These are `#[ignore]`-gated and run via `make test-live` / `make test-live-<harness>`.

### Definition of Done

- **Fixture-driven tests (per harness, using the `fake_*` test binaries):** firing the token mid-stream and pre-first-output both terminate the fake subprocess and yield `TurnEnd { Cancelled }`; the fake-process-group cleanup (the `fake_codex` killpg harness already exists) confirms `killpg` reached the tree.
- **Live tests (developer-local):** `live_<harness>_cancel_*` for all four harnesses; SIGTERM behavior for Gemini/Antigravity verified and recorded in their research notes.
- **Docs:** each adapter carries a comment explaining its cancel path and quirk; research notes updated with observed SIGTERM behavior.
- **Cross-check:** the dispatcher and `commands.rs` contain zero harness-specific cancellation branches (the abstraction held).

---

## M4.4 — Per-agent message queue

### Goal & Outcome

Add per-agent FIFO queueing so a send to a busy agent waits rather than being refused, and dispatches automatically when the agent frees up.

Outcomes:
- Sending to a busy agent enqueues the message; it dispatches automatically when that agent's in-flight turn reaches a terminal state.
- The next queued message starts on idle regardless of *how* the prior turn ended (completed, failed, or cancelled) — cancelling an in-flight turn does not flush the queue.
- A queued-but-undispatched message can be removed; removal hands its text back for editing (the backend simply drops it from the queue and reports what was removed).
- Queue state is per-agent, in-memory, and lost on restart (consistent with in-flight turns).

### Implementation Outline

**Where the queue lives — and why the dispatcher can now actually dispatch (review Topic 1, decision 13).** The original plan put the queue in the dispatcher but the dispatcher, as built, is *handed* the adapter, cwd, and emitter on every `send_message` call — so at the moment a queued message should fire, it had nothing to fire it with. Fix per decision 13: the dispatcher holds the queue + per-agent state, and a per-agent **app-provided dispatch-context factory**. The queue is `VecDeque<QueuedMessage>` inside the entry, where `QueuedMessage` carries only `{ queued_message_id, send_id, prompt }` — **not** a frozen `DispatchOptions` or emitter. At start time (immediate or dequeued) the dispatcher calls the factory, which returns the adapter, cwd, the per-dispatch `SessionMetaObservingEmitter`, and a freshly-read `DispatchOptions` (`is_first_dispatch_after_attach` from `needs_session_meta` *at that moment*). This is load-bearing: a second review confirmed `commands.rs` wraps the emitter and computes `DispatchOptions` per dispatch (`commands.rs:660-674`); freezing either at enqueue would break Codex post-attach `SessionMeta` forcing for queued turns. The factory is invoked **after the per-agent state lock is released** (never across `dispatch().await`) and respects `AppState`'s lock order. `send_message` stops taking adapter/cwd/emitter as per-call parameters.

**Enqueue vs. dispatch decision + wire contract (Topic 1 / Agent 2's queue-contract finding).** `send_message` currently fails with `Busy` if in-flight. Change the contract: if the agent is idle, dispatch immediately; if in-flight, enqueue. The return type becomes a typed union so the frontend can render queued state and reconcile its optimistic user turn:
- idle → `{ status: "started", turn_id }` (turn_id minted at real dispatch, as today, when `TurnStart` fires);
- busy → `{ status: "queued", queued_message_id, send_id }` (no turn_id yet — a queued item has no adapter stream).

A queued message later dispatches and emits a normal `turn_start` at that point. `remove_queued_message(queued_message_id)` drops it and returns `{ agent_id, prompt, send_id }` so the compose bar can restore the text. **Workflow-step dispatch keeps fail-fast** (system-design §7) — so the enqueue-vs-fail-fast choice is a caller-selectable parameter (manual send enqueues; a future workflow step requests fail-fast). For M4, only the compose-bar path enqueues; keep the fail-fast path available.

**Auto-dispatch on idle — atomic terminal→next handoff (review Topic 3).** When a turn reaches terminal state the transition must be atomic under a *single* state-lock hold, to avoid a TOCTOU against a concurrent manual `send_message`: if the queue is **non-empty**, transition `InFlight(old) → InFlight(next)` (dequeue + arm the next turn's guard) **without ever passing through `Idle`**; if **empty**, transition to `Idle`. *Then* release the lock and start the next turn via the factory — **never holding the mutex across `dispatch().await`**. The original wording (drop to Idle → re-lock → dequeue) has a gap: a manual send landing between the Idle transition and the re-lock would either double-dispatch (two turns in flight, violating one-per-agent) or strand the queued message. Consequence: the `AgentIdleGuard` drop is **no longer the sole idle path** — it returns the agent to `Idle` *only when the queue is empty*; a non-empty queue hands the guard off to the next turn under the same lock (update the header invariant to match). This is the single trickiest concurrency path in M4; if the safe sequencing isn't obvious against the actual code, stop and confirm.

**Removal.** A `remove_queued_message` command (by a queued-message id) drops it from the agent's queue and returns enough for the frontend to restore the text. Cancelling an in-flight turn (M4.2) explicitly does **not** touch the queue.

**Ordering interaction with cancellation.** Decide and document: when an in-flight turn is cancelled and queued messages exist, the next one dispatches on idle just as after a normal completion. This is intended (cancellation stops *that* turn, not the user's other queued intent).

### Definition of Done

- **Unit/integration tests (mock adapter):**
  - Send to busy agent → queued; on terminal event the queued message auto-dispatches; ordering is FIFO across ≥2 queued messages.
  - Auto-dispatch fires after completion, after failure, and after cancellation.
  - Remove a queued message → it does not dispatch; remaining queue order preserved; removal returns the message payload.
  - Cancelling the in-flight turn leaves the queue intact.
  - No deadlock / no mutex held across `.await` (exercise concurrent enqueue + drain).
  - **Terminal→next-turn race:** a terminal event with a non-empty queue racing a concurrent manual `send_message` never yields two in-flight turns and never strands the queued message (exactly one runs first, the other queues).
  - **Stale-options guard:** a queued first-post-attach Codex turn still dispatches with `is_first_dispatch_after_attach: true` recomputed at dequeue (extends the existing four-dispatch `needs_session_meta` test to a queued turn).
  - Queue is empty after restart (in-memory only) — assert no persistence path was added for queued messages.
- **Docs:** dispatcher doc comment describes the queue, the enqueue-vs-fail-fast caller choice, and the in-memory/restart semantics.

---

## M4.5 — Event coalescing + concurrency load test

### Goal & Outcome

Keep the UI responsive under multi-agent fan-out by coalescing high-frequency text deltas, and prove the concurrent-streaming path holds up.

Outcomes:
- Streamed text chunks for an agent are batched over a short window into fewer, larger UI events; structured and terminal events are never coalesced or reordered.
- 3+ agents streaming simultaneously do not produce UI-blocking event floods.
- Text still appears effectively real-time to the user.

### Implementation Outline

**Targeted coalescing, not the ring buffer.** The M1.4 deferral lists several options (§10 ring buffer, coalescing, rate limiting, size caps). Choose **per-agent text-chunk coalescing on a short time window** (~25ms is a reasonable starting point — make it a named constant with a rationale comment, tunable). The volume driver is token deltas; structured events (ToolStarted/Completed, TurnEnd/Cancelled, RateLimit, SessionMeta) are low-frequency and must pass through immediately and in order. **Flush the coalescing buffer before emitting any structured or terminal event** so text never arrives after the event that logically follows it.

**Where it lives (review Topic 5 — not a generic emitter wrapper).** `EventEmitter::emit` is synchronous (`dispatcher/src/lib.rs:44`), so a wrapper around it cannot host a timer cleanly (it would block, spawn unmanaged timer tasks, or risk reordering). Put the coalescing in the dispatcher's **per-turn drain task** (`drain_stream`), which is already async but today is a plain `while let Some(event) = stream.next().await` loop — **introduce a `select!`** there over the event stream and a flush interval (note: M4.3 adds `select!` to the *adapters'* `run_producer`, not to `drain_stream`, so this is a new `select!`, not an extension). Keep a per-drain-task text buffer, flush on tick, and flush before any non-text event and on stream end/cancel. Because the buffer is task-local (one per turn), it is **not** shared mutable state and has no lock-order impact. Keep it backend-side of the IPC boundary so the wire carries fewer, larger messages; do not change the `NormalizedEvent` wire shape — coalescing concatenates `ContentChunk` text, it introduces no variant. (The frontend already coalesces adjacent chunks in reducer state, so this targets IPC volume, not render shape.)

**Interaction with cancellation (M4.2/M4.3):** the synthesized `Cancelled` terminal event must trigger a flush of any buffered text first, so partial output already shown is consistent up to the cancel point.

### Definition of Done

- **Unit/integration tests:** a burst of N text chunks within the window emits as one (or few) coalesced event(s) with concatenated text in order; a structured event interleaved in the burst forces a flush and preserves ordering (all preceding text emitted before the structured event); a terminal/cancel event flushes pending text.
- **Load test:** drive 3+ concurrent mock streams each emitting many chunks; assert no unbounded growth and that ordering invariants hold per agent. This is the "load-tests the concurrent-streaming path under heavier fan-out" the v1 outline asks for.
- **Docs:** the coalescing window constant carries a rationale comment; note that the ring-buffer/durable-snapshot option is deliberately deferred (M8) unless load testing shows coalescing is insufficient.
---

## M4.6 — Project switcher (frontend + state reshape)

### Goal & Outcome

Make all of the user's projects — across every working directory they've added — loadable in one session as a single flat list, where switching which one is displayed is display-only and background projects keep running.

Outcomes:
- The loaded app always knows: the set of working directories the user has added (the user-global `workspace.yaml`), the projects in each, the displayed project, and the agents per project — presented as one flat project list, each project labelled with its directory.
- The user can add and remove working directories. Removing one drops it from the workspace, drains its projects' in-flight turns, releases their locks, and leaves the on-disk `.switchboard/` untouched; its on-disk state is not deleted.
- Projects from a currently-unavailable directory (unmounted / moved / deleted) still appear in the list, marked unavailable, with a remove action — sourced from the `workspace.yaml` cached snapshot.
- The user can switch which project's transcript + sidebar is displayed; switching away does not stop the other projects' agents or tear down their event subscriptions, whether they live in the same directory or another.
- A never-opened-this-session project shows a brief loading indicator while its transcript hydrates, then is instant on subsequent visits.
- App startup does not parse every project's full transcript history (eager registry across all directories, lazy hydration).
- After restart, each of the user's messages appears once (grouped by `send_id` across the recipients of a fan-out), and failed/cancelled turns appear as markers — the unified view merges the conversation journal (user side + non-completed outcomes) with harness session files (completed-turn agent content).

### Implementation Outline

**Workspace registry + backend reshape.** `AppState` today binds a single `Directory` (`Mutex<Option<Directory>>`). Reshape it to hold *multiple* loaded directories concurrently, backed by a user-global workspace registry the **app layer** owns (the registry is user-global state; core has no user-global concept). Persist it to `workspace.yaml` in the OS config dir (resolved via the `directories` crate — add it with `cargo add` per the dependency policy; not yet a workspace dependency), written with an atomic temp-write+rename — either promote `core::io::write_yaml` to `pub` and reuse it, or re-implement the same pattern app-side (implementer's choice). The registry stores, per directory, its path and a cached snapshot of its projects (each project's full `ProjectSummary`: `{ id, name, created_at }`, so unavailable rows keep identity + ordering); load it on startup, refresh a directory's cached snapshot whenever that directory is read successfully **and** after any project create/rename in it, and fall back to the cache only when the directory can't be read.

`init_directory` becomes "add a directory to the workspace" — **additive and idempotent**: it must never clear *other* directories' loaded projects, listeners, register-cache entries, or locks (the M4.1 rebind-clears-everything behavior is single-directory-only and does not carry forward — see the transitional note in the Critical premise). Add a `remove_directory` command scoped to **only** that directory's projects: for a *loaded* directory it drains those projects' in-flight turns and releases their locks via the **M4.2 cancel-all-and-drain helper** (passing exactly that directory's agents) then drops the entry, leaving `.switchboard/` on disk; for an *unavailable* directory (its projects came from the cache, never loaded or locked) it simply drops the cached entry — nothing to drain.

The flat project list aggregates projects across all workspace directories; a project carries its owning directory for labelling and routing (the agent spawn cwd). **Eager registry load must not lock every project:** read each directory's rosters through the non-locking pure read (the `directory.open_project` path `enumerate_all_projects` already uses, which does *not* register an `instance.lock`); acquire the per-project lock **lazily** on first activation/dispatch. Locking every project across every directory at startup would scale lock count + cost with total project count and stop a second process from opening anything. There is no single "bound"/"active" directory anymore — only a displayed *project*. Switchboard is **single-instance** (enforce via Tauri's single-instance plugin: a second launch focuses the running window); with one process there is one `workspace.yaml` writer, so no cross-process clobber. The per-project `instance.lock` stays as defense-in-depth.

**Session-id uniqueness widens here for the global harnesses (decision 8).** Now that directories are concurrent, the attach collision scan for **Codex and Antigravity** (sidecar scan) must cover **all workspace directories**, not just one; the **Claude and Gemini** scan (`AgentRecord.session_id`) stays **per-directory** (their session files are cwd-namespaced, so same-id-different-cwd is a legitimately distinct session — widening it uniformly would false-reject). Cost note: the Codex/Antigravity scan now spans every workspace directory.

**Frontend state reshape (the core of this sub-milestone).** Today the frontend effectively represents one selected project at a time. Promote app state to hold, concurrently: the workspace (directories + availability), the flat `projects` list (each labelled with its directory), `activeProjectId`, and an agents-by-project structure, with the existing per-agent `transcripts` / `runtimes` maps continuing to be keyed globally by `agent_id` (they already are — agents are globally unique, so no per-project nesting of transcript state is needed; only the *roster* is per-project). The unified transcript and sidebar render the displayed project by filtering the agent roster to `activeProjectId`, then reading the global per-agent maps. Confirm this against `src/lib/state/index.svelte.ts` before restructuring.

**Switching is display-only.** Selecting a project calls the backend open/set-active path and updates `activeProjectId`. **Do not unregister per-agent listeners on switch** — they already persist for the session (`registerAgent` in the state module). A background project's streaming agent therefore keeps updating its global `runtimes`/`transcripts` entry; switching back shows current state with no reconnect. Document this invariant in the state module.

**Eager registry, lazy hydration.** On startup, load every workspace directory's projects and their agent rosters (cheap — `list_projects` + `list_agents` per directory, backed by the M4.1 register-cache; an unavailable directory contributes its cached snapshot instead). Defer transcript hydration (the expensive session-file parse) until the first time a project becomes active; cache the hydrated state and don't re-hydrate on subsequent switches. Show a loading indicator during first hydration. Background agents stream regardless of hydration (live events append to the per-agent transcript; hydration backfills history).

**Hydration merge — the conversation journal (decision 6, review Topic 3).** Hydrating a project no longer takes user turns from harness session files, and it becomes **project-scoped**, not per-agent (review found the current `load_transcript(agent_id)` per-agent shape can't dedup fan-out user messages across agents). Add a project-level backend command — `load_project_conversation(project_id)` — returning a merged shape: grouped user sends, completed-turn agent content, failed/cancelled outcome markers, parse warnings, and metadata. (If you keep the per-agent `load_transcript`, add a separate `load_conversation_journal(project_id)` and specify the exact frontend merge algorithm + tests — but the single project-scoped command is preferred.) The two sources **partition** by completed-vs-not, so there is no correlation or dedup between them. The merge is, ordered by timestamp (`started_at` for turns; for a user message, the `min(at)` of its `send_id` group):
- **user turns** ← the journal's *send* records, **grouped by `send_id`** and rendered once, attributed to the union of their `agent_id`s (a fan-out shows "User → B|C" a single time; a prompt replicated across N harness files is never shown N times). A partially-queued fan-out shows only the recipients whose turns started (decision 6) — intended, not a bug.
- **completed-turn content** ← harness session files, **assistant-role content only**, for turns that completed — filter out the harness files' *user-role* entries (they are per-agent context, not the canonical user record). Harness representations of *failed* turns are not rendered here; their marker comes from the journal (next bullet), so there is no double-render.
- **failed / cancelled turns** ← the journal's *outcome* records (the failure reason rides in the record). No journal↔harness correlation needed — the partition guarantees these turns aren't also coming from the harness side (decision 5/6).

Live in-flight turns continue to overlay via the reducer as today; the journal/harness merge is the post-hydration backfill. The frontend turn-status union needs a `cancelled` value (distinct from `failed`). **Per-agent view (decision 14): enabled, not surfaced** — the raw per-agent harness file (including the user message as that agent saw it) stays available for a future "what did this agent see" view; M4 keeps the data but does not build that UI.

**Clarify the "streaming on startup" non-issue (record as a comment/doc, since it's a recurring confusion):** harness subprocesses are children of the Switchboard process and die with it, so on a fresh launch *nothing* is mid-stream — there is no in-flight turn to reconnect to. "Background projects keep running" is strictly a within-a-running-session statement (you dispatched in project X, switched to Y; X keeps streaming). Crash recovery of *workflows* (checkpoint-based, step-boundary) is a separate M6 concern and does not resume mid-stream.

**Switcher UI.** A flat project list in the sidebar spanning all workspace directories, each project labelled with its directory and marking the displayed one, with the in-flight/has-activity state visible enough that the user knows a background project is doing something. Affordances to add a working directory and to remove one (and to remove an unavailable directory's entry). Single window only; no multi-window.

### Definition of Done

- **Component tests (mock `invoke` + `listen`, per the AGENTS.md frontend testing guidance):**
  - Switching the active project changes the displayed roster/transcript but does not unregister listeners (assert the listener for a backgrounded agent still updates state when an event arrives after the switch).
  - First activation of a project triggers hydration (loading indicator shown, then history present); second activation does not re-hydrate.
  - Eager load populates the flat project list and rosters across multiple directories without hydrating transcripts (assert hydration is not called at startup for non-active projects).
  - A background agent in a *different directory* receiving a `content_chunk` / `turn_end` after a switch-away updates its state correctly (background activity spans directories).
  - **Workspace registry (backend):** adding a directory persists it to `workspace.yaml` and is idempotent; startup loads projects across all recorded directories; an unavailable directory contributes its cached `{ id, name }` snapshot and its projects render marked unavailable; a successful read refreshes the cached snapshot. `remove_directory` drops the entry, drains its projects' in-flight turns and releases their locks before returning (via the M4.2 helper), and leaves `.switchboard/` on disk (re-adding restores the projects).
  - Post-restart hydration of a project that had a fan-out send renders the user message **once** (grouped by `send_id`), renders a cancelled *and* a failed recipient each as a marker (from the journal, carrying the failure reason) at its `started_at`, does not double-render a harness-recorded failed turn against its journal marker (the partition holds), and does not render harness-file user-role entries.
- **Manual verification (state explicitly if you cannot run the UI):** in `make dev`, add two working directories, dispatch in a project in one, switch to a project in the other, confirm the first keeps streaming and is current on return; remove a directory and confirm its projects leave the list without deleting on-disk state.
- **Docs:** the state module documents the display-only switch, eager/lazy strategy, and the "nothing streams across restart" clarification.

---

## M4.7 — Multi-recipient compose + fan-out

### Goal & Outcome

Let one composed message target multiple agents, creating independent turns, with busy recipients queued rather than blocking the send.

Outcomes:
- The compose bar supports selecting multiple recipients.
- One Send to N recipients creates N independent turns; idle recipients start immediately, busy recipients are queued (M4.4), and the user can see which are which inline.
- Cancelling or erroring one recipient's turn has no effect on the others.
- Removing a queued recipient's message returns its text to the user (Switchboard never silently discards authored text).

### Implementation Outline

**Multi-select recipient picker.** Extend the existing single-select picker (`ComposeBar.svelte`) to multi-select, tracking `selectedRecipientIds`. Preselection ergonomics (last-sent-to) can remain for the single-recipient case; define sensible behavior for multi (e.g., last set, or none) — this is a UI judgment, make it against the code.

**Dispatch = N independent sends sharing one `send_id`.** On submit, mint a single `send_id` for the action, then call the send path once per recipient, **passing that same `send_id` on every call**. The backend writes one per-recipient journal send record per call (M4.2); sharing the `send_id` is what lets hydration group them into one "User → B|C" message. There is **no aggregation** — the resulting turns are independent (system-design §7 "Sends and turns"). Idle recipients dispatch immediately; busy recipients enqueue via M4.4 and render a queued state inline. In the unified view the user's message renders **once** (keyed by `send_id`), not once per recipient. If a per-recipient backend call fails after the optimistic append, only that recipient gets the failure treatment (`failSendStart`), not the whole send.

**`send_id` correlation.** `send_id` was introduced in M4.2 (carried by the journal send/outcome records and the queued-message payload). Attribute all N turns of this send to the one `send_id` so the UI groups the user message once and the journal can relate the send to its turns' outcomes.

**Queued/independent UX.** Show, per recipient, whether its turn is running or queued ("queued — agent X is busy"), and offer remove-from-queue on queued ones (calls the M4.4 removal command; restores text to the compose bar). A single send having some turns running and others queued is expected and must read clearly in the unified transcript (ordering by `started_at` already handles temporal placement).

**No backend `send_message_many` required.** Frontend preflight + per-recipient calls are sufficient for M4; do not build a batch backend command unless testing shows the per-call path is inadequate.

### Definition of Done

- **Component tests (mock `invoke` + `listen`):**
  - Multi-select send to 3 agents creates 3 turns; 2 idle start immediately, 1 busy is queued; the queued one auto-appears as a turn when its agent frees (drive the event sequence).
  - One recipient's `turn_end` with a failure outcome, and one cancelled, leave the others' turns intact and rendered.
  - Remove a queued recipient → text returns to compose bar, no turn dispatched for it.
  - Per-recipient IPC failure after optimistic append fails only that recipient.
  - A multi-select send renders the user's message once (one user turn keyed by `send_id`), not once per recipient.
- **Manual verification (or explicit can't-run note):** fan out to 2–3 agents in `make dev`; confirm parallel streaming and correct queued behavior when one is busy.
- **Docs:** none beyond inline rationale; the semantics live in system-design §7.

---

## M4.8 — Context menu: cancel + open session file

### Goal & Outcome

Surface the per-agent actions M4 supports directly where the user works.

Outcomes:
- The user can cancel an agent's in-flight turn from the sidebar entry and from the agent's turns in the transcript.
- The user can open an agent's underlying harness session file in their default editor.

### Implementation Outline

This is a small sub-milestone — compress accordingly. Add a context menu (sidebar entry + transcript turn) exposing exactly two actions for M4:
- **Cancel in-flight turn** — enabled only when the agent's `run_status` is in-flight; calls `cancel_turn(agent_id)` (M4.2). Show the partial output remaining (M4.2/system-design §7) labelled cancelled.
- **Open session file** — opens the harness JSONL session file via the OS default opener (Tauri's shell/opener capability). The path is derivable from the agent's harness + session-id (the adapters already encode these paths; reuse that logic rather than re-deriving the encoding).

**Explicitly deferred from M4** (record in a comment / the plan, so the next agent doesn't add them speculatively): fork-session and reset/remove. They are part of the v1 product (system-design §7 lists them) but bring deletion/lifecycle complexity (and fork is Claude-only) that is out of M4's scope.

### Definition of Done

- **Tests:** cancel action is disabled when the agent is idle and enabled when in-flight, and invokes the command; open-session-file invokes the opener with the correct path (mock the opener). Keep these light — the heavy cancellation logic is tested in M4.2/M4.3.
- **Docs:** note in the component which actions are intentionally absent in M4 and why.

---

## Acceptance — M4 as a whole

Mirrors the v1 outline's acceptance, updated for the resolved decisions:

- Spawn 3 agents; multi-select a single message to 2 of them; both stream in parallel (or one streams and one queues if busy, then auto-dispatches).
- Sending to a busy agent **queues** the message (no longer "gating UX"); the queued state is visible and removable (removal restores text).
- Opening the same project in a second Switchboard instance is refused with a clear error.
- Cancelling an in-flight turn cleanly terminates the harness subprocess group (all four harnesses), the agent returns to idle and is re-promptable, partial buffered output remains visible labelled cancelled, and after restart the turn shows as cancelled (from the journal outcome record) though its partial content is gone.
- A fan-out send shows the user's message **once** (e.g. "User → B|C"), both live and after restart; an instantly-cancelled *started* turn still shows the user's message after restart (the journal send record is written at turn-start). A recipient still queued when the app closes is absent after restart (intended — its turn never ran).
- Cancelling/erroring one recipient in a multi-recipient send leaves the others intact.
- Switching projects is display-only; background projects keep streaming.
- Registering the same (harness, session-id) in two projects in the directory is rejected.

## Out of scope for M4 (do not build)

- Cross-agent dependency chaining / auto-forward / fan-in (workflow engine — M6).
- Workflow-level cancel and the partial-failure → human-in-the-loop pause design (M6).
- Fork-session, reset/remove agent actions; agent or project deletion lifecycle.
- Queued-message persistence across restart (in-memory only by decision).
- The §10 ring buffer / durable event snapshot (revisit in M8 only if M4.5 load testing shows coalescing is insufficient).
- Windows multi-instance lock, polished walk-away (close-to-tray, sleep), partial-output review UI (M8).
- Multi-window.
- **Stall detection threshold + UX (open question 10.18).** The frontend already has a `heartbeat_timeout` path; the *threshold* and the surfaced UX are deferred to M8, where they belong alongside machine-sleep / walk-away handling (sleep is a primary cause of stalls). Out of M4 by decision — was tentatively flagged for "M2/M4 expansion" in the v1 outline.
- **Structured rollback-failure `CoreError` variant** (v1-noted low priority). Intentionally left as-is (stderr logging via `log_rollback_failure_to_stderr`) — the failure requires both `append_jsonl` and `remove_dir_all` to fail in the same call, which is exceedingly rare; not worth the churn in M4.

## Notes for the implementing agent

- The `HarnessAdapter::dispatch` signature change (adding the cancellation token) is a deliberate breaking change to a workspace-internal trait — it is the clean way to give adapters cancellation and is preferred over side-channels. Update all four adapters and the mock together.
- Keep the dispatcher harness-agnostic. If you find yourself writing `match harness { Codex => ... }` anywhere in the dispatcher or `commands.rs` for cancellation, stop — the harness-specific logic belongs in the adapter (this is the M2/M3 abstraction-load-bearing principle; violating it here is a regression).
- Preserve the existing concurrency discipline: `std::sync::Mutex` for O(1) state flips, never held across `.await`. The `AgentIdleGuard` Drop returns the agent to idle **only when the queue is empty**; a non-empty queue hands off `InFlight(old) → InFlight(next)` under the same lock (M4.4) — so "idle" has exactly one *empty-queue* path, not an unconditional one.
- Follow the existing tagged-error / wire-union conventions when adding `Cancelled` and the new commands (snake_case serde tags; additive `#[non_exhaustive]`; TS default branches degrade gracefully).
- Run `make check` before each review handoff; run `make test-live` (or the per-harness target) for the M4.3 adapter-touching work.
