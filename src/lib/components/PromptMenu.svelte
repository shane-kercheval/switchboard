<script lang="ts">
  import { tick } from "svelte";
  import type { Prompt } from "$lib/types";
  import { promptDisplayName } from "$lib/prompt";
  import { cn } from "$lib/utils";

  /// A typeahead popover over the cached prompt list. Opened by the compose
  /// bar's prompt button (or `/` on an empty textarea); picking a prompt enters
  /// prompt mode. Reads only the passed-in cached list — it never fetches, so it
  /// opens instantly. Owns its own search field and keyboard navigation, mirroring
  /// the `@`-recipient menu's nav model.
  let {
    prompts,
    loading = false,
    onpick,
    onclose,
  }: {
    prompts: Prompt[];
    /// The cache hasn't been read yet — show a loading row rather than the
    /// "no prompts" empty state, which would otherwise flash on first open.
    loading?: boolean;
    onpick: (prompt: Prompt) => void;
    onclose: () => void;
  } = $props();

  let query = $state("");
  let highlighted = $state(0);
  let searchEl = $state<HTMLInputElement | undefined>(undefined);

  const filtered = $derived.by(() => {
    const q = query.trim().toLowerCase();
    if (q === "") return prompts;
    return prompts.filter((p) =>
      `${p.title ?? ""} ${p.provider}:${p.name} ${p.description ?? ""}`.toLowerCase().includes(q),
    );
  });

  // Keep the highlight in range as the filtered set shrinks/grows.
  $effect(() => {
    if (highlighted > filtered.length - 1) highlighted = Math.max(0, filtered.length - 1);
  });

  // Autofocus the search field on open so the user can type to filter immediately.
  $effect(() => {
    void tick().then(() => searchEl?.focus());
  });

  function promptKey(p: Prompt): string {
    return `${p.provider}:${p.name}`;
  }

  function onKeydown(event: KeyboardEvent): void {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      if (filtered.length > 0) highlighted = (highlighted + 1) % filtered.length;
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      if (filtered.length > 0) highlighted = (highlighted - 1 + filtered.length) % filtered.length;
    } else if (event.key === "Enter") {
      event.preventDefault();
      const pick = filtered[highlighted];
      if (pick !== undefined) onpick(pick);
    } else if (event.key === "Escape") {
      event.preventDefault();
      onclose();
    }
  }
</script>

<!-- Opens upward (`bottom-full`): the trigger sits near the bottom of the
     window, so a downward list would run off-screen — same posture as the `@`
     menu. The prompt list renders *above* the search field, which stays anchored
     just over the trigger while results grow upward. -->
<div
  class="border-border/90 bg-raised absolute inset-x-0 bottom-full z-20 mb-1 overflow-hidden rounded-lg border p-1 shadow-[0_10px_28px_rgba(0,0,0,0.12)]"
  data-testid="prompt-menu"
  role="listbox"
>
  <div class="max-h-64 overflow-y-auto" data-testid="prompt-menu-scroll">
    {#each filtered as prompt, i (promptKey(prompt))}
      <button
        type="button"
        class={cn(
          "flex w-full cursor-pointer flex-col gap-0.5 rounded-md px-2.5 py-1.5 text-left outline-none select-none",
          i === highlighted ? "bg-panel/80" : "",
        )}
        data-testid={`prompt-option-${promptKey(prompt)}`}
        role="option"
        aria-selected={i === highlighted}
        onmousemove={() => (highlighted = i)}
        onclick={() => onpick(prompt)}
      >
        <span class="flex items-baseline gap-1.5">
          <span class="text-fg text-sm font-medium">{promptDisplayName(prompt)}</span>
          <span class="text-muted font-mono text-[11px]">{prompt.provider}</span>
        </span>
        {#if prompt.description}
          <span class="text-muted truncate text-xs">{prompt.description}</span>
        {/if}
      </button>
    {/each}
    {#if filtered.length === 0}
      {#if loading && prompts.length === 0}
        <div class="text-muted px-2.5 py-2 text-sm select-none" data-testid="prompt-menu-loading">
          Loading prompts…
        </div>
      {:else}
        <div class="text-muted px-2.5 py-2 text-sm select-none" data-testid="prompt-menu-empty">
          {prompts.length === 0 ? "No prompts available" : "No matching prompts"}
        </div>
      {/if}
    {/if}
  </div>
  <input
    bind:this={searchEl}
    bind:value={query}
    onkeydown={onKeydown}
    type="text"
    placeholder="Search prompts…"
    data-testid="prompt-menu-search"
    class="border-border bg-panel text-fg placeholder:text-muted focus-visible:ring-accent mt-1 w-full rounded-md border px-2.5 py-1.5 text-sm focus-visible:ring-2 focus-visible:outline-none"
  />
</div>
