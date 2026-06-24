<script lang="ts">
  import { tick } from "svelte";
  import { BookmarkPlus } from "@lucide/svelte";
  import type { WorkflowListing } from "$lib/types";
  import { cn } from "$lib/utils";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { ICON_BUTTON_CLASS } from "$lib/components/ui/iconButton";

  /// A typeahead popover over the project's workflows (built-in + directory),
  /// modeled on `PromptMenu`. Opened by the compose bar's `+ Workflow` button.
  /// Picking an invocable workflow enters workflow mode; a non-invocable one
  /// (uses a not-yet-runnable step) and a parse-error row are shown but not
  /// pickable. Built-ins are tagged read-only and offer "Copy to my workflows".
  let {
    workflows,
    loading = false,
    onpick,
    oncopy,
    onopenfolder,
    onclose,
  }: {
    workflows: WorkflowListing[];
    loading?: boolean;
    onpick: (workflow: WorkflowListing) => void;
    oncopy?: (workflow: WorkflowListing) => void;
    /// Open the user-global workflows folder (where the user adds their own).
    onopenfolder?: () => void;
    onclose: () => void;
  } = $props();

  let query = $state("");
  let highlighted = $state(0);
  let searchEl = $state<HTMLInputElement | undefined>(undefined);

  const filtered = $derived.by(() => {
    const q = query.trim().toLowerCase();
    if (q === "") return workflows;
    return workflows.filter((w) => `${w.name} ${w.description ?? ""}`.toLowerCase().includes(q));
  });

  // A workflow can be selected only when it parsed and is invocable.
  function selectable(w: WorkflowListing): boolean {
    return w.parse_error === null && w.invocable;
  }

  $effect(() => {
    if (highlighted > filtered.length - 1) highlighted = Math.max(0, filtered.length - 1);
  });

  $effect(() => {
    void tick().then(() => searchEl?.focus());
  });

  function workflowKey(w: WorkflowListing): string {
    return `${w.is_builtin ? "builtin" : "dir"}:${w.name}`;
  }

  function pick(w: WorkflowListing): void {
    if (selectable(w)) onpick(w);
  }

  function onKeydown(event: KeyboardEvent): void {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      if (filtered.length > 0) highlighted = (highlighted + 1) % filtered.length;
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      if (filtered.length > 0) highlighted = (highlighted - 1 + filtered.length) % filtered.length;
    } else if (event.key === "Enter") {
      event.preventDefault();
      const w = filtered[highlighted];
      if (w !== undefined) pick(w);
    } else if (event.key === "Escape") {
      event.preventDefault();
      onclose();
    }
  }
</script>

<div
  class="border-border/90 bg-raised absolute inset-x-0 bottom-full z-20 mb-1 overflow-hidden rounded-lg border p-1 shadow-[0_10px_28px_rgba(0,0,0,0.12)]"
  data-testid="workflow-menu"
  role="listbox"
>
  <!-- Inset the rows from the right edge (`pr-2`) so the scrollbar doesn't sit on
       top of the per-row copy button; `scrollbar-gutter: stable` reserves a proper
       lane on WebKit versions that support it (older ones ignore it, hence the
       padding). -->
  <div
    class="max-h-64 [scrollbar-gutter:stable] overflow-y-auto pr-3"
    data-testid="workflow-menu-scroll"
  >
    {#each filtered as workflow, i (workflowKey(workflow))}
      {@const builtin = workflow.is_builtin}
      {@const canPick = selectable(workflow)}
      <div class="relative" role="presentation" onmousemove={() => (highlighted = i)}>
        <button
          type="button"
          class={cn(
            "flex w-full flex-col gap-0.5 rounded-md px-2.5 py-1.5 text-left outline-none select-none",
            canPick ? "cursor-pointer" : "cursor-not-allowed opacity-70",
            i === highlighted && canPick ? "bg-panel/80" : "",
            builtin ? "pr-10" : "",
          )}
          data-testid={`workflow-option-${workflowKey(workflow)}`}
          role="option"
          aria-selected={i === highlighted}
          aria-disabled={!canPick}
          onclick={() => pick(workflow)}
        >
          <span class="flex items-baseline gap-1.5">
            <span class="text-fg text-sm font-medium">{workflow.name}</span>
            {#if builtin}
              <span
                class="border-border/80 text-muted rounded border px-1 text-[10px] tracking-wide uppercase"
                data-testid={`workflow-builtin-tag-${workflowKey(workflow)}`}
              >
                Built-in · read-only
              </span>
            {/if}
          </span>
          {#if workflow.parse_error}
            <span
              class="text-status-failed truncate text-xs"
              data-testid={`workflow-parse-error-${workflowKey(workflow)}`}
            >
              {workflow.parse_error}
            </span>
          {:else if !workflow.invocable}
            <span
              class="text-status-failed truncate text-xs"
              data-testid={`workflow-not-invocable-${workflowKey(workflow)}`}
            >
              step type not supported in this version
            </span>
          {:else if workflow.description}
            <span class="text-muted truncate text-xs">{workflow.description}</span>
          {/if}
        </button>
        {#if builtin && oncopy}
          <!-- Save-to-my-library glyph (bookmark-plus), not a clipboard-copy icon:
               this writes the built-in into the user's own editable workflows.
               "Copy" stays in the tooltip as the plainer description. -->
          <Tooltip label="Copy to my workflows">
            {#snippet trigger(props)}
              <button
                {...props}
                type="button"
                class={cn(
                  ICON_BUTTON_CLASS,
                  "hover:bg-accent-soft hover:text-accent absolute top-1/2 right-1.5 -translate-y-1/2",
                )}
                data-testid={`workflow-copy-${workflowKey(workflow)}`}
                aria-label="Copy to my workflows"
                onclick={() => oncopy(workflow)}
              >
                <BookmarkPlus size={16} strokeWidth={2} aria-hidden="true" />
              </button>
            {/snippet}
          </Tooltip>
        {/if}
      </div>
    {/each}
    {#if filtered.length === 0}
      {#if loading && workflows.length === 0}
        <div class="text-muted px-2.5 py-2 text-sm select-none" data-testid="workflow-menu-loading">
          Loading workflows…
        </div>
      {:else}
        <div class="text-muted px-2.5 py-2 text-sm select-none" data-testid="workflow-menu-empty">
          {workflows.length === 0 ? "No workflows available" : "No matching workflows"}
        </div>
      {/if}
    {/if}
  </div>
  <input
    bind:this={searchEl}
    bind:value={query}
    onkeydown={onKeydown}
    type="text"
    autocorrect="off"
    autocapitalize="off"
    spellcheck="false"
    placeholder="Search workflows…"
    data-testid="workflow-menu-search"
    class="border-border bg-panel text-fg placeholder:text-muted focus-visible:ring-accent mt-1 w-full rounded-md border px-2.5 py-1.5 text-sm focus-visible:ring-2 focus-visible:outline-none"
  />
  {#if onopenfolder}
    <button
      type="button"
      class="text-muted hover:text-fg mt-1 w-full rounded-md px-2.5 py-1 text-left text-xs"
      data-testid="workflow-menu-open-folder"
      onclick={() => onopenfolder()}
    >
      Open local workflows folder…
    </button>
  {/if}
</div>
