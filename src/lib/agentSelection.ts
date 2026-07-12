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
    { label: "GPT-5.6 Sol", value: "gpt-5.6-sol" },
    { label: "GPT-5.6 Terra", value: "gpt-5.6-terra" },
    { label: "GPT-5.6 Luna", value: "gpt-5.6-luna" },
    { label: "GPT-5.5", value: "gpt-5.5" },
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

/// How the **model** picker renders per harness — the single source of truth
/// both the create form and the sidebar change-model dialog read, so the two
/// can't drift. Segmented (a toggle) for the short curated lists; a dropdown
/// only for Gemini, whose list is long with long labels that would truncate as
/// pills. Effort is always segmented (every effort set is short single words),
/// so there is no `EFFORT_PRESENTATION`. Antigravity's value is inert — it has
/// no model picker (the form shows a note instead). The sidebar additionally
/// falls back to a dropdown when it must show an off-catalog persisted value
/// whose label length is unbounded (see `Sidebar.svelte`).
export const MODEL_PRESENTATION: Record<HarnessKind, "segmented" | "dropdown"> = {
  claude_code: "segmented",
  codex: "segmented",
  gemini: "dropdown",
  antigravity: "segmented",
};

/// Per-harness effort options. Empty for Gemini (config-only) and Antigravity
/// (folded into the model name). Codex `none` is a *real* level (forces no
/// extended reasoning), distinct from leaving effort unset. This is the **full**
/// per-harness set; Codex effort validity is additionally **per-model** (see
/// `effortOptionsFor`), so a form scoped to a chosen model must derive its
/// options through that helper rather than reading this map directly.
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
    { label: "Max", value: "max" },
    { label: "Ultra", value: "ultra" },
  ],
  gemini: [],
  antigravity: [],
};

/// Codex effort levels only the GPT-5.6 model family accepts. Earlier Codex
/// models 400 on these — verified live @ codex 0.144.1: `gpt-5.5 + max` is
/// rejected with the server enumerating `none…xhigh`, while Sol/Terra/Luna
/// accept every level (incl. `ultra`). A Codex model not in
/// `CODEX_MAX_ULTRA_MODELS` is offered the list minus these levels.
const CODEX_HIGH_TIER_EFFORTS: ReadonlySet<string> = new Set(["max", "ultra"]);

/// Codex models that accept `max`/`ultra`. **When adding a Codex model that
/// supports them, add it here** (the "Model catalog" step in
/// `docs/harness-update-review.md`), else the picker silently withholds those
/// levels for it.
const CODEX_MAX_ULTRA_MODELS: ReadonlySet<string> = new Set([
  "gpt-5.6-sol",
  "gpt-5.6-terra",
  "gpt-5.6-luna",
]);

/// The effort options valid for a given harness **and model**. Only Codex is
/// model-dependent: `max`/`ultra` are withheld from **curated** Codex models
/// known to reject them (`gpt-5.5`) rather than offered as a first-class picker
/// state that fails at turn time. A null/unset or **off-catalog** model (e.g. an
/// attached session running an id we don't curate) stays permissive — its
/// validity is unknown, so we keep it reactive, matching the model picker's own
/// "curated suggestions, not a validated allow-list" policy. A new curated
/// Codex model is treated as legacy until added to `CODEX_MAX_ULTRA_MODELS`
/// (fail-safe: withhold the risky levels until confirmed). Account/plan gating
/// remains reactive for every harness.
export function effortOptionsFor(
  harness: HarnessKind,
  model: string | undefined,
): SelectionOption[] {
  const base = EFFORT_OPTIONS[harness];
  if (harness !== "codex" || model == null || model === "") return base;
  const isCuratedLegacy =
    MODEL_OPTIONS.codex.some((o) => o.value === model) && !CODEX_MAX_ULTRA_MODELS.has(model);
  return isCuratedLegacy ? base.filter((o) => !CODEX_HIGH_TIER_EFFORTS.has(o.value)) : base;
}

/// Create-form preselected model per harness. `undefined` only where the
/// harness has no model capability (Antigravity). Attach does NOT use these —
/// it defaults to "keep current" so attaching never silently overrides the
/// session's existing model.
export const DEFAULT_MODEL: Record<HarnessKind, string | undefined> = {
  claude_code: "opus",
  codex: "gpt-5.6-terra",
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
