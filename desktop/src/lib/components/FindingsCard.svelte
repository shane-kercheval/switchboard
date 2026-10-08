<script lang="ts">
  /// A code review the agent delivered as tool input (Claude Code's
  /// `ReportFindings`), shown as part of its answer rather than as a tool row:
  /// the model was told the host renders it, so this is often the only place
  /// the review appears. Header plus one compact row per finding — status
  /// badge, `file:line`, the short claim — and a row expands to the full
  /// summary and failure scenario. Display-only apart from copy, which copies
  /// the Rust-built markdown so it matches what forward sends.
  ///
  /// `expandable={false}` is for collapsed response previews: they sit inside a
  /// fixed-height clip, so growing a row there would only add hidden content.
  /// Expanding the response gives the full card.
  import type { FindingsFacet } from "$lib/types";
  import Badge from "$lib/components/ui/Badge.svelte";
  import CopyButton from "$lib/components/ui/CopyButton.svelte";
  import Markdown from "$lib/components/ui/Markdown.svelte";
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

  function toggle(index: number): void {
    if (openRows.has(index)) openRows.delete(index);
    else openRows.add(index);
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
  aria-label={findingsHeader(facet)}
  data-testid="findings-card"
>
  <header class="flex items-center gap-2 py-1 pr-1 pl-2">
    <ClipboardCheck class="text-muted h-3.5 w-3.5 shrink-0" aria-hidden="true" />
    <span class="text-fg min-w-0 flex-1 truncate font-medium" data-testid="findings-header"
      >{findingsHeader(facet)}</span
    >
    <CopyButton text={facet.text} label="Copy review" testid="findings-copy" />
  </header>
  {#if facet.findings.length > 0}
    <ol class="border-border border-t py-0.5" data-testid="findings-list">
      {#each facet.findings as finding, index (index)}
        {@const open = expandable && openRows.has(index)}
        {@const badge = findingBadge(finding)}
        <!-- The model writes Markdown `code` spans: a one-line title drops the
             backticks (keeping identifiers' underscores intact), and the
             expanded detail renders the Markdown. -->
        {@const title = (finding.short_summary ?? finding.summary).replaceAll("`", "")}
        <li data-testid="finding-row">
          {#snippet rowLine()}
            {#if badge}
              <Badge
                class={cn("shrink-0 normal-case", BADGE_TONE[badge.tone])}
                testid="finding-badge">{badge.label}</Badge
              >
            {/if}
            <!-- The title keeps a readable minimum; category and location give
                 way first, so long metadata in a narrow column truncates
                 rather than squeezing the finding out. -->
            {#if finding.category}
              <span class="text-muted max-w-[30%] min-w-0 truncate" data-testid="finding-category"
                >{finding.category}</span
              >
            {/if}
            <span
              class="text-muted max-w-[40%] min-w-0 truncate font-mono"
              data-testid="finding-location">{findingShortLocation(finding)}</span
            >
            <span
              class={cn("text-fg min-w-24 flex-1", open ? "whitespace-normal" : "truncate")}
              data-testid="finding-title">{title}</span
            >
          {/snippet}
          {#if expandable}
            <button
              type="button"
              data-layout-toggle
              class="hover:bg-hover flex w-full items-center gap-2 px-2 py-1 text-left"
              aria-expanded={open}
              data-testid="finding-toggle"
              onclick={() => toggle(index)}
            >
              {@render rowLine()}
              <span
                class={cn(
                  "text-muted flex h-4 w-4 shrink-0 items-center justify-center transition-transform",
                  open && "rotate-90",
                )}
                aria-hidden="true">›</span
              >
            </button>
          {:else}
            <div class="flex items-center gap-2 px-2 py-1">
              {@render rowLine()}
            </div>
          {/if}
          {#if open}
            <div class="space-y-1.5 pt-0.5 pr-2 pb-2 pl-2" data-testid="finding-detail">
              <div class="text-muted font-mono break-all" data-testid="finding-full-location">
                {findingLocation(finding)}
              </div>
              {#if finding.short_summary}
                <Markdown text={finding.summary} />
              {/if}
              {#if finding.failure_scenario}
                <div data-testid="finding-scenario">
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
