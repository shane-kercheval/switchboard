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

  const roleLabel = $derived(
    context === "default" ? "Default" : context === "start" ? "Start with" : "Current",
  );
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
      if (current(axis) === value) {
        interactionMessage = {
          axis,
          text: `Choose another ${roleLabel.toLowerCase()} before removing this value.`,
        };
        return;
      }
      if (choices(axis).length === 1) {
        interactionMessage = {
          axis,
          text: "At least one quick choice is required once this axis is configured.",
        };
        return;
      }
      onChange(
        withAxis(
          axis,
          choices(axis).filter((choice) => choice !== value),
          current(axis),
        ),
      );
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

  function showSelector(axis: Axis): boolean {
    const axisChoices = choices(axis);
    return axisChoices.length > 1 || (context === "current" && axisChoices[0] !== current(axis));
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
  <fieldset class="space-y-2" {disabled} data-testid={`${testidPrefix}-${axis}`}>
    <legend class="text-fg text-sm font-medium">{label}</legend>
    <div class={cn(SEGMENTED_CONTAINER_CLASS, "flex flex-wrap")}>
      {#each options as option (option.value)}
        {@const selected = axisChoices.includes(option.value)}
        {@const locked = selected && axisCurrent === option.value}
        <button
          type="button"
          aria-pressed={selected}
          aria-disabled={locked}
          {disabled}
          title={locked
            ? `Choose another ${roleLabel.toLowerCase()} before removing this value.`
            : undefined}
          class={cn(
            SEGMENTED_ITEM_CLASS,
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
    {:else if axisChoices.length === 1 && !showSelector(axis)}
      <p class="text-muted text-xs" data-testid={`${testidPrefix}-${axis}-implicit`}>
        {roleLabel}: {optionLabel(options, axisChoices[0] ?? "")}
      </p>
    {:else if axisChoices.length > 0}
      <label class="block space-y-1">
        <span class="text-muted text-xs">{roleLabel}</span>
        <Select
          value={axisCurrent ?? ""}
          {disabled}
          options={axisChoices.map((value) => ({
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
  {#if modelSupported}
    {@render axisEditor("model", "Model quick choices", modelOptions)}
  {/if}
  {#if effortSupported}
    {@render axisEditor("effort", "Reasoning effort quick choices", effortOptions)}
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
