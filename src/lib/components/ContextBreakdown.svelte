<script lang="ts">
  /// The context-breakdown panel: what is occupying one agent's context window,
  /// per category and per item.
  ///
  /// Opened from the context meter's icon or the agent menu. The meter on
  /// the card answers "how full"; this answers "with what" — which is the
  /// question you actually act on, because the answer is usually a tool list
  /// nobody chose to load.
  ///
  /// **Opening is the refresh.** Every entry point dispatches a fresh report,
  /// so the panel carries no Refresh or Analyze button: the spinner says a
  /// report is on its way, and re-opening is how you ask for another one —
  /// including after a failure. A button would only name an action the open
  /// already performed.
  ///
  /// **While a report is in flight the panel shows only the spinner**, even
  /// when the agent has a previous breakdown. Rendering the old numbers first
  /// and swapping them a few seconds later reads as the panel changing its
  /// mind; one loading state that resolves once is calmer than a correct value
  /// that arrives twice. A *settled* failure keeps the previous report, which
  /// is a different case — nothing further is coming to replace it (see
  /// `dispatchContextReport`).
  import { SUPPLEMENTAL_TOOLTIP_DELAY } from "$lib/components/ui/tooltip";
  import ExpandCollapseIcon from "$lib/components/ui/ExpandCollapseIcon.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import Meter from "$lib/components/ui/Meter.svelte";
  import Dialog from "$lib/components/ui/Dialog.svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import { breakdownView } from "$lib/contextBreakdown";
  import { formatTokens } from "$lib/utils";
  import type { ContextReportRequest } from "$lib/state/types";
  import type { ContextReport } from "$lib/types";

  type Props = {
    open: boolean;
    onClose: () => void;
    agentName: string;
    report: ContextReport | undefined;
    /// When the report was measured (ISO-8601).
    ///
    /// **Always rendered when present, however fresh.** A breakdown measures one
    /// instant and every turn after it grows the context it describes, so an
    /// unqualified one is a number the reader cannot place — and "it looks
    /// recent" is not something the panel can know.
    at?: string | null;
    request?: ContextReportRequest;
  };

  let { open = $bindable(), onClose, agentName, report, at, request }: Props = $props();

  const view = $derived(breakdownView(report));
  const inFlight = $derived(request?.phase === "queued" || request?.phase === "running");

  const loadingLabel = $derived.by(() => {
    const action = view === null ? "analysis" : "refresh";
    return request?.phase === "queued"
      ? `Context ${action} queued…`
      : `${action === "analysis" ? "Analyzing" : "Refreshing"} context…`;
  });

  /// The one place a settled request is described. `done` says nothing: the
  /// numbers above it are the message.
  const requestNote = $derived.by(() => {
    if (request === undefined || request.phase === "done") return null;
    if (request.phase === "failed") {
      return `Couldn't analyze the context: ${request.error ?? "the harness reported no reason"}`;
    }
    if (request.phase === "cancelled") return "The analysis was cancelled.";
    return null;
  });

  let openSections = $state<Record<string, boolean>>({});
  let rawOpen = $state(false);

  /// Absolute, not relative. A relative string is computed once at render with
  /// no timer behind it, so a panel left open would sit at "12 minutes ago"
  /// indefinitely — stale text about staleness. Matches the agent card's
  /// environment row.
  function formatMeasuredAt(iso: string): string {
    return new Date(iso).toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "numeric",
      minute: "2-digit",
    });
  }

  const SECTION_LABEL = "text-muted text-[10px] tracking-wide uppercase";
</script>

<Dialog bind:open {onClose} title="Context breakdown · {agentName}" contentClass="max-w-xl">
  <div class="space-y-3" data-testid="context-breakdown">
    {#if inFlight}
      <div
        class="text-muted flex min-h-24 items-center justify-center gap-2 text-sm"
        role="status"
        aria-live="polite"
        data-testid="context-breakdown-loading"
      >
        <Spinner class="h-4 w-4" />
        <span>{loadingLabel}</span>
      </div>
    {:else if view === null}
      <p class="text-muted text-sm" data-testid="context-breakdown-empty">
        No context breakdown is available.
      </p>
    {:else}
      {#if view.usage !== null}
        <div data-testid="context-breakdown-usage">
          <Meter
            label={view.model ?? "Context used"}
            value={view.usage.fraction}
            detail="{formatTokens(view.usage.usedTokens)} / {formatTokens(view.usage.windowTokens)}"
          />
        </div>
      {/if}
      {#if at != null}
        <p class="text-muted text-[11px] italic" data-testid="context-breakdown-as-of">
          as of {formatMeasuredAt(at)}
        </p>
      {/if}

      {#if view.unparsed}
        <p class="text-muted text-sm" data-testid="context-breakdown-unparsed">
          Switchboard couldn't read this breakdown — the harness may have changed how it reports
          one. The report itself is below, exactly as the harness printed it.
        </p>
      {/if}

      {#if view.categories.length > 0}
        <div class="space-y-1.5" data-testid="context-breakdown-categories">
          <!-- Index-keyed, like every other harness-supplied list on the card:
               these are replaced wholesale, hold no per-row state, and the
               harness guarantees no uniqueness a name key would demand. -->
          {#each view.categories as category, i (i)}
            {#if category.fraction === null}
              <div class="text-muted flex items-baseline gap-2 text-[11px]">
                <span class="min-w-0 truncate">{category.name}</span>
                <span class="ml-auto shrink-0 tabular-nums">{category.detail}</span>
              </div>
            {:else}
              <Meter
                label={category.name}
                value={category.fraction}
                detail={category.detail}
                testid="context-breakdown-category"
              />
            {/if}
          {/each}
        </div>
      {/if}

      {#each view.sections as section, si (si)}
        {@const sectionOpen = openSections[section.key] ?? false}
        <div data-testid="context-breakdown-section-{section.key}">
          <button
            type="button"
            onclick={() => (openSections[section.key] = !sectionOpen)}
            class="text-muted hover:text-fg hover:bg-hover -mx-1 flex w-full cursor-pointer items-center gap-1.5 rounded px-1 py-0.5 text-left transition-colors"
            aria-expanded={sectionOpen}
            data-testid="context-breakdown-toggle-{section.key}"
          >
            <span class={SECTION_LABEL}>{section.label}</span>
            <span class="text-muted ml-auto shrink-0 text-[11px] tabular-nums">
              {formatTokens(section.tokens)}
            </span>
            <ExpandCollapseIcon expanded={sectionOpen} size={11} strokeWidth={1.8} />
          </button>
          {#if sectionOpen}
            <div class="mt-1 space-y-1" data-testid="context-breakdown-rows-{section.key}">
              {#each section.groups as group, gi (gi)}
                {#if group.label !== null}
                  <div class="text-muted flex items-baseline gap-2 pt-1 text-[11px] opacity-70">
                    <span class="min-w-0 truncate">{group.label}</span>
                    <span class="ml-auto shrink-0 tabular-nums">{formatTokens(group.tokens)}</span>
                  </div>
                {/if}
                {#each group.rows as row, ri (ri)}
                  <div class="text-muted flex items-baseline gap-2 text-[11px]">
                    {#if row.title === null}
                      <span class="min-w-0 truncate">{row.name}</span>
                    {:else}
                      <!-- The app's tooltip, never the browser's native `title`
                           — same full-path-on-hover case the agent card's
                           environment row handles, and `focusable={false}`
                           because it only expands text already in the DOM. -->
                      <Tooltip
                        label={row.title}
                        delayDuration={SUPPLEMENTAL_TOOLTIP_DELAY}
                        focusable={false}
                      >
                        {#snippet trigger(props)}
                          <span {...props} class="min-w-0 cursor-default truncate">{row.name}</span>
                        {/snippet}
                      </Tooltip>
                    {/if}
                    {#if row.detail !== null}
                      <span class="shrink-0 opacity-70">{row.detail}</span>
                    {/if}
                    <span class="ml-auto shrink-0 tabular-nums">{row.tokens}</span>
                  </div>
                {/each}
              {/each}
            </div>
          {/if}
        </div>
      {/each}
    {/if}

    {#if requestNote !== null}
      <p class="text-status-failed text-[11px]" data-testid="context-breakdown-request-note">
        {requestNote}
      </p>
    {/if}

    {#if !inFlight && view !== null}
      <div class="flex items-center justify-end pt-1">
        <button
          type="button"
          onclick={() => (rawOpen = !rawOpen)}
          class="text-muted hover:text-fg hover:bg-hover -mr-1 flex cursor-pointer items-center gap-1 rounded px-1 py-0.5 text-[11px] transition-colors"
          aria-expanded={rawOpen}
          data-testid="context-breakdown-raw-toggle"
        >
          Raw report
          <ExpandCollapseIcon expanded={rawOpen} size={11} strokeWidth={1.8} />
        </button>
      </div>
    {/if}

    {#if !inFlight && rawOpen && view !== null}
      <pre
        class="bg-active text-muted max-h-64 overflow-auto rounded p-2 text-[10px] whitespace-pre-wrap"
        data-testid="context-breakdown-raw">{view.raw}</pre>
    {/if}
  </div>
</Dialog>
