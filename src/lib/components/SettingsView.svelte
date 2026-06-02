<script lang="ts">
  import { theme, type ThemeMode } from "$lib/theme.svelte";
  import { agentCopy } from "$lib/agentCopy.svelte";
  import type { AgentCopyMode } from "$lib/agentCopyMode";
  import { cn } from "$lib/utils";
  import { ICON_BUTTON_CLASS, ICON_SIZE } from "$lib/components/ui/iconButton";
  import {
    SEGMENTED_CONTAINER_CLASS,
    SEGMENTED_ITEM_CLASS,
    SEGMENTED_ITEM_ACTIVE_CLASS,
    SEGMENTED_ITEM_INACTIVE_CLASS,
  } from "$lib/components/ui/segmentedControl";
  import HarnessStatusList from "$lib/components/HarnessStatusList.svelte";

  let { onClose }: { onClose: () => void } = $props();

  const themeOptions: { mode: ThemeMode; label: string }[] = [
    { mode: "system", label: "System" },
    { mode: "light", label: "Light" },
    { mode: "dark", label: "Dark" },
  ];

  const copyOptions: { mode: AgentCopyMode; label: string }[] = [
    { mode: "last_answer_block", label: "Final Response" },
    { mode: "full_answer", label: "Entire Response" },
  ];

  const shortcuts = [
    { action: "Toggle projects sidebar", keys: ["⌘", "B"] },
    { action: "Toggle agents sidebar", keys: ["⌘", "⇧", "B"] },
    { action: "Toggle both sidebars", keys: ["⌘", "⌥", "B"] },
    { action: "Toggle settings", keys: ["⌘", ","] },
  ];

  const sectionClass = "border-border space-y-3 border-t pt-5";
  const sectionHeadingClass = "text-fg text-base font-semibold";
</script>

<div class="flex flex-1 overflow-y-auto px-8 pb-7" data-testid="settings-view">
  <div class="w-full max-w-2xl">
    <div class="flex h-16 items-center justify-between gap-3">
      <h1 class="text-fg text-2xl font-semibold">Settings</h1>
      <button
        type="button"
        class={cn(ICON_BUTTON_CLASS, "hover:bg-panel")}
        aria-label="Close settings"
        data-testid="settings-close"
        onclick={onClose}
      >
        <svg
          width={ICON_SIZE}
          height={ICON_SIZE}
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M18 6 6 18M6 6l12 12" />
        </svg>
      </button>
    </div>

    <section class={sectionClass}>
      <div>
        <h2 class={sectionHeadingClass}>Theme</h2>
      </div>

      <div
        class={cn(SEGMENTED_CONTAINER_CLASS, "inline-grid w-56 grid-cols-3")}
        role="radiogroup"
        aria-label="Theme"
      >
        {#each themeOptions as option (option.mode)}
          <button
            type="button"
            role="radio"
            class={cn(
              SEGMENTED_ITEM_CLASS,
              "flex items-center justify-center",
              theme.mode === option.mode
                ? SEGMENTED_ITEM_ACTIVE_CLASS
                : SEGMENTED_ITEM_INACTIVE_CLASS,
            )}
            aria-checked={theme.mode === option.mode}
            onclick={() => theme.set(option.mode)}
          >
            {option.label}
          </button>
        {/each}
      </div>
    </section>

    <section class={cn(sectionClass, "mt-7")}>
      <div>
        <h2 class={sectionHeadingClass}>Agent Message Copy Behavior</h2>
        <p class="text-muted mt-1 text-sm leading-relaxed">
          Agent responses can include progress notes before tool calls, then a final response after
          the work is done. Choose what the copy button captures.
        </p>
      </div>

      <div
        class={cn(SEGMENTED_CONTAINER_CLASS, "inline-grid w-72 grid-cols-2")}
        role="radiogroup"
        aria-label="Agent message copy"
      >
        {#each copyOptions as option (option.mode)}
          <button
            type="button"
            role="radio"
            class={cn(
              SEGMENTED_ITEM_CLASS,
              "flex items-center justify-center",
              agentCopy.mode === option.mode
                ? SEGMENTED_ITEM_ACTIVE_CLASS
                : SEGMENTED_ITEM_INACTIVE_CLASS,
            )}
            aria-checked={agentCopy.mode === option.mode}
            onclick={() => agentCopy.set(option.mode)}
          >
            {option.label}
          </button>
        {/each}
      </div>
    </section>

    <section class={cn(sectionClass, "mt-7")}>
      <h2 class={sectionHeadingClass}>Shortcuts</h2>

      <div>
        {#each shortcuts as shortcut (shortcut.action)}
          <div class="flex min-h-11 items-center justify-between gap-4 py-2">
            <div class="text-fg text-sm">{shortcut.action}</div>
            <div class="flex items-center gap-1">
              {#each shortcut.keys as key, index (`${shortcut.action}-${index}`)}
                <kbd
                  class="border-border bg-raised text-muted flex h-6 min-w-6 items-center justify-center rounded border px-1.5 font-mono text-[11px] font-medium"
                >
                  {key}
                </kbd>
              {/each}
            </div>
          </div>
        {/each}
      </div>
    </section>

    <section class={cn(sectionClass, "mt-7")}>
      <div>
        <h2 class={sectionHeadingClass}>Supported CLIs</h2>
        <p class="text-muted mt-1 text-sm leading-relaxed">
          The AI coding CLIs Switchboard runs. Install and sign in to the ones you want to use.
        </p>
      </div>
      <HarnessStatusList />
    </section>

    <div class="h-10" aria-hidden="true"></div>
  </div>
</div>
