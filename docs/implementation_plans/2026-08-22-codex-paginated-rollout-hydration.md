# Codex paginated-rollout hydration (G30)

**Status: planned.** Branch: `fix/codex-paginated-rollout-hydration`. One PR; milestones below are commits, not separate review units.

## Problem

Codex switched newly created threads to a new rollout format: `session_meta.history_mode: "paginated"` (TUI threads at CLI 0.147.0, `codex exec` threads at 0.148.0; Switchboard currently runs against 0.149.0). In that mode, Codex's persistence policy stops writing four `event_msg` record types our disk parser (`crates/harness/src/codex/session_file.rs`) reads:

| Dropped record | What we used it for |
|---|---|
| `event_msg/user_message` | the hydrated `Turn::User` (prompt text) |
| `event_msg/agent_message` | the agent turn's `TurnItem::Text` (answer text) |
| `event_msg/patch_apply_end` | structured edit content — for **both** reopen hydration and the live `ToolFacetUpdated` enrichment |
| `event_msg/mcp_tool_call_end` | structured MCP results (`content`, `is_error`) |

The same content now rides a new record, `event_msg/item_completed`, whose `item` field is a `TurnItem` variant (`UserMessage`, `AgentMessage`, `Reasoning`, `CommandExecution`, `FileChange`, `McpToolCall`, …). Our parser skips it, so a paginated rollout hydrates **silently** (zero warnings) into agent turns with `items: []`, no `Turn::User`, and edits degraded to `facet_kind: other`. Two live tests catch it: `live_codex_apply_patch_emits_edit_facet` and `live_codex_transcript_load_via_captured_locator_round_trips` (live suite 11/13).

Everything else survives paginated mode unchanged — `session_meta`, `task_started` (incl. `model_context_window`), `task_complete`, `token_count`, `turn_context` (incl. the `turn_id` hydration key, model, effort, cwd), `world_state`, and **every `response_item` record** (so tool *rows* still hydrate today; only their text siblings and structured enrichment are gone).

Full operational record: `docs/harness-behavior.md` **G30** and the §6 "Codex 0.148.0" version note. Review reasoning: the 2026-08-22 entry in `docs/harness-update-review.md`.

## Required reading (before implementing)

Upstream source at the reviewed tag — read these, do not trust this plan's paraphrase if they disagree:

- Persistence policy (what is/isn't written per mode): <https://github.com/openai/codex/blob/rust-v0.149.0/codex-rs/rollout/src/policy.rs> (`should_persist_event_msg`)
- `TurnItem` variant definitions (the `item` payload shapes): <https://github.com/openai/codex/blob/rust-v0.149.0/codex-rs/protocol/src/items.rs>
- Where `exec` opts into paginated (and the legacy fallback on store rejection): <https://github.com/openai/codex/blob/rust-v0.149.0/codex-rs/exec/src/lib.rs> (`thread_start_params_from_config`, `start_thread`)

In-repo: `docs/harness-behavior.md` §3.1/§3.6/G30/§6, `crates/harness/src/codex/session_file.rs` (both `CodexReconstruction` and the `Enrichment` path), `crates/harness/src/codex/facets.rs`, and the live tests named above.

## Verified ground truth (probes on 2026-08-22, codex-cli 0.149.0)

These facts were established empirically against real rollouts and drive the design; they must survive into code comments/fixture READMEs where load-bearing:

1. **The durable mode predicate is `session_meta.history_mode == "paginated"`** — never a CLI-version comparison. Old files have no `history_mode` field at all (→ legacy). Both upstream callers only *request* pagination and retry with `history_mode: None` if the store rejects it, so a paginated-capable CLI can still write legacy files. Resuming a legacy thread keeps writing legacy records (probe-verified both directions).
2. **`item_completed` coexists with `response_item` records for the same content, and the IDs only partially agree.** `AgentMessage`/`Reasoning` items share their `response_item` id exactly (`msg_…`/`rs_…`); `CommandExecution`/`FileChange` items carry a synthetic `exec-<uuid>` id matching **neither** the `custom_tool_call` record id (`ctc_…`) **nor** its `call_id`. ID-based joining for tools is impossible; naive both-channel ingestion double-counts every tool.
3. **A failed tool call can have no `item_completed` record at all.** Observed: a failed `apply_patch` produced `custom_tool_call` + `custom_tool_call_output` (with the failure text) but no `FileChange` item — and no live-stream item either. So `item_completed` is an incomplete record of tool activity; `response_item` is the complete one.
4. **File-order adjacency holds:** when a tool's `item_completed` exists, it lands *between* that tool's `custom_tool_call` and `custom_tool_call_output` records.
5. **`item_completed` tool payloads are strictly richer than what the dropped records carried:**
   - `FileChange`: `changes: { "<abs path>": { type, unified_diff, move_path } }` plus `stdout`/`stderr`/`status` — a real per-file unified diff (better than `patch_apply_end`'s V4A text) and a rename field (`move_path`) we never had.
   - `CommandExecution`: unwrapped `command` array, `parsed_cmd`, separate `stdout`/`stderr`, `aggregated_output`, and a first-class integer `exit_code` — vs. today's format-sensitive `Script failed`/`Process exited with code` string sniffing on the wrapper output.
   - `McpToolCall`: `{ server, tool, arguments, status, result: CallToolResult }` per the upstream struct — **source-inferred only; no real capture exists yet** (M1 closes this).
6. **Text-block case inconsistency:** `UserMessage.content` blocks use `{"type": "text", …}` (lowercase) while `AgentMessage.content` blocks use `{"type": "Text", …}` (capitalized). Extraction must tolerate both — read the `text` field of each block rather than gating on the exact `type` string.
7. `Reasoning` items carry `summary_text: []` and encrypted raw content — nothing renderable, consistent with Codex's existing no-thinking-prose posture (`harness-behavior.md` §3.2). We skip them.

## Design (decided in review; do not re-litigate)

**Branch on mode; per-content-type canonical sources.** `CodexReconstruction` reads `session_meta.history_mode` and holds it as parser state. Missing/unknown values default to **legacy** (old files predate the field; an unknown future value degrades to today's behavior, never to silence-plus-guessing). The canonical source per content type:

| Content | Legacy rollouts (unchanged) | Paginated rollouts |
|---|---|---|
| User prompt text | `event_msg/user_message` | `item_completed/UserMessage` |
| Agent answer text | `event_msg/agent_message` | `item_completed/AgentMessage` |
| Reasoning | skipped | skipped (encrypted; fact 7) |
| Tool **rows** (identity, input, output, error) | `response_item` (`custom_tool_call`/`function_call` + outputs) | **same — `response_item` stays canonical** |
| Tool **enrichment** (structured edits, exit codes, MCP results) | `event_msg/patch_apply_end`, `mcp_tool_call_end` | `item_completed/{CommandExecution, FileChange, McpToolCall}` **overlaid onto** the `response_item` rows |
| Turn boundaries / usage / model / effort / `hydration_key` | unchanged | unchanged (those records all survive) |

Rationale for the two non-obvious calls, which must be recorded in code comments:

- **Why not `item_completed`-as-sole-tool-source on paginated files:** fact 3 — failed calls can be `item_completed`-less, so replacing `response_item` would silently drop failed tool rows (the worst failure class: lost error evidence). `response_item` covers every call; `item_completed` upgrades the ones it covers.
- **Why the overlay pairs positionally, not by ID:** fact 2 makes ID-joining impossible. The pairing rule is: an `item_completed` tool item applies to the **most recently opened tool row that has not yet received an overlay**, of a compatible kind (`CommandExecution`/`FileChange` → `exec`/`apply_patch`-shaped builtin rows; `McpToolCall` → MCP rows). Fact 4 (adjacency between `custom_tool_call` and its output) makes this deterministic in observed files. This is the same positional-pairing class the codebase treats as a hazard elsewhere (Antigravity G24), so the overlay must **fail soft**: an overlay that finds no compatible open row is dropped with a `ParseWarning`, never mis-attached to an already-overlaid or kind-incompatible row.
- **Overlay semantics follow the existing precedence rule** already documented in §3.1: explicit structured status (now including `CommandExecution.exit_code` and `FileChange.status`) outranks the format-sensitive string-sniffing fallbacks (`Script failed`, `Process exited with code`). On paginated files the overlay sets `is_error` from `exit_code`/`status`, replaces the `Other`/paths-only facet with a content-bearing one (`unified_diff` → the edit-pair representation `patch_apply_end_facet` produces today; `move_path` → the file's rename), and may upgrade the row's displayed command from the wrapper JS to the clean `command`/`parsed_cmd`. Do not weaken the legacy path's behavior in the process.

**Both consumers get the fix.** `session_file.rs` has two readers of the same bytes: `CodexReconstruction` (reopen hydration) and the `Enrichment` struct filled by `parse_session_content` (post-terminal re-read that powers live `ToolFacetUpdated`, usage, rate limits). Fixing only reconstruction leaves live edit facets broken. The `Enrichment` patch-facet source becomes mode-dependent the same way: `patch_apply_end` on legacy, `item_completed/FileChange` on paginated, feeding the **existing** live pairing mechanism unchanged (read `mod.rs` for how facets reach live rows today — reuse it, only swap the source).

**Out of scope** (explicitly, to keep this matched to the problem): no re-architecture of the legacy path; no reading of `response_item/message` (the doc-comment rationale at its skip arm still holds — text is single-sourced per mode); no `Reasoning`/`WebSearch`/collab-item surfacing; no frontend changes (the wire shapes are unchanged); no README entry (fixed before any user-facing release symptom).

---

## M1 — Ground truth: MCP capture + fixture set

### Goal & Outcome

Close the one evidence gap (the `McpToolCall` item shape is source-inferred, not observed) and turn the already-captured real rollouts into the fixture set every later milestone tests against.

- A real paginated rollout containing an MCP tool call (success, and error if capturable) exists and its `item_completed/McpToolCall` shape is documented.
- Sanitized fixtures exist under `crates/harness/tests/fixtures/codex/` covering: paginated text-only turn; paginated shell+edit turn **including the failed-`apply_patch` asymmetry** (a `custom_tool_call` with no `item_completed`); paginated MCP turn; and a legacy-mode control file.
- The captured shapes are recorded before any parser work begins (per the project's document-before-probing discipline), so an interruption loses nothing.

### Implementation Outline

Probe first: run a small live `codex exec --json` turn in a scratch directory whose `~/.codex/config.toml` MCP servers are available (the developer's `tiddly` servers are configured; a prompt like "call the <server> get_context tool, then reply ack" suffices — keep the response tiny per the live-test cost policy). Capture the rollout from `~/.codex/sessions/<date>/rollout-*.jsonl`. If an MCP *error* shape can be captured cheaply (e.g. calling a nonexistent tool), do; if not, record that as a known gap in the fixture README rather than fabricating one.

Then build fixtures by sanitizing the real captures from the 2026-08-22 probes plus the new MCP capture (paths, prompt text, thread ids — follow the sanitization conventions of the existing `codex/` fixtures). Preserve verbatim: record order (facts 3–4 are ordering facts), the `history_mode` field, the ID shapes (`msg_…`, `ctc_…`, `call_…`, `exec-<uuid>`), and the text-block case split (fact 6). A short fixture README (or header comments, matching existing fixture convention) states what each file pins and cites facts 1–6.

### Definition of Done

- MCP capture done and its shape recorded (or the error-variant gap explicitly noted).
- Fixtures committed; no parser changes yet. A trivial ingestion smoke test (parse each fixture, assert no panic) is optional — the real assertions land with M2/M3.

---

## M2 — Mode detection + paginated text hydration

### Goal & Outcome

Reopening a paginated Codex thread shows the conversation's text again.

- The disk parser detects `history_mode` per file; legacy files (including all pre-existing ones, which lack the field) parse byte-for-byte as today.
- On paginated files, `Turn::User` hydrates from `item_completed/UserMessage` and agent answer text from `item_completed/AgentMessage` — each rendered exactly once.
- Unknown `item_completed` item types are skipped silently (the parser's existing unknown-type convention).

### Implementation Outline

Add mode state to `CodexReconstruction`, read from `session_meta` at ingest (default legacy per fact 1 — put that rationale in the comment). Add an `item_completed` arm to `handle_event_msg` that dispatches on `item.type`, active regardless of mode for dispatch but with each handler gated so legacy files (which per upstream policy only carry `Plan`/extension items in `item_completed`) cannot double-ingest — simplest is: text handlers no-op unless mode is paginated, and symmetric guards keep the legacy `user_message`/`agent_message` arms from firing on paginated files (they shouldn't exist there, but the guard makes the single-source invariant structural rather than trusted).

`UserMessage` follows the existing `user_message` handling contract: push to `self.turns` directly, anchored to the open builder's `started_at` (the anchoring rationale in the current comment carries over). `AgentMessage` follows the `agent_message` contract: append a `TurnItem::Text` to the open builder. Text extraction reads each content block's `text` field tolerant of the `"text"`/`"Text"` case split (fact 6 — comment it).

Update the parser's regression-tripwire comment at the `response_item/message` skip arm: it currently claims the `event_msg` text records "flow alongside in every observed Codex session" — no longer true; rewrite it to state the per-mode single-source rule and point at G30.

### Definition of Done

- Unit/fixture tests: paginated text-only fixture yields user + agent turns with correct text, exactly once; legacy control fixture output unchanged (assert against current expected output, not just "no crash"); a file *without* `history_mode` takes the legacy path; text-block case tolerance covered both ways.
- All existing legacy fixtures and unit tests pass untouched (NEVER weaken or delete them to get green).

---

## M3 — Paginated tool overlay (exit codes, edits, MCP)

### Goal & Outcome

Tool rows on reopened paginated threads carry trustworthy status and full structured content — better than legacy rollouts ever did.

- Shell rows get `is_error` from the real `exit_code` (no string sniffing on these threads) and keep their facet.
- Edit rows recover `facet_kind: edit` **with content** (per-file diffs from `unified_diff`) and surface renames via `move_path` — which also closes the §5 "`*** Move to:`" open capture.
- MCP rows recover structured results/`is_error` from `McpToolCall.result`.
- A failed tool call with no `item_completed` record still hydrates exactly as today (fact 3) — nothing lost, nothing duplicated.

### Implementation Outline

Extend the M2 `item_completed` dispatch with `CommandExecution`/`FileChange`/`McpToolCall` handlers implementing the overlay contract from the Design section: pair positionally to the most recent open, un-overlaid, kind-compatible tool row; fail soft with a `ParseWarning` on no match; structured status outranks the string-sniffing fallbacks (mirror how `patch_apply_end`/`mcp_tool_call_end` already interact with `function_call_output`-derived state — the "don't overwrite an authoritative result" ordering rules there are the pattern to follow, and the pairing-hazard rationale must land in a comment citing facts 2–4).

For `FileChange`, convert `unified_diff` into the same edit-pair representation the existing `patch_apply_end_facet` produces so the frontend renders both generations identically; carry `move_path` through the facet's rename representation (check what `EditedFile`/`ChangeKind` support — if rename isn't representable today, representing it is in scope, inventing a broader facet redesign is not). For `CommandExecution`, decide at implementation time (reading how the frontend consumes `ToolFacet::Shell` and the row's `name`/`input`) whether to upgrade the displayed command from the wrapper JS to the clean `command` string — do it if it's a facet-level change, skip it with a code comment if it would ripple further; the load-bearing part is `exit_code` → `is_error`.

`McpToolCall` is implemented against the M1 capture, not the upstream struct alone. If M1 could not capture the error variant, handle `status`/`result.is_error` per the upstream struct but mark the error path source-inferred in a comment.

### Definition of Done

- Fixture tests against the M1 shell+edit fixture: exit-code-derived `is_error` (success and failure), edit facet with diff content and exactly-once rows, the `item_completed`-less failed `apply_patch` hydrating unchanged, overlay-without-match producing a warning (synthesize by reordering a fixture copy), MCP success (+ error if captured) rows carrying structured results.
- Legacy fixtures still byte-identical in output.
- `docs/harness-behavior.md` §5's `*** Move to:` open-capture entry updated (resolved for paginated threads via `move_path`; still open for legacy V4A).

---

## M4 — Live edit enrichment from `FileChange`

### Goal & Outcome

Edits made by a live Codex turn on a paginated thread show their diffs in the running app, not just after reopen.

- The post-terminal enrichment re-read supplies `ToolFacetUpdated` content for live `file_change` rows on paginated threads (today it finds no `patch_apply_end` and supplies nothing — the live row stays a paths-only Edit facet).
- Legacy threads' enrichment behavior is unchanged.

### Implementation Outline

Make `parse_session_content`'s patch-facet extraction mode-aware using the same mode detection as M2 (shared helper, not a re-derivation — establish it in M2 with this consumer in mind): `patch_apply_end` on legacy, `item_completed/FileChange` (converted via the M3 facet builder) on paginated. Feed the **existing** downstream pairing/emission mechanism in `mod.rs` untouched; if that mechanism keys facets in a way `FileChange` can't satisfy (it lacks `call_id` — fact 2), resolve it inside the enrichment parse (the file also contains the `custom_tool_call` records, so the M3 positional pairing can recover the `call_id` there), not by changing the live-side contract.

Note the known asymmetry for the test below: a failed `apply_patch` produces no live `file_change` row *and* no `FileChange` item (fact 3), so counts match; comment this where the pairing happens.

### Definition of Done

- Fixture-driven adapter test (the `fake_codex` pattern): a staged paginated session file plus a replayed live stream yields `ToolFacetUpdated` with diff content for the live edit row — the offline twin of `live_codex_apply_patch_emits_edit_facet`.
- Legacy enrichment tests untouched and green.

---

## M5 — Documentation + full verification

### Goal & Outcome

The single-source-of-truth docs describe the now-supported third generation, and the drift-detection suite is green.

- `make check` passes; `make test-live-codex` passes **13/13**, including the two currently-failing tests.
- `docs/harness-behavior.md` reflects reality: G30 closed, the third rollout generation documented, stale caveats lifted.

### Definition of Done (this milestone is mostly its DoD)

- Run `make test`, `make lint`, then `make test-live-codex`. If live tests still fail, that is a finding to fix, not to annotate around. Consider extending `live_codex_transcript_load_via_captured_locator_round_trips` to assert the rollout it exercised was actually `history_mode: paginated` — otherwise a future upstream legacy-fallback would let it pass without exercising the new path (silent-guard erosion; cheap assertion, add it).
- `docs/harness-behavior.md`: mark **G30** ✅ closed with the shipped mechanism (per-mode sources + overlay + both consumers); update §3.6's Codex generation table (three generations, third now supported, per-mode edit/exit-code sources); lift the §3.1 caveats this fix resolves (the paginated cells in the parity table, the `patch_apply_end`-absence caveat) and note that paginated threads now have *structured* exit codes where legacy threads keep the string-sniffing fallback; update §6's 0.148.0 note tail from "fix scope: G30" to "fixed" with a pointer.
- `docs/harness-update-review.md`: update the Codex row's follow-up clause (live 13/13, G30 closed).
- Blow-by-blow reasoning goes in the PR description (git owns chronology) — not into the docs.
- Known limitations recorded, not silently dropped: MCP error-variant capture status (from M1); legacy threads keep string-sniffed exit status (unchanged, by design); `Reasoning` items intentionally skipped.
