<script lang="ts">
  /// The ⌘⇧P command palette: a centered modal over a filtered, grouped command
  /// list. Presentation only — the caller (`App.svelte`) composes the context-
  /// aware `commands` list (its own navigation/project commands plus whatever the
  /// active view contributed via the command registry). Mirrors the compose-bar
  /// menus' interaction model: substring filter, ↑/↓ navigation that skips
  /// disabled rows, Enter/click to run, Escape to close.
  import { tick } from "svelte";
  import Dialog from "$lib/components/ui/Dialog.svelte";
  import { shortcut } from "$lib/platform";
  import type { Command } from "$lib/state/commandPalette.svelte";
  import { cn } from "$lib/utils";

  let {
    open = $bindable(),
    commands,
    onClose,
  }: {
    open: boolean;
    commands: Command[];
    onClose?: () => void;
  } = $props();

  let query = $state("");
  let highlighted = $state(0);
  let searchEl = $state<HTMLInputElement | undefined>(undefined);
  let listEl = $state<HTMLDivElement | undefined>(undefined);

  const filtered = $derived.by(() => {
    const q = query.trim().toLowerCase();
    if (q === "") return commands;
    return commands.filter((c) =>
      `${c.title} ${c.group} ${c.keywords ?? ""}`.toLowerCase().includes(q),
    );
  });

  /// Render groups in first-appearance order so the caller controls section
  /// ordering by the order it emits commands. Each item carries its index into
  /// `filtered` (the highlight is tracked by that index), computed in this single
  /// pass — avoids an O(n) `indexOf` per row in the template.
  const groups = $derived.by(() => {
    const order: string[] = [];
    // Function-local grouping scratch, recreated each derivation and never
    // observed reactively — a plain Map is correct here.
    // eslint-disable-next-line svelte/prefer-svelte-reactivity
    const byGroup = new Map<string, { command: Command; index: number }[]>();
    filtered.forEach((command, index) => {
      const existing = byGroup.get(command.group);
      if (existing === undefined) {
        order.push(command.group);
        byGroup.set(command.group, [{ command, index }]);
      } else {
        existing.push({ command, index });
      }
    });
    return order.map((name) => ({ name, items: byGroup.get(name)! }));
  });

  function isEnabled(index: number): boolean {
    const c = filtered[index];
    return c !== undefined && c.disabled !== true;
  }

  function firstEnabled(): number {
    for (let i = 0; i < filtered.length; i++) if (isEnabled(i)) return i;
    return -1;
  }

  // Reset the query and selection whenever the palette opens, so each open
  // starts on the first item. The Dialog unmounts its body on close, so the
  // input remounts on each open; the query/highlight live here (parent stays
  // mounted), so reset them. (The keep-highlight-enabled effect below nudges to
  // the first *enabled* row if index 0 is disabled.) Focus is handled by
  // `onOpenAutoFocus` on the Dialog (bits-ui would otherwise focus the header ✕).
  $effect(() => {
    if (open) {
      query = "";
      highlighted = 0;
    }
  });

  function focusSearch(event: Event): void {
    event.preventDefault();
    void tick().then(() => searchEl?.focus());
  }

  // Keep the highlight on an enabled row as the filtered set changes.
  $effect(() => {
    void filtered;
    if (!isEnabled(highlighted)) highlighted = firstEnabled();
  });

  // Keep the highlighted row visible while navigating by keyboard. `block:
  // "nearest"` is a no-op when the row is already fully in view, so mouse-hover
  // highlight changes don't trigger any scrolling.
  $effect(() => {
    const id = filtered[highlighted]?.id;
    if (id === undefined || listEl === undefined) return;
    void tick().then(() => {
      listEl
        ?.querySelector<HTMLElement>(`[data-testid="command-option-${id}"]`)
        ?.scrollIntoView({ block: "nearest" });
    });
  });

  function step(direction: 1 | -1): void {
    const n = filtered.length;
    if (n === 0) return;
    let next = highlighted;
    for (let i = 0; i < n; i++) {
      next = (next + direction + n) % n;
      if (isEnabled(next)) {
        highlighted = next;
        return;
      }
    }
  }

  function run(command: Command | undefined): void {
    if (command === undefined || command.disabled === true) return;
    open = false;
    void command.run();
  }

  function onKeydown(event: KeyboardEvent): void {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      step(1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      step(-1);
    } else if (event.key === "Enter") {
      event.preventDefault();
      run(filtered[highlighted]);
    } else if (event.key === "Escape") {
      event.preventDefault();
      open = false;
    }
  }
</script>

<Dialog
  bind:open
  title="Command Palette"
  contentClass="max-w-xl"
  onOpenAutoFocus={focusSearch}
  {onClose}
>
  <div data-testid="command-palette">
    <input
      bind:this={searchEl}
      bind:value={query}
      onkeydown={onKeydown}
      type="text"
      placeholder="Type a command…"
      data-testid="command-palette-search"
      class="border-border bg-panel text-fg placeholder:text-muted focus-visible:ring-accent w-full rounded-md border px-2.5 py-1.5 text-sm focus-visible:ring-2 focus-visible:outline-none"
    />

    <div
      bind:this={listEl}
      class="mt-2 max-h-80 overflow-y-auto"
      data-testid="command-palette-list"
      role="listbox"
    >
      {#each groups as group (group.name)}
        <div role="group" aria-label={group.name}>
          <div
            class="text-muted px-2.5 pt-1.5 pb-0.5 text-[11px] font-medium tracking-wide uppercase select-none"
            aria-hidden="true"
          >
            {group.name}
          </div>
          {#each group.items as { command, index } (command.id)}
            <button
              type="button"
              class={cn(
                "flex w-full items-center justify-between gap-3 rounded-md px-2.5 py-1 text-left outline-none select-none",
                command.disabled ? "cursor-default opacity-40" : "cursor-pointer",
                index === highlighted && !command.disabled ? "bg-panel/80" : "",
              )}
              data-testid={`command-option-${command.id}`}
              role="option"
              aria-selected={index === highlighted}
              aria-disabled={command.disabled === true}
              onmousemove={() => {
                if (!command.disabled) highlighted = index;
              }}
              onclick={() => run(command)}
            >
              <span class="text-fg truncate text-sm">{command.title}</span>
              {#if command.shortcut}
                <span class="text-muted shrink-0 font-mono text-[11px]">
                  {shortcut(...command.shortcut)}
                </span>
              {/if}
            </button>
          {/each}
        </div>
      {/each}
      {#if filtered.length === 0}
        <div class="text-muted px-2.5 py-3 text-sm select-none" data-testid="command-palette-empty">
          No matching commands
        </div>
      {/if}
    </div>
  </div>
</Dialog>
