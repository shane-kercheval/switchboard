<script lang="ts">
  /// Renders one file's structured diff (from the backend `file_diff` command) as
  /// rows — unified or side-by-side. The diff *structure* comes from libgit2 (no
  /// text re-parsing); this component only lays it out and highlights each line's
  /// content through the shared Prism path. Content is sanitized in
  /// `highlightDiffLine` before `{@html}` (agent-authored file content in a
  /// privileged webview — see `diff.ts`).
  import { highlightDiffLine, toSideBySide } from "$lib/diff";
  import type { DiffLine, FileDiff, DiffStyle } from "$lib/types";

  let { diff, style, language }: { diff: FileDiff; style: DiffStyle; language: string } = $props();

  // Pre-fold each hunk for side-by-side once per diff/style change (not per render).
  const sideBySideHunks = $derived(
    style === "side_by_side" ? diff.hunks.map((h) => toSideBySide(h.lines)) : [],
  );

  function lineBg(origin: DiffLine["origin"]): string {
    if (origin === "added") return "bg-diff-added-soft";
    if (origin === "removed") return "bg-diff-removed-soft";
    return "";
  }

  function marker(origin: DiffLine["origin"]): string {
    if (origin === "added") return "+";
    if (origin === "removed") return "-";
    return " ";
  }

  // A side-by-side half's background follows the line it shows; an absent line
  // (the padding side of an uneven change block) is a faint filler so the columns
  // still read as aligned.
  function halfBg(line: DiffLine | null): string {
    return line === null ? "bg-panel/40" : lineBg(line.origin);
  }
</script>

<!-- eslint-disable svelte/no-at-html-tags -- the sole `{@html}` (in the `content`
     snippet) renders `highlightDiffLine` output, which is DOMPurify-sanitized
     before return (see diff.ts); raw line text never reaches the DOM. -->
<div
  class="diff-view bg-raised min-w-0 font-mono text-xs leading-5"
  data-testid="diff-view"
  data-style={style}
>
  {#if diff.binary}
    <p class="text-muted px-3 py-6 text-center text-sm" data-testid="diff-binary">
      Binary file — no preview.
    </p>
  {:else if diff.hunks.length === 0}
    <p class="text-muted px-3 py-6 text-center text-sm" data-testid="diff-empty">No changes.</p>
  {:else}
    {#each diff.hunks as hunk, hi (hi)}
      <div
        class="text-muted bg-panel/80 border-border/40 sticky top-0 z-10 border-y px-3 py-0.5 select-none"
      >
        {hunk.header}
      </div>

      {#if style === "unified"}
        {#each hunk.lines as line, li (li)}
          <div
            class={`flex ${lineBg(line.origin)}`}
            data-testid="diff-line"
            data-origin={line.origin}
          >
            {@render gutter(line.old_lineno)}
            {@render gutter(line.new_lineno)}
            <span class="text-muted w-4 shrink-0 text-center select-none"
              >{marker(line.origin)}</span
            >
            {@render content(line)}
          </div>
        {/each}
      {:else}
        {#each sideBySideHunks[hi] as row, ri (ri)}
          <div class="grid min-w-[48rem] grid-cols-2">
            {@render half(row.left, row.left?.old_lineno ?? null, false)}
            {@render half(row.right, row.right?.new_lineno ?? null, true)}
          </div>
        {/each}
      {/if}
    {/each}

    {#if diff.truncated}
      <p
        class="text-muted border-border/40 border-t px-3 py-2 text-center text-[11px]"
        data-testid="diff-truncated"
      >
        Diff truncated — this file is too large to show in full.
      </p>
    {/if}
  {/if}
</div>

{#snippet gutter(lineNo: number | null)}
  <span class="text-muted/70 w-10 shrink-0 px-1 text-right tabular-nums select-none"
    >{lineNo ?? ""}</span
  >
{/snippet}

{#snippet content(line: DiffLine)}
  <code class="min-w-0 flex-1 px-1 break-all whitespace-pre-wrap"
    >{@html highlightDiffLine(line.content, language)}</code
  >
{/snippet}

{#snippet half(line: DiffLine | null, lineNo: number | null, isRight: boolean)}
  <div
    class={`flex min-w-0 ${halfBg(line)} ${isRight ? "" : "border-border/30 border-r"}`}
    data-testid="diff-line"
    data-origin={line?.origin ?? "empty"}
  >
    {@render gutter(lineNo)}
    {#if line}
      <span class="text-muted w-4 shrink-0 text-center select-none">{marker(line.origin)}</span>
      {@render content(line)}
    {:else}
      <span class="flex-1"></span>
    {/if}
  </div>
{/snippet}
