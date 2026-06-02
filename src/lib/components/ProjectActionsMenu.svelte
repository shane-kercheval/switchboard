<script lang="ts">
  /// Per-project kebab menu in the projects sidebar — the single host for
  /// per-project actions. Rename (M1) is here; Delete (M2) and Archive/Unarchive
  /// (M3) become items on this same menu. The row owns the inline-rename editor
  /// state (an `<input>` can't live inside the row's select button), so Rename is
  /// a callback the menu triggers, mirroring `AgentActionsMenu`/`onRename`.
  import type { ProjectListing } from "$lib/types";
  import DropdownMenu from "$lib/components/ui/DropdownMenu.svelte";
  import DropdownMenuItem from "$lib/components/ui/DropdownMenuItem.svelte";

  let {
    project,
    onRename,
    open = $bindable(false),
  }: { project: ProjectListing; onRename: () => void; open?: boolean } = $props();
</script>

<DropdownMenu
  bind:open
  triggerLabel={`Actions for ${project.name}`}
  triggerTestid="project-actions-trigger"
  triggerClass="text-muted hover:text-fg hover:bg-raised flex h-6 w-6 items-center justify-center rounded-full transition-colors"
  contentTestid="project-actions-menu"
>
  {#snippet trigger()}
    <svg viewBox="0 0 24 24" fill="currentColor" class="h-4 w-4" aria-hidden="true">
      <circle cx="12" cy="5" r="1.6" />
      <circle cx="12" cy="12" r="1.6" />
      <circle cx="12" cy="19" r="1.6" />
    </svg>
  {/snippet}
  <DropdownMenuItem onSelect={onRename} data-testid="project-action-rename">
    Rename project
  </DropdownMenuItem>
</DropdownMenu>
