# Embedded terminal — implementation plan

A parallel-track feature: an embedded, per-project terminal panel (xterm.js + a real PTY), like VS Code's bottom terminal. The user gets a shell scoped to the project's working directory without leaving Switchboard — natural alongside running agents, and a strong pair with the git/worktree view (`2026-05-31-git-view.md`), where "open in terminal" otherwise shells out to the system terminal.

## How to read this doc

This is a standalone plan for a feature **not** part of the v1 milestone roadmap (`2026-05-12-v1.md`) and **separate from** the git-view plan. It neither blocks nor is blocked by either. Three dependency-ordered milestones, each independently demoable and reviewable.

Written for an implementing agent that will **read the referenced code before acting but was not part of the design conversation.** Discussion-dependent decisions are captured here; anything recoverable from the code (naming, local structure, which helper to reuse) is left to the agent's judgment.

## Read before implementing

Internal (the plumbing this feature reuses or deliberately diverges from):

- `AGENTS.md` — crate layout (pure-Rust crates `core`/`harness`/`dispatcher` carry **no Tauri dependency**; `app` owns Tauri), the `#[tauri::command]`-shim-over-`*_impl` pattern, the version-pinning policy (**use `cargo add` / `pnpm add`, never hand-edit manifests**), and the testing vocabulary.
- `crates/harness/src/subprocess.rs` — **the single most important file to read.** `terminate_then_kill` (SIGTERM → 2s grace → SIGKILL on the **process group**), `apply_path_env`, the stderr-drain pattern. The terminal reuses the process-group teardown semantics.
- `crates/harness/src/claude_code/mod.rs` — the spawn config (`process_group(0)`, `kill_on_drop(true)`) and the `run_producer` long-lived-task shape (a `tokio::select!` loop reading a child's output against a `CancellationToken`). The terminal's PTY reader task mirrors this shape.
- `crates/dispatcher/src/lib.rs` — the `EventEmitter` trait, the `agent:<id>` channel-naming convention, the `CancellationToken` cancel path, and `shutdown_agent` (atomic teardown). Read these to understand the convention the terminal **departs from** for its output transport (see decision 3).
- `crates/app/src/state.rs` — `AppState`, the documented **lock-order convention**, `persist_*` helpers.
- `crates/app/src/lib.rs` — the `#[tauri::command]` shims and `AppHandleEmitter` (the global-`emit` bridge).
- `src/App.svelte` — the active-project center pane (the vertical flex column holding `UnifiedTranscript` + `ComposeBar`, with the agents `Sidebar` beside it) — **where the terminal panel inserts.**
- `src/lib/state/index.svelte.ts` — `registerAgent` / `unregisterAgents`, the `listenerRegistry` of `UnlistenFn`, and the teardown path in `unregisterAgents`. The terminal's per-project lifecycle mirrors this.
- `src/lib/state/workspace.svelte.ts` — `activateProject` (display-only switch; background work keeps running) and `removeDirectory` (the teardown trigger).
- `docs/implementation_plans/2026-05-12-v1-m4-dispatcher-contention-cancel.md` §M4.5 — the coalescing investigation that was **built, measured, and reverted** for harness chunks. The terminal reaches the **opposite** conclusion (decision 4); read M4.5 so the divergence is deliberate, not accidental.
- `docs/ui-conventions.md` — semantic tokens (a component names a role, never a hue), the primitive set, theming. The terminal's colors derive from tokens.

External (read the relevant one before its milestone):

- `portable-pty` — https://docs.rs/portable-pty/ — the WezTerm PTY crate used to allocate the pseudo-terminal and spawn the shell (M1).
- xterm.js — https://xtermjs.org/ , docs https://github.com/xtermjs/xterm.js , and the fit addon https://github.com/xtermjs/xterm.js/tree/master/addons/addon-fit . **Use the scoped packages `@xterm/xterm` and `@xterm/addon-fit`** (the unscoped `xterm` / `xterm-addon-fit` are deprecated) (M3).
- Tauri 2 channels — https://v2.tauri.app/develop/calling-frontend/#channels and https://docs.rs/tauri/latest/tauri/ipc/struct.Channel.html — the streaming transport for PTY output (M2). Read this before M2; the byte-encoding choice (decision 3) depends on what the Channel API supports efficiently in the project's Tauri version.

## Scope

**In scope (v1 of this feature):**

- **One terminal per project**, shell `cwd` = the project's working directory, in a **collapsible bottom panel** of the active-project view, toggled from the title bar.
- A **real PTY** running the user's **login shell**, so it behaves like a normal terminal (full PATH, profile, interactive programs, colors).
- Full I/O: keystrokes → PTY, PTY output → xterm.js, **resize** (xterm fit → PTY `cols/rows`), and a visible **"shell exited"** state with restart when the shell ends.
- **Background-alive, display-only switching**: a project's terminal keeps running when the user switches to another project (consistent with how agents behave); switching back shows its prior output.
- **Clean teardown** on explicit close, on the project's directory being removed from the workspace, and on **app quit** (decision 5 — load-bearing: an interactive shell will not self-exit, so a missed kill orphans it).
- Terminal **theming from semantic tokens** (light/dark parity).

**Explicitly out of scope (deferred):**

- **Multiple terminals per project, tabs, and splits.** v1 is exactly one. (This reshapes the per-project keying — an accepted future breaking change; see decision 6.)
- **Scrollback persistence across app restart.** Scrollback is in-memory for the session only.
- **Shell integration niceties** (cwd tracking, command decorations, prompt markers), link detection, search.
- **Running agents through the terminal as a product feature.** A user *can* type `claude` interactively in it — that is an emergent capability, not a designed surface. Note in the plan that this incidentally touches system-design's deferred "in-app launch of harness's interactive TUI"; do **not** build UI around it or wire it to the dispatcher. Flag in the PR for the maintainer to frame.
- Windows support. macOS-first matches v1; the PTY layer should not hard-code away from Unix, but ConPTY is not a v1 target.

## Architecture decisions (made in the design conversation)

Load-bearing, chosen over alternatives; they must survive into the code (crate/module docs and comments at the decision sites), not just here.

1. **New pure-Rust crate `crates/terminal/` (`switchboard-terminal`), no Tauri dependency.** Mirrors `core`/`harness`/`dispatcher`: PTY spawn, read/write, resize, and lifecycle are headless-testable. `app` depends on it and bridges it to Tauri (commands + channel). A PTY does **not** fit the `HarnessAdapter` trait (it's a long-lived interactive session, not a per-turn dispatch), so it does not belong in `harness` — but it reuses `harness`'s process-group teardown semantics (decision 5).

2. **The terminal spawns the user's *login* shell, so it does NOT need `apply_path_env`.** The harness adapters spawn a non-interactive binary and must inject the login PATH (`apply_path_env`) because they bypass the shell. A login shell (`$SHELL -l`) sources the user's profile and resolves PATH itself — that's the whole point of a real terminal. State this contrast at the spawn site; it's why the terminal's spawn is simpler than the adapters'.

3. **Stream PTY output over a Tauri `ipc::Channel`, deliberately departing from the global-`emit` convention.** Every existing backend→frontend event uses global `app.emit` on `agent:<id>`. Terminal output is the one genuinely high-throughput, ordered, per-terminal byte stream in the app — exactly what `ipc::Channel` exists for, and a poor fit for the global event bus (namespace pollution, per-event overhead, fan-out semantics it doesn't need). Input (keystrokes — human-paced) and resize (rare) go through ordinary `#[tauri::command]`s. **This is the one place the feature breaks an established convention; justify it in a comment at the transport boundary.** Before implementing, confirm against the Tauri-channels docs the most efficient byte encoding the project's Tauri version supports (raw bytes vs. base64 string) — the requirement is: preserve exact byte order, lose nothing (a dropped or reordered byte corrupts escape sequences), and avoid per-byte JSON array overhead.

4. **Batch PTY reads on a small coalescing window — the opposite of the M4.5 harness conclusion, and deliberately so.** M4.5 built output coalescing, measured harness streams as *coarse and model-paced*, and reverted it as a no-op. Terminal output is **not** model-paced — `cat`, build logs, and `yes` genuinely flood. So the PTY reader accumulates bytes and flushes on a size **or** short time threshold, sending fewer, larger channel messages. **Never drop or truncate** to shed load (that corrupts the stream); batching reduces message count without data loss. True backpressure (gating further reads on xterm's async `write` callback) is a refinement — note it as a known consideration, not a v1 requirement. Cite M4.5 at the batching site so the divergence reads as intentional.

5. **Reuse the process-group SIGTERM→grace→SIGKILL teardown, and kill terminals on every exit path including app quit.** Spawn the PTY child in its own process group (`process_group(0)`) and tear it down exactly as `harness::subprocess::terminate_then_kill` does, so the shell *and its children* die together. If that function can be promoted to a shared/`pub` location cheaply, reuse it; otherwise replicate its small body (it's a ~15-line `nix` `killpg` sequence) in the terminal crate — but the **semantics must match**. **Critically:** unlike a harness turn (which self-terminates when its CLI finishes), an interactive shell runs forever until killed. A PTY left alive when Switchboard quits becomes an orphan reparented to init. So the app **must** kill all live terminals on quit (a Tauri exit/window-destroy hook), on explicit panel close, and on directory removal. This is a stronger lifecycle obligation than anything in the harness path; state it where the quit hook is wired.

6. **One terminal per project, keyed by `ProjectId` — no multi-terminal indirection now.** v1 is exactly one terminal per project, so the session registry keys on `ProjectId` directly. Multiple-terminals-per-project would reshape this to a `TerminalId`; that is an accepted future breaking change (the project is pre-launch — a clean reshape later beats speculative indirection now). Do not build the `TerminalId` layer speculatively.

7. **Background-alive, display-only switching — mirror the agent model.** Switching projects is display-only; a backgrounded project's PTY keeps running and accumulating output (just as backgrounded agents keep streaming). Switching back shows prior output. The simplest way to preserve scrollback without a Rust-side replay buffer is to **keep the project's xterm view mounted but hidden** when inactive; recommend that for v1 and record the memory tradeoff (a few hidden terminals is fine for the handful of projects in play) and the limitation (in-memory only — lost on app restart). A Rust scrollback buffer for restart-persistence is a later add.

---

## M1 — PTY core crate (`crates/terminal/`)

### Goal & Outcome

A pure-Rust crate that owns a pseudo-terminal running a login shell, exposing a clean handle — no Tauri, no UI.

Once complete:

- Given a working directory, the crate **spawns the user's login shell on a PTY** with that `cwd`, in its own process group.
- It exposes a **handle** that: streams shell **output** as bytes (a channel/stream the caller drains), accepts **input** bytes (keystrokes), accepts **resize** (`cols`/`rows`), reports **shell exit** (the shell process ended — e.g. the user typed `exit`), and supports explicit **close** (terminate the process group).
- Output is **batched** (decision 4) — the handle emits coalesced byte chunks, not one message per read syscall — with no data loss or reordering.
- Closing the handle (or dropping it) **tears down the shell and all its children** via the process-group SIGTERM→grace→SIGKILL sequence (decision 5).

### Implementation Outline

Create the workspace member (`cargo new --lib crates/terminal`, wire into the workspace; `cargo add portable-pty`, `cargo add tokio-util` if the `CancellationToken` isn't already transitively available — **never** hand-edit manifests). Name it `switchboard-terminal`.

Public surface (shape is the contract; exact names are the agent's against `portable-pty`):

- An **open** function taking the working directory (and optional shell override; default to `$SHELL`, falling back to a sensible macOS default) → returns a session handle.
- The handle owns the PTY master and the child, and runs a **reader task** modeled on `harness`'s `run_producer`: a `tokio::select!` loop reading the PTY master against a `CancellationToken`, accumulating bytes, and flushing batched chunks onto an output channel (`mpsc`) on a size-or-time threshold (decision 4). On read EOF → emit a terminal **exited** signal and stop. On cancel → run the process-group teardown and stop.
- **write**(bytes) → PTY master; **resize**(cols, rows) → `PtySize` on the master; **close**() → cancel the token (reader task performs teardown).

Honor: login shell, **no `apply_path_env`** (decision 2); `process_group(0)` at spawn; teardown semantics matching `terminate_then_kill` (decision 5); batching with no drop (decision 4). Use `thiserror` for a typed boundary error (spawn failure — shell not found, cwd missing — vs. mid-session I/O error).

`portable-pty`'s reader is blocking; bridge it to async appropriately (a dedicated blocking read loop feeding the batching task, or the crate's async support — the agent decides against the API). Keep the PTY master writable from the async side for input/resize while the read loop runs.

### Definition of Done

- **Integration tests** (Cargo `tests/`, public API) against a real PTY in a tempdir: open a shell with a given `cwd`; write a command that echoes a known string (e.g. `echo`); assert the batched output contains it. Write `exit\n`; assert the **exited** signal fires. Open, then `close()`; assert the child process group is gone (no orphan). Resize; assert no error and (where observable) the shell sees new dimensions (e.g. `tput cols` after a resize, or just that the call succeeds without error if the former is flaky).
- A spawn against a **non-existent cwd** or bogus shell returns a typed error, not a panic.
- Batching is exercised: a burst of output arrives as a small number of coalesced chunks, and concatenating all chunks reproduces the exact bytes with no loss/reorder (a high-volume `seq`/`yes | head` style producer makes a good fixture).
- Crate module doc states: login-shell-so-no-PATH-injection (decision 2), the batching rationale citing M4.5 (decision 4), and the process-group teardown obligation (decision 5). Known limitation recorded: macOS/Unix-first (no ConPTY).

---

## M2 — Tauri bridge: commands, channel transport, lifecycle

### Goal & Outcome

The backend surface the frontend calls, with the high-throughput output transport and the load-bearing cleanup wiring.

Once complete:

- A per-project **terminal session registry** in app state holds at most one live `switchboard-terminal` session per project (decision 6).
- Commands exist to **open** a terminal for a project (streaming its output over an `ipc::Channel`), **send input**, **resize**, and **close**.
- PTY output reaches the frontend over a **per-terminal `ipc::Channel`** (decision 3), not the global event bus.
- Terminals are **torn down on app quit, on directory removal, and on explicit close** (decision 5) — no orphaned shells under any exit path.

### Implementation Outline

**Session registry.** Add a terminal manager to `AppState` — a `Mutex<HashMap<ProjectId, TerminalSession>>` (or a dedicated `TerminalManager` struct holding it), where `TerminalSession` wraps the M1 handle plus whatever is needed to push output to the frontend. **Update the documented lock-order convention in `state.rs`** to place the new mutex and document the choice (it's accessed by terminal commands largely standalone).

**Commands** (each a `*_impl` free function + thin `#[tauri::command]` shim; errors stringified at the boundary as existing commands do):

- **open**: takes a `ProjectId` and an `ipc::Channel` (passed from the frontend). Resolves the project's working directory (reuse the existing project/directory lookup), opens an M1 session with that `cwd`, spawns a task that drains the M1 output channel and forwards each batched chunk **onto the `ipc::Channel`** (decision 3), and stores the session keyed by `ProjectId`. If a session already exists for the project, return it/no-op rather than spawning a second (decision 6). Send the **exited** signal over the channel too (a tagged message distinct from output bytes) so the frontend can show the exited state.
- **input**: `ProjectId` + bytes → the session's write. Human-paced; an ordinary command is fine (decision 3).
- **resize**: `ProjectId` + cols/rows → the session's resize.
- **close**: `ProjectId` → cancel/close the M1 handle and remove the registry entry.

**Transport detail (decision 3).** Read the Tauri-channels docs first. The output channel carries a stream of messages that are *either* a batched output payload *or* the exited signal — model this as a small tagged type (a serde enum) so the frontend can discriminate. Pick the byte encoding (raw vs. base64) per what the Tauri version supports efficiently; document the choice and why at the boundary, alongside the justification for using a channel over global `emit`.

**Lifecycle wiring (decision 5 — the load-bearing part).**
- **App quit / window destroy**: register a Tauri exit (or window close-requested / destroyed) handler that closes **all** sessions in the registry. State plainly in a comment that this is mandatory because an interactive shell does not self-terminate, unlike harness turns.
- **Directory removal**: in the existing `remove_directory` path (the backend counterpart to `workspace.svelte.ts`'s `removeDirectory`), close the terminals of every project belonging to the removed directory — paralleling how `unregisterAgents` tears down that directory's agents.
- **Explicit close**: the close command above.

**Wire types + api.** Add the channel message type, and the open/input/resize/close `invoke` wrappers, to `src/lib/types.ts` / `src/lib/api.ts` following existing conventions (and the Tauri `Channel` construction on the frontend). No rendering yet (M3).

### Definition of Done

- **Integration tests** on the `*_impl` functions (the established `app` test style): open creates exactly one session for a project and a second open is a no-op/returns the existing one (decision 6); input/resize/close operate on the right session; close removes the registry entry and tears down the process group.
- **Lifecycle tests**: the app-quit handler closes all sessions (assert the registry empties and process groups are gone); directory removal closes that directory's projects' terminals and leaves other projects' terminals running.
- Output forwarded over the channel round-trips exactly (a known-output command produces matching bytes on the channel); the exited signal is delivered as a discriminable message.
- Lock-order doc updated with rationale. Decision 3 (channel-over-emit) and decision 5 (quit teardown obligation) are documented at their sites.

---

## M3 — Frontend terminal panel (xterm.js)

### Goal & Outcome

The visible terminal: a collapsible bottom panel wired to the M2 backend, themed to match the app.

Once complete:

- A **title-bar toggle** opens/closes a **collapsible bottom panel** in the active-project view; the panel is **resizable** by dragging its top edge.
- The panel hosts an **xterm.js** terminal wired to the project's PTY: output from the channel renders; keystrokes go to the PTY; resizing the panel (or window) **fits** xterm and pushes new `cols/rows` to the PTY.
- Switching projects is **display-only**: a backgrounded project's terminal keeps running and shows its prior output on return (decision 7).
- When the shell **exits**, the panel shows an exited state with a **restart** affordance.
- The terminal is **themed from semantic tokens**, correct in light and dark.

### Implementation Outline

**State module.** Add `src/lib/state/terminal.svelte.ts` mirroring the agent-state lifecycle pattern (`registerAgent`/`unregisterAgents`): a per-`ProjectId` structure holding the xterm instance (or a handle to it), the Tauri `Channel`, and open/visible flags. Open a project's terminal lazily (first time the panel is shown for it); keep it alive across project switches (decision 7). Tear it down — call the M2 close and dispose the xterm instance — when the project's directory is removed, hooking the existing `removeDirectory` teardown in `workspace.svelte.ts` (the same place `unregisterAgents` is called).

**Panel placement.** Insert the panel into the active-project center column in `App.svelte` — the vertical flex column currently holding `UnifiedTranscript` + `ComposeBar`. Add the terminal as a collapsible, height-bounded child of that column (between the transcript and the compose bar, or below it — the agent's call against the layout), so the transcript flexes and the terminal takes a bounded, resizable height. Do not disturb the agents `Sidebar` beside the column. Gate rendering on an active project + the open toggle.

**xterm wiring.** `pnpm add @xterm/xterm @xterm/addon-fit`. Construct the terminal, load the fit addon, attach to the panel element. On the M2 output channel message: if output → `term.write(bytes)`; if exited → show the exited/restart state. On `term.onData` → the M2 input command. On panel/window resize → fit addon recompute → M2 resize command (debounced/rAF-batched is fine). Send the initial size on open. **Background-alive (decision 7):** keep the xterm view mounted-but-hidden when its project isn't active rather than disposing it, so scrollback survives a switch; record the in-memory-only limitation.

**Theming.** Build xterm's theme object (`ITheme`) from the app's semantic tokens (read the CSS custom properties, or map from the same token source `harnessDisplay`/`app.css` expose), so the terminal tracks light/dark with the rest of the app. Per `ui-conventions.md`, do not hard-code hex values — derive from tokens. Reuse the `syntax-*` palette where ANSI colors map naturally rather than inventing new hues.

**Resize handle.** No split/resize primitive exists in the codebase; implement a minimal drag-to-resize on the panel's top edge with pointer events (no library) plus a collapse toggle. Keep it small — this is not a general split system.

**Preferences (minimal).** Optional font-size and shell-override could surface in the existing `SettingsView` following its persistence pattern; keep to what's clearly useful and defer the rest. Not load-bearing — a sensible default font size and `$SHELL` are enough for v1.

**Component-level tests required** (per AGENTS.md, for components wrapping `invoke`/channel + reactive state): mock the command/channel surface and exercise open, output rendering, input dispatch, resize, the exited/restart state, and the toggle.

### Definition of Done

- Toggle opens/closes the panel; the panel resizes by dragging; the Projects experience and agents sidebar are unaffected.
- A typed command runs and its output renders; keystrokes reach the shell (an `echo` round-trips visibly); resizing fits xterm and the shell observes the new size.
- Project switch keeps a backgrounded terminal running and shows its prior output on return (decision 7); the in-memory-scrollback limitation is recorded.
- Shell `exit` shows the exited state and restart re-opens a working terminal.
- Terminal colors derive from tokens and are correct in light and dark.
- **Component tests** (mocked transport) cover open, output, input, resize, exited/restart, and toggle; **state tests** cover the per-project lifecycle and teardown on directory removal.

---

## Cross-milestone notes

- **Dependency order is M1 → M2 → M3.** M1 is the headless PTY engine; M2 is the Tauri bridge + lifecycle (the load-bearing cleanup work); M3 is pure frontend. This mirrors the git-view plan's crate→bridge→UI split for consistency.
- **Establish-once, reuse-later:** M1 defines the session-handle contract; M2 defines the channel-transport and teardown conventions; M3 consumes both. The two deliberate divergences from existing conventions — `ipc::Channel` over global `emit` (decision 3) and reverse-of-M4.5 batching (decision 4) — are each justified at their site so a future reader doesn't "fix" them back.
- **The one truly load-bearing risk is lifecycle (decision 5).** An orphaned interactive shell is the failure mode that doesn't exist in the harness path. Treat the app-quit teardown as a first-class requirement, not polish.
- **Per AGENTS.md:** complete each milestone (code + tests + docs) and stop for human review before the next; do not commit until approved. Use `cargo add` / `pnpm add` for every dependency. No milestone/"added in MN" references in code — describe rules directly. When an assumption is load-bearing and ambiguous against the actual code, ask before implementing.
- **Framing note for the maintainer:** the terminal incidentally lets a user run a harness's interactive TUI (type `claude`), which system-design currently defers. v1 builds no product surface around that; surface it in the PR so the maintainer can decide whether/when to make it a designed feature.
