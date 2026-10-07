<script lang="ts">
  import type { AgentSelection, HarnessKind } from "$lib/types";
  import {
    EFFORT_OPTIONS,
    MODEL_OPTIONS,
    activatableEffortValues,
    effortSupportFor,
    resolveModelChange,
    selectionIsValid,
    type SelectionOption,
  } from "$lib/agentSelection";
  import { cn } from "$lib/utils";
  import {
    HARNESS_LABEL,
    SUPPORTS_EFFORT_SELECTION,
    SUPPORTS_MODEL_SELECTION,
  } from "$lib/harnessDisplay";
  import Select from "$lib/components/ui/Select.svelte";
  import {
    SEGMENTED_CONTAINER_CLASS,
    SEGMENTED_ITEM_ACTIVE_CLASS,
    SEGMENTED_ITEM_CLASS,
    SEGMENTED_ITEM_INACTIVE_CLASS,
  } from "$lib/components/ui/segmentedControl";

  type Context = "default" | "start" | "current";
  type Axis = "model" | "effort";
  type InteractionMessage = { axis: Axis; text: string };
  type Props = {
    harness: HarnessKind;
    selection: AgentSelection;
    context: Context;
    onChange: (selection: AgentSelection) => void;
    disabled?: boolean;
    testidPrefix?: string;
  };

  let {
    harness,
    selection,
    context,
    onChange,
    disabled = false,
    testidPrefix = "agent-selection",
  }: Props = $props();
  let interactionMessage = $state<InteractionMessage | null>(null);

  const roleLabel = $derived(context === "current" ? "Current" : "Default");
  const modelOptions = $derived(
    optionsWithPersisted(MODEL_OPTIONS[harness], selection.model_choices),
  );
  const effortOptions = $derived(
    optionsWithPersisted(EFFORT_OPTIONS[harness], selection.effort_choices),
  );
  const modelSupported = $derived(SUPPORTS_MODEL_SELECTION[harness]);
  const effortSupported = $derived(SUPPORTS_EFFORT_SELECTION[harness]);
  const effortSupport = $derived(effortSupportFor(harness, selection.model));
  const effortDefaultIsIndependent = $derived(
    context === "default" && effortSupport.kind === "none",
  );
  const validEfforts = $derived(activatableEffortValues(selection, harness));
  const pairValid = $derived(selectionIsValid(selection, harness));
  const interactionResetKey = $derived(
    JSON.stringify([
      harness,
      context,
      selection.model,
      selection.effort,
      selection.model_choices,
      selection.effort_choices,
    ]),
  );

  function clearInteractionMessage(_resetKey: string): void {
    interactionMessage = null;
  }

  $effect(() => {
    clearInteractionMessage(interactionResetKey);
  });

  function optionsWithPersisted(
    curated: SelectionOption[],
    persisted: readonly string[],
  ): SelectionOption[] {
    const options = [...curated];
    for (const value of persisted) {
      if (!options.some((option) => option.value === value)) options.push({ label: value, value });
    }
    return options;
  }

  function choices(axis: Axis): string[] {
    return axis === "model" ? selection.model_choices : selection.effort_choices;
  }

  function current(axis: Axis): string | null {
    return axis === "model" ? selection.model : selection.effort;
  }

  function orderedChoices(axis: Axis, values: readonly string[] = choices(axis)): string[] {
    const selected = new Set(values);
    const options = axis === "model" ? modelOptions : effortOptions;
    return options.filter((option) => selected.has(option.value)).map((option) => option.value);
  }

  function withAxis(axis: Axis, values: string[], value: string | null): AgentSelection {
    return axis === "model"
      ? { ...selection, model_choices: values, model: value }
      : { ...selection, effort_choices: values, effort: value };
  }

  function toggleChoice(axis: Axis, value: string): void {
    if (disabled) return;
    interactionMessage = null;
    const selected = choices(axis).includes(value);
    if (selected) {
      if (choices(axis).length === 1) {
        interactionMessage = {
          axis,
          text: "At least one quick choice is required once this axis is configured.",
        };
        return;
      }
      const nextChoices = choices(axis).filter((choice) => choice !== value);
      if (current(axis) !== value) {
        onChange(withAxis(axis, nextChoices, current(axis)));
        return;
      }
      const fallback = orderedChoices(axis, nextChoices)[0] ?? null;
      const next = withAxis(axis, nextChoices, fallback);
      if (axis === "effort" || fallback === null) {
        onChange(next);
        return;
      }
      // Settings defaults are independent axes. Inspect the fallback model so
      // removing a no-effort default preserves the separate effort default.
      if (context === "default" && effortSupportFor(harness, fallback).kind === "none") {
        onChange(next);
        return;
      }
      const resolved = resolveModelChange(next, harness, fallback);
      onChange(resolved.ok ? resolved.selection : next);
      return;
    }

    const nextChoices = [...choices(axis), value];
    if (current(axis) !== null) {
      onChange(withAxis(axis, nextChoices, current(axis)));
      return;
    }
    if (axis === "model") {
      onChange({ ...selection, model_choices: nextChoices, model: value });
      return;
    }
    const next = withAxis(
      axis,
      nextChoices,
      effortDefaultIsIndependent ? value : effortCompatible(value) ? value : null,
    );
    onChange(next);
  }

  function chooseCurrent(axis: Axis, value: string): void {
    if (disabled) return;
    interactionMessage = null;
    if (axis === "effort") {
      if (!effortDefaultIsIndependent && !validEfforts.has(value)) {
        interactionMessage = {
          axis,
          text: "That reasoning effort is not available for the current model.",
        };
        return;
      }
      onChange({ ...selection, effort: value });
      return;
    }
    // Settings defaults are independent axes. Inspect the target model here so
    // switching to a no-effort default preserves the separate effort default.
    if (context === "default" && effortSupportFor(harness, value).kind === "none") {
      onChange({ ...selection, model: value });
      return;
    }
    const resolved = resolveModelChange(selection, harness, value);
    if (!resolved.ok) {
      interactionMessage = {
        axis,
        text: `${resolved.reason} Add a compatible reasoning effort quick choice, then try again.`,
      };
      return;
    }
    onChange(resolved.selection);
  }

  function optionLabel(options: SelectionOption[], value: string): string {
    return options.find((option) => option.value === value)?.label ?? value;
  }

  function effortCompatible(value: string): boolean {
    const support = effortSupportFor(harness, selection.model);
    return (
      support.kind === "unknown" ||
      (support.kind === "known" && support.options.some((option) => option.value === value))
    );
  }

  function effortDisabled(value: string): boolean {
    if (effortDefaultIsIndependent) return false;
    return !effortCompatible(value);
  }

  function assignmentUnavailableMessage(axis: Axis): string | null {
    if (axis !== "effort" || effortDefaultIsIndependent || effortSupport.kind !== "none") {
      return null;
    }
    return selection.model === null
      ? "Choose a model before assigning the reasoning effort."
      : "The current model does not use reasoning effort.";
  }
</script>

{#snippet axisEditor(axis: Axis, label: string, options: SelectionOption[])}
  {@const axisChoices = choices(axis)}
  {@const axisCurrent = current(axis)}
  {@const unavailableMessage = assignmentUnavailableMessage(axis)}
  {@const compact = options.length > 5}
  <fieldset class="space-y-2" {disabled} data-testid={`${testidPrefix}-${axis}`}>
    <legend class="text-fg text-sm font-medium">{label}</legend>
    <div
      class={cn(SEGMENTED_CONTAINER_CLASS, "grid w-full", compact && "gap-0.5")}
      style={`grid-template-columns: repeat(${Math.max(1, options.length)}, minmax(0, 1fr));`}
      data-testid={`${testidPrefix}-${axis}-choices`}
    >
      {#each options as option (option.value)}
        {@const selected = axisChoices.includes(option.value)}
        {@const locked = selected && axisChoices.length === 1}
        <button
          type="button"
          aria-pressed={selected}
          aria-disabled={locked}
          {disabled}
          title={locked
            ? "At least one quick choice is required once this axis is configured."
            : undefined}
          class={cn(
            SEGMENTED_ITEM_CLASS,
            "flex min-w-0 items-center justify-center truncate text-center",
            compact && "px-1 text-[11px]",
            selected ? SEGMENTED_ITEM_ACTIVE_CLASS : SEGMENTED_ITEM_INACTIVE_CLASS,
            locked && "cursor-not-allowed opacity-60",
          )}
          data-testid={`${testidPrefix}-${axis}-choice-${option.value}`}
          onclick={() => toggleChoice(axis, option.value)}
        >
          {option.label}
        </button>
      {/each}
    </div>

    {#if unavailableMessage !== null}
      <p class="text-muted text-xs" data-testid={`${testidPrefix}-${axis}-unavailable`}>
        {unavailableMessage}
      </p>
    {:else if context !== "current" && axisChoices.length > 1}
      <label class="block space-y-1">
        <span class="text-muted text-xs">{roleLabel}</span>
        <Select
          value={axisCurrent ?? ""}
          {disabled}
          options={orderedChoices(axis).map((value) => ({
            value,
            label: optionLabel(options, value),
            disabled: axis === "effort" && effortDisabled(value),
          }))}
          placeholder={axisCurrent === null ? "Select a value" : undefined}
          aria-label={`${roleLabel} ${label.toLowerCase()}`}
          data-testid={`${testidPrefix}-${axis}-current`}
          onchange={(event) => chooseCurrent(axis, event.currentTarget.value)}
        />
      </label>
    {/if}
    {#if interactionMessage?.axis === axis}
      <p
        class="text-muted text-xs"
        role="status"
        data-testid={`${testidPrefix}-interaction-message`}
      >
        {interactionMessage.text}
      </p>
    {/if}
  </fieldset>
{/snippet}

<div class="space-y-4" data-testid={testidPrefix}>
  {#if context === "start"}
    <p class="text-muted text-sm leading-relaxed">
      Choose which models and reasoning efforts are available for quick switching from the Agents
      sidebar, and which ones this agent uses by default.
    </p>
  {:else if context === "current"}
    <p class="text-muted text-sm leading-relaxed">
      Choose which models and reasoning efforts are available for quick switching from the Agents
      sidebar.
    </p>
  {/if}
  {#if modelSupported}
    {@render axisEditor("model", "Model", modelOptions)}
  {/if}
  {#if effortSupported}
    {@render axisEditor("effort", "Reasoning Effort", effortOptions)}
  {/if}
  {#if !modelSupported && !effortSupported}
    <p class="text-muted text-xs leading-relaxed" data-testid={`${testidPrefix}-unsupported`}>
      {HARNESS_LABEL[harness]}'s model and effort are selected inside {HARNESS_LABEL[harness]} and cannot
      be changed from Switchboard.
    </p>
  {/if}
  {#if !pairValid}
    <p class="text-status-failed text-xs" role="alert" data-testid={`${testidPrefix}-invalid`}>
      Choose a reasoning effort supported by the current model before saving.
    </p>
  {/if}
</div>
