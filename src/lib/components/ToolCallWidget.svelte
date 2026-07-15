<script lang="ts">
  /// One tool call as a borderless row: facet icon · bold normalized verb ·
  /// muted provenance detail · chevron + status glyph. Collapsed it is chrome-
  /// free — a run of tool calls reads as a set held together by the icon
  /// column, not by boxes. Expanded content hangs under the row behind a thin
  /// left rule (the same idiom as fan-out response columns), directly on the
  /// reading surface — a wrapping fill made every open row a gray slab. Fills
  /// mark only true content blocks: output / raw JSON on `panel` (that token's
  /// documented job), file edits and writes in a bordered diff canvas.
  /// The row's detail line hides while open: the body shows the full,
  /// untruncated version, so keeping both duplicated every value.
  ///
  /// The body renders ONLY while open. This is load-bearing, not styling: the
  /// previous `<details>`-based widget rendered its body unconditionally and
  /// stringified every tool call's full raw input whether or not it was ever
  /// expanded — a 500 KB write built a 500 KB string and DOM node nobody asked
  /// for. Formatting raw JSON and output must stay gated behind `open`. The
  /// deliberate exception: successful/running Edit and Write facets and
  /// input-derived MCP mutation previews render inline — watching requested
  /// changes stream by is the point of the row. A failed/cancelled operation
  /// suppresses that attempted content and shows its status output instead;
  /// the Git view or remote service is authoritative for what actually changed.
  /// Facet content is capped and off-window rows aren't mounted at all
  /// (transcript render-windowing). Inline content is further capped to a
  /// preview length (both while streaming and once settled — flipping
  /// full→capped when a turn ends would be jarring); expanding the row un-caps
  /// it and reveals output + raw input. Collapsed edits get a short exact-diff
  /// attempt: ordinary and large-but-simple changes keep their preview, while
  /// a genuinely complex comparison is prepared asynchronously on expansion.
  /// File-facet detail is suppressed in both states because the inline per-file
  /// headers already carry the paths.
  import type { ToolCall } from "$lib/state/index.svelte";
  import type { FileDiff } from "$lib/types";
  import { onDestroy } from "svelte";
  import AsyncToolDiff from "$lib/components/AsyncToolDiff.svelte";
  import DiffView from "$lib/components/DiffView.svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import { languageForPath } from "$lib/diff";
  import {
    COLLAPSED_EDIT_DIFF_TIMEOUT_MS,
    createExpandedDiffCoordinator,
    synthesizeCollapsedEditDiffs,
    synthesizeMcpTextCreationDiff,
    synthesizeMcpTextEditDiff,
    synthesizeWriteDiff,
    truncateDiff,
  } from "$lib/toolDiff";
  import { formatToolInput, redactDisplay } from "$lib/toolInput";
  import {
    isGenericFacet,
    knownMcpMutation,
    toolDetail,
    toolIcon,
    toolRowState,
    toolVerb,
  } from "$lib/toolRow";
  import { cn } from "$lib/utils";
  import { CircleCheck, CircleDotDashed, Circle } from "@lucide/svelte";

  let { tool, turnSettled = true }: { tool: ToolCall; turnSettled?: boolean } = $props();

  // Start collapsed; the row itself carries the common case, and avoiding
  // automatic expansion keeps concurrent/fast tool calls from moving the page.
  let open = $state(false);
  let expandedDiffCoordinator = $state(createExpandedDiffCoordinator());

  const facet = $derived(tool.facet);
  const mutation = $derived(knownMcpMutation(facet));
  const rowState = $derived(toolRowState(tool));
  const verb = $derived(toolVerb(facet, tool.name));
  const deleteFacet = $derived(facet.facet_kind === "edit" && verb === "Delete");
  const detail = $derived(
    facet.facet_kind === "edit" || facet.facet_kind === "write" || facet.facet_kind === "read"
      ? undefined
      : toolDetail(facet, tool.input),
  );
  const FacetIcon = $derived(toolIcon(facet));
  const failed = $derived(rowState === "failed");
  const cancelled = $derived(rowState === "cancelled");
  const interrupted = $derived(failed || cancelled);
  const fileContentFacet = $derived(facet.facet_kind === "edit" || facet.facet_kind === "write");
  const inlineContentFacet = $derived(
    fileContentFacet || facet.facet_kind === "read" || mutation !== undefined,
  );
  const outputPreview = $derived(boundedOutputPreview(tool.output));
  // Collapsed rows use only the bounded preview result. Once the user expands
  // the row, scanning the retained full value is intentional: it determines
  // whether the complete output or the no-details fallback should render.
  const hasOutput = $derived(
    open ? tool.output !== undefined && /\S/.test(tool.output) : outputPreview.hasContent,
  );
  const statusPreview = $derived(
    failed
      ? outputPreview.hasContent
        ? outputPreview.text
        : "Tool failed without error details."
      : "Tool cancelled before completion.",
  );
  const collapsedFileDiffs = $derived.by(() =>
    facet.facet_kind === "edit"
      ? synthesizeCollapsedEditDiffs(facet.files, COLLAPSED_EDIT_DIFF_TIMEOUT_MS)
      : [],
  );
  const collapsedMcpEditDiff = $derived.by(() =>
    mutation?.mutation_kind === "text_edit"
      ? synthesizeMcpTextEditDiff(
          mutation.before,
          mutation.after,
          mutation.content_truncated,
          COLLAPSED_EDIT_DIFF_TIMEOUT_MS,
        )
      : undefined,
  );

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

  /// Inline file diffs preview at most this many lines (per file) until the row
  /// is expanded — a large change shouldn't dominate the transcript, but you
  /// can still watch the first chunk stream in and expand for the rest.
  const INLINE_DIFF_PREVIEW_LINES = 25;

  /// Failed output is visible while collapsed, unlike ordinary tool output.
  /// Bound both the source inspected and the text mounted in the DOM so the
  /// preview does not defeat the row's lazy-rendering contract.
  const OUTPUT_PREVIEW_SOURCE_CAP = 2_048;
  const OUTPUT_PREVIEW_TEXT_CAP = 240;

  /// Bookmark fields are inline too, so their collapsed representation must
  /// not mount a complete backend-capped value. Expansion intentionally
  /// reveals the full captured field, matching diff previews.
  const RECORD_FIELD_PREVIEW_SOURCE_CAP = 2_048;
  const RECORD_FIELD_PREVIEW_TEXT_CAP = 500;

  // Paths below the compact header are content, not one-line metadata. Allow
  // breaks anywhere because absolute paths often contain no natural spaces.
  const FILE_PATH_CLASS = "min-w-0 whitespace-normal [overflow-wrap:anywhere]";

  function boundedOutputPreview(value: string | undefined): {
    text: string;
    hasContent: boolean;
  } {
    if (value === undefined || value === "") return { text: "", hasContent: false };
    const source = value.slice(0, OUTPUT_PREVIEW_SOURCE_CAP);
    const normalized = source.replace(/\s+/g, " ").trim();
    if (normalized === "") return { text: "", hasContent: false };
    const text = normalized.slice(0, OUTPUT_PREVIEW_TEXT_CAP).trimEnd();
    const truncated =
      normalized.length > OUTPUT_PREVIEW_TEXT_CAP || value.length > OUTPUT_PREVIEW_SOURCE_CAP;
    return { text: truncated ? `${text}…` : text, hasContent: true };
  }

  function cappedRawInput(input: unknown): { text: string; truncated: boolean } {
    const formatted = formatToolInput(input) ?? "";
    if (formatted.length <= RAW_DISPLAY_CAP) return { text: formatted, truncated: false };
    return { text: formatted.slice(0, RAW_DISPLAY_CAP), truncated: true };
  }

  function mutationTarget(): string {
    if (mutation === undefined) return "";
    return `${mutation.target}${mutation.target_truncated ? "…" : ""}`;
  }

  function boundedRecordFieldPreview(value: string): { text: string; truncated: boolean } {
    const source = value.slice(0, RECORD_FIELD_PREVIEW_SOURCE_CAP);
    const normalized = source.replace(/\s+/g, " ").trim();
    const text = normalized.slice(0, RECORD_FIELD_PREVIEW_TEXT_CAP).trimEnd();
    const truncated =
      normalized.length > RECORD_FIELD_PREVIEW_TEXT_CAP ||
      value.length > RECORD_FIELD_PREVIEW_SOURCE_CAP;
    return { text: truncated ? `${text}…` : text, truncated };
  }

  function todoStatusIcon(status: string): typeof Circle {
    if (status === "completed") return CircleCheck;
    if (status === "in_progress") return CircleDotDashed;
    return Circle;
  }

  function setOpen(next: boolean): void {
    if (next === open) return;
    if (next) {
      expandedDiffCoordinator = createExpandedDiffCoordinator();
    } else {
      expandedDiffCoordinator.cancel();
    }
    open = next;
  }

  onDestroy(() => expandedDiffCoordinator.cancel());
</script>

{#snippet inlineDiff(fullDiff: FileDiff, language: string, expandTestid: string)}
  {@const preview = truncateDiff(fullDiff, INLINE_DIFF_PREVIEW_LINES)}
  {@const capped = !open && preview.hiddenLines > 0}
  <!-- Cap the inline diff in both live and settled states — flipping full to
       capped when a turn ends would be jarring. Expansion shows all captured
       content at full height. Always unified: side-by-side needs more width
       than a transcript row can guarantee. -->
  <div class="border-border/60 overflow-hidden rounded border">
    <div
      class={cn(
        capped && "[mask-image:linear-gradient(to_bottom,black_calc(100%_-_3rem),transparent)]",
      )}
    >
      <DiffView diff={capped ? preview.diff : fullDiff} style="unified" {language} compact />
    </div>
  </div>
  {#if capped}
    <button
      type="button"
      class="text-muted hover:text-fg text-[11px] transition-colors"
      data-testid={expandTestid}
      onclick={() => setOpen(true)}
    >
      Show {preview.hiddenLines} more {preview.hiddenLines === 1 ? "line" : "lines"}
    </button>
  {/if}
{/snippet}

{#snippet deferredEditPreview(testid: string)}
  <button
    type="button"
    class="text-muted hover:text-fg text-[11px] transition-colors"
    data-testid={testid}
    onclick={() => setOpen(true)}
  >
    Complex edit — expand to prepare diff
  </button>
{/snippet}

{#snippet inlineAddedContent(
  rendered: { diff: FileDiff; hiddenLines: number },
  language: string,
  contentTestid: string,
  expandTestid: string,
)}
  {@const capped = rendered.hiddenLines > 0}
  <div class="border-border/60 overflow-hidden rounded border" data-testid={contentTestid}>
    <div
      class={cn(
        capped && "[mask-image:linear-gradient(to_bottom,black_calc(100%_-_3rem),transparent)]",
      )}
    >
      <DiffView diff={rendered.diff} style="unified" {language} compact />
    </div>
  </div>
  {#if capped}
    <button
      type="button"
      class="text-muted hover:text-fg text-[11px] transition-colors"
      data-testid={expandTestid}
      onclick={() => setOpen(true)}
    >
      Show {rendered.hiddenLines} more {rendered.hiddenLines === 1 ? "line" : "lines"}
    </button>
  {/if}
{/snippet}

<div class="text-xs" data-testid="turn-tool" data-tool-use-id={tool.tool_use_id}>
  <button
    type="button"
    class="hover:bg-hover flex min-h-7 w-full items-center gap-2 rounded-md px-1.5 py-1 text-left"
    aria-expanded={open}
    data-testid="tool-row"
    onclick={() => setOpen(!open)}
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

  {#if open || inlineContentFacet || interrupted}
    <div
      class="border-border/70 mt-1 ml-[13px] space-y-2 border-l py-0.5 pl-4"
      data-testid="tool-body"
    >
      {#if open && facet.facet_kind === "shell"}
        <section class="space-y-1" aria-label="Command">
          <pre
            class="text-fg max-h-44 overflow-y-auto font-mono whitespace-pre-wrap"
            data-testid="tool-command">{redactDisplay(facet.command)}</pre>
          {#if facet.cwd}
            <div class="text-muted font-mono text-[11px]">in {facet.cwd}</div>
          {/if}
        </section>
      {:else if !interrupted && facet.facet_kind === "edit"}
        {#each facet.files as file, index (file.path)}
          <section class="space-y-1" aria-label="File edit" data-testid="tool-edit-file">
            <div class="text-muted flex items-start gap-2 font-mono text-[11px]">
              <span class={cn(FILE_PATH_CLASS, "flex-1")} data-testid="tool-edit-path">
                {file.path}
              </span>
              {#if verb === "Edit" && facet.files.length > 1 && file.change !== "modified"}
                <span class="shrink-0">({file.change})</span>
              {/if}
            </div>
            {#if !deleteFacet || open}
              {@const diff = collapsedFileDiffs[index]}
              {#if file.edits.length === 0}
                <!-- A live Codex edit announces paths without content; the facet
                     is upgraded from the session file at turn end. Empty edits on
                     a settled turn mean the content never became available. -->
                <p class="text-muted" data-testid="tool-edit-pending">
                  {turnSettled
                    ? "Diff content unavailable for this edit."
                    : "Diff will appear when the turn completes."}
                </p>
              {:else if diff !== undefined}
                {@render inlineDiff(diff, languageForPath(file.path), "tool-edit-expand")}
              {:else if open}
                <AsyncToolDiff
                  sourceKind="file"
                  {file}
                  coordinator={expandedDiffCoordinator}
                  language={languageForPath(file.path)}
                  testid="tool-edit-async"
                />
              {:else}
                {@render deferredEditPreview("tool-edit-deferred")}
              {/if}
            {/if}
          </section>
        {/each}
      {:else if !interrupted && facet.facet_kind === "write"}
        {@const rendered = synthesizeWriteDiff(
          facet.path,
          facet.content,
          facet.truncated,
          open ? undefined : INLINE_DIFF_PREVIEW_LINES,
        )}
        <section class="space-y-1" aria-label="File write" data-testid="tool-write-file">
          <div
            class={cn("text-muted font-mono text-[11px]", FILE_PATH_CLASS)}
            data-testid="tool-write-path"
          >
            {facet.path}
          </div>
          {@render inlineAddedContent(
            rendered,
            languageForPath(facet.path),
            "tool-write-content",
            "tool-write-expand",
          )}
        </section>
      {:else if !interrupted && mutation?.mutation_kind === "text_edit"}
        <section class="space-y-1" aria-label="Requested content edit" data-testid="tool-mcp-edit">
          {#if open}
            <div class="text-muted truncate font-mono text-[11px]" data-testid="tool-mcp-target">
              {mutationTarget()}
            </div>
          {/if}
          {#if collapsedMcpEditDiff !== undefined}
            {@render inlineDiff(collapsedMcpEditDiff, "markdown", "tool-mcp-edit-expand")}
          {:else if open}
            <AsyncToolDiff
              sourceKind="mcp"
              before={mutation.before}
              after={mutation.after}
              contentTruncated={mutation.content_truncated}
              coordinator={expandedDiffCoordinator}
              language="markdown"
              testid="tool-mcp-edit-async"
            />
          {:else}
            {@render deferredEditPreview("tool-mcp-edit-deferred")}
          {/if}
        </section>
      {:else if !interrupted && mutation?.mutation_kind === "text_creation"}
        <section
          class="space-y-1"
          aria-label="Requested content creation"
          data-testid="tool-mcp-creation"
        >
          {#if open}
            <div class="text-muted truncate font-mono text-[11px]" data-testid="tool-mcp-target">
              {mutationTarget()}
            </div>
          {/if}
          {#if mutation.content === ""}
            <p class="text-muted" data-testid="tool-mcp-empty-creation">
              Created without body content.
            </p>
          {:else}
            {@const rendered = synthesizeMcpTextCreationDiff(
              mutation.content,
              mutation.content_truncated,
              open ? undefined : INLINE_DIFF_PREVIEW_LINES,
            )}
            {@render inlineAddedContent(
              rendered,
              "markdown",
              "tool-mcp-creation-content",
              "tool-mcp-creation-expand",
            )}
          {/if}
        </section>
      {:else if !interrupted && mutation?.mutation_kind === "record_creation"}
        <section
          class="space-y-1"
          aria-label="Requested record creation"
          data-testid="tool-mcp-record-creation"
        >
          {#if open}
            <div class="text-muted truncate font-mono text-[11px]" data-testid="tool-mcp-target">
              {mutationTarget()}
            </div>
          {/if}
          <dl class="border-border/60 divide-border/40 divide-y overflow-hidden rounded border">
            {#each mutation.fields as field, index (`${field.label}:${index}`)}
              {@const preview = boundedRecordFieldPreview(field.value)}
              <div class="bg-diff-added-soft flex min-w-0 gap-3 px-2 py-1">
                <dt class="text-muted w-20 shrink-0 font-medium">{field.label}</dt>
                <dd
                  class={cn(
                    "text-fg min-w-0 flex-1 break-words",
                    open ? "whitespace-pre-wrap" : "truncate",
                  )}
                  data-testid="tool-mcp-record-field"
                >
                  {open ? field.value : preview.text}
                </dd>
              </div>
            {/each}
          </dl>
          {#if mutation.fields_truncated}
            <p class="text-muted text-[11px]" data-testid="tool-mcp-record-truncated">
              Bookmark details truncated.
            </p>
          {/if}
        </section>
      {:else if !interrupted && facet.facet_kind === "read"}
        <div
          class={cn("text-muted font-mono text-[11px]", FILE_PATH_CLASS)}
          data-testid="tool-read-path"
        >
          {facet.path}
        </div>
      {:else if open && facet.facet_kind === "search"}
        <div class="text-muted font-mono text-[11px]" data-testid="tool-search-detail">
          <span class="text-fg">{facet.pattern}</span>
          {#if facet.path}
            <span> in {facet.path}</span>
          {/if}
        </div>
      {:else if open && facet.facet_kind === "todo"}
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

      {#if interrupted && (!open || !hasOutput)}
        <p
          class={cn(
            "truncate font-mono text-[11px]",
            failed ? "text-status-failed" : "text-status-cancelled",
          )}
          data-testid="tool-status-preview"
        >
          {statusPreview}
        </p>
      {/if}

      {#if open && hasOutput}
        <section class="space-y-1" aria-label={failed ? "Tool error" : "Tool output"}>
          <div class="text-muted text-[10px] font-semibold tracking-wide uppercase">
            {failed ? "Error" : "Output"}
          </div>
          <pre
            class={cn(
              "bg-panel max-h-44 overflow-y-auto rounded px-2 py-1.5 font-mono whitespace-pre-wrap",
              failed ? "text-status-failed" : "text-muted",
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
