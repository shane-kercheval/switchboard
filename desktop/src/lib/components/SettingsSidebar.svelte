<script lang="ts">
  import { ArrowLeft } from "@lucide/svelte";
  import SidebarPanel from "$lib/components/ui/SidebarPanel.svelte";
  import { SETTINGS_SECTIONS, type SettingsSection } from "$lib/settingsSections";
  import { windowDragRegion } from "$lib/windowDrag";
  import { cn } from "$lib/utils";

  let {
    section,
    onSelect,
    onClose,
    returnLabel = "Back to project",
  }: {
    section: SettingsSection;
    onSelect: (section: SettingsSection) => void;
    onClose: () => void;
    returnLabel?: string;
  } = $props();
</script>

<SidebarPanel width={224} testid="settings-sidebar">
  <div class="h-11 shrink-0" data-tauri-drag-region use:windowDragRegion></div>
  <div class="px-3 py-2">
    <button
      type="button"
      class="text-muted hover:bg-hover hover:text-fg focus-visible:ring-focus flex w-full items-center gap-2 rounded-md px-2 py-2 text-left text-sm focus-visible:ring-1"
      data-testid="settings-back"
      onclick={onClose}
    >
      <ArrowLeft size={16} aria-hidden="true" />
      {returnLabel}
    </button>
  </div>
  <nav aria-label="Settings categories" class="min-h-0 flex-1 space-y-1 overflow-y-auto px-3 pb-4">
    {#each SETTINGS_SECTIONS as item (item.id)}
      <button
        type="button"
        class={cn(
          "focus-visible:ring-focus w-full rounded-md px-3 py-2 text-left text-sm focus-visible:ring-1",
          section === item.id
            ? "bg-raised text-fg font-medium"
            : "text-muted hover:bg-hover hover:text-fg",
        )}
        aria-current={section === item.id ? "page" : undefined}
        aria-controls={`settings-panel-${item.id}`}
        onclick={() => onSelect(item.id)}
      >
        {item.label}
      </button>
    {/each}
  </nav>
</SidebarPanel>
