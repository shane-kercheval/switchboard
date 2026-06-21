<script lang="ts">
  import { allRuns, cancelRun, abandonRun } from "$lib/state/workflows.svelte";
  import { projects } from "$lib/state/workspace.svelte";
  import { cn } from "$lib/utils";

  /// The app-global workflow run indicator: a compact count badge in the top bar
  /// that expands to a list of every active / interrupted / retained-failed run
  /// across all loaded projects (not just the active one). A lightweight
  /// orchestration tracker — the run's *substance* is in the transcript; this
  /// only tracks step/status and offers Cancel (running) / Abandon (terminal).
  let expanded = $state(false);
  // The last cancel/abandon failure, surfaced in the popover so the action isn't
  // a silent dead button; cleared on the next successful action.
  let actionError = $state<string | null>(null);

  const runs = $derived.by(() => allRuns());

  function statusLabel(status: string): string {
    if (status === "running") return "Running";
    if (status === "failed") return "Failed";
    return "Interrupted";
  }

  // The run list is app-global, so each row names its project (two projects can
  // run the same workflow). Falls back to a short id if the project isn't loaded.
  function projectLabel(projectId: string): string {
    return projects.list.find((p) => p.id === projectId)?.name ?? projectId.slice(0, 8);
  }

  async function onCancel(runId: string): Promise<void> {
    try {
      await cancelRun(runId);
      actionError = null;
    } catch (err) {
      actionError = `Couldn't cancel: ${err instanceof Error ? err.message : String(err)}`;
      console.warn("[switchboard] cancelRun failed", err);
    }
  }

  async function onAbandon(projectId: string, runId: string): Promise<void> {
    try {
      await abandonRun(projectId, runId);
      actionError = null;
    } catch (err) {
      actionError = `Couldn't abandon: ${err instanceof Error ? err.message : String(err)}`;
      console.warn("[switchboard] abandonRun failed", err);
    }
  }
</script>

{#if runs.length > 0}
  <div class="relative" data-testid="workflow-run-indicator">
    <button
      type="button"
      class="border-border bg-panel text-fg hover:bg-raised inline-flex h-6.5 shrink-0 items-center gap-1.5 rounded-full border px-2 text-xs"
      data-testid="workflow-run-indicator-toggle"
      aria-expanded={expanded}
      onclick={() => (expanded = !expanded)}
    >
      <span
        class={cn(
          "inline-block h-1.5 w-1.5 rounded-full",
          runs.some((r) => r.run.status === "running") ? "bg-accent" : "bg-status-failed",
        )}
      ></span>
      <span class="font-medium" data-testid="workflow-run-indicator-count">
        {runs.length} workflow{runs.length === 1 ? "" : "s"}
      </span>
    </button>

    {#if expanded}
      <div
        class="border-border/90 bg-raised absolute top-full right-0 z-30 mt-1 w-72 overflow-hidden rounded-lg border p-1 shadow-[0_10px_28px_rgba(0,0,0,0.12)]"
        data-testid="workflow-run-list"
      >
        {#each runs as { projectId, run } (run.run_id)}
          <div
            class="flex items-center justify-between gap-2 rounded-md px-2 py-1.5"
            data-testid={`workflow-run-${run.run_id}`}
          >
            <div class="min-w-0">
              <div class="flex items-baseline gap-1.5">
                <span class="text-fg truncate text-sm font-medium">{run.workflow}</span>
                <span
                  class="text-muted shrink-0 truncate text-[11px]"
                  data-testid={`workflow-run-project-${run.run_id}`}
                >
                  {projectLabel(projectId)}
                </span>
              </div>
              <div class="text-muted text-xs">
                {statusLabel(run.status)} · step {run.step + 1}/{run.total}
                {#if run.reason}<span class="text-status-failed"> · {run.reason}</span>{/if}
              </div>
            </div>
            {#if run.status === "running"}
              <button
                type="button"
                class="text-muted hover:text-fg shrink-0 text-xs"
                data-testid={`workflow-run-cancel-${run.run_id}`}
                onclick={() => void onCancel(run.run_id)}
              >
                Cancel
              </button>
            {:else}
              <button
                type="button"
                class="text-muted hover:text-fg shrink-0 text-xs"
                data-testid={`workflow-run-abandon-${run.run_id}`}
                onclick={() => void onAbandon(projectId, run.run_id)}
              >
                Abandon
              </button>
            {/if}
          </div>
        {/each}
        {#if actionError}
          <p class="text-status-failed px-2 py-1 text-xs" data-testid="workflow-run-action-error">
            {actionError}
          </p>
        {/if}
      </div>
    {/if}
  </div>
{/if}
