# Playbook: reviewing a harness CLI update

**Use this when a harness CLI (Claude Code, Codex, Gemini, or Antigravity `agy`) ships a new version and we need to know whether it affects Switchboard** — before or after upgrading. You may have no prior context on this project; this playbook tells you what to read, what to check, and what to produce. Keep the review evidence-based: **verify claims against our actual code, don't trust the changelog's framing.**

## Last reviewed

The version each harness was last vetted against. **Update the relevant row at the end of every review** (§6). A harness whose installed version is ahead of this is unchecked.

| Harness | Last reviewed | Date | How / result |
|---|---|---|---|
| Codex | `0.137.0` | 2026-06-04 | changelog + code review — no impact |
| Claude Code | `2.1.153` | 2026-05-27 | subagent stream/disk probe (3 parent-tagged shapes, disk-side collapse verified — see `harness-behavior.md` §6); prior auth-failure/overage probes 2026-05-25 against 2.1.149 still current |
| Gemini | `0.42.0` | 2026-05-27 | core probes from M3; logged-out auth shape captured 2026-05-27 |
| Antigravity (`agy`) | `1.0.2` | 2026-05-26 | hands-on probes (transcript, quota, auth) |

## 1. Get context first (read in this order)

1. **`AGENTS.md`** (repo root) — what Switchboard is, the crate layout, and the load-bearing rule: **the dispatcher/app are harness-agnostic; all harness-specific logic lives in the per-harness adapter** (`crates/harness/src/<harness>/`). A change "affects us" only where it touches an adapter's contract with the CLI.
2. **`docs/system-design.md` §9 (Harness integration)** — the adapter pattern and the capability matrix (what each harness exposes natively / derived / unavailable).
3. **`docs/research/harness-behavior.md`** — **the single source of truth** for how each harness behaves in the scenarios we depend on and how our adapter/frontend handles each. This is the spec the update is measured against. (Raw probe provenance is frozen under `docs/research/archive/<harness>-cli-observed.md`; cite it, don't edit it.)
4. **The adapter source for the harness under review**: `crates/harness/src/<harness>/` — at minimum `mod.rs` (spawn + args + outcome classification), `parser.rs` (event/stream parsing), `session_file.rs` (on-disk fields we read). `crates/harness/src/events.rs` defines the normalized event/`FailureKind`/`TurnOutcome` types every adapter maps into.

## 2. Get the update's changes

Prefer the changelog + commit diff over guessing. Per-harness sources:

| Harness | Repo / source | Get changes |
|---|---|---|
| Codex | `openai/codex`, tags `rust-vX.Y.Z` | `gh release view rust-v<new> --repo openai/codex --json body -q .body`; `gh api repos/openai/codex/compare/rust-v<old>...rust-v<new> -q '.commits[].commit.message'` |
| Claude Code | `anthropics/claude-code` (npm `@anthropic-ai/claude-code`) | `gh release view` / GitHub releases / npm changelog |
| Gemini CLI | `google-gemini/gemini-cli` (npm `@google/gemini-cli`) | `gh release view` / GitHub releases |
| Antigravity (`agy`) | **No public changelog** — ships with the Antigravity desktop app and auto-updates | There is nothing to read; review = **re-probe** (see §5). Note the new version (`agy --version`). |

If a code-level look is needed, fetch the specific PRs (`gh pr view <n> --repo <repo>`).

## 3. The dependency surface — what to check

For each changelog item, ask "does it touch one of these contracts?" Most items won't. The contracts Switchboard depends on (all per-harness; confirm specifics in `harness-behavior.md` + the adapter):

- **Invocation flags** — do the flags the adapter passes still exist and mean the same thing? (e.g. `--skip-git-repo-check`, `--dangerously-bypass-approvals-and-sandbox`, `-p`/`--print`, `--resume`/`--session-id`/`--conversation`, `--skip-trust`/`--yolo`/`--dangerously-skip-permissions`.) Renamed/removed flags are **breaking**. Grep the adapter's arg-builder.
- **Stream/event vocabulary** — did the JSON event `type`s or the fields we parse change? Map against `parser.rs`. *Additive* fields/events are usually safe **but verify the parser tolerates unknown types/fields** (most skip-by-default); *renamed/removed* event types or fields we read are breaking.
- **Turn-completion & error detection** — the terminal signal (`result` / `turn.completed`+`turn.failed` / `result.status` / transcript terminal record) and the error discriminants (`is_error`, `turn.failed`, status:error). Did the shape change?
- **Exit-code semantics** — especially the "exits 0 on SIGTERM / on error" cases the cancel path and outcome classification rely on.
- **Session-file layout & fields** — path/naming and the specific fields we read (rate limits, context window, model, usage/tokens, thinking). Map against `session_file.rs`.
- **Session-file record ordering invariants** — does the parser assume strict time-ordering of records, or does it tolerate out-of-order writes? Claude 2.1.150+ writes some `tool_result` records before their matching `tool_use` (within the same turn); our Claude parser tolerates this via a deferred-results queue. If a future Claude version delays a `tool_use` across a *turn boundary* the queue's per-session scope would no longer be safe. Re-verify after Claude updates by running `live_claude_tool_results_bind_after_restart` against a multi-tool prompt (`make test-live-claude`).
- **Auth-failure shape** — the strings/fields our detector matches.
- **Failure / quota message shapes** — substrings we classify on. (We prefer passing harness messages **verbatim**, which is drift-resilient; flag any place we substring-match.)
- **Resume command** — the interactive TUI-resume invocation we generate/copy (M4.8).
- **Config / registry sources** — MCP-server config path & schema, skills directories (read for the sidebar counts).
- **Session-id assignment** — caller-controlled vs server-assigned (changes the capture/sidecar path).

## 4. Assess impact — verify against our code

For every changelog item that *might* touch the surface above, **confirm what we actually use** before calling it impacting. The changelog describes the CLI's whole behavior; we use a slice of it. Examples of the discipline:
- "Added `trace_id` to `turn.started`" → check `parser.rs`: if we **skip** that event, zero impact.
- "Removed legacy `--profile` plumbing" → grep the adapter: if we **never pass `--profile`**, no impact to our invocation (a user's stale config is their problem, surfaces as a normal harness error).
- "Changed usage-limit copy" → if we pass the message **verbatim** (not substring-classified), display is resilient; only a stale captured *string* in the docs.

Classify each relevant item: **breaking** (a contract we rely on changed) vs **additive/benign** (verify tolerance) vs **no-impact** (we don't use it).

## 5. Antigravity (`agy`) — review = re-probe

`agy` has no changelog and auto-updates, so "review the update" means re-running the probes its behavior rests on (it has no structured stream — we tail `transcript.jsonl`, and it exits 0 on everything). After an `agy` version bump, re-verify: a normal turn still writes a parseable `transcript.jsonl` with a terminal `PLANNER_RESPONSE`; the auth-failure stdout line still matches; the session-file/log paths are unchanged. Probe commands and captured shapes are in `archive/antigravity-cli-observed.md`. Always run `agy -p … < /dev/null` (it hangs on an open stdin).

## 6. Record findings & verify

- **Report** the verdict (safe-to-update / breaking) with the specific impacted items and a recommendation.
- **Update the "Last reviewed" table** at the top of this file (version, date, how/result).
- **Record in `docs/research/harness-behavior.md` *only if the update actually impacts Switchboard*.** §6 "Version notes" is for things that change what Switchboard does or documents — a contract that changed, a captured shape/string now stale, a behavior we depend on that shifted. A **no-impact review gets NO §6 entry** — the "Last reviewed" table above is its complete record. Do not add "reviewed X, nothing relevant" commentary or per-update changelog summaries; that bloats the single-source-of-truth doc. Also update any now-stale captured shape or cell. **Do not edit the archived `*-cli-observed.md` files** (frozen provenance).
- **If a contract genuinely changed**, the fix goes in the **adapter** (never a `match harness {…}` in the dispatcher or `commands.rs`).
- **Run the live tests** for any adapter-touching change: `make test-live-<harness>` (claude / codex / gemini / antigravity). These spawn the real CLI and are the project's drift-detection mechanism — they need the developer's logged-in session and cost a little quota (see AGENTS.md "Live testing"). The fixture/`make test` path won't catch CLI drift.

## 7. If you need a live capture

Some shapes can't be read from a changelog (auth-failure output, quota messages, new event payloads). To capture: ask the developer to run the relevant CLI in the target state and share stdout/stderr/exit (e.g. logged-out: `gemini -p "hi" --output-format stream-json < /dev/null`; or a small headless turn with `--output-format stream-json` / `--json`). Quota/credit walls usually can't be forced on demand — record that as a known gap rather than guessing the string.
