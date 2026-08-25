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

import type {
  AgentProfile,
  AgentProfileSlot,
  AgentRecord,
  HarnessKind,
  Preferences,
} from "./types";
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
  /// Stable slugs from `agy models` (probed @ 1.1.19, 2026-08-25), with the
  /// harness's own display names. **Effort-bearing models are listed by their
  /// base slug**, not the effort-folded variant `agy models` also prints
  /// (`gemini-3.1-pro`, not `gemini-3.1-pro-high`): the folded form conflicts
  /// with `--effort`, and the two-control shape is what matches every other
  /// harness here. Which levels each accepts lives in `effortOptionsFor`.
  ///
  /// "(Thinking)" is Google's own display name for the two Claude models, not
  /// an effort label this picker adds — they have no effort axis at all
  /// (probed: `--effort is not supported for model "claude-sonnet-4-6"`).
  ///
  /// `GPT-OSS 120B (Medium)` is different and deliberately absent from
  /// `ANTIGRAVITY_MODEL_EFFORTS`: `medium` is a real level for it, but it is
  /// the *only* one and is optional (probed — bare dispatch works, `low`/`high`
  /// are rejected with `available: medium`). A control whose single choice
  /// cannot change the outcome is not worth showing, so the effort picker stays
  /// hidden for it. Its turn footer still reads back `medium`, because the turn
  /// genuinely ran at it.
  ///
  /// Curated rather than fetched: `agy models` needs auth and network, and its
  /// `--output-format json` is advertised but rejected @ 1.1.19. A retired
  /// entry fails loudly and cheaply — `agy` rejects an unknown model
  /// pre-dispatch, quota-free, listing what is available.
  antigravity: [
    { label: "Gemini 3.7 Flash", value: "gemini-3.7-flash" },
    { label: "Gemini 3.6 Flash", value: "gemini-3.6-flash" },
    { label: "Gemini 3.5 Flash", value: "gemini-3.5-flash" },
    { label: "Gemini 3.1 Pro", value: "gemini-3.1-pro" },
    { label: "Claude Sonnet 4.6 (Thinking)", value: "claude-sonnet-4-6" },
    { label: "Claude Opus 4.6 (Thinking)", value: "claude-opus-4-6-thinking" },
    { label: "GPT-OSS 120B (Medium)", value: "gpt-oss-120b" },
  ],
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
  // Seven entries with names as long as "Claude Sonnet 4.6 (Thinking)" would
  // truncate as pills.
  antigravity: "dropdown",
};

/// Per-harness effort options. Empty for Gemini (config-only). Codex `none` is
/// a *real* level (forces no extended reasoning), distinct from leaving effort
/// unset. This is the **full** per-harness set; effort validity is additionally
/// **per-model** for Codex *and* Antigravity (see `effortOptionsFor`), so a
/// form scoped to a chosen model must derive its options through that helper
/// rather than reading this map directly — for Antigravity especially, where
/// several models have no effort axis at all.
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
  antigravity: [
    { label: "Low", value: "low" },
    { label: "Medium", value: "medium" },
    { label: "High", value: "high" },
  ],
};

/// Antigravity effort levels **per model**, keyed by the slugs in
/// `MODEL_OPTIONS.antigravity`. Probed @ 1.1.19: `agy` validates this
/// client-side before dispatch, so a wrong entry fails loudly and quota-free
/// with the CLI naming the valid set — unlike Claude, which silently degrades.
///
/// A model absent from this map has **no effort axis**: `agy` rejects
/// `--effort` for it outright. Note Gemini 3.1 Pro offers only low/high while
/// the Flash models add medium — the sets genuinely differ, which is why this
/// is per-model rather than one list.
const ANTIGRAVITY_MODEL_EFFORTS: Record<string, readonly string[]> = {
  "gemini-3.7-flash": ["low", "medium", "high"],
  "gemini-3.6-flash": ["low", "medium", "high"],
  "gemini-3.5-flash": ["low", "medium", "high"],
  "gemini-3.1-pro": ["low", "high"],
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
  const unset = model == null || model === "";
  if (harness === "antigravity") {
    // An effort is meaningless without a model here: `agy` decides validity
    // from the model, and several models have no axis at all. Returning an
    // empty set is what hides the control (see `AgentProfileEditor`), so an
    // unselected or no-axis model shows no effort picker rather than one whose
    // every option would be rejected at dispatch.
    if (unset) return [];
    const levels = ANTIGRAVITY_MODEL_EFFORTS[model];
    if (levels === undefined) return [];
    return base.filter((option) => levels.includes(option.value));
  }
  if (harness !== "codex" || unset) return base;
  const isCuratedLegacy =
    MODEL_OPTIONS.codex.some((o) => o.value === model) && !CODEX_MAX_ULTRA_MODELS.has(model);
  return isCuratedLegacy ? base.filter((o) => !CODEX_HIGH_TIER_EFFORTS.has(o.value)) : base;
}

/// Whether this harness/model pair **requires** an effort to dispatch.
///
/// Antigravity-only: `agy` rejects a bare axis-bearing model with
/// `requires --effort (available: …)`, so the picker must not offer a
/// "Default" for those. Every other harness treats effort as optional — an
/// unset effort means "pass no flag, let the harness choose".
export function effortIsRequired(harness: HarnessKind, model: string | undefined): boolean {
  return harness === "antigravity" && effortOptionsFor(harness, model).length > 0;
}

/// Built-in fallback used until persisted preferences load. This same shape is
/// also the reset value for a missing `agent_defaults` key; once loaded, Add
/// Agent and new-project seeding read the user's preferences directly.
export const DEFAULT_AGENT_PROFILES: Preferences["agent_defaults"] = {
  claude_code: {
    primary: { model: "opus", effort: "high" },
    secondary: null,
  },
  codex: {
    primary: { model: "gpt-5.6-sol", effort: "high" },
    secondary: null,
  },
  gemini: {
    primary: { model: "auto", effort: null },
    secondary: null,
  },
  // Matches the model Antigravity itself defaults to, and carries an explicit
  // effort because `agy` rejects an effort-bearing model dispatched without one.
  antigravity: {
    primary: { model: "gemini-3.1-pro", effort: "high" },
    secondary: null,
  },
};

/// Useful starting values when someone enables a secondary profile for the
/// first time and their global defaults do not already define one.
export const SUGGESTED_SECONDARY_PROFILE: Record<HarnessKind, AgentProfile> = {
  claude_code: { model: "sonnet", effort: "medium" },
  codex: { model: "gpt-5.6-terra", effort: "medium" },
  gemini: { model: "gemini-2.5-flash", effort: null },
  // A cheaper, faster tier than the primary default, matching the pattern the
  // other harnesses use for their secondary.
  antigravity: { model: "gemini-3.5-flash", effort: "medium" },
};

/// A model-derived name stops describing an agent once it can switch between
/// two profiles. Keep that case stable and harness-shaped instead. This map is
/// intentionally explicit rather than slugging display labels: agent names are
/// persisted identifiers, and a future label edit must not rename the default.
const MULTI_PROFILE_AGENT_NAME: Record<HarnessKind, string> = {
  claude_code: "claude",
  codex: "codex",
  gemini: "gemini",
  antigravity: "antigravity",
};

export function primaryProfile(agent: AgentRecord): AgentProfile {
  return { model: agent.model ?? null, effort: agent.effort ?? null };
}

export function secondaryProfile(agent: AgentRecord): AgentProfile | null {
  return agent.profiles?.secondary ?? null;
}

export function activeProfileSlot(agent: AgentRecord): AgentProfileSlot {
  return agent.profiles?.active ?? "primary";
}

export function activeProfile(agent: AgentRecord): AgentProfile {
  return activeProfileSlot(agent) === "secondary" && secondaryProfile(agent) !== null
    ? secondaryProfile(agent)!
    : primaryProfile(agent);
}

/// The model-derived agent name for a primary-only create, with effort appended
/// where the harness has that axis (`opus-high`, `gpt-5-5-medium`, …).
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

/// The profile-aware create name shared by the dialog and new-project seeding.
/// A secondary-capable agent uses its short harness name because neither
/// profile alone describes it.
export function defaultAgentNameForProfiles(
  harness: HarnessKind,
  primary: AgentProfile,
  secondary: AgentProfile | null,
): string {
  return secondary === null
    ? defaultAgentName(harness, primary.model ?? undefined, primary.effort ?? undefined)
    : MULTI_PROFILE_AGENT_NAME[harness];
}
