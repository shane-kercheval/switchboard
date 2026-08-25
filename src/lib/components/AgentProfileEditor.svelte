<script lang="ts">
  import type { AgentProfile, HarnessKind } from "$lib/types";
  import {
    MODEL_OPTIONS,
    MODEL_PRESENTATION,
    SUGGESTED_SECONDARY_PROFILE,
    effortIsRequired,
    effortOptionsFor,
    type SelectionOption,
  } from "$lib/agentSelection";
  import {
    HARNESS_LABEL,
    SUPPORTS_EFFORT_SELECTION,
    SUPPORTS_MODEL_SELECTION,
  } from "$lib/harnessDisplay";
  import SelectionPicker from "$lib/components/ui/SelectionPicker.svelte";
  import { cn } from "$lib/utils";

  type Props = {
    harness: HarnessKind;
    primary: AgentProfile;
    secondary: AgentProfile | null;
    secondarySuggestion?: AgentProfile | null;
    disabled?: boolean;
    allowUnset?: boolean;
    testidPrefix?: string;
    legacyPrimaryTestids?: boolean;
  };

  let {
    harness,
    primary = $bindable(),
    secondary = $bindable(),
    secondarySuggestion = null,
    disabled = false,
    allowUnset = false,
    testidPrefix = "agent-profile",
    legacyPrimaryTestids = false,
  }: Props = $props();

  const modelSupported = $derived(SUPPORTS_MODEL_SELECTION[harness]);
  const effortSupported = $derived(SUPPORTS_EFFORT_SELECTION[harness]);

  function withCurrent(options: SelectionOption[], current: string | null): SelectionOption[] {
    if (current == null || current === "" || options.some((o) => o.value === current)) {
      return options;
    }
    return [{ label: current, value: current }, ...options];
  }

  function modelOptions(profile: AgentProfile): SelectionOption[] {
    const options = allowUnset
      ? [{ label: "Default", value: "" }, ...MODEL_OPTIONS[harness]]
      : MODEL_OPTIONS[harness];
    return withCurrent(options, profile.model);
  }

  function effortOptions(profile: AgentProfile): SelectionOption[] {
    const base = effortOptionsFor(harness, profile.model ?? undefined);
    // No "Default" where the harness demands a level: `agy` rejects an
    // effort-bearing model dispatched without one, so offering "Default" would
    // offer a choice that fails every turn.
    const options =
      allowUnset && !effortIsRequired(harness, profile.model ?? undefined)
        ? [{ label: "Default", value: "" }, ...base]
        : base;
    return withCurrent(options, profile.effort);
  }

  /// Whether to render the effort control at all for this profile.
  ///
  /// Harness capability is necessary but not sufficient: Antigravity supports
  /// the axis, yet several of its models have none, and with no model selected
  /// there is nothing to derive levels from. An empty option set means there is
  /// no valid choice to present, so the control is hidden rather than shown
  /// empty or filled with options that would be rejected at dispatch.
  function showEffort(profile: AgentProfile): boolean {
    if (!effortSupported) return false;
    if (effortOptionsFor(harness, profile.model ?? undefined).length > 0) return true;
    // An off-catalog persisted value still needs somewhere to display.
    return profile.effort != null && profile.effort !== "";
  }

  function presentation(profile: AgentProfile): "segmented" | "dropdown" {
    const offCatalog =
      profile.model != null &&
      profile.model !== "" &&
      !MODEL_OPTIONS[harness].some((o) => o.value === profile.model);
    return MODEL_PRESENTATION[harness] === "dropdown" || offCatalog ? "dropdown" : "segmented";
  }

  function setModel(slot: "primary" | "secondary", value: string): void {
    const current = slot === "primary" ? primary : secondary;
    if (current === null) return;
    const model = value === "" ? null : value;
    const validEfforts = effortOptionsFor(harness, model ?? undefined);
    const keepUnset = current.effort === null && allowUnset;
    const effort =
      // A model whose axis is mandatory cannot stay unset — pick a level rather
      // than persist a profile that fails on dispatch.
      keepUnset && !effortIsRequired(harness, model ?? undefined)
        ? null
        : current.effort !== null && validEfforts.some((o) => o.value === current.effort)
          ? current.effort
          : (validEfforts.find((option) => option.value === "medium")?.value ??
            validEfforts[0]?.value ??
            null);
    const next = { ...current, model, effort };
    if (slot === "primary") primary = next;
    else secondary = next;
  }

  function setEffort(slot: "primary" | "secondary", value: string): void {
    const current = slot === "primary" ? primary : secondary;
    if (current === null) return;
    const next = { ...current, effort: value === "" ? null : value };
    if (slot === "primary") primary = next;
    else secondary = next;
  }

  function toggleSecondary(): void {
    secondary =
      secondary === null
        ? { ...(secondarySuggestion ?? SUGGESTED_SECONDARY_PROFILE[harness]) }
        : null;
  }

  function pickerTestid(slot: "primary" | "secondary", axis: "model" | "effort"): string {
    return legacyPrimaryTestids && slot === "primary"
      ? `${axis}-select`
      : `${testidPrefix}-${slot}-${axis}`;
  }
</script>

{#snippet profileFields(label: string, slot: "primary" | "secondary", profile: AgentProfile)}
  <div
    class="border-border bg-panel space-y-3 rounded-md border p-3"
    data-testid={`${testidPrefix}-${slot}`}
  >
    <div class="text-fg text-sm font-medium">{label}</div>
    {#if modelSupported}
      <label class="block space-y-1">
        <span class="text-muted text-xs">Model</span>
        <SelectionPicker
          bind:value={() => profile.model ?? "", (value) => setModel(slot, value)}
          options={modelOptions(profile)}
          {disabled}
          testid={pickerTestid(slot, "model")}
          ariaLabel={`${label} model`}
          presentation={presentation(profile)}
        />
      </label>
    {/if}
    {#if showEffort(profile)}
      <label class="block space-y-1">
        <span class="text-muted text-xs">Reasoning effort</span>
        <SelectionPicker
          bind:value={() => profile.effort ?? "", (value) => setEffort(slot, value)}
          options={effortOptions(profile)}
          {disabled}
          testid={pickerTestid(slot, "effort")}
          ariaLabel={`${label} reasoning effort`}
        />
      </label>
    {/if}
  </div>
{/snippet}

<div class="space-y-3" data-testid={testidPrefix}>
  {#if modelSupported || effortSupported}
    {@render profileFields("Primary", "primary", primary)}

    <div class="flex items-start justify-between gap-4">
      <div class="min-w-0">
        <div class="text-fg text-sm">Secondary configuration</div>
        <p class="text-muted mt-0.5 text-xs">Add a second model and effort for quick switching.</p>
      </div>
      <button
        type="button"
        role="switch"
        aria-checked={secondary !== null}
        aria-label="Enable secondary configuration"
        data-testid={`${testidPrefix}-secondary-toggle`}
        class={cn(
          "relative mt-0.5 inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors outline-none",
          disabled ? "cursor-not-allowed opacity-50" : "cursor-pointer",
          secondary !== null ? "bg-accent" : "bg-active",
        )}
        {disabled}
        onclick={toggleSecondary}
      >
        <span
          class={cn(
            "bg-raised inline-block h-4 w-4 transform rounded-full transition-transform",
            secondary !== null ? "translate-x-4" : "translate-x-0.5",
          )}
        ></span>
      </button>
    </div>

    {#if secondary !== null}
      {@render profileFields("Secondary", "secondary", secondary)}
    {/if}
  {:else}
    <p class="text-muted text-xs leading-relaxed" data-testid={`${testidPrefix}-unsupported`}>
      {HARNESS_LABEL[harness]}'s model and effort are selected inside {HARNESS_LABEL[harness]} and cannot
      be changed from Switchboard.
    </p>
  {/if}
</div>
