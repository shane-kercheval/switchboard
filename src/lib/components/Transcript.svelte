<script lang="ts">
  import type { AgentTranscript } from "$lib/types";

  let { transcript }: { transcript: AgentTranscript } = $props();

  // Auto-pin scroll to bottom unless the user has scrolled up.
  let container = $state<HTMLDivElement | null>(null);
  let pinned = $state<boolean>(true);

  function onScroll(): void {
    if (!container) return;
    const distanceFromBottom =
      container.scrollHeight - container.scrollTop - container.clientHeight;
    pinned = distanceFromBottom < 32;
  }

  // Whenever the transcript changes and we're pinned, scroll to bottom.
  $effect(() => {
    // Re-run on transcript changes by reading a derived value.
    const _ = transcript.turns.length;
    void _;
    if (pinned && container) {
      container.scrollTop = container.scrollHeight;
    }
  });
</script>

<div
  bind:this={container}
  onscroll={onScroll}
  data-testid="transcript"
  class="flex-1 overflow-y-auto p-4"
>
  {#if transcript.turns.length === 0}
    <p class="text-sm text-neutral-500">No messages yet. Type a prompt below.</p>
  {/if}
  <div class="space-y-4">
    {#each transcript.turns as turn (turn.id)}
      <div class="space-y-1">
        {#if turn.role === "user"}
          <div class="text-xs font-semibold tracking-wide text-neutral-500 uppercase">You</div>
          <div class="text-sm whitespace-pre-wrap text-neutral-900">{turn.text}</div>
        {:else}
          <div class="flex items-center gap-2 text-xs font-semibold tracking-wide uppercase">
            <span class="text-neutral-500">Agent</span>
            {#if turn.status === "streaming"}
              <span class="text-amber-700">streaming…</span>
            {:else if turn.status === "failed"}
              <span class="text-red-700">failed</span>
            {/if}
          </div>
          <div class="text-sm whitespace-pre-wrap text-neutral-800">{turn.text}</div>
          {#if turn.status === "failed" && turn.error}
            <div class="text-xs text-red-700" data-testid="turn-error">{turn.error}</div>
          {/if}
        {/if}
      </div>
    {/each}
  </div>
</div>
