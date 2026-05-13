# Research: Gemini CLI for Switchboard

**Captured:** 2026-05-13
**Tool version:** gemini-cli v0.42.0 (stable; weekly stable / preview / nightly release channels)
**Status:** **Docs-derived only.** This document was assembled from Google's official Gemini CLI docs, the open-source repo, and Google's pricing pages. Hands-on probing (fixture capture, stream-event verification, SIGTERM behaviour, session-file inspection) is **pending** and lands in M3 implementation alongside fixture capture for the Gemini adapter. Anything labeled "verify in M3" below is a known unknown.

**Companion to:** [claude-code-cli-observed.md](claude-code-cli-observed.md) and [codex-cli-observed.md](codex-cli-observed.md) — Gemini is the third harness Switchboard targets, slotted as M3 (per the v1 roadmap) ahead of the multi-agent UI and dispatcher work.

## Why we're researching it

Switchboard's M3 adds Gemini CLI as a third harness. The rationale, from the conversation that triggered this research: ironing out the per-harness adapter abstraction with three concrete implementations before moving to multi-agent UI / dispatcher contention (current M4) hardens the architecture before more complexity lands on top.

## CLI surface

**Binary:** `gemini`. Installable via `npm i -g @google/gemini-cli`, `npx @google/gemini-cli`, `brew install gemini-cli`, or MacPorts. Open source at <https://github.com/google-gemini/gemini-cli>.

**Headless mode trigger:** `-p` / `--prompt` flag, OR any non-TTY stdin (e.g., piped input). When triggered, no interactive prompts fire — output goes to stdout per the configured format. This is first-class, not bolted on.

**Relevant flags (planned probe coverage in M3):**

| Flag | Purpose |
|---|---|
| `-p, --prompt <text>` | Headless prompt. Triggers non-interactive mode. |
| `--output-format <fmt>` | `json` (single blob at completion) or stream-json (NDJSON). Both documented. |
| `-r, --resume [tag]` | Resume the most recent session, or a named checkpoint. |
| `-m, --model <model>` | Model selection (e.g., `gemini-2.5-pro`, `gemini-2.5-flash`, `gemini-3.1-pro-preview`). |
| `--checkpointing` | **Removed as a flag in v0.11**; moved to `~/.gemini/settings.json`. Flag-relocation churn worth tracking. |

The full flag surface is not yet captured here — defer to M3 fixture capture.

## Stream format

**Two formats supported, per Google's docs:**

1. **`--output-format json`** — single JSON blob at completion: `{response, stats, error?}`. Closest analog: Codex's batch JSON (no real-time visibility).
2. **Stream-json (NDJSON)** — newline-delimited events with types `init` / `message` / `tool_use` / `tool_result` / `error` / `result`. **This is the closest analog to Claude Code's stream-json of any CLI surveyed.** `result` is the terminal event, semantically equivalent to Claude Code's `result` event.

**Exit codes documented:** `0` success, `1` general error, `42` input error, `53` turn-limit exceeded.

**For Switchboard:** the stream-json format maps almost 1:1 onto Switchboard's existing `AdapterEvent` contract — `init` → `SessionMeta`, `message` content → `ContentChunk`, `tool_use` / `tool_result` → M2.2's `ToolStarted` / `ToolCompleted`, `result` → `TurnEnd`. The mapping table needs verification once fixtures are captured in M3.

## Session storage and resume

**Session ID model:** Gemini CLI **assigns its own session ID** — there is no documented `--session-id <uuid>` flag for the caller to pre-generate the ID. This matches Codex's model and is the opposite of Claude Code's caller-controlled UUID.

**Storage location:** `~/.gemini/` (verify in M3 — exact subdirectory structure not yet captured). Not per-cwd encoded the way Claude Code stores `~/.claude/projects/<encoded-cwd>/<uuid>.jsonl`.

**Resume mechanism:**
- `--resume` / `-r` flag with no argument: resume most recent session.
- `--resume <tag>` with a tag: resume a named checkpoint. Tags are managed via `/resume save <tag>` and `/resume resume <tag>` slash-commands inside the interactive CLI.

**Implication for Switchboard:** Switchboard needs a Codex-style sidecar to map `AgentId` → Gemini's CLI-assigned session ID. The pattern is already planned for Codex in M2 — Gemini reuses it. The session-file enrichment pattern (read `~/.gemini/.../<session>.jsonl` for metadata not on the stream) is also expected; verify in M3.

## Authentication

Four methods documented, all working in headless mode:

1. **Google OAuth personal account** — Gemini Code Assist for individuals. **Free tier: 1,000 requests/day.** This is the standout for Switchboard's onboarding story.
2. **Gemini API key** — issued from Google AI Studio. Free tier: 250 requests/user/day.
3. **Vertex AI** — Google Cloud project (Express or regular). Per-token billing.
4. **Google Workspace / Code Assist Standard/Enterprise** — enterprise subscription.

Auth method determines quota bucket (see Pricing).

## Pricing — the key finding

**Google does NOT split headless from interactive billing.** This is the most permissive of the three vendors:

| Auth method | Quota | Headless vs interactive split? |
|---|---|---|
| OAuth personal (free) | 1,000 req/day | No — same pool |
| Google AI Pro subscription | 1,500 req/day | No — same pool |
| Google AI Ultra subscription | 2,000 req/day | No — same pool |
| Code Assist Standard | 1,500 req/day | No — same pool |
| Code Assist Enterprise | 2,000 req/day | No — same pool |
| Gemini API key (free) | 250 req/user/day | N/A — pay-as-you-go |
| Vertex AI | Per-token, no daily cap | N/A — pay-as-you-go |

Compare to Claude Code (split into dedicated Agent SDK credit pool on 2026-06-15) and Codex (one pool, auth-method-determined). Google sits closest to Codex but with a more generous free tier.

**Per-token pricing (Vertex AI / API):**

| Model | Input ($/M) | Output ($/M) | Context window |
|---|---|---|---|
| Gemini 3.1 Pro Preview | $2 (≤200k) / $4 (>200k) | $12 (≤200k) / $18 (>200k) | 1M |
| Gemini 2.5 Pro | $1.25 (≤200k) | $10 (≤200k) | 1M |
| Gemini 2.5 Flash | $0.30 | $2.50 | 1M |

**For comparison:** Claude Sonnet ~$3/$15 per M; Claude Opus ~$15/$75. Gemini 2.5 Pro is ~2.4× cheaper than Sonnet at a similar tier with the same 1M context window. Gemini 2.5 Flash is cheap enough to be a fan-out workhorse.

## MCP / tool use

Yes — MCP servers are configurable in `~/.gemini/settings.json`. Parity with Claude Code and Codex on this dimension. **Verify in M3:** exact JSON shape of MCP tool events on the stream.

## Process model

Single Node process per invocation. **Documentation is silent on SIGTERM cancellation semantics.** This is a known unknown that needs empirical verification before Switchboard's M4 cancellation work can rely on it.

## Maturity and known risks

**Maturity:** v0.42 with weekly stable releases since mid-2025. Headless mode, output formats, MCP support are all documented stable. **API surface is settling but not yet 1.0** — the `--checkpointing` flag relocation in v0.11 is the kind of churn subprocess integrations have to absorb.

**Risks for Switchboard:**

1. **Pre-1.0 churn.** Subprocess integrations break when CLI flags move. Pin to a known-good version in M3; monitor release notes.
2. **SIGTERM behaviour undocumented.** Verify in M3 before relying on it for M4 cancel.
3. **Auth-mode detection complexity.** Four auth methods × per-method quota types adds branching to Switchboard's billing-awareness UI (M4 / M7 design decision).
4. **Maintenance burden of a third pricing table.** If Switchboard ever derives cost from tokens (open M4 design decision), Gemini adds a third per-model pricing table to keep current.

## Switchboard-implications summary

**Strengths:**

- **Cleanest billing model of the three.** No headless-vs-interactive split to explain. 1,000-req/day free OAuth tier removes the monetary barrier for users trying Switchboard.
- **Stream format maps 1:1 onto existing `AdapterEvent` vocabulary.** Less translation work than Codex.
- **Cheapest per-token rates at coding-grade tier.** Gemini 2.5 Pro at $1.25/$10 vs Sonnet at $3/$15.
- **Most of M2's abstraction work transfers directly.** CLI-owned session ID + sidecar pattern is identical to Codex.

**Weaknesses:**

- Pre-1.0 stability; expect flag churn.
- Undocumented SIGTERM behaviour.
- Adds a third per-vendor surface (pricing table, research notes, fixture maintenance).

**Slot in roadmap:** new M3 (was multi-agent UI, now M4). Reasoning: third concrete adapter forces the abstraction to be load-bearing before more complexity lands on top of it.

## Pending verification (M3 fixture capture)

The following are docs-derived and need hands-on confirmation before the adapter implementation can rely on them:

1. Exact event shapes for `init` / `message` / `tool_use` / `tool_result` / `error` / `result` on the stream-json output.
2. Session-file location and format under `~/.gemini/`.
3. SIGTERM behaviour (does the subprocess exit cleanly? does it propagate to child processes?).
4. Empty / whitespace-only prompt behaviour.
5. Behaviour when `--resume <tag>` references a non-existent tag (error code? silent fresh session? — parallel to Claude Code's `--resume <unknown-uuid>` rejection).
6. Auth-method detection from a Switchboard perspective (how does the adapter know which billing pool the user is on?).
7. Tool-use stream shape relative to Switchboard's M2.2 `ToolStarted` / `ToolCompleted` event vocabulary.
8. Concurrent invocation safety (parallel `gemini -p` from the same cwd — verified safe for Claude Code, presumed safe here).

## Sources

- <https://github.com/google-gemini/gemini-cli> — repo
- <https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/headless.md> — headless docs
- <https://geminicli.com/docs/resources/quota-and-pricing/> — quota and pricing
- <https://ai.google.dev/gemini-api/docs/pricing> — Gemini API pricing
- <https://geminicli.com/docs/cli/tutorials/session-management/> — sessions
- <https://geminicli.com/docs/cli/checkpointing/> — checkpointing
