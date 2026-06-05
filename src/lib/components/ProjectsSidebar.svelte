<script lang="ts">
  import {
    activateProject,
    backgroundCompletedProjectIds,
    liveProjectSends,
    projects,
    deleteProject,
    renameProject,
    selection,
    setProjectArchived,
    workspace,
  } from "$lib/state/workspace.svelte";
  import { cancelSend } from "$lib/state/index.svelte";
  import type { ProjectId, ProjectListing } from "$lib/types";
  import { validateProjectName, normalizeProjectName } from "$lib/projectName";
  import type { NameValidation } from "$lib/nameValidation";
  import { basename, cn, relativeTime } from "$lib/utils";
  import Input from "$lib/components/ui/Input.svelte";
  import SidebarPanel from "$lib/components/ui/SidebarPanel.svelte";
  import SidebarSection from "$lib/components/ui/SidebarSection.svelte";
  import Badge from "$lib/components/ui/Badge.svelte";
  import SettingsButton from "$lib/components/ui/SettingsButton.svelte";
  import SidebarToggleButton from "$lib/components/ui/SidebarToggleButton.svelte";
  import PlusIcon from "$lib/components/ui/PlusIcon.svelte";
  import StopIcon from "$lib/components/ui/StopIcon.svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { ICON_BUTTON_CLASS } from "$lib/components/ui/iconButton";
  import { SEGMENTED_CONTAINER_CLASS } from "$lib/components/ui/segmentedControl";
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

  let deleteConfirmProjectId = $state<ProjectId | null>(null);
  let deletingProjectId = $state<ProjectId | null>(null);
  let archiveError = $state<{ projectId: ProjectId; message: string } | null>(null);
  let deleteError = $state<{ projectId: ProjectId; message: string } | null>(null);
  let relativeNow = $state(Date.now());

  /// `Active | Archived` view filter. Default `Active`. A true either/or split:
  /// each view shows exactly the projects whose `archived` flag matches it
  /// (sorting is unchanged — `projects.list` stays `last_activity`-ordered).
  let archivedView = $state<"active" | "archived">("active");

  /// Free-text filter, composed *after* the archived view: case-insensitive
  /// substring over the project name and the directory basename (the segment
  /// shown on the row, not the full path). Frontend-only — `projects.list` is
  /// already in memory.
  let searchQuery = $state("");
  /// The current Active/Archived view *before* search — used to tell "this view
  /// is empty" apart from "the search hid everything" in the empty state.
  const projectsInView = $derived(
    projects.list.filter((p) => p.archived === (archivedView === "archived")),
  );
  const visibleProjects = $derived.by(() => {
    const q = searchQuery.trim().toLowerCase();
    if (q === "") return projectsInView;
    return projectsInView.filter(
      (p) => p.name.toLowerCase().includes(q) || basename(p.directory).toLowerCase().includes(q),
    );
  });
  // Filled selected pill, but a lightened medium-gray fill (`bg-muted/65`)
  // rather than the near-black `bg-primary` the dialog toggles use — softer for
  // a control that lives in the main view. `text-primary-fg` is the contrasting
  // label in both themes.
  const segmentClass = (selected: boolean): string =>
    cn(
      "flex h-4 items-center rounded-full px-2 text-[11px] font-medium transition-colors",
      selected ? "bg-muted/65 text-primary-fg" : "text-muted hover:bg-raised",
    );
  const projectActionClass =
    "text-muted hover:bg-border/60 hover:text-fg focus-visible:ring-accent focus-visible:bg-border/60 focus-visible:text-fg inline-flex h-6 w-6 items-center justify-center rounded-full transition-colors focus-visible:ring-2 focus-visible:outline-none";
  const projectDeleteClass =
    "text-muted hover:bg-status-failed-soft/70 hover:text-status-failed focus-visible:ring-accent focus-visible:bg-status-failed-soft/70 focus-visible:text-status-failed inline-flex h-6 w-6 items-center justify-center rounded-full transition-colors focus-visible:ring-2 focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-50";

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
    deleteConfirmProjectId = null;
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

  /// Focus + select the edit field once it mounts. Deferred a frame so the
  /// input is mounted and ready before selection.
  function focusSelect(node: HTMLInputElement): void {
    requestAnimationFrame(() => {
      node.focus();
      node.select();
    });
  }

  $effect(() => {
    if (
      deleteConfirmProjectId !== null &&
      !visibleProjects.some((project) => project.id === deleteConfirmProjectId)
    ) {
      deleteConfirmProjectId = null;
    }
  });

  $effect(() => {
    function refreshRelativeTimes(): void {
      relativeNow = Date.now();
    }
    function refreshWhenVisible(): void {
      if (document.visibilityState === "visible") refreshRelativeTimes();
    }
    const interval = setInterval(refreshRelativeTimes, 60_000);
    document.addEventListener("visibilitychange", refreshWhenVisible);
    return () => {
      clearInterval(interval);
      document.removeEventListener("visibilitychange", refreshWhenVisible);
    };
  });

  async function toggleArchive(project: ProjectListing): Promise<void> {
    archiveError = null;
    deleteError = null;
    try {
      await setProjectArchived(project.id, !project.archived);
      if (deleteConfirmProjectId === project.id) deleteConfirmProjectId = null;
    } catch (err) {
      archiveError = {
        projectId: project.id,
        message: err instanceof Error ? err.message : String(err),
      };
    }
  }

  function startDelete(project: ProjectListing): void {
    archiveError = null;
    deleteError = null;
    deleteConfirmProjectId = project.id;
  }

  function cancelDelete(projectId: ProjectId): void {
    if (deleteConfirmProjectId === projectId) deleteConfirmProjectId = null;
  }

  function disarmDeleteOnLeave(node: HTMLElement, projectId: ProjectId): { destroy: () => void } {
    const handlePointerLeave = (): void => cancelDelete(projectId);
    node.addEventListener("pointerleave", handlePointerLeave);
    return {
      destroy: () => node.removeEventListener("pointerleave", handlePointerLeave),
    };
  }

  async function confirmDelete(project: ProjectListing): Promise<void> {
    deletingProjectId = project.id;
    deleteError = null;
    try {
      await deleteProject(project.id);
      if (deleteConfirmProjectId === project.id) deleteConfirmProjectId = null;
    } catch (err) {
      deleteConfirmProjectId = null;
      deleteError = {
        projectId: project.id,
        message: err instanceof Error ? err.message : String(err),
      };
    } finally {
      deletingProjectId = null;
    }
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
      <div class="flex items-center gap-1.5">
        <div
          class={cn(SEGMENTED_CONTAINER_CLASS, "flex")}
          role="tablist"
          aria-label="Filter projects"
          data-testid="project-view-toggle"
        >
          <button
            type="button"
            role="tab"
            aria-selected={archivedView === "active"}
            class={segmentClass(archivedView === "active")}
            data-testid="project-view-active"
            onclick={() => (archivedView = "active")}
          >
            Active
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={archivedView === "archived"}
            class={segmentClass(archivedView === "archived")}
            data-testid="project-view-archived"
            onclick={() => (archivedView = "archived")}
          >
            Archived
          </button>
        </div>
        <button
          type="button"
          class={ICON_BUTTON_CLASS}
          aria-label="Add a project"
          data-testid="add-project"
          onclick={onAddProject}
        >
          <PlusIcon />
        </button>
      </div>
    {/snippet}

    <!-- Search lives pinned under the header (not crammed beside the toggle/+
         in the narrow header), filtering the current Active/Archived view. Only
         shown when there's something to search. -->
    {#snippet subheader()}
      {#if projects.list.length > 0}
        <div class="relative px-2 pb-1.5">
          <Input
            bind:value={searchQuery}
            placeholder="Search projects"
            aria-label="Search projects"
            data-testid="project-search"
            class="h-7 pr-7 text-xs"
          />
          {#if searchQuery !== ""}
            <button
              type="button"
              class="text-muted hover:text-fg absolute top-1/2 right-3.5 -translate-y-1/2"
              aria-label="Clear search"
              data-testid="project-search-clear"
              onclick={() => (searchQuery = "")}
            >
              <svg
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
                stroke-linejoin="round"
                class="h-3.5 w-3.5"
                aria-hidden="true"
              >
                <path d="M18 6 6 18M6 6l12 12" />
              </svg>
            </button>
          {/if}
        </div>
      {/if}
    {/snippet}

    {#if visibleProjects.length === 0}
      <p class="text-muted px-3 py-3 text-xs">
        {#if searchQuery.trim() !== "" && projectsInView.length > 0}
          No projects match.
        {:else if projects.list.length === 0}
          No projects yet.
        {:else if archivedView === "archived"}
          No archived projects.
        {:else}
          No active projects.
        {/if}
      </p>
    {/if}
    <div class="flex flex-col gap-0.5 px-2 pb-2">
      {#each visibleProjects as project (project.id)}
        {@const liveSends = liveProjectSends(project.id)}
        {@const busy = liveSends.size > 0}
        {@const completed = !busy && project.id in backgroundCompletedProjectIds}
        {@const editing = editingProjectId === project.id}
        {@const highlighted =
          project.id === (selection.loadingProjectId ?? selection.activeProjectId)}
        <div
          class={cn(
            "group hover:bg-raised/70 flex w-full flex-col rounded-md",
            highlighted && "bg-raised hover:bg-raised",
          )}
          data-testid="project-row"
          data-project-id={project.id}
          data-active={highlighted}
          use:disarmDeleteOnLeave={project.id}
        >
          <div class="flex w-full items-center">
            {#if editing}
              <!-- Edit mode swaps the select button for an inline editor. Blur
                   cancels; the save button prevents mousedown so click commits
                   before blur-cancel fires. The directory line stays for
                   context. -->
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
                    aria-describedby={renameError
                      ? `project-rename-error-${project.id}`
                      : undefined}
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
                  <span class="truncate" title={project.directory}
                    >{basename(project.directory)}</span
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
                  <span class="truncate" title={project.directory}
                    >{basename(project.directory)}</span
                  >
                  <span>·</span>
                  <span class="shrink-0"
                    >{relativeTime(project.last_activity, new Date(relativeNow))}</span
                  >
                </div>
              </button>
              <div class="flex shrink-0 items-center gap-0.5 pr-1.5">
                {#if !busy && !completed}
                  <div
                    class="pointer-events-none flex max-w-0 items-center gap-0.5 overflow-hidden opacity-0 transition-[max-width,opacity] group-hover:pointer-events-auto group-hover:max-w-[3.25rem] group-hover:opacity-100"
                  >
                    {#if deleteConfirmProjectId === project.id}
                      <Tooltip label="Cancel delete" delayDuration={1000}>
                        {#snippet trigger(props)}
                          <button
                            {...props}
                            type="button"
                            class={projectActionClass}
                            aria-label="Cancel delete"
                            tabindex="-1"
                            data-testid="project-delete-cancel"
                            onclick={() => cancelDelete(project.id)}
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
                              <path d="M18 6 6 18M6 6l12 12" />
                            </svg>
                          </button>
                        {/snippet}
                      </Tooltip>
                      <Tooltip label="Confirm delete" delayDuration={1000}>
                        {#snippet trigger(props)}
                          <button
                            {...props}
                            type="button"
                            class="text-status-failed hover:bg-status-failed-soft/70 focus-visible:ring-accent focus-visible:bg-status-failed-soft/70 inline-flex h-6 w-6 items-center justify-center rounded-full transition-colors focus-visible:ring-2 focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-50"
                            disabled={deletingProjectId === project.id}
                            aria-label="Confirm delete"
                            tabindex="-1"
                            data-testid="project-delete-confirm"
                            onclick={() => void confirmDelete(project)}
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
                        {/snippet}
                      </Tooltip>
                    {:else}
                      <Tooltip
                        label={project.archived ? "Unarchive project" : "Archive project"}
                        delayDuration={1000}
                      >
                        {#snippet trigger(props)}
                          <button
                            {...props}
                            type="button"
                            class={projectActionClass}
                            aria-label={project.archived ? "Unarchive project" : "Archive project"}
                            tabindex="-1"
                            data-testid="project-action-archive"
                            onclick={() => void toggleArchive(project)}
                          >
                            {#if project.archived}
                              <svg
                                viewBox="0 0 24 24"
                                fill="none"
                                stroke="currentColor"
                                stroke-width="1.8"
                                stroke-linecap="round"
                                stroke-linejoin="round"
                                class="h-4 w-4"
                                aria-hidden="true"
                              >
                                <path d="M9 14 4 9l5-5" />
                                <path d="M4 9h10a6 6 0 0 1 0 12h-2" />
                              </svg>
                            {:else}
                              <svg
                                viewBox="0 0 24 24"
                                fill="none"
                                stroke="currentColor"
                                stroke-width="1.8"
                                stroke-linecap="round"
                                stroke-linejoin="round"
                                class="h-4 w-4"
                                aria-hidden="true"
                              >
                                <rect x="3" y="4" width="18" height="4" rx="1" />
                                <path d="M5 8v10a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8" />
                                <path d="M10 12h4" />
                              </svg>
                            {/if}
                          </button>
                        {/snippet}
                      </Tooltip>
                      <Tooltip delayDuration={1000}>
                        {#snippet trigger(props)}
                          <button
                            {...props}
                            type="button"
                            class={projectDeleteClass}
                            disabled={!project.available}
                            aria-label="Delete project"
                            tabindex="-1"
                            data-testid="project-action-delete"
                            onclick={() => startDelete(project)}
                          >
                            <svg
                              viewBox="0 0 24 24"
                              fill="none"
                              stroke="currentColor"
                              stroke-width="1.8"
                              stroke-linecap="round"
                              stroke-linejoin="round"
                              class="h-4 w-4"
                              aria-hidden="true"
                            >
                              <path d="M3 6h18" />
                              <path d="M8 6V4h8v2" />
                              <path d="M19 6 18 20H6L5 6" />
                              <path d="M10 11v5M14 11v5" />
                            </svg>
                          </button>
                        {/snippet}
                        <div class="max-w-56">
                          <div class="text-[13px] font-medium">Delete project</div>
                          <div class="text-primary-fg/75 mt-1 text-xs leading-4">
                            Removes Switchboard's files for this project; your code and agent
                            session files are kept.
                          </div>
                        </div>
                      </Tooltip>
                    {/if}
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
          {#if archiveError?.projectId === project.id}
            <div class="text-status-failed px-2.5 pb-2 text-xs" data-testid="project-archive-error">
              Couldn't update project: {archiveError.message}
            </div>
          {/if}
          {#if deleteError?.projectId === project.id}
            <div class="text-status-failed px-2.5 pb-2 text-xs" data-testid="project-delete-error">
              Couldn't delete project: {deleteError.message}
            </div>
          {/if}
        </div>
      {/each}
    </div>
  </SidebarSection>
</SidebarPanel>
