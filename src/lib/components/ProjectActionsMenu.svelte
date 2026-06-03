<script lang="ts">
  /// Per-project kebab menu in the projects sidebar — the single host for
  /// per-project actions. Rename (M1) and Delete (M2) live here; Archive/Unarchive
  /// (M3) joins them. The row owns the inline-rename editor state (an `<input>`
  /// can't live inside the row's select button), so Rename is a callback the menu
  /// triggers; Delete is self-contained here via an inline confirm sub-view,
  /// mirroring `AgentActionsMenu`'s remove flow (no separate dialog).
  import type { ProjectListing } from "$lib/types";
  import { deleteProject } from "$lib/state/workspace.svelte";
  import DropdownMenu from "$lib/components/ui/DropdownMenu.svelte";
  import DropdownMenuItem from "$lib/components/ui/DropdownMenuItem.svelte";

  let {
    project,
    onRename,
    open = $bindable(false),
  }: { project: ProjectListing; onRename: () => void; open?: boolean } = $props();

  // Inline-confirm state for Delete — no dialog. The first click swaps the menu's
  // contents to a focused confirm view while keeping it open (the Delete item
  // sets `closeOnSelect={false}`); `deleting` guards against a double-confirm
  // while the backend teardown is in flight.
  let confirmingDelete = $state(false);
  let deleting = $state(false);
  let deleteError = $state<string | null>(null);

  // Reopening the menu resets the confirm affordance to its idle state.
  $effect(() => {
    if (!open) {
      confirmingDelete = false;
      deleteError = null;
    }
  });

  function startDelete(): void {
    deleteError = null;
    confirmingDelete = true;
  }

  function cancelDelete(): void {
    confirmingDelete = false;
  }

  // Confirm: remove the project. On success the row unmounts as the list
  // updates, so we just close the menu; on failure we revert to the menu and
  // surface the error, keeping the project.
  async function confirmDelete(): Promise<void> {
    deleting = true;
    deleteError = null;
    try {
      await deleteProject(project.id);
      confirmingDelete = false;
      open = false;
    } catch (err) {
      deleteError = err instanceof Error ? err.message : String(err);
      confirmingDelete = false;
    } finally {
      deleting = false;
    }
  }
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
  {#if confirmingDelete}
    <!-- Delete swaps the whole menu to a focused confirm view; the copy is
         honest about the blast radius (Switchboard state only). -->
    <div class="w-56 px-2.5 pt-1.5 pb-1" data-testid="project-delete-prompt">
      <p class="text-fg text-xs">Delete "{project.name}"?</p>
      <p class="text-muted mt-0.5 text-xs" data-testid="project-delete-detail">
        Removes Switchboard's files for this project; your code and agent session files are kept.
      </p>
    </div>
    <DropdownMenuItem
      onSelect={confirmDelete}
      closeOnSelect={false}
      disabled={deleting}
      class="text-status-failed"
      data-testid="project-delete-confirm"
    >
      Delete project
    </DropdownMenuItem>
    <DropdownMenuItem
      onSelect={cancelDelete}
      closeOnSelect={false}
      data-testid="project-delete-cancel"
    >
      Cancel
    </DropdownMenuItem>
  {:else}
    <DropdownMenuItem onSelect={onRename} data-testid="project-action-rename">
      Rename project
    </DropdownMenuItem>
    <DropdownMenuItem
      onSelect={startDelete}
      closeOnSelect={false}
      class="text-status-failed"
      data-testid="project-action-delete"
    >
      Delete project
    </DropdownMenuItem>
    {#if deleteError !== null}
      <div class="text-status-failed px-2.5 py-1.5 text-xs" data-testid="project-delete-error">
        Couldn't delete project: {deleteError}
      </div>
    {/if}
  {/if}
</DropdownMenu>
