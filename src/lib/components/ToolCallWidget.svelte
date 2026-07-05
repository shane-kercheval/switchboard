<script lang="ts">
  import type { ToolCall } from "$lib/state/index.svelte";
  import Disclosure from "$lib/components/ui/Disclosure.svelte";
  import Badge from "$lib/components/ui/Badge.svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import { formatToolInput, toolInputPreview } from "$lib/toolInput";
  import { cn } from "$lib/utils";

  let { tool }: { tool: ToolCall } = $props();

  const isRunning = $derived(tool.completed_at === undefined && tool.stopped_at === undefined);
  const hasOutput = $derived(tool.output !== undefined && tool.output !== "");
  const inputPreview = $derived(toolInputPreview(tool.input));
  const formattedInput = $derived(formatToolInput(tool.input));
  const hasInput = $derived(formattedInput !== undefined && formattedInput !== "");

  // Start collapsed; the header preview carries the common case, and avoiding
  // automatic expansion keeps concurrent/fast tool calls from moving the page.
  let userOpen = $state<boolean | null>(null);
  const open = $derived(userOpen ?? false);
  function toggle(): void {
    userOpen = !open;
  }

  function kindLabel(kind: ToolCall["kind"]): string {
    if (kind === "mcp") return "MCP";
    if (kind === "plugin") return "Plugin";
    return "Tool";
  }
</script>

<Disclosure {open} ontoggle={toggle} testid="turn-tool" data-tool-use-id={tool.tool_use_id}>
  {#snippet header()}
    {#if tool.kind === "builtin" || tool.kind === "other"}
      <span class="text-muted shrink-0 text-[10px] font-semibold tracking-wide uppercase"
        >{kindLabel(tool.kind)}</span
      >
    {:else}
      <Badge class="shrink-0">{kindLabel(tool.kind)}</Badge>
    {/if}
    <span class="text-muted max-w-48 min-w-0 truncate font-mono">{tool.name}</span>
    {#if inputPreview}
      <span class="text-muted/70 shrink-0" aria-hidden="true">·</span>
      <span class="text-muted min-w-0 flex-1 truncate font-mono" data-testid="tool-input-preview"
        >{inputPreview}</span
      >
    {/if}
    <!-- Status as an icon: muted spinner while running, red alert on error, a
         muted check on success. Success stays quiet (gray, not green) so the
         common case doesn't draw the eye; errors get the one strong color. -->
    {#if isRunning}
      <span class="ml-auto shrink-0" role="status" aria-label="running" data-testid="tool-running">
        <Spinner class="h-3.5 w-3.5" />
      </span>
    {:else if tool.stop_reason === "cancelled"}
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="1.5"
        stroke-linecap="round"
        stroke-linejoin="round"
        class="text-status-cancelled ml-auto h-4 w-4 shrink-0"
        role="img"
        aria-label="cancelled"
        data-testid="tool-cancelled"
      >
        <circle cx="12" cy="12" r="9" />
        <path d="M9 9l6 6" />
        <path d="m15 9-6 6" />
      </svg>
    {:else if tool.stop_reason === "failed"}
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="1.5"
        stroke-linecap="round"
        stroke-linejoin="round"
        class="text-status-failed ml-auto h-4 w-4 shrink-0"
        role="img"
        aria-label="failed"
        data-testid="tool-error"
      >
        <circle cx="12" cy="12" r="9" />
        <path d="M12 8v4.5" />
        <path d="M12 16h.01" />
      </svg>
    {:else if tool.is_error}
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="1.5"
        stroke-linecap="round"
        stroke-linejoin="round"
        class="text-status-failed ml-auto h-4 w-4 shrink-0"
        role="img"
        aria-label="failed"
        data-testid="tool-error"
      >
        <circle cx="12" cy="12" r="9" />
        <path d="M12 8v4.5" />
        <path d="M12 16h.01" />
      </svg>
    {:else}
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="1.5"
        stroke-linecap="round"
        stroke-linejoin="round"
        class="text-muted ml-auto h-4 w-4 shrink-0"
        role="img"
        aria-label="completed"
        data-testid="tool-done"
      >
        <circle cx="12" cy="12" r="9" />
        <path d="m8.5 12 2.5 2.5 4.5-5" />
      </svg>
    {/if}
  {/snippet}

  {#if hasInput || hasOutput}
    <div class="border-border/70 space-y-2 border-t px-2.5 py-2">
      {#if hasInput}
        <section class="space-y-1" aria-label="Tool input">
          <div class="text-muted text-[10px] font-semibold tracking-wide uppercase">Input</div>
          <pre
            class="text-muted bg-panel/60 max-h-44 overflow-y-auto rounded px-2 py-1.5 font-mono text-xs whitespace-pre-wrap"
            data-testid="tool-input">{formattedInput}</pre>
        </section>
      {/if}
      {#if hasOutput}
        <section class="space-y-1" aria-label="Tool output">
          <div class="text-muted text-[10px] font-semibold tracking-wide uppercase">Output</div>
          <pre
            class={cn(
              "max-h-44 overflow-y-auto font-mono text-xs whitespace-pre-wrap",
              tool.is_error ? "text-status-failed" : "text-muted",
            )}
            data-testid="tool-output">{tool.output}</pre>
        </section>
      {/if}
    </div>
  {/if}
</Disclosure>
