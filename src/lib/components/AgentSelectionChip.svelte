<script lang="ts">
  import { ChevronsUpDown } from "@lucide/svelte";
  import type { AgentSelection, HarnessKind } from "$lib/types";
  import {
    EFFORT_OPTIONS,
    MODEL_OPTIONS,
    activatableEffortValues,
    effortSupportFor,
    resolveModelChange,
    type SelectionOption,
  } from "$lib/agentSelection";
  import DropdownMenu from "$lib/components/ui/DropdownMenu.svelte";
  import DropdownMenuItem from "$lib/components/ui/DropdownMenuItem.svelte";
  import { cn } from "$lib/utils";

  type Axis = "model" | "effort";
  type Props = {
    axis: Axis;
    harness: HarnessKind;
    selection: AgentSelection;
    busy?: boolean;
    onActivate: (selection: AgentSelection) => void;
  };

  let { axis, harness, selection, busy = false, onActivate }: Props = $props();

  const values = $derived(axis === "model" ? selection.model_choices : selection.effort_choices);
  const current = $derived(axis === "model" ? selection.model : selection.effort);
  const catalog = $derived(axis === "model" ? MODEL_OPTIONS[harness] : EFFORT_OPTIONS[harness]);
  const effortSupport = $derived(effortSupportFor(harness, selection.model));
  const activatableEfforts = $derived(activatableEffortValues(selection, harness));
  const axisName = $derived(axis === "model" ? "model" : "reasoning effort");
  // One configured current choice is metadata, two preserve the old one-click
  // swap, and larger sets use a menu so activation never depends on cycling order.
  const presentation = $derived(
    values.length >= 3
      ? "menu"
      : values.length === 1 && values[0] === current
        ? "static"
        : "toggle",
  );

  function label(value: string | null): string {
    if (value === null) return "Harness default";
    return catalog.find((option: SelectionOption) => option.value === value)?.label ?? value;
  }

  function resultFor(
    value: string,
  ): { ok: true; selection: AgentSelection } | { ok: false; reason: string } {
    if (axis === "model") {
      const result = resolveModelChange(selection, harness, value);
      return result.ok
        ? result
        : { ok: false, reason: `${result.reason} Open Model settings to add one.` };
    }
    if (effortSupport.kind === "known" && !activatableEfforts.has(value)) {
      return { ok: false, reason: "Unavailable for the current model" };
    }
    return { ok: true, selection: { ...selection, effort: value } };
  }

  function activate(value: string): void {
    if (busy || value === current) return;
    const result = resultFor(value);
    if (result.ok) onActivate(result.selection);
  }

  function directTarget(): string | null {
    if (values.length === 0) return null;
    return values.find((value) => value !== current) ?? values[0] ?? null;
  }

  function activationLabel(value: string): string {
    const base = label(value);
    const result = resultFor(value);
    if (axis !== "model" || !result.ok || result.selection.effort === selection.effort) {
      return base;
    }
    if (result.selection.effort === null) return `${base} — reasoning effort cleared`;
    const effortLabel =
      EFFORT_OPTIONS[harness].find((option) => option.value === result.selection.effort)?.label ??
      result.selection.effort;
    return `${base} — effort becomes ${effortLabel}`;
  }
</script>

{#if presentation === "static"}
  <span
    class="bg-panel text-muted inline-flex min-w-0 items-center rounded px-1.5 py-0.5 text-[11px] leading-4"
    aria-label={`Current ${axisName}: ${label(current)}`}
    data-testid={`agent-${axis}-chip`}
  >
    <span class="truncate" title={current ?? undefined}>{label(current)}</span>
  </span>
{:else if presentation === "toggle"}
  {@const target = directTarget()}
  {@const result = target === null ? null : resultFor(target)}
  <button
    type="button"
    class={cn(
      "bg-panel text-muted hover:bg-hover hover:text-fg inline-flex min-w-0 items-center gap-1 rounded px-1.5 py-0.5 text-[11px] leading-4 transition-colors",
      (busy || result?.ok === false) && "cursor-not-allowed opacity-60",
    )}
    disabled={busy || target === null || result?.ok === false}
    title={result?.ok === false
      ? result.reason
      : target === null
        ? undefined
        : activationLabel(target)}
    aria-label={`Current ${axisName}: ${label(current)}. Switch to ${target === null ? "another choice" : activationLabel(target)}.`}
    data-testid={`agent-${axis}-chip`}
    onclick={() => {
      if (target !== null) activate(target);
    }}
  >
    <span class="truncate">{label(current)}</span>
    <ChevronsUpDown size={10} strokeWidth={1.8} class="shrink-0" aria-hidden="true" />
  </button>
{:else}
  <DropdownMenu
    triggerClass={cn(
      "bg-panel text-muted hover:bg-hover hover:text-fg inline-flex min-w-0 items-center gap-1 rounded px-1.5 py-0.5 text-[11px] leading-4 transition-colors",
      busy && "pointer-events-none opacity-60",
    )}
    triggerLabel={`Current ${axisName}: ${label(current)}. Choose ${axisName}.`}
    triggerTestid={`agent-${axis}-chip`}
    triggerDisabled={busy}
    contentTestid={`agent-${axis}-menu`}
    align="start"
  >
    {#snippet trigger()}
      <span class="truncate" title={current ?? undefined}>{label(current)}</span>
      <ChevronsUpDown size={10} strokeWidth={1.8} class="shrink-0" aria-hidden="true" />
    {/snippet}
    {#each values as value (value)}
      {@const result = resultFor(value)}
      <DropdownMenuItem
        onSelect={() => activate(value)}
        disabled={busy || value === current || !result.ok}
        tooltip={!result.ok ? result.reason : undefined}
        data-testid={`agent-${axis}-option-${value}`}
      >
        {activationLabel(value)}{value === current ? " (current)" : ""}
      </DropdownMenuItem>
    {/each}
  </DropdownMenu>
{/if}
