<script lang="ts">
  /// The context-breakdown panel: what is occupying one agent's context window,
  /// per category and per item.
  ///
  /// Opened from the context meter's chevron or the agent menu. The meter on
  /// the card answers "how full"; this answers "with what" — which is the
  /// question you actually act on, because the answer is usually a tool list
  /// nobody chose to load.
  ///
  /// **Nothing here is fetched.** The panel renders whatever report the agent
  /// already has, and the Refresh button dispatches a new one. That keeps the
  /// panel honest about two states the alternative would blur: an agent that has
  /// never been analyzed (empty state, not a spinner) and one measured a while
  /// ago (shown, with its age).
  import { SUPPLEMENTAL_TOOLTIP_DELAY } from "$lib/components/ui/tooltip";
  import ExpandCollapseIcon from "$lib/components/ui/ExpandCollapseIcon.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import Meter from "$lib/components/ui/Meter.svelte";
  import Dialog from "$lib/components/ui/Dialog.svelte";
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
    onRefresh: () => void;
  };

  let { open = $bindable(), onClose, agentName, report, at, request, onRefresh }: Props = $props();

  const view = $derived(breakdownView(report));
  /// One request at a time: while this is true the button is disabled, so a
  /// second click cannot orphan the first request's correlation.
  const inFlight = $derived(request?.phase === "queued" || request?.phase === "running");

  const buttonLabel = $derived.by(() => {
    if (request?.phase === "queued") return "Queued — runs after the current turn";
    if (request?.phase === "running") return "Analyzing…";
    return view === null ? "Analyze context" : "Refresh";
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
    {#if view === null}
      <p class="text-muted text-sm" data-testid="context-breakdown-empty">
        Nothing has measured this agent's context yet. Analyzing it asks the harness for a breakdown
        — it runs locally, costs nothing, and takes about a second.
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
            class="text-muted hover:text-fg flex w-full items-center gap-1.5 text-left"
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

    <div class="flex items-center gap-2 pt-1">
      <button
        type="button"
        class="border-border hover:bg-active rounded border px-2 py-1 text-xs disabled:opacity-50"
        disabled={inFlight}
        onclick={onRefresh}
        data-testid="context-breakdown-refresh"
      >
        {buttonLabel}
      </button>
      {#if view !== null}
        <button
          type="button"
          onclick={() => (rawOpen = !rawOpen)}
          class="text-muted hover:text-fg ml-auto flex items-center gap-1 text-[11px]"
          aria-expanded={rawOpen}
          data-testid="context-breakdown-raw-toggle"
        >
          Raw report
          <ExpandCollapseIcon expanded={rawOpen} size={11} strokeWidth={1.8} />
        </button>
      {/if}
    </div>

    {#if rawOpen && view !== null}
      <pre
        class="bg-active text-muted max-h-64 overflow-auto rounded p-2 text-[10px] whitespace-pre-wrap"
        data-testid="context-breakdown-raw">{view.raw}</pre>
    {/if}
  </div>
</Dialog>
