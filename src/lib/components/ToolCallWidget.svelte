<script lang="ts">
  /// One tool call as a borderless row: facet icon · bold normalized verb ·
  /// muted provenance detail · chevron + status glyph. Collapsed it is chrome-
  /// free — a run of tool calls reads as a set held together by the icon
  /// column, not by boxes. Expanded content hangs under the row behind a thin
  /// left rule (the same idiom as fan-out response columns), directly on the
  /// reading surface — a wrapping fill made every open row a gray slab. Fills
  /// mark only true content blocks: output / raw JSON / written content on
  /// `panel` (that token's documented job), the diff in a bordered canvas.
  /// The row's detail line hides while open: the body shows the full,
  /// untruncated version, so keeping both duplicated every value.
  ///
  /// The body renders ONLY while open. This is load-bearing, not styling: the
  /// previous `<details>`-based widget rendered its body unconditionally and
  /// stringified every tool call's full raw input whether or not it was ever
  /// expanded — a 500 KB write built a 500 KB string and DOM node nobody asked
  /// for. Formatting (raw JSON, output, Write content) must stay gated behind
  /// `open`. The one deliberate exception: Edit facets render their diff
  /// inline without expansion — watching the changes stream by is the point
  /// of the row — which is safe to do eagerly because edit content is capped
  /// at the facet level and off-window rows aren't mounted at all (transcript
  /// render-windowing). Expansion on an edit row reveals only output and raw
  /// input. Its detail line is suppressed in both states: the inline per-file
  /// headers already carry the paths.
  import type { ToolCall } from "$lib/state/index.svelte";
  import DiffView from "$lib/components/DiffView.svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import { languageForPath } from "$lib/diff";
  import { synthesizeEditDiff } from "$lib/toolDiff";
  import { formatToolInput, redactDisplay } from "$lib/toolInput";
  import { isGenericFacet, toolDetail, toolIcon, toolRowState, toolVerb } from "$lib/toolRow";
  import { cn } from "$lib/utils";
  import { CircleCheck, CircleDotDashed, Circle } from "@lucide/svelte";

  let { tool, turnSettled = true }: { tool: ToolCall; turnSettled?: boolean } = $props();

  const facet = $derived(tool.facet);
  const rowState = $derived(toolRowState(tool));
  const verb = $derived(toolVerb(facet, tool.name));
  const detail = $derived(facet.facet_kind === "edit" ? undefined : toolDetail(facet, tool.input));
  const FacetIcon = $derived(toolIcon(facet));
  const hasOutput = $derived(tool.output !== undefined && tool.output !== "");

  // Start collapsed; the row itself carries the common case, and avoiding
  // automatic expansion keeps concurrent/fast tool calls from moving the page.
  let open = $state(false);
  // Raw provenance for specialized facets sits behind its own reveal — the
  // facet body already shows the same information in readable form, so the
  // JSON envelope is one click further. Generic facets (`other` and any
  // discriminant this build doesn't know) have no body, so their raw input
  // shows directly on expand — an unknown kind must degrade to exactly the
  // generic treatment, never a lesser one.
  let rawOpen = $state(false);

  const generic = $derived(isGenericFacet(facet));
  const showRaw = $derived(open && (generic || rawOpen));

  /// Cap what the *renderer* does with the raw input: the wire payload is
  /// uncapped by design (the raw input is the provenance escape hatch), so the
  /// display is where a multi-megabyte input must stop.
  const RAW_DISPLAY_CAP = 50_000;

  function cappedRawInput(input: unknown): { text: string; truncated: boolean } {
    const formatted = formatToolInput(input) ?? "";
    if (formatted.length <= RAW_DISPLAY_CAP) return { text: formatted, truncated: false };
    return { text: formatted.slice(0, RAW_DISPLAY_CAP), truncated: true };
  }

  function todoStatusIcon(status: string): typeof Circle {
    if (status === "completed") return CircleCheck;
    if (status === "in_progress") return CircleDotDashed;
    return Circle;
  }
</script>

<div class="text-xs" data-testid="turn-tool" data-tool-use-id={tool.tool_use_id}>
  <button
    type="button"
    class="hover:bg-hover flex min-h-7 w-full items-center gap-2 rounded-md px-1.5 py-1 text-left"
    aria-expanded={open}
    data-testid="tool-row"
    onclick={() => (open = !open)}
  >
    <FacetIcon class="text-muted h-3.5 w-3.5 shrink-0" aria-hidden="true" />
    <span class="text-fg shrink-0 font-medium" data-testid="tool-verb">{verb}</span>
    {#if detail !== undefined && !open}
      <span class="text-muted min-w-0 flex-1 truncate font-mono" data-testid="tool-detail"
        >{detail}</span
      >
    {:else}
      <span class="flex-1"></span>
    {/if}
    <span
      class={cn(
        "text-muted flex h-4 w-4 shrink-0 items-center justify-center transition-transform",
        open && "rotate-90",
      )}
      aria-hidden="true">›</span
    >
    <!-- Status is the only color on the row. The quiet success check earns its
         place because with the chrome gone it is the row's sole completion
         signal; errors get the one strong color. -->
    {#if rowState === "running"}
      <span class="shrink-0" role="status" aria-label="running" data-testid="tool-running">
        <Spinner class="h-3.5 w-3.5" />
      </span>
    {:else if rowState === "cancelled"}
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="1.5"
        stroke-linecap="round"
        stroke-linejoin="round"
        class="text-status-cancelled h-4 w-4 shrink-0"
        role="img"
        aria-label="cancelled"
        data-testid="tool-cancelled"
      >
        <circle cx="12" cy="12" r="9" />
        <path d="M9 9l6 6" />
        <path d="m15 9-6 6" />
      </svg>
    {:else if rowState === "failed"}
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="1.5"
        stroke-linecap="round"
        stroke-linejoin="round"
        class="text-status-failed h-4 w-4 shrink-0"
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
        class="text-muted h-4 w-4 shrink-0"
        role="img"
        aria-label="completed"
        data-testid="tool-done"
      >
        <circle cx="12" cy="12" r="9" />
        <path d="m8.5 12 2.5 2.5 4.5-5" />
      </svg>
    {/if}
  </button>

  {#if open || facet.facet_kind === "edit"}
    <div
      class="border-border/70 mt-1 ml-[13px] space-y-2 border-l py-0.5 pl-4"
      data-testid="tool-body"
    >
      {#if facet.facet_kind === "shell"}
        <section class="space-y-1" aria-label="Command">
          <pre
            class="text-fg max-h-44 overflow-y-auto font-mono whitespace-pre-wrap"
            data-testid="tool-command">{redactDisplay(facet.command)}</pre>
          {#if facet.cwd}
            <div class="text-muted font-mono text-[11px]">in {facet.cwd}</div>
          {/if}
        </section>
      {:else if facet.facet_kind === "edit"}
        {#each facet.files as file (file.path)}
          <section class="space-y-1" aria-label="File edit" data-testid="tool-edit-file">
            <div class="text-muted flex items-center gap-2 font-mono text-[11px]">
              <span class="min-w-0 truncate" title={file.path}>{file.path}</span>
              {#if facet.files.length > 1 && file.change !== "modified"}
                <span class="shrink-0">({file.change})</span>
              {/if}
            </div>
            {#if file.edits.length === 0}
              <!-- A live Codex edit announces paths without content; the facet
                   is upgraded from the session file at turn end. Empty edits on
                   a settled turn mean the content never became available. -->
              <p class="text-muted" data-testid="tool-edit-pending">
                {turnSettled
                  ? "Diff content unavailable for this edit."
                  : "Diff will appear when the turn completes."}
              </p>
            {:else}
              <div class="border-border/60 max-h-80 overflow-y-auto rounded border">
                <!-- Always unified: side-by-side needs a 48rem minimum width,
                     which a transcript row can't guarantee (pane splits and
                     fan-out columns shrink it). The Git view still honors the
                     user's diff-style preference; this diff is snippet-scoped. -->
                <DiffView
                  diff={synthesizeEditDiff(file)}
                  style="unified"
                  language={languageForPath(file.path)}
                  compact
                />
              </div>
            {/if}
          </section>
        {/each}
      {:else if facet.facet_kind === "write"}
        <section class="space-y-1" aria-label="File write">
          <div class="text-muted truncate font-mono text-[11px]" title={facet.path}>
            {facet.path}
          </div>
          <pre
            class="text-muted bg-panel max-h-44 overflow-y-auto rounded px-2 py-1.5 font-mono whitespace-pre-wrap"
            data-testid="tool-write-content">{facet.content}</pre>
          {#if facet.truncated}
            <p class="text-muted text-[11px]" data-testid="tool-write-truncated">
              Content truncated — the full write is larger than shown.
            </p>
          {/if}
        </section>
      {:else if facet.facet_kind === "read"}
        <div class="text-muted font-mono text-[11px]" data-testid="tool-read-path">
          {facet.path}
        </div>
      {:else if facet.facet_kind === "search"}
        <div class="text-muted font-mono text-[11px]" data-testid="tool-search-detail">
          <span class="text-fg">{facet.pattern}</span>
          {#if facet.path}
            <span> in {facet.path}</span>
          {/if}
        </div>
      {:else if facet.facet_kind === "todo"}
        <ul class="space-y-0.5" data-testid="tool-todo">
          {#each facet.items as item, i (i)}
            {@const StatusIcon = todoStatusIcon(item.status)}
            <li class="flex items-start gap-1.5">
              <StatusIcon class="text-muted mt-0.5 h-3.5 w-3.5 shrink-0" aria-hidden="true" />
              <span class={cn("min-w-0", item.status === "completed" ? "text-muted" : "text-fg")}
                >{item.content}</span
              >
            </li>
          {/each}
        </ul>
      {/if}

      {#if open && hasOutput}
        <section class="space-y-1" aria-label="Tool output">
          <div class="text-muted text-[10px] font-semibold tracking-wide uppercase">Output</div>
          <pre
            class={cn(
              "bg-panel max-h-44 overflow-y-auto rounded px-2 py-1.5 font-mono whitespace-pre-wrap",
              tool.is_error ? "text-status-failed" : "text-muted",
            )}
            data-testid="tool-output">{tool.output}</pre>
        </section>
      {/if}

      {#if open && !generic}
        <button
          type="button"
          class="text-muted hover:text-fg text-[11px] transition-colors"
          data-testid="tool-raw-toggle"
          onclick={() => (rawOpen = !rawOpen)}
        >
          {rawOpen ? "Hide raw input" : "Show raw input"}
        </button>
      {/if}
      {#if showRaw}
        {@const raw = cappedRawInput(tool.input)}
        <section class="space-y-1" aria-label="Tool input">
          <div class="text-muted text-[10px] font-semibold tracking-wide uppercase">
            Input · <span class="font-mono normal-case" data-testid="tool-raw-name"
              >{tool.name}</span
            >
          </div>
          <pre
            class="text-muted bg-panel max-h-44 overflow-y-auto rounded px-2 py-1.5 font-mono whitespace-pre-wrap"
            data-testid="tool-input">{raw.text}</pre>
          {#if raw.truncated}
            <p class="text-muted text-[11px]" data-testid="tool-input-truncated">
              Raw input truncated for display.
            </p>
          {/if}
        </section>
      {/if}
    </div>
  {/if}
</div>
