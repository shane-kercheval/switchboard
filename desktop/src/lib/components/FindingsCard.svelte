<script lang="ts">
  /// A code review the agent delivered as tool input (Claude Code's
  /// `ReportFindings`), shown as part of its answer rather than as a tool row:
  /// the model was told the host renders it, so this is often the only place
  /// the review appears. Each finding leads with its short claim, followed by
  /// status and location metadata. A row expands to the full
  /// summary and failure scenario. Copy uses the Rust-built markdown so it
  /// matches what forward sends.
  ///
  /// `expandable={false}` is for collapsed response previews: they sit inside a
  /// fixed-height clip, so growing a row there would only add hidden content.
  /// Expanding the response gives the full card.
  import type { FindingsFacet } from "$lib/types";
  import Badge from "$lib/components/ui/Badge.svelte";
  import CopyButton from "$lib/components/ui/CopyButton.svelte";
  import ExpandCollapseIcon from "$lib/components/ui/ExpandCollapseIcon.svelte";
  import Markdown from "$lib/components/ui/Markdown.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { ICON_BUTTON_CLASS } from "$lib/components/ui/iconButton";
  import {
    findingBadge,
    findingLocation,
    findingShortLocation,
    findingsHeader,
    type FindingBadge,
  } from "$lib/findings";
  import { cn } from "$lib/utils";
  import { ClipboardCheck } from "@lucide/svelte";
  import { SvelteSet } from "svelte/reactivity";

  let { facet, expandable = true }: { facet: FindingsFacet; expandable?: boolean } = $props();

  const openRows = new SvelteSet<number>();
  const header = $derived(findingsHeader(facet));
  const allExpanded = $derived(
    facet.findings.length > 0 && facet.findings.every((_finding, index) => openRows.has(index)),
  );
  const toggleAllLabel = $derived(allExpanded ? "Collapse all findings" : "Expand all findings");

  function toggle(index: number): void {
    if (openRows.has(index)) openRows.delete(index);
    else openRows.add(index);
  }

  function toggleAll(): void {
    const expand = !allExpanded;
    openRows.clear();
    if (expand) {
      for (let index = 0; index < facet.findings.length; index++) openRows.add(index);
    }
  }

  const BADGE_TONE: Record<FindingBadge["tone"], string> = {
    failed: "bg-status-failed-soft text-status-failed",
    warning: "bg-warning-soft text-warning",
    accent: "bg-accent-soft text-accent",
    neutral: "",
  };
</script>

<section
  class="border-border rounded-md border text-xs"
  aria-label={header}
  data-testid="findings-card"
>
  <header class="flex items-center gap-2 py-1.5 pr-2 pl-3">
    <ClipboardCheck class="text-muted h-3.5 w-3.5 shrink-0" aria-hidden="true" />
    <span class="text-fg min-w-0 flex-1 truncate" data-testid="findings-header">
      <span class="font-medium">Code review</span><span class="text-muted"
        >{header.slice("Code review".length)}</span
      >
    </span>
    <div class="flex shrink-0 items-center gap-0.5">
      {#if expandable && facet.findings.length > 0}
        <Tooltip label={toggleAllLabel} side="bottom" reopen="fresh-hover">
          {#snippet trigger(props)}
            <button
              {...props}
              type="button"
              data-layout-toggle
              class={cn(
                ICON_BUTTON_CLASS,
                "focus-visible:ring-focus focus-visible:ring-1 focus-visible:outline-none",
              )}
              aria-label={toggleAllLabel}
              data-testid="findings-toggle-all"
              onclick={toggleAll}
            >
              <ExpandCollapseIcon expanded={allExpanded} size={16} />
            </button>
          {/snippet}
        </Tooltip>
      {/if}
      <CopyButton text={facet.text} label="Copy review" testid="findings-copy" />
    </div>
  </header>
  {#if facet.findings.length > 0}
    <ol class="border-border border-t py-1" data-testid="findings-list">
      {#each facet.findings as finding, index (index)}
        {@const open = expandable && openRows.has(index)}
        {@const badge = findingBadge(finding)}
        {@const location = findingLocation(finding)}
        {@const shortLocation = findingShortLocation(finding)}
        <!-- The model writes Markdown `code` spans: a one-line title drops the
             backticks (keeping identifiers' underscores intact), and the
             expanded detail renders the Markdown. -->
        {@const title = (finding.short_summary ?? finding.summary).replaceAll("`", "")}
        <li data-testid="finding-row">
          {#snippet rowContent()}
            <span class="flex min-w-0 items-start gap-2">
              <span
                class={cn(
                  "text-fg min-w-0 flex-1 text-sm leading-5 font-medium",
                  open ? "break-words whitespace-normal" : "truncate",
                )}
                data-testid="finding-title">{title}</span
              >
              {#if expandable}
                <span
                  class={cn(
                    "text-muted mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center transition-transform",
                    open && "rotate-90",
                  )}
                  aria-hidden="true">›</span
                >
              {/if}
            </span>
            <span class="mt-1 flex min-w-0 flex-wrap items-baseline gap-x-2 gap-y-1">
              {#if badge}
                <Badge
                  class={cn("shrink-0 normal-case", BADGE_TONE[badge.tone])}
                  testid="finding-badge">{badge.label}</Badge
                >
              {/if}
              {#if finding.category}
                <Badge
                  class="max-w-full rounded-full font-medium [overflow-wrap:anywhere] normal-case"
                  testid="finding-category">{finding.category}</Badge
                >
              {/if}
              <span class="text-muted flex min-w-0 flex-1 basis-32">
                <span
                  class={cn("min-w-0 font-mono", open ? "[overflow-wrap:anywhere]" : "truncate")}
                  data-testid="finding-location">{open ? location : shortLocation}</span
                >
              </span>
            </span>
          {/snippet}
          {#if expandable}
            <button
              type="button"
              data-layout-toggle
              class="hover:bg-hover focus-visible:ring-focus block w-full px-3 py-1.5 text-left focus-visible:ring-1 focus-visible:outline-none focus-visible:ring-inset"
              aria-expanded={open}
              data-testid="finding-toggle"
              onclick={() => toggle(index)}
            >
              {@render rowContent()}
            </button>
          {:else}
            <div class="px-3 py-1.5">
              {@render rowContent()}
            </div>
          {/if}
          {#if open}
            <div
              class="bg-review-detail mx-3 mt-1.5 mb-2 space-y-2 rounded-md p-3"
              data-testid="finding-detail"
            >
              {#if finding.short_summary}
                <Markdown text={finding.summary} />
              {/if}
              {#if finding.failure_scenario}
                <div class="space-y-1" data-testid="finding-scenario">
                  <div class="text-muted font-medium">Failure scenario</div>
                  <Markdown text={finding.failure_scenario} />
                </div>
              {/if}
            </div>
          {/if}
        </li>
      {/each}
    </ol>
  {/if}
</section>
