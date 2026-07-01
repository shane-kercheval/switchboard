# Claude thinking/reasoning — per-model redaction lift (M4.10 follow-up)

> Created 2026-06-01 from a live re-probe of Claude Code 2.1.159, **on top of PR #11 (`149c177`, "Render thinking/reasoning blocks … (M4.10)")** which already shipped the `ThinkingWidget` + render branch. M4.10 scoped Claude reasoning as **unavailable** (text server-redacted to empty, probed @ 2.1.157) with a "re-probe on CLI bump" trigger. That trigger fired: the redaction is now **per-model**, so the renderer M4.10 built "for Antigravity, free if Claude's redaction lifts" now needs to actually serve Claude. The frontend is done; this closes the remaining backend + docs gaps.

## Background — what changed and what we verified

Verified by live probe @ **2.1.159** (2026-06-01), mirroring Switchboard's exact spawn flags (`-p --output-format stream-json --include-partial-messages --verbose --dangerously-skip-permissions`):

- **The redaction is per-model, not per-CLI-version.** `claude-sonnet-4-6` streams **real, non-empty** thinking — `content_block_start` of `type:"thinking"`, then `thinking_delta` events with non-empty `delta.thinking` (158 deltas on a logic puzzle; 2 on a file-read+tool task), closed by `signature_delta`; the same non-empty blocks land on disk (confirmed in two real session transcripts: `switchboard-ui-updates`, `switchboard-per-message-metadata`). `claude-opus-4-8` still emits the thinking *block* (`content_block_start: thinking` + `signature_delta`) but with text **redacted to empty** — exactly the old 2.1.157 behavior (10 empty deltas on a puzzle, 2 on a file-read, 0 non-empty in each).
- **Thinking must be induced.** It appears only when thinking is enabled in the user's Claude config (the dev's global `~/.claude/settings.json` has `alwaysThinkingEnabled: true`) AND the prompt is non-trivial enough that the model chooses to reason. A trivial prompt yields none even on Sonnet. This is the model's per-turn decision, not a Switchboard lever.
- **Model self-report is unreliable.** A `claude-sonnet-4-6` turn answered "I'm Opus 4.8" after a TUI `/model` switch (which only changes the default for *new* sessions, not the running one). The authoritative model is the structured `system/init.model` / `message.model`, never the prose.

## State of play on `origin/main` (post-PR #11) — what's done, what's left

- ✅ **Live rendering is already correct.** `parser.rs` already emits non-empty `thinking_delta` as `ContentKind::Thinking` and collapses empty ones to a non-rendering `Liveness` heartbeat. The reducer keeps `thinking` items separate from `text`. PR #11's `ThinkingWidget.svelte` (via `ui/Disclosure.svelte`) + the `UnifiedTranscript.svelte` branch (`item.item_kind === "text"` → `item.kind === "thinking"` → `ThinkingWidget`) render it distinctly, collapsed, with a preview header and a muted Markdown body. **So a live Sonnet turn already shows the reasoning widget today.** No frontend work in this plan.
- ❌ **The hydrate path drops thinking (M1).** `crates/harness/src/claude_code/session_file.rs` (catch-all `_` arm, currently ~:355) silently skips `thinking` blocks, so a **reopened** Sonnet project loses its reasoning entirely — worse than plain text, it shows nothing. This is the one real code gap and the live/reopen asymmetry to close.
- ❌ **Docs and code comments still assume universal redaction (M2).** `harness-behavior.md` §3.2 (Claude row "❌ nothing to surface") / §7.1, and the `events.rs` / `types.ts` `ContentKind` comments, all state Claude redacts unconditionally. They predate the per-model finding.
- **Decision — do NOT surface empty (Opus) thinking.** For Opus the wire carries `content_block_start: thinking` + `signature_delta`, but Switchboard deliberately collapses the empty deltas to content-free `Liveness` and creates **no turn item**. Surfacing an empty "thinking happened" placeholder would need a backend change and we decided **against** it — an empty widget is noise and reads as a bug. Keep the empty → `Liveness` path as-is (it keeps the heartbeat alive during a long redacted block). If Opus's redaction later lifts, its now-non-empty deltas flow through the same `Thinking` path and the existing widget picks them up for free.

## Required reading before implementing

- This file's Background + State-of-play (the per-model finding and the do-not-surface-empty decision are discussion-derived and not recoverable from the code).
- `docs/harness-behavior.md` §3.2 (reasoning availability, updated for the per-model finding alongside this plan) and §7.1 (the redaction issue tracker).
- `crates/harness/src/parser.rs` (the live thinking/empty → `Liveness` contract that M1 must mirror on the hydrate side: empty → no item, non-empty → `Thinking`).
- `crates/harness/src/claude_code/session_file.rs` — `handle_assistant`'s block match (the `"text"` / `"tool_use"` arms and the catch-all to extend), and the deferred-tool-result reconstruction it sits inside.
- `src/lib/components/ThinkingWidget.svelte` + `UnifiedTranscript.svelte` (PR #11) — to confirm the render branch already consumes a `{item_kind:"text", kind:"thinking"}` item, so M1 needs no frontend change.
- Claude extended-thinking / `stream-json` context: https://platform.claude.com/docs/en/build-with-claude/extended-thinking and the tracked issues in §7.1 (#31326, #20127, #32810).

---

## M1 — Backend: emit `Thinking` on the Claude hydrate path

Compact, isolated change — the one piece of unbuilt functionality. **Goal:** a reopened Claude (Sonnet) project shows the same reasoning the live stream showed, closing the live/reopen asymmetry, while redacted (Opus) blocks continue to surface nothing.

**Outcome:**
- Reopening a project whose Claude transcript contains non-empty `thinking` blocks reconstructs them as turn items distinct from the answer (rendered by the existing `ThinkingWidget`, no frontend change).
- Redacted (empty-text) `thinking` blocks produce **no** item on reopen — parity with the live path's empty → `Liveness` → no-item behavior.

**Implementation outline:**
- In `session_file.rs`, `handle_assistant` matches `"text"` → `TurnItem::Text { kind: ContentKind::Text, … }` and `"tool_use"` → `TurnItem::Tool { … }`, with a catch-all `_` arm that skips everything else (the comment names `thinking`). Add a `"thinking"` arm that reads the block's `thinking` string and, **only when non-empty**, pushes `TurnItem::Text { kind: ContentKind::Thinking, text }`; empty/absent falls through to skip (no item). Reuse the existing `TurnItem::Text` construction — the only differences are `ContentKind::Thinking` and the source field name (`thinking`, not `text`).
- **No wire/TS/frontend change.** `TurnItem::Text` serializes with its `kind`; `LoadedTurnItem`'s text variant already carries `kind: ContentKind`; the reducer maps it to `{item_kind:"text", kind:"thinking"}`, which the PR #11 render branch already routes to `ThinkingWidget`. A hydrated thinking item therefore renders identically to a live one with zero additional code.
- Why non-empty-only: mirrors the live parser's contract (empty thinking → `Liveness`, never content) so live and reopened transcripts match for the same session, and keeps the "don't surface empty Opus thinking" decision consistent across both paths.

**Definition of done:**
- Unit test (alongside the existing `session_file.rs` reconstruction tests): a record with a non-empty `thinking` block followed by a `text` block reconstructs to two items — `TurnItem::Text { kind: Thinking }` then `TurnItem::Text { kind: Text }` — in order, neither folded into the other.
- Unit test: a record with an **empty** `thinking` block (`thinking:""`, signature present) produces **no** thinking item (Opus-redaction parity).
- The stale module/`_`-arm comments describing `thinking` as "dropped/skipped" are corrected to describe the new behavior.
- Manual check (`make dev`): reopen a Sonnet project that has thinking turns and confirm the reasoning widget now persists across reopen (it currently vanishes).
- No live test needed (hydrate reads recorded files); the real-session transcripts cited in Background already exercise the shape. If a fixture is added, prefer a trimmed real Sonnet record.

---

## M2 — Docs & comments: replace "universal redaction" with the per-model truth

Small. The `harness-behavior.md` **findings** were updated alongside this plan; this milestone covers the in-code comments and any remaining capability-statement flips, so chronology lives in git, not the source.

**Outcome:**
- The doc and code comments describe the post-fix reality: Claude reasoning is available and rendered distinctly on non-redacting models (Sonnet 4.6) — live (PR #11) and on reopen (M1) — and redacted on Opus 4.8; the renderer is no longer described as "Antigravity-only."

**Implementation outline:**
- `crates/harness/src/events.rs` `ContentKind` doc comment and the `src/lib/types.ts` `ContentKind` comment: replace the "Claude's thinking text is server-redacted to empty in `-p` mode" universal claim with the per-model reality (Sonnet 4.6 returns non-empty reasoning live + on disk; Opus 4.8 still redacts to empty → surfaces only as `Liveness`). Note that it renders via the existing `ThinkingWidget` and is reconstructed on hydrate (M1). Point to `harness-behavior.md` §3.2 rather than restating detail; keep it short.
- `harness-behavior.md` §3.2 Claude row + the "Surfacing (frontend)" Claude portion, §3.3 (model self-report caveat + `/model` "new sessions only"), §6 (2.1.159 version note), and §7.1 (intro, #31326 exposure cell, workaround verdict): flip from "unavailable / built for Antigravity" to the per-model served reality. **Do not touch PR #11's Gemini G17 content** (§1.1/§1.2/§3.2 Gemini row/§4 G17/§6 Gemini note) — these edits are Claude-scoped. *(Recorded alongside this plan; verify on review that the text matches the post-implementation reality.)*
- Add a one-line back-reference in the original M4.10 plan section noting this follow-up superseded its "Claude unavailable" conclusion (plan-doc cross-reference; no milestone refs in code).

**Definition of done:**
- `make check` clean (fmt, lint, test, type-check).
- No remaining source comment or doc cell asserts the redaction is universal or that Claude surfaces no reasoning; each reflects the per-model reality and points to §3.2.
- Known limitation recorded: Opus 4.8 reasoning remains unavailable (redacted); re-probe **per-model** on CLI bump (the trigger that fired this time).
