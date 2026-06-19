/// Curated, per-harness model and effort option lists for the create/attach
/// pickers, plus each harness's preselected create-form default.
///
/// **These are suggestions, not a validated allow-list.** No harness exposes a
/// queryable model list and Codex values are plan-gated per account, so the
/// lists are hardcoded from live-verified probes (`harness-behavior.md`
/// §3.3/§3.4) and patched as models ship/sunset. A curated value that's
/// out-of-plan for the user's account still dispatches and fails reactively as
/// a normal failed turn — we don't pre-validate. Claude uses durable aliases
/// (`opus`/`sonnet`/`haiku` — "latest of family", no maintenance); the
/// per-turn transcript footer shows the resolved id.
///
/// The picker gate (shown vs. note) lives in `harnessDisplay.ts`
/// (`SUPPORTS_MODEL_SELECTION` / `SUPPORTS_EFFORT_SELECTION`); a harness with no
/// capability has an empty list here and no default.

import type { HarnessKind } from "./types";
import { HARNESS_DEFAULT_AGENT_NAME } from "./harnessDisplay";

/// One picker option: `value` is the alias/id submitted to the backend,
/// `label` the friendlier display text.
export type SelectionOption = { label: string; value: string };

/// Per-harness model options. Empty for Antigravity (model is harness-owned
/// config we can't set — the form renders a note instead).
export const MODEL_OPTIONS: Record<HarnessKind, SelectionOption[]> = {
  claude_code: [
    { label: "Fable", value: "fable" },
    { label: "Opus", value: "opus" },
    { label: "Sonnet", value: "sonnet" },
    { label: "Haiku", value: "haiku" },
  ],
  codex: [
    { label: "GPT-5.5", value: "gpt-5.5" },
    { label: "GPT-5.4 mini", value: "gpt-5.4-mini" },
  ],
  gemini: [
    { label: "Auto", value: "auto" },
    { label: "Gemini 2.5 Pro", value: "gemini-2.5-pro" },
    { label: "Gemini 2.5 Flash", value: "gemini-2.5-flash" },
    { label: "Gemini 2.5 Flash-Lite", value: "gemini-2.5-flash-lite" },
    { label: "Gemini 3 Pro (preview)", value: "gemini-3-pro-preview" },
    { label: "Gemini 3 Flash (preview)", value: "gemini-3-flash-preview" },
    { label: "Gemini 3.1 Pro (preview)", value: "gemini-3.1-pro-preview" },
    { label: "Gemini 3.1 Flash-Lite (preview)", value: "gemini-3.1-flash-lite-preview" },
  ],
  antigravity: [],
};

/// Per-harness effort options. Empty for Gemini (config-only) and Antigravity
/// (folded into the model name). Codex `none` is a *real* level (forces no
/// extended reasoning), distinct from leaving effort unset.
export const EFFORT_OPTIONS: Record<HarnessKind, SelectionOption[]> = {
  claude_code: [
    { label: "Low", value: "low" },
    { label: "Medium", value: "medium" },
    { label: "High", value: "high" },
    { label: "XHigh", value: "xhigh" },
    { label: "Max", value: "max" },
  ],
  codex: [
    { label: "None", value: "none" },
    { label: "Minimal", value: "minimal" },
    { label: "Low", value: "low" },
    { label: "Medium", value: "medium" },
    { label: "High", value: "high" },
    { label: "XHigh", value: "xhigh" },
  ],
  gemini: [],
  antigravity: [],
};

/// Create-form preselected model per harness. `undefined` only where the
/// harness has no model capability (Antigravity). Attach does NOT use these —
/// it defaults to "keep current" so attaching never silently overrides the
/// session's existing model.
export const DEFAULT_MODEL: Record<HarnessKind, string | undefined> = {
  claude_code: "opus",
  codex: "gpt-5.5",
  gemini: "auto",
  antigravity: undefined,
};

/// Create-form preselected effort per harness. `undefined` where the harness
/// has no effort capability (Gemini, Antigravity).
export const DEFAULT_EFFORT: Record<HarnessKind, string | undefined> = {
  claude_code: "high",
  codex: "medium",
  gemini: undefined,
  antigravity: undefined,
};

/// The auto-derived agent name for a create: named after the model it'll run,
/// with effort appended where the harness has that axis — so a roster of
/// auto-created agents reads as `opus-high`, `gpt-5-5-medium`, … at a glance.
/// Harnesses with no concrete model to name after fall back to the bare harness
/// name: Antigravity (model is harness-owned) and Gemini left on `auto` (it
/// picks up whatever model was last used).
///
/// The result is **guaranteed** to be a valid agent name. Model ids are
/// vendor-shaped strings this module is built to edit as models ship/sunset
/// (`gpt-5.5`, a future `provider/model`, …), so rather than trust the current
/// curated values to be clean we slugify: every run of characters outside the
/// agent-name charset (letters/digits/`-`/`_`, mirroring
/// `nameValidation.ALLOWED_NAME`) collapses to a single `-`, leading/trailing
/// separators are trimmed, and an empty result falls back to the harness slug.
export function defaultAgentName(
  harness: HarnessKind,
  model: string | undefined,
  effort: string | undefined,
): string {
  if (!model || model === "auto") return HARNESS_DEFAULT_AGENT_NAME[harness];
  const raw = effort ? `${model}-${effort}` : model;
  const slug = raw.replace(/[^A-Za-z0-9_-]+/g, "-").replace(/^-+|-+$/g, "");
  return slug === "" ? HARNESS_DEFAULT_AGENT_NAME[harness] : slug;
}
