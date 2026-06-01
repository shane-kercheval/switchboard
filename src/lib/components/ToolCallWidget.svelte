<script lang="ts">
  import type { ToolCall } from "$lib/state/index.svelte";
  import Disclosure from "$lib/components/ui/Disclosure.svelte";
  import Badge from "$lib/components/ui/Badge.svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import { cn } from "$lib/utils";

  let { tool }: { tool: ToolCall } = $props();

  const isRunning = $derived(tool.completed_at === undefined);
  const hasOutput = $derived(tool.output !== undefined && tool.output !== "");

  // Open while running (so streaming output is visible) and collapsed once done.
  // A manual toggle (`userOpen`) takes over from then on, so completion won't
  // yank a panel the user opened shut.
  let userOpen = $state<boolean | null>(null);
  const open = $derived(userOpen ?? isRunning);
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
    <span class="text-muted min-w-0 truncate font-mono">{tool.name}</span>
    <!-- Status as an icon: muted spinner while running, red alert on error, a
         muted check on success. Success stays quiet (gray, not green) so the
         common case doesn't draw the eye; errors get the one strong color. -->
    {#if isRunning}
      <span class="ml-auto shrink-0" role="status" aria-label="running" data-testid="tool-running">
        <Spinner class="h-3.5 w-3.5" />
      </span>
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

  {#if hasOutput}
    <div class="border-border/70 border-t px-2.5 py-2">
      <pre
        class={cn(
          "max-h-44 overflow-y-auto font-mono text-xs whitespace-pre-wrap",
          tool.is_error ? "text-status-failed" : "text-muted",
        )}>{tool.output}</pre>
    </div>
  {/if}
</Disclosure>
