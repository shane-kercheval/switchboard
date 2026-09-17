<script lang="ts">
  /// The *action* row for a manual context compaction — the user asking the
  /// harness to summarize the conversation so far and continue from the summary.
  ///
  /// A sibling of `CompactionMarker.svelte` in visual language (borderless,
  /// history icon, one-line label, muted detail) but not in content: that one is
  /// the harness's own recap, this one is what Switchboard did. It has a
  /// success/failure state, which the marker has none of, and the two render
  /// beside each other after a completed compaction.
  ///
  /// Ungrouped by construction — a compaction has no prompt above it, because it
  /// is not something the user said.
  import { History } from "@lucide/svelte";

  let {
    status,
    before,
    after,
    error,
  }: {
    status: "streaming" | "complete" | "failed" | "cancelled";
    /// Context occupancy before and after, when the turn reported any. Rendered
    /// **whenever both are present, whatever the status** — a compaction that
    /// succeeded and then exited non-zero terminates `failed` while legitimately
    /// carrying its post-compaction occupancy, and the sidebar bar moves on that
    /// same usage. Without the counts the row would say "failed" with no numbers
    /// while the bar dropped, which reads as a contradiction.
    before?: number;
    after?: number;
    /// The harness's own explanation, when it declined or broke.
    error?: string;
  } = $props();

  /// Tokens at transcript density: `23.4k`, `990`. Not a general-purpose
  /// formatter — it exists so the before/after pair reads as one short phrase.
  function tokens(n: number): string {
    if (n < 1000) return String(n);
    const thousands = n / 1000;
    return `${thousands < 10 ? thousands.toFixed(1) : Math.round(thousands)}k`;
  }

  const counts = $derived(
    before !== undefined && after !== undefined
      ? `${tokens(before)} → ${tokens(after)} tokens`
      : undefined,
  );

  const label = $derived.by(() => {
    switch (status) {
      case "streaming":
        return "Compacting context…";
      case "complete":
        return "Context compacted";
      case "cancelled":
        return "Compaction cancelled";
      case "failed":
        return error === undefined || error.trim() === ""
          ? "Compaction failed"
          : `Compaction failed: ${error.trim()}`;
    }
  });
</script>

<div
  class="flex min-h-7 items-center gap-2 px-1.5 py-1 text-xs"
  data-testid="compaction-turn"
  data-status={status}
>
  <History class="text-muted h-3.5 w-3.5 shrink-0" aria-hidden="true" />
  <span
    class={status === "failed"
      ? "text-status-failed shrink-0 font-medium"
      : "text-fg shrink-0 font-medium"}
    data-testid="compaction-turn-label">{label}</span
  >
  {#if counts !== undefined}
    <span class="text-muted min-w-0 truncate" data-testid="compaction-turn-counts">· {counts}</span>
  {/if}
</div>
