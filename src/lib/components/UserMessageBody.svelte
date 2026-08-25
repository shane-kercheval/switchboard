<script lang="ts">
  import { convertFileSrc } from "@tauri-apps/api/core";
  import type { UnifiedRow } from "$lib/state/unified";
  import Markdown from "$lib/components/ui/Markdown.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { SUPPLEMENTAL_TOOLTIP_DELAY } from "$lib/components/ui/tooltip";

  type UserRow = Extract<UnifiedRow, { kind: "user" }>;
  type QuotedSegment =
    | { kind: "text"; text: string }
    | { kind: "quote"; start: string; inner: string; end: string };

  let { row }: { row: UserRow } = $props();

  const QUOTED_BLOCK_SENTINEL = /^=== START (?:forwarded|response) from .+ ===$/m;
  const QUOTED_BLOCK =
    /(=== START (forwarded|response) from (.+?) ===)\n([\s\S]*?)\n(=== END \2 from \3 ===)/g;

  function splitQuotedSegments(text: string): QuotedSegment[] {
    const segments: QuotedSegment[] = [];
    let last = 0;
    for (const match of text.matchAll(QUOTED_BLOCK)) {
      const index = match.index ?? 0;
      const between = text.slice(last, index).replace(/^\n+|\n+$/g, "");
      if (between !== "") segments.push({ kind: "text", text: between });
      segments.push({
        kind: "quote",
        start: match[1]!,
        inner: match[4]!,
        end: match[5]!,
      });
      last = index + match[0].length;
    }
    const tail = text.slice(last).replace(/^\n+|\n+$/g, "");
    if (tail !== "") segments.push({ kind: "text", text: tail });
    return segments;
  }
</script>

{#if QUOTED_BLOCK_SENTINEL.test(row.text)}
  {#each splitQuotedSegments(row.text) as segment, i (i)}
    {#if segment.kind === "quote"}
      <div class="border-muted my-3 border-l-2 pl-3" data-testid="quoted-block">
        <div class="font-mono text-xs font-bold">{segment.start}</div>
        <Markdown text={segment.inner} />
        <div class="font-mono text-xs font-bold">{segment.end}</div>
      </div>
    {:else}
      <Markdown text={segment.text} />
    {/if}
  {/each}
{:else}
  <Markdown text={row.text} />
{/if}

{#if row.attachments.length > 0}
  <div class="mt-1.5 flex flex-wrap gap-1.5" data-testid="user-attachments">
    {#each row.attachments as attachment (attachment.path)}
      {#if attachment.kind === "image"}
        <Tooltip
          label={attachment.original_name}
          delayDuration={SUPPLEMENTAL_TOOLTIP_DELAY}
          focusable={false}
        >
          {#snippet trigger(props)}
            <img
              {...props}
              src={convertFileSrc(attachment.path)}
              alt={attachment.original_name}
              data-testid={`attachment-thumb-${attachment.label}`}
              class="border-border h-16 w-16 rounded-md border object-cover"
            />
          {/snippet}
        </Tooltip>
      {:else}
        <span
          class="border-border bg-panel text-fg inline-flex max-w-[14rem] items-center gap-1.5 rounded-full border px-2 py-px text-xs"
          data-testid={`attachment-file-${attachment.label}`}
          data-kind={attachment.kind}
        >
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.8"
            stroke-linecap="round"
            stroke-linejoin="round"
            class="text-muted h-3.5 w-3.5 shrink-0"
            aria-hidden="true"
          >
            <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" />
            <path d="M14 3v5h5" />
          </svg>
          <Tooltip
            label={attachment.original_name}
            delayDuration={SUPPLEMENTAL_TOOLTIP_DELAY}
            focusable={false}
          >
            {#snippet trigger(props)}
              <span {...props} class="truncate">{attachment.original_name}</span>
            {/snippet}
          </Tooltip>
        </span>
      {/if}
    {/each}
  </div>
{/if}
