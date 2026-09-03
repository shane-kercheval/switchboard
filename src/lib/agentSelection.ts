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

import type { AgentSelection, HarnessKind, Preferences } from "./types";
import { HARNESS_DEFAULT_AGENT_NAME } from "./harnessDisplay";

/// One picker option: `value` is the alias/id submitted to the backend,
/// `label` the friendlier display text.
export type SelectionOption = { label: string; value: string };

/// Per-harness model options. Every harness has a real list — Antigravity's
/// arrived with agy 1.1.19, which made `--model` a per-invocation flag.
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
  ],
  /// Stable slugs from `agy models` (probed @ 1.1.19, 2026-08-25), with the
  /// harness's own display names. **Effort-bearing models are listed by their
  /// base slug**, not the effort-folded variant `agy models` also prints
  /// (`gemini-3.1-pro`, not `gemini-3.1-pro-high`): the folded form conflicts
  /// with `--effort`, and the two-control shape is what matches every other
  /// harness here. Which levels each accepts lives in `effortOptionsFor`.
  ///
  /// The two Claude models have **no effort axis at all** — `agy` rejects the
  /// flag outright (probed: `--effort is not supported for model
  /// "claude-sonnet-4-6"`). Their `(Thinking)` suffix is part of Google's own
  /// display name, not an effort this picker could offer; note it survives in
  /// the Opus *slug* (`claude-opus-4-6-thinking`), which is where it actually
  /// belongs.
  ///
  /// The labels drop both `(Thinking)` and the `Claude ` prefix, deliberately.
  /// Neither disambiguates anything: agy offers no non-thinking variant of
  /// either model, and `Sonnet`/`Opus` name exactly one vendor's models — the
  /// same reason Claude Code's own picker reads `Opus`/`Sonnet`/`Haiku` bare.
  /// Together they took the longest entry from 28 characters to 10, which is
  /// what let this picker be a toggle instead of a dropdown.
  ///
  /// The Gemini-branded Antigravity models keep their `Gemini` prefix on
  /// purpose; that is not an inconsistency. `3.7 Flash` and `3.1 Pro` are version numbers with no
  /// product attached, while `Sonnet` and `Opus` are unambiguous on their own.
  ///
  /// The known cost of dropping `(Thinking)`: the turn footer shows agy's
  /// announced name, and `split_announced_model` leaves the suffix attached
  /// because it is not a real level — so the footer reads
  /// `Claude Sonnet 4.6 (Thinking)` where the picker reads `Sonnet 4.6`. A
  /// cosmetic mismatch on two models, traded for a control that fits.
  ///
  /// GPT-OSS is different: `medium` is a *real* level for it (probed — bare
  /// dispatch works, `--effort medium` works, `low`/`high` are rejected with
  /// `available: medium`). So it carries a single-value effort entry rather
  /// than a `(Medium)` suffix baked into its label. That is not decoration —
  /// it makes the picker agree with the transcript. `split_announced_model`
  /// already splits a trailing parenthetical off as effort when it names a real
  /// level, so agy's announced `GPT-OSS 120B (Medium)` renders in the turn
  /// footer as model `GPT-OSS 120B` + effort `medium`. A label carrying the
  /// suffix would contradict the footer directly below it.
  ///
  /// Only the newest Flash generation is offered. agy's catalog also lists
  /// 3.6 and 3.5 Flash; three near-identical generations is a picker that costs
  /// the user a decision without giving them one. Retiring an entry does not
  /// strand an agent already using it — the slug stays valid at dispatch, and
  /// the sidebar falls back to a dropdown for an off-catalog persisted value.
  ///
  /// Curated rather than fetched: `agy models` needs auth and network, and its
  /// `--output-format json` is advertised but rejected @ 1.1.19. A retired
  /// entry fails loudly and cheaply — `agy` rejects an unknown model
  /// pre-dispatch, quota-free, listing what is available.
  antigravity: [
    { label: "Gemini 3.7 Flash", value: "gemini-3.7-flash" },
    { label: "Gemini 3.1 Pro", value: "gemini-3.1-pro" },
    { label: "Sonnet 4.6", value: "claude-sonnet-4-6" },
    { label: "Opus 4.6", value: "claude-opus-4-6-thinking" },
    { label: "GPT-OSS 120B", value: "gpt-oss-120b" },
  ],
};

/// How the **model** picker renders per harness — the single source of truth
/// both the create form and the sidebar change-model dialog read, so the two
/// can't drift. Segmented (a toggle) for the short curated lists; a dropdown
/// only for a harness whose list is long with labels long enough to truncate as
/// pills. Effort is always segmented (every effort set is short
/// single words), so there is no `EFFORT_PRESENTATION`. The sidebar
/// additionally falls back to a dropdown when it must show an off-catalog
/// persisted value whose label length is unbounded (see `Sidebar.svelte`).
/// Uniform today (every harness is `segmented`); retained per harness as an
/// extension point — see the note above `SUPPORTS_MODEL_SELECTION` in
/// `harnessDisplay.ts` for why these maps are not collapsed.
export const MODEL_PRESENTATION: Record<HarnessKind, "segmented" | "dropdown"> = {
  claude_code: "segmented",
  codex: "segmented",
  // Five entries after retiring the older Flash generations, longest label
  // "Gemini 3.7 Flash" at 16 characters — comparable to Codex's segmented row.
  // Both this and the `(Thinking)` drop above were judged against the running
  // app: five pills carrying the suffixes were tried first and rejected.
  antigravity: "segmented",
};

/// Per-harness effort options. Codex `none` is
/// a *real* level (forces no extended reasoning), distinct from leaving effort
/// unset. This is the **full** per-harness set; effort validity is additionally
/// **per-model** for Antigravity (see `effortOptionsFor`), so a form scoped to a
/// chosen model must derive its options through that helper rather than reading
/// this map directly — several Antigravity models have no effort axis at all,
/// and the helper is what hides the control for them.
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
/// The sets genuinely differ per model — Flash takes all three levels, 3.1 Pro
/// only low/high, GPT-OSS only medium — which is why this is a per-model map
/// rather than one list. `null` explicitly means no effort axis; absence means
/// an unknown/off-catalog model whose persisted effort must survive.
const ANTIGRAVITY_MODEL_EFFORTS: Record<string, readonly string[] | null> = {
  "gemini-3.7-flash": ["low", "medium", "high"],
  "gemini-3.1-pro": ["low", "high"],
  "claude-sonnet-4-6": null,
  "claude-opus-4-6-thinking": null,
  // Single-valued, and that is the point: the control renders as one
  // already-selected option, which states what the turn will run at instead of
  // leaving the user to infer it from a label suffix. Kept in this map (rather
  // than absent, which hides the control) so the picker and the turn footer
  // describe the model the same way.
  "gpt-oss-120b": ["medium"],
};

export type EffortSupport =
  | { kind: "known"; options: SelectionOption[] }
  | { kind: "none" }
  | { kind: "unknown"; options: SelectionOption[] };

export function effortSupportFor(harness: HarnessKind, model: string | null): EffortSupport {
  if (harness !== "antigravity") return { kind: "known", options: EFFORT_OPTIONS[harness] };
  if (model === null || model === "") return { kind: "none" };
  const levels = ANTIGRAVITY_MODEL_EFFORTS[model];
  if (levels === undefined) return { kind: "unknown", options: EFFORT_OPTIONS.antigravity };
  if (levels === null) return { kind: "none" };
  return {
    kind: "known",
    options: EFFORT_OPTIONS.antigravity.filter((option) => levels.includes(option.value)),
  };
}

/// The effort options valid for a given harness **and model**. Only Antigravity
/// is model-dependent, because only there does a mismatch **fail the dispatch**:
/// its levels differ per model and several models have no axis at all. Every
/// other harness gets its full list, and an invalid level is discovered
/// reactively at dispatch — Claude silently runs a lower level, Codex fails the
/// turn with the server's own "Supported values are …" enumeration. That matches
/// the model picker's own "curated suggestions, not a validated allow-list"
/// policy, under which account/plan gating is already reactive everywhere.
export function effortOptionsFor(
  harness: HarnessKind,
  model: string | undefined,
): SelectionOption[] {
  const support = effortSupportFor(harness, model ?? null);
  return support.kind === "none" ? [] : support.options;
}

/// Whether this harness/model pair **requires** an effort to dispatch.
///
/// Antigravity-only: `agy` rejects a bare axis-bearing model with
/// `requires --effort (available: …)`, so the picker must not offer a
/// "Default" for those. Every other harness treats effort as optional — an
/// unset effort means "pass no flag, let the harness choose".
export function effortIsRequired(harness: HarnessKind, model: string | undefined): boolean {
  return harness === "antigravity" && effortSupportFor(harness, model ?? null).kind === "known";
}

/// Built-in fallback used until persisted preferences load. This same shape is
/// also the reset value for a missing `agent_defaults` key; once loaded, Add
/// Agent and new-project seeding read the user's preferences directly.
export const DEFAULT_AGENT_SELECTIONS: Preferences["agent_defaults"] = {
  claude_code: {
    model_choices: ["fable", "opus"],
    effort_choices: ["medium", "high"],
    default_model: "opus",
    default_effort: "medium",
  },
  codex: {
    model_choices: ["gpt-5.6-sol", "gpt-5.6-terra"],
    effort_choices: ["medium", "high"],
    default_model: "gpt-5.6-terra",
    default_effort: "medium",
  },
  // Both carry explicit effort because `agy` rejects these effort-bearing
  // models when dispatched without one.
  antigravity: {
    model_choices: ["gemini-3.7-flash", "gemini-3.1-pro"],
    effort_choices: ["medium", "high"],
    default_model: "gemini-3.7-flash",
    default_effort: "medium",
  },
};

/// A model-derived name stops describing an agent once it can switch between
/// multiple model or effort choices. Keep that case stable and harness-shaped instead. This map is
/// intentionally explicit rather than slugging display labels: agent names are
/// persisted identifiers, and a future label edit must not rename the default.
const MULTI_CHOICE_AGENT_NAME: Record<HarnessKind, string> = {
  claude_code: "claude",
  codex: "codex",
  antigravity: "antigravity",
};

export function activatableEffortValues(
  selection: AgentSelection,
  harness: HarnessKind,
): Set<string> {
  const support = effortSupportFor(harness, selection.model);
  if (support.kind === "none") return new Set();
  if (support.kind === "unknown") return new Set(selection.effort_choices);
  const valid = new Set(support.options.map((option) => option.value));
  return new Set(selection.effort_choices.filter((effort) => valid.has(effort)));
}

export type ModelChangeResolution =
  | { ok: true; selection: AgentSelection }
  | { ok: false; reason: string };

export function resolveModelChange(
  selection: AgentSelection,
  harness: HarnessKind,
  model: string,
): ModelChangeResolution {
  if (harness !== "antigravity") return { ok: true, selection: { ...selection, model } };
  const support = effortSupportFor(harness, model);
  if (support.kind === "none") {
    return { ok: true, selection: { ...selection, model, effort: null } };
  }
  if (support.kind === "unknown") return { ok: true, selection: { ...selection, model } };
  const valid = new Set(support.options.map((option) => option.value));
  if (selection.effort !== null && valid.has(selection.effort)) {
    return { ok: true, selection: { ...selection, model } };
  }
  const effort = EFFORT_OPTIONS.antigravity.find(
    (option) => valid.has(option.value) && selection.effort_choices.includes(option.value),
  )?.value;
  if (effort === undefined) {
    return {
      ok: false,
      reason: "No configured reasoning effort is compatible with that model.",
    };
  }
  return { ok: true, selection: { ...selection, model, effort } };
}

export function selectionIsValid(selection: AgentSelection, harness: HarnessKind): boolean {
  if (harness !== "antigravity") return true;
  const support = effortSupportFor(harness, selection.model);
  if (support.kind !== "known") return true;
  return (
    selection.effort !== null && support.options.some((option) => option.value === selection.effort)
  );
}

export type NewAgentSelectionResolution =
  | { ok: true; selection: AgentSelection }
  | { ok: false; selection: AgentSelection; reason: string };

/// Convert independent axis defaults into the pair a new agent can dispatch.
/// A default effort remains meaningful in Settings even while the default
/// model has no effort axis; only this creation boundary clears or resolves it.
export function selectionForNewAgent(
  defaults: Preferences["agent_defaults"][HarnessKind],
  harness: HarnessKind,
): NewAgentSelectionResolution {
  const selection: AgentSelection = {
    model: defaults.default_model,
    effort: defaults.default_effort,
    model_choices: [...defaults.model_choices],
    effort_choices: [...defaults.effort_choices],
  };
  if (harness !== "antigravity") return { ok: true, selection };
  if (selection.model === null) {
    return { ok: true, selection: { ...selection, effort: null } };
  }
  const resolved = resolveModelChange(selection, harness, selection.model);
  if (resolved.ok) return resolved;
  return {
    ok: false,
    selection,
    reason: "No configured reasoning effort is compatible with the starting model.",
  };
}

/// The model-derived agent name for a single-choice create, with effort appended
/// where the harness has that axis (`opus-high`, `gpt-5-5-medium`, …).
/// Harnesses with no concrete model to name after fall back to the bare harness
/// name: an agent created without a concrete model (it
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
  if (!model) return HARNESS_DEFAULT_AGENT_NAME[harness];
  const raw = effort ? `${model}-${effort}` : model;
  const slug = raw.replace(/[^A-Za-z0-9_-]+/g, "-").replace(/^-+|-+$/g, "");
  return slug === "" ? HARNESS_DEFAULT_AGENT_NAME[harness] : slug;
}

/// The quick-choice-aware create name shared by the dialog and new-project seeding.
/// An agent with multiple choices uses its short harness name because no single
/// model/effort pair describes it.
export function defaultAgentNameForSelection(
  harness: HarnessKind,
  selection: AgentSelection,
): string {
  return selection.model_choices.length > 1 || selection.effort_choices.length > 1
    ? MULTI_CHOICE_AGENT_NAME[harness]
    : defaultAgentName(harness, selection.model ?? undefined, selection.effort ?? undefined);
}
