<script lang="ts">
  import {
    activateProject,
    backgroundCompletedProjectIds,
    liveProjectSends,
    projects,
    renameProject,
    selection,
    workspace,
  } from "$lib/state/workspace.svelte";
  import { cancelSend } from "$lib/state/index.svelte";
  import type { ProjectId, ProjectListing } from "$lib/types";
  import { validateProjectName, normalizeProjectName } from "$lib/projectName";
  import type { NameValidation } from "$lib/nameValidation";
  import { basename, cn, relativeTime } from "$lib/utils";
  import ProjectActionsMenu from "$lib/components/ProjectActionsMenu.svelte";
  import SidebarPanel from "$lib/components/ui/SidebarPanel.svelte";
  import SidebarSection from "$lib/components/ui/SidebarSection.svelte";
  import Badge from "$lib/components/ui/Badge.svelte";
  import SettingsButton from "$lib/components/ui/SettingsButton.svelte";
  import SidebarToggleButton from "$lib/components/ui/SidebarToggleButton.svelte";
  import PlusIcon from "$lib/components/ui/PlusIcon.svelte";
  import StopIcon from "$lib/components/ui/StopIcon.svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import { ICON_BUTTON_CLASS } from "$lib/components/ui/iconButton";
  import { windowDragRegion } from "$lib/windowDrag";

  let {
    onAddProject,
    onOpenSettings,
    onProjectSelect,
    onToggleSidebar,
    settingsOpen = false,
  }: {
    onAddProject: () => void;
    onOpenSettings: () => void;
    onProjectSelect: () => void;
    onToggleSidebar: () => void;
    settingsOpen?: boolean;
  } = $props();

  function cancelAllForProject(projectId: string): void {
    for (const [sendId, agentIds] of liveProjectSends(projectId)) {
      cancelSend(sendId, agentIds);
    }
  }
  /// Inline rename editor (mirrors the agent-card rename in `Sidebar.svelte`).
  /// Only one row edits at a time, so a single `editingProjectId` + `draftName`
  /// suffices; `renameError` holds a backend rejection (the live format/
  /// uniqueness check is `renameValidation`, the frontend mirror of the backend
  /// rules — the backend stays authoritative).
  let editingProjectId = $state<ProjectId | null>(null);
  let draftName = $state<string>("");
  let renaming = $state<boolean>(false);
  let renameError = $state<string | null>(null);

  /// Project names are unique per directory, so validate against the *other*
  /// projects sharing the edited project's directory, excluding itself. Empty is
  /// suppressed from the message (disables save without nagging mid-edit), same
  /// as the agent editor.
  const editingProject = $derived(
    editingProjectId === null
      ? null
      : (projects.list.find((p) => p.id === editingProjectId) ?? null),
  );
  const renameSiblings = $derived(
    editingProject === null
      ? []
      : projects.list.filter((p) => p.directory === editingProject.directory),
  );
  const renameValidation = $derived<NameValidation>(
    editingProjectId === null
      ? { ok: true }
      : validateProjectName(draftName, renameSiblings, editingProjectId),
  );
  const renameMessage = $derived(
    renameValidation.ok || renameValidation.reason === "empty" ? null : renameValidation.message,
  );
  const canSave = $derived(renameValidation.ok && !renaming);

  function startEdit(project: ProjectListing): void {
    editingProjectId = project.id;
    draftName = project.name;
    renameError = null;
  }

  function cancelEdit(): void {
    editingProjectId = null;
    renameError = null;
  }

  /// Commit the draft. An unchanged verbatim name skips the round-trip. On
  /// success the row updates and we leave edit mode; on a backend rejection we
  /// stay in edit mode and surface it.
  async function commitEdit(project: ProjectListing): Promise<void> {
    if (!canSave) return;
    const next = normalizeProjectName(draftName);
    if (next === project.name) {
      cancelEdit();
      return;
    }
    renaming = true;
    renameError = null;
    try {
      await renameProject(project.id, next);
      editingProjectId = null;
    } catch (err) {
      renameError = err instanceof Error ? err.message : String(err);
    } finally {
      renaming = false;
    }
  }

  function onRenameKeydown(event: KeyboardEvent, project: ProjectListing): void {
    if (event.key === "Enter") {
      event.preventDefault();
      void commitEdit(project);
    } else if (event.key === "Escape") {
      event.preventDefault();
      cancelEdit();
    }
  }

  /// Focus + select the edit field once it mounts. Deferred a frame so it wins
  /// the dropdown menu's on-close focus restore (the "Rename" item closes the
  /// menu, returning focus to its trigger); focusing synchronously would be
  /// stolen back and fire the input's blur-cancel. (Same rationale as the agent
  /// editor's `focusSelect`.)
  function focusSelect(node: HTMLInputElement): void {
    requestAnimationFrame(() => {
      node.focus();
      node.select();
    });
  }
</script>

<SidebarPanel side="left" width="w-72" testid="projects-sidebar">
  <div
    class="flex h-11 shrink-0 items-center justify-end px-3"
    data-tauri-drag-region
    use:windowDragRegion
  >
    <SettingsButton pressed={settingsOpen} testid="settings-button" onclick={onOpenSettings} />
    <SidebarToggleButton
      side="left"
      expanded={true}
      label="Hide projects sidebar"
      testid="projects-sidebar-toggle"
      onclick={onToggleSidebar}
    />
  </div>

  {#if !workspace.persistable}
    <div
      class="border-warning/30 bg-warning-soft text-warning border-b px-3 py-2 text-xs"
      data-testid="not-persistable-banner"
    >
      Couldn't read your saved workspace — your project list won't be saved this session.
    </div>
  {/if}

  <SidebarSection title="Projects">
    {#snippet action()}
      <button
        type="button"
        class={ICON_BUTTON_CLASS}
        aria-label="Add a project"
        data-testid="add-project"
        onclick={onAddProject}
      >
        <PlusIcon />
      </button>
    {/snippet}

    {#if projects.list.length === 0}
      <p class="text-muted px-3 py-3 text-xs">No projects yet.</p>
    {/if}
    <div class="flex flex-col gap-0.5 px-2 pb-2">
      {#each projects.list as project (project.id)}
        {@const liveSends = liveProjectSends(project.id)}
        {@const busy = liveSends.size > 0}
        {@const completed = !busy && project.id in backgroundCompletedProjectIds}
        {@const editing = editingProjectId === project.id}
        <div
          class={cn(
            "group hover:bg-raised/70 flex w-full items-center rounded-md transition-colors",
            project.id === selection.activeProjectId && "bg-raised hover:bg-raised",
          )}
          data-testid="project-row"
          data-project-id={project.id}
          data-active={project.id === selection.activeProjectId}
        >
          {#if editing}
            <!-- Edit mode swaps the select button for an inline editor. Blur
                 cancels (never persist on blur); the save button's
                 mousedown-preventDefault keeps focus so its click commits before
                 blur-cancel fires. The directory line stays for context. -->
            <div class="flex min-w-0 flex-1 flex-col gap-0.5 px-2.5 py-2">
              <div class="flex w-full items-center gap-2">
                <input
                  use:focusSelect
                  bind:value={draftName}
                  class={cn(
                    "text-fg border-border bg-panel h-6 min-w-0 flex-1 rounded border px-1.5 text-[13px] font-semibold",
                    "focus-visible:ring-accent focus-visible:ring-1 focus-visible:outline-none",
                    renameMessage && "border-status-failed",
                  )}
                  aria-label="Project name"
                  aria-invalid={!renameValidation.ok}
                  aria-describedby={renameError ? `project-rename-error-${project.id}` : undefined}
                  title={renameMessage ?? undefined}
                  data-testid="project-rename-input"
                  onkeydown={(event) => onRenameKeydown(event, project)}
                  onblur={cancelEdit}
                />
                <button
                  type="button"
                  class={cn(
                    ICON_BUTTON_CLASS,
                    "shrink-0 disabled:cursor-not-allowed disabled:opacity-50",
                  )}
                  disabled={!canSave}
                  aria-label="Save name"
                  title="Save"
                  data-testid="project-rename-save"
                  onmousedown={(event) => event.preventDefault()}
                  onclick={() => void commitEdit(project)}
                >
                  <svg
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="2"
                    stroke-linecap="round"
                    stroke-linejoin="round"
                    class="h-4 w-4"
                    aria-hidden="true"
                  >
                    <path d="M20 6 9 17l-5-5" />
                  </svg>
                </button>
              </div>
              <div class="text-muted flex w-full items-center gap-1 text-xs leading-4">
                <span class="truncate" title={project.directory}>{basename(project.directory)}</span
                >
              </div>
              {#if renameError}
                <div
                  id={`project-rename-error-${project.id}`}
                  class="text-status-failed text-xs"
                  data-testid="project-rename-error"
                >
                  {renameError}
                </div>
              {/if}
            </div>
          {:else}
            <!-- Double-click the row to rename (the kebab's "Rename" item is the
                 other entry point); single-click still activates the project. -->
            <button
              type="button"
              class="flex min-w-0 flex-1 flex-col items-start gap-0.5 px-2.5 py-2 text-left"
              onclick={() => {
                onProjectSelect();
                void activateProject(project.id);
              }}
              ondblclick={() => startEdit(project)}
            >
              <div class="flex w-full items-center gap-2">
                <span class="text-fg truncate text-[13px] font-semibold">
                  {project.name}
                </span>
                {#if !project.available}
                  <Badge class="ml-auto shrink-0" testid="project-unavailable">unavailable</Badge>
                {/if}
              </div>
              <div class="text-muted flex w-full items-center gap-1 text-xs leading-4">
                <span class="truncate" title={project.directory}>{basename(project.directory)}</span
                >
                <span>·</span>
                <span class="shrink-0">{relativeTime(project.last_activity)}</span>
              </div>
            </button>
            <div class="flex shrink-0 items-center gap-0.5 pr-1.5">
              <!-- Kebab first so the status icon (spinner/checkmark) stays the
                   rightmost element, flush to the edge — the kebab's reserved
                   (opacity-0) slot sits to its LEFT and only becomes visible on
                   hover/focus/menu-open, so a completed checkmark never looks
                   indented. Shown only for available, non-busy projects:
                   Rename/Delete need on-disk access (M3's offline Archive will
                   relax `available`), and while busy the spinner/cancel owns the
                   slot — the kebab isn't rendered at all so hovering to cancel
                   never shifts it. Mutating a project mid-run is intentionally
                   unavailable — stop it first. -->
              {#if project.available && !busy}
                <div
                  class="opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100 has-[[data-state=open]]:opacity-100"
                >
                  <ProjectActionsMenu {project} onRename={() => startEdit(project)} />
                </div>
              {/if}
              {#if busy}
                <button
                  type="button"
                  class="group/cancel text-muted hover:bg-status-failed-soft/70 hover:text-status-failed focus-visible:ring-accent focus-visible:bg-status-failed-soft/70 focus-visible:text-status-failed inline-flex h-6 w-6 items-center justify-center rounded-full transition-colors focus-visible:ring-2 focus-visible:outline-none"
                  aria-label="Cancel all running agents"
                  data-testid="project-cancel"
                  onclick={() => cancelAllForProject(project.id)}
                >
                  <Spinner
                    class="h-5 w-5 group-hover/cancel:hidden group-focus-visible/cancel:hidden"
                  />
                  <StopIcon
                    class="hidden h-5 w-5 group-hover/cancel:block group-focus-visible/cancel:block"
                  />
                </button>
              {:else if completed}
                <div class="flex items-center" data-testid="project-completed">
                  <svg
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.5"
                    stroke-linecap="round"
                    stroke-linejoin="round"
                    class="text-accent h-5 w-5"
                    aria-hidden="true"
                  >
                    <circle cx="12" cy="12" r="9" />
                    <path d="m8.5 12 2.5 2.5 4.5-5" />
                  </svg>
                </div>
              {/if}
            </div>
          {/if}
        </div>
      {/each}
    </div>
  </SidebarSection>
</SidebarPanel>
