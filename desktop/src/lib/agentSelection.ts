/// Curated, per-harness model and effort option lists for the create/attach
/// pickers, plus each harness's preselected create-form default.
///
/// **These are suggestions, not a validated allow-list.** Only `agy` lists its
/// models, and Codex values are plan-gated per account, so the lists are
/// hardcoded from live-verified probes (`harness-behavior.md` §3.3/§3.4) and
/// patched as models ship/sunset. A curated value that's out-of-plan for the
/// user's account still dispatches and fails reactively as a normal failed
/// turn — we don't pre-validate. Claude uses durable aliases
/// (`opus`/`sonnet`/`haiku` — "latest of family", no maintenance); the
/// per-turn transcript footer shows the resolved id.
///
/// The picker gate (shown vs. note) lives in `harnessDisplay.ts`
/// (`SUPPORTS_MODEL_SELECTION` / `SUPPORTS_EFFORT_SELECTION`); a harness with no
/// capability has an empty list here and no default.

import type { AgentSelection, AgentSelectionChanges, HarnessKind, Preferences } from "./types";
import { HARNESS_DEFAULT_AGENT_NAME } from "./harnessDisplay";
import antigravityModels from "./antigravityModels.json";

/// One picker option: `value` is the alias/id submitted to the backend,
/// `label` the friendlier display text.
export type SelectionOption = { label: string; value: string };

/// One Antigravity model and the levels `--effort` accepts for it, `null` for a
/// model with no effort axis.
type AntigravityModel = SelectionOption & { efforts: readonly string[] | null };

/// Antigravity's models live in `antigravityModels.json` rather than here
/// because the Rust live test `live_antigravity_model_table_matches_the_cli_model_list`
/// reads the same file and compares it with `agy`'s own list. One file means
/// the picker and the test cannot disagree about what Switchboard offers.
const ANTIGRAVITY_MODELS: readonly AntigravityModel[] = antigravityModels.models;

/// Per-harness model options. Every harness has a real list — Antigravity's
/// arrived with agy 1.1.19, which made `--model` a per-invocation flag.
export const MODEL_OPTIONS: Record<HarnessKind, SelectionOption[]> = {
  claude_code: [
    { label: "Fable", value: "fable" },
    { label: "Opus", value: "opus" },
    { label: "Sonnet", value: "sonnet" },
    { label: "Haiku", value: "haiku" },
  ],
  /// Codex has no family aliases — `-m` takes an exact id and an agent stays on
  /// it — so the labels keep the generation: a bare `Sol` would read like
  /// Claude's self-upgrading `Opus`. Which generations are offered, and why,
  /// is recorded in `harness-behavior.md` §3.3.
  codex: [
    { label: "GPT-6 Astra", value: "gpt-6-astra" },
    { label: "GPT-6.1 Sol", value: "gpt-6.1-sol" },
    { label: "GPT-5.6 Terra", value: "gpt-5.6-terra" },
    { label: "GPT-6 Luna", value: "gpt-6-luna" },
  ],
  /// Exactly the models `agy models` lists (agy 1.4.2, 2026-10-19), read from
  /// `antigravityModels.json`. **Effort-bearing models are listed by their base
  /// slug**, not the effort-folded variant `agy models` prints
  /// (`gemini-3.8-flash`, not `gemini-3.8-flash-high`): the folded form
  /// conflicts with `--effort`, and the two-control shape is what matches every
  /// other harness here. Which levels each accepts lives in `effortOptionsFor`.
  ///
  /// The labels drop agy's `Claude ` prefix, deliberately: `Sonnet`/`Opus` name
  /// exactly one vendor's models — the same reason Claude Code's own picker
  /// reads `Opus`/`Sonnet`/`Haiku` bare — and the shorter labels are what let
  /// this picker be a toggle instead of a dropdown. The cost is cosmetic: the
  /// turn footer shows agy's announced name, `Claude Opus 5.5`, where the
  /// picker reads `Opus 5.5`.
  ///
  /// The Gemini-branded Antigravity models keep their `Gemini` prefix on
  /// purpose; that is not an inconsistency. `3.8 Flash` and `4 Argon` say
  /// nothing about whose model they are, while `Sonnet` and `Opus` are
  /// unambiguous on their own.
  ///
  /// GPT-OSS takes one level: `medium` is a *real* level for it (probed — bare
  /// dispatch works, `--effort medium` works, `low`/`high` are rejected with
  /// `available: medium`). So it carries a single-value effort entry rather
  /// than a `(Medium)` suffix baked into its label. That is not decoration —
  /// it makes the picker agree with the transcript. `split_announced_model`
  /// already splits a trailing parenthetical off as effort when it names a real
  /// level, so agy's announced `GPT-OSS 120B (Medium)` renders in the turn
  /// footer as model `GPT-OSS 120B` + effort `medium`. A label carrying the
  /// suffix would contradict the footer directly below it.
  ///
  /// Fixed rather than fetched when the picker opens: `agy models` needs auth
  /// and network, which would give one harness's picker a spinner and a failure
  /// state. Google changes the list without a CLI release (Gemini 3.1 Pro and
  /// both Claude 4.6 models were gone by 1.4.2), so the live test named above
  /// is how a change gets noticed. A retired model fails pre-dispatch and costs
  /// nothing, but given with an effort `agy` reports it as `--effort is not
  /// supported for model`, not as an unknown model.
  ///
  /// Removing an entry changes what a new install is seeded with, never what a
  /// user already configured: no agent or saved preference is migrated. The
  /// sidebar falls back to a dropdown for an off-catalog persisted value, and a
  /// persisted choice is re-appended to the picker, so it stays visible and
  /// removable rather than becoming stuck.
  antigravity: ANTIGRAVITY_MODELS.map(({ label, value }) => ({ label, value })),
};

/// Short display name for an *observed* Claude model id, for places too narrow
/// for the full id — the per-model weekly usage window's label on the agent
/// card. The stream reports ids like `claude-fable-5-1`, while the picker above
/// is keyed by family alias, so a hyphen-delimited segment match maps one onto
/// the other and the card names the model with the same word the user selected
/// it by. Segment match rather than substring so a family name can't be found
/// inside an unrelated token. An unrecognized id falls through verbatim: long,
/// but never wrong, and the cell's tooltip has room for it.
export function claudeModelFamilyLabel(modelId: string): string {
  const segments = new Set(modelId.split("-"));
  for (const option of MODEL_OPTIONS.claude_code) {
    if (segments.has(option.value)) return option.label;
  }
  return modelId;
}

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
  // Five entries, longest label "Gemini 3.8 Flash" at 16 characters —
  // comparable to Codex's segmented row. Judged against the running app: five
  // pills carrying Google's full names, prefixes and suffixes included, were
  // tried first and rejected.
  antigravity: "segmented",
};

/// Per-harness effort options offered by Switchboard. Codex also accepts `none`
/// and `minimal` on some older models, but neither is offered: both are unused in
/// practice and GPT-6 Astra rejects them. Effort validity is additionally
/// **per-model** for Antigravity (see `effortOptionsFor`), so a form scoped to a
/// chosen model must derive its options through that helper rather than reading
/// this map directly — an Antigravity model can have no effort axis at all (agy
/// 1.1.19 offered two), and the helper is what hides the control for one.
export const EFFORT_OPTIONS: Record<HarnessKind, SelectionOption[]> = {
  claude_code: [
    { label: "Low", value: "low" },
    { label: "Medium", value: "medium" },
    { label: "High", value: "high" },
    { label: "XHigh", value: "xhigh" },
    { label: "Max", value: "max" },
  ],
  codex: [
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
/// `MODEL_OPTIONS.antigravity`. `agy` validates the pair client-side before
/// dispatch, so a wrong entry fails loudly and quota-free with the CLI naming
/// the valid set — unlike Claude, which silently degrades.
///
/// The sets differ per model — GPT-OSS takes only medium where the rest take
/// all three, and agy 1.1.19 had models with only low/high and with none —
/// which is why this is a per-model map rather than one list. `null`
/// explicitly means no effort axis; absence means an unknown/off-catalog model
/// whose persisted effort must survive. A single-valued entry is the point for
/// GPT-OSS: the control renders as one already-selected option, which states
/// what the turn will run at, so the picker and the turn footer describe the
/// model the same way.
const ANTIGRAVITY_MODEL_EFFORTS: Record<string, readonly string[] | null> = Object.fromEntries(
  ANTIGRAVITY_MODELS.map(({ value, efforts }) => [value, efforts]),
);

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
/// its levels differ per model and a model can have no axis at all. Every
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
/// Antigravity-only, and the window's rule rather than the CLI's: `agy` has
/// accepted a bare axis-bearing model since 1.3.4 (it picks the model's default
/// level), but the window's pickers always store a level for one, so they offer
/// no "Default" for those. Core does not enforce it, so an agent whose effort
/// was cleared through MCP dispatches a bare model and `agy` picks its default.
/// Every other harness treats effort as optional — an unset effort means "pass
/// no flag, let the harness choose".
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
    model_choices: ["gpt-6.1-sol", "gpt-5.6-terra"],
    effort_choices: ["medium", "high"],
    default_model: "gpt-6.1-sol",
    default_effort: "medium",
  },
  // Both carry explicit effort because the window's pickers always store one
  // for a model with an effort axis (see `effortIsRequired`). Mirrored by the
  // Rust built-in in `preferences.rs`.
  antigravity: {
    model_choices: ["gemini-3.8-flash", "gemini-4-argon"],
    effort_choices: ["medium", "high"],
    default_model: "gemini-3.8-flash",
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

function sameChoices(a: string[], b: string[]): boolean {
  return a.length === b.length && a.every((value, i) => value === b[i]);
}

/// The parts of `after` that differ from `before`, which is all a save sends.
/// `before` is what the user started from — the record when they opened the
/// editor or clicked the chip — so a setting another client changed since then
/// is not sent back over that client's change.
export function selectionChanges(
  before: AgentSelection,
  after: AgentSelection,
): AgentSelectionChanges {
  const changes: AgentSelectionChanges = {};
  if (after.model !== before.model) changes.model = after.model;
  if (after.effort !== before.effort) changes.effort = after.effort;
  if (!sameChoices(after.model_choices, before.model_choices)) {
    changes.model_choices = after.model_choices;
  }
  if (!sameChoices(after.effort_choices, before.effort_choices)) {
    changes.effort_choices = after.effort_choices;
  }
  return changes;
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
