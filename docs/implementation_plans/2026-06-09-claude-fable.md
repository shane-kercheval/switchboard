# Adding Claude Fable 5 — findings & justification

**Date:** 2026-06-09 · **Claude Code:** 2.1.170 · **Author:** investigation by agent (live probes against the dev's logged-in `claude` session)

This is **not** a milestone implementation plan — it's a record of what was checked and why
adding Claude Fable 5 to Switchboard is a **one-line** change. It exists so a future agent can
see the probe evidence behind that one line without re-deriving it. Canonical behavior lives in
`docs/research/harness-behavior.md` (§3.2 thinking, §3.3 model, §3.4 effort, §6 version notes,
§7.1 redaction); this doc just narrates the reasoning.

## Context

Claude Code 2.1.170 shipped **Claude Fable 5** ("a Mythos-class model … capabilities exceed those
of any model we've ever made generally available"). The same release was reviewed for harness-CLI
impact separately (see the "Last reviewed" table in `harness-update-review.md`: **no impact** — no
flag/stream/session-file contract changed). This doc covers the *second* question: what does
first-class Fable support require?

## The change

A single picker entry in `src/lib/agentSelection.ts`:

```ts
MODEL_OPTIONS.claude_code: [
  { label: "Fable", value: "fable" },   // ← added
  { label: "Opus",  value: "opus" },
  { label: "Sonnet", value: "sonnet" },
  { label: "Haiku", value: "haiku" },
]
```

`DEFAULT_MODEL.claude_code` was left at `"opus"` — making Fable the create-form default is a
product decision, not a technical requirement, and was deliberately not taken here.

## Why nothing else is needed

Switchboard's Claude path treats the model id as **opaque data**, so a new model rides existing
machinery end-to-end. Each link was verified:

- **Invocation** — `claude_code/mod.rs::build_args` pushes `--model <value>` verbatim before the
  `--` separator. `fable` is just another string. No code path branches on the model value.
- **Model readback** — `parser.rs` reads `system/init.model` and `result.modelUsage` keys as opaque
  strings (skip-by-default dispatch; no per-model branch). A new id flows through untouched.
- **Context-window / cost sidebar** — driven by `modelUsage.<id>.contextWindow` / `costUSD`, keyed
  by whatever id the harness reports. Fable reports `claude-fable-5` with a 1M context window; the
  existing render handles it.
- **Thinking** — Fable redacts extended-thinking to empty in `-p` mode (see probes), which the
  `ThinkingWidget` path already handles by surfacing nothing (same as Opus 4.8). No change.
- **Model selection mechanics** — Claude model is session-sticky and per-turn overridable; `fable`
  uses the identical `--model` mechanism as the other aliases. The "send `--model` every turn"
  decision (§3.3) already covers it.
- **Capability-table test** — `agentSelection.test.ts` only asserts invariants (list non-empty,
  default ∈ list). `opus` is still the default and still in the list, so the addition is green.

## Probe evidence (live, claude 2.1.170, 2026-06-09)

Tiny cost-disciplined prompts against the dev's subscription session.

1. **Alias resolves.** `claude -p … --model fable -- "Reply with the single word ack"` →
   `system/init.model = claude-fable-5`; `modelUsage` keyed `claude-fable-5`
   (`contextWindow: 1000000`, `maxOutputTokens: 64000`). So `fable` is a real, durable family
   alias — same approach as `opus`/`sonnet`/`haiku` (the picker submits the alias; the per-turn
   footer shows the resolved id). No error; `result.model` absent (normal — `modelUsage` is the
   source).

2. **Effort honored (all levels).** Same prompt with `--effort low` → 4 output tokens;
   `--effort max` → 23 (≈6× more reasoning); neither errors. Fable sits with Opus 4.8/4.7 in the
   "honors all five `--effort` levels" group (§3.4). The picker already offers all five universally
   and degrades safely, so no per-model effort matrix is needed.

3. **Thinking redacted to empty (Opus-class, not Sonnet-class).** A reasoning prompt with
   `--include-partial-messages` produced **one `signature_delta` and zero `thinking_delta` chars** —
   i.e. a signed thinking block with the text server-redacted, exactly like `claude-opus-4-8` and
   unlike `claude-sonnet-4-6` (which streams real reasoning). This is the per-model,
   server-flag-gated redaction tracked in §7.1; it has moved between releases/models before, so
   **re-probe per-model on each CLI bump** rather than assuming.

## Net classification

Fable 5 is **Opus-class** on the two per-model axes Switchboard tracks (honors all effort levels;
redacts thinking in `-p` mode). It required no adapter, parser, sidebar, or wire change — only the
curated picker list, which is designed to be hand-edited as models ship.

## Follow-ups (not done here)

- **Product:** decide whether Fable becomes `DEFAULT_MODEL.claude_code` (currently `opus`).
- **Optional live test:** the Claude live tests cover sonnet/opus; a `--model fable` dispatch that
  asserts `claude-fable-5` readback would extend the drift canary. Low value, cheap.
