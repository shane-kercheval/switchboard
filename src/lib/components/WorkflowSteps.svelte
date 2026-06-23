<script lang="ts">
  import type {
    RecipientRef,
    WorkflowStepInfo,
    WorkflowInputValue,
    WorkflowRunStatus,
  } from "$lib/types";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import { cn } from "$lib/utils";

  /// The ordered step list for a workflow, in two modes:
  ///  - `preview` (in the composer): every row neutral; slot recipients resolve
  ///    live against the form's `inputs` as the user assigns agents.
  ///  - `live` (replacing compose during a run): per-step done / active / pending /
  ///    failed state, derived here from the run's `current` index + `status` (the
  ///    shared convention — callers pass raw run fields, not per-step states).
  /// One component so preview and live never diverge in how a step reads.
  type Props = {
    steps: WorkflowStepInfo[];
    mode: "preview" | "live";
    /// Preview only: the form's bound input values, for resolving `slot` recipients.
    inputs?: Record<string, WorkflowInputValue>;
    /// Live only: the run's current/failing step index and status.
    current?: number;
    status?: WorkflowRunStatus;
    reason?: string | null;
  };

  let { steps, mode, inputs, current = 0, status = "running", reason = null }: Props = $props();

  type StepState = "done" | "active" | "pending" | "failed" | "preview";

  function stepState(index: number): StepState {
    if (mode === "preview") return "preview";
    if (index < current) return "done";
    if (index === current) return status === "running" ? "active" : "failed";
    return "pending";
  }

  /// Resolve a row's recipient refs to display names. A `literal` is shown
  /// verbatim; a `slot` resolves against `inputs` in preview (and falls back to the
  /// input name when unbound), and in live mode is already a literal — an
  /// unresolved slot there just shows the input name.
  ///
  /// Agents are the first-class unit: a slot bound by selecting a *pane* resolves
  /// to its member agent names here — we deliberately do NOT collapse a pane's
  /// members back to a pane name. A pane is a selection convenience (and a
  /// keyboard shortcut), not a displayed entity, which keeps every recipient
  /// surface consistent and sidesteps stale/ambiguous pane references (pane
  /// membership is mutable; the run resolved to concrete agents at invoke).
  function names(refs: RecipientRef[]): string[] {
    return refs.flatMap((r) => {
      if (r.kind === "literal") return [r.name];
      const bound = inputs?.[r.input];
      if (bound === undefined) return [r.input];
      if (typeof bound === "string") return bound.trim() === "" ? [r.input] : [bound];
      return bound.length === 0 ? [r.input] : bound;
    });
  }
</script>

<ol class="flex flex-col gap-1.5" data-testid="workflow-steps">
  {#each steps as step, i (i)}
    {@const state = stepState(i)}
    {@const recipients = names(step.recipients)}
    {@const feeds = names(step.feeds_from)}
    <li
      class="flex items-start gap-2 text-sm"
      data-testid={`workflow-step-${i}`}
      data-step-state={state}
    >
      <span class="mt-0.5 flex h-3.5 w-3.5 shrink-0 items-center justify-center" aria-hidden="true">
        {#if state === "active"}
          <Spinner class="h-3.5 w-3.5" />
        {:else if state === "done"}
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2.5"
            stroke-linecap="round"
            stroke-linejoin="round"
            class="text-accent h-3.5 w-3.5"
          >
            <path d="M20 6 9 17l-5-5" />
          </svg>
        {:else if state === "failed"}
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2.5"
            stroke-linecap="round"
            stroke-linejoin="round"
            class="text-status-failed h-3.5 w-3.5"
          >
            <path d="M18 6 6 18M6 6l12 12" />
          </svg>
        {:else}
          <!-- pending / preview: a hollow, dim marker -->
          <span
            class={cn(
              "h-2 w-2 rounded-full border",
              state === "pending" ? "border-muted/40" : "border-muted/60",
            )}
          ></span>
        {/if}
      </span>
      <span class="flex min-w-0 flex-col gap-0.5">
        <span class="flex flex-wrap items-baseline gap-x-1.5">
          <span class={cn("font-medium", state === "pending" ? "text-muted" : "text-fg")}
            >{step.label}</span
          >
          {#if recipients.length > 0}
            <span class="text-muted text-xs" data-testid={`workflow-step-recipients-${i}`}>
              → {recipients.join(", ")}
            </span>
          {/if}
        </span>
        {#if feeds.length > 0}
          <span class="text-muted text-[11px] leading-4" data-testid={`workflow-step-feeds-${i}`}>
            ↪ feeds from {feeds.join(", ")}
          </span>
        {/if}
        {#if state === "failed" && reason}
          <span class="text-status-failed text-xs" data-testid={`workflow-step-reason-${i}`}
            >{reason}</span
          >
        {/if}
      </span>
    </li>
  {/each}
</ol>
