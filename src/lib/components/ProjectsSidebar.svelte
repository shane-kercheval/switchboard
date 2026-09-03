<script lang="ts">
  import {
    Archive,
    ArchiveRestore,
    Check,
    Code2,
    FolderOpen,
    GitBranch,
    MoreHorizontal,
    Terminal,
    Trash2,
    X,
  } from "@lucide/svelte";
  import {
    activateProject,
    backgroundCompletedProjectIds,
    liveProjectSends,
    projectIsIdle,
    projects,
    projectDeletions,
    deleteProject,
    renameProject,
    selection,
    setProjectArchived,
    workspace,
  } from "$lib/state/workspace.svelte";
  import { revealProjectBranch } from "$lib/state/gitView.svelte";
  import { cancelSend } from "$lib/state/index.svelte";
  import type { ProjectId, ProjectListing } from "$lib/types";
  import { projectIsAvailable } from "$lib/types";
  import { validateProjectName, normalizeProjectName } from "$lib/projectName";
  import type { NameValidation } from "$lib/nameValidation";
  import { basename, cn, relativeTime } from "$lib/utils";
  import {
    layout,
    PROJECTS_SIDEBAR_DEFAULT_WIDTH,
    SIDEBAR_MIN_WIDTH,
    sidebarMaxWidth,
  } from "$lib/layout.svelte";
  import Input from "$lib/components/ui/Input.svelte";
  import ResizeHandle from "$lib/components/ui/ResizeHandle.svelte";
  import SidebarPanel from "$lib/components/ui/SidebarPanel.svelte";
  import SidebarSection from "$lib/components/ui/SidebarSection.svelte";
  import Badge from "$lib/components/ui/Badge.svelte";
  import SettingsButton from "$lib/components/ui/SettingsButton.svelte";
  import SidebarToggleButton from "$lib/components/ui/SidebarToggleButton.svelte";
  import PlusIcon from "$lib/components/ui/PlusIcon.svelte";
  import StopIcon from "$lib/components/ui/StopIcon.svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import StatusDot from "$lib/components/ui/StatusDot.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { SUPPLEMENTAL_TOOLTIP_DELAY } from "$lib/components/ui/tooltip";
  import { workflowRuns, cancelRun } from "$lib/state/workflows.svelte";
  import DropdownMenu from "$lib/components/ui/DropdownMenu.svelte";
  import DropdownMenuItem from "$lib/components/ui/DropdownMenuItem.svelte";
  import { ICON_BUTTON_ON_PANEL_CLASS, ROW_ACTION_ICON_CLASS } from "$lib/components/ui/iconButton";
  import { openInEditor, openInTerminal, revealInFinder } from "$lib/api";
  import {
    SEGMENTED_MAIN_CONTAINER_CLASS,
    SEGMENTED_MAIN_ITEM_ACTIVE_CLASS,
    SEGMENTED_MAIN_ITEM_CLASS,
    SEGMENTED_MAIN_ITEM_INACTIVE_CLASS,
  } from "$lib/components/ui/segmentedControl";
  import { windowDragRegion } from "$lib/windowDrag";
  import { PROJECT_DELETE_TOOLTIP } from "$lib/projectDeletion";

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

  // A failed Stop is surfaced inline, matching how `ComposeBar` already treats the
  // same action on the run's own live view — without it this is a silent dead
  // button. Note what this does *not* cover: `cancel_workflow_run` only requests
  // cancellation and never waits, so a run that ignores the request reports success
  // here and keeps the row spinning. That case has no error to show, which is why
  // Delete stays available during a running workflow rather than relying on Stop.
  function stopWorkflowRun(projectId: ProjectId, runId: string): void {
    workflowStopError = null;
    cancelRun(runId).catch((e: unknown) => {
      workflowStopError = {
        projectId,
        message: `Couldn't stop the workflow: ${e instanceof Error ? e.message : String(e)}`,
      };
    });
  }

  /// Live width during a resize drag; the store commits on pointer-up.
  let draftWidth = $state<number | null>(null);

  let pendingDelete = $state<{ projectId: ProjectId; via: "menu" | "quick" } | null>(null);
  let archiveError = $state<{ projectId: ProjectId; message: string } | null>(null);
  let gitRevealError = $state<{ projectId: ProjectId; message: string } | null>(null);
  let openActionError = $state<{ projectId: ProjectId; message: string } | null>(null);
  let workflowStopError = $state<{ projectId: ProjectId; message: string } | null>(null);
  let openProjectActionsId = $state<ProjectId | null>(null);
  let openActionSeq = 0;
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
  const segmentClass = (selected: boolean): string =>
    cn(
      SEGMENTED_MAIN_ITEM_CLASS,
      selected ? SEGMENTED_MAIN_ITEM_ACTIVE_CLASS : SEGMENTED_MAIN_ITEM_INACTIVE_CLASS,
    );
  const addProjectClass = cn(
    ICON_BUTTON_ON_PANEL_CLASS,
    "focus-visible:ring-focus focus-visible:bg-raised focus-visible:text-fg transition-colors focus-visible:ring-1 focus-visible:outline-none",
  );
  const projectRowActionClass = cn(
    ROW_ACTION_ICON_CLASS,
    "group-data-[active=true]:hover:bg-control-hover group-data-[actions-open=true]:hover:bg-control-hover",
  );

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

  const directoryLabel = (project: ProjectListing): string => project.directory;
  const directoryBasename = (project: ProjectListing): string => basename(project.directory);

  function startEdit(project: ProjectListing): void {
    pendingDelete = null;
    editingProjectId = project.id;
    draftName = project.name;
    renameError = null;
  }

  function cancelEdit(): void {
    editingProjectId = null;
    renameError = null;
  }

  function selectProject(project: ProjectListing): void {
    gitRevealError = null;
    onProjectSelect();
    void activateProject(project.id);
  }

  function onProjectButtonClick(event: MouseEvent, project: ProjectListing): void {
    if (event.detail > 1) return;
    selectProject(project);
  }

  function onProjectButtonMouseDown(event: MouseEvent, project: ProjectListing): void {
    if (event.detail < 2) return;
    event.preventDefault();
    startEdit(project);
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
    if (openProjectActionsId !== null) {
      const openProject = visibleProjects.find((project) => project.id === openProjectActionsId);
      const openProjectCompleted =
        openProject !== undefined &&
        projectIsIdle(openProject.id) &&
        openProject.id in backgroundCompletedProjectIds;
      if (openProject === undefined || (openProjectCompleted && archivedView !== "archived")) {
        openProjectActionsId = null;
      }
    }
    if (
      pendingDelete?.via === "menu" &&
      (!visibleProjects.some((project) => project.id === pendingDelete?.projectId) ||
        openProjectActionsId !== pendingDelete.projectId)
    ) {
      pendingDelete = null;
    }
    if (
      pendingDelete?.via === "quick" &&
      !visibleProjects.some((project) => project.id === pendingDelete?.projectId)
    ) {
      pendingDelete = null;
    }
    if (
      gitRevealError !== null &&
      !visibleProjects.some((project) => project.id === gitRevealError?.projectId)
    ) {
      gitRevealError = null;
    }
    // A stop failure must not outlive the run it describes: if the run terminalizes
    // on its own afterwards, the row goes idle and a stale "couldn't stop" line
    // would sit under a project that has finished.
    if (
      workflowStopError !== null &&
      (!visibleProjects.some((project) => project.id === workflowStopError?.projectId) ||
        projectIsIdle(workflowStopError.projectId))
    ) {
      workflowStopError = null;
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
    gitRevealError = null;
    try {
      await setProjectArchived(project.id, !project.archived);
      if (pendingDelete?.projectId === project.id) pendingDelete = null;
    } catch (err) {
      archiveError = {
        projectId: project.id,
        message: err instanceof Error ? err.message : String(err),
      };
    }
  }

  function startDelete(project: ProjectListing): void {
    archiveError = null;
    gitRevealError = null;
    pendingDelete = { projectId: project.id, via: "menu" };
  }

  function startQuickDelete(project: ProjectListing): void {
    archiveError = null;
    gitRevealError = null;
    pendingDelete = { projectId: project.id, via: "quick" };
    focusProjectControl(project.id, "project-quick-delete-cancel");
  }

  function cancelDelete(projectId: ProjectId): void {
    if (pendingDelete?.projectId === projectId) pendingDelete = null;
  }

  async function confirmDelete(project: ProjectListing): Promise<void> {
    const via = pendingDelete?.projectId === project.id ? pendingDelete.via : null;
    let failed = false;
    try {
      await deleteProject(project.id);
    } catch {
      // The shared deletion store retains the visible failure.
      failed = true;
    } finally {
      if (pendingDelete?.projectId === project.id) pendingDelete = null;
      if (failed && via !== null) {
        focusProjectControl(
          project.id,
          via === "quick" ? "project-quick-delete" : "project-actions-trigger",
        );
      }
    }
  }

  function cancelQuickDelete(projectId: ProjectId): void {
    if (pendingDelete?.projectId !== projectId || pendingDelete.via !== "quick") return;
    pendingDelete = null;
    focusProjectControl(projectId, "project-quick-delete");
  }

  function focusProjectControl(projectId: ProjectId, testid: string): void {
    requestAnimationFrame(() => {
      const row = document.querySelector<HTMLElement>(`[data-project-id="${projectId}"]`);
      row?.querySelector<HTMLElement>(`[data-testid="${testid}"]`)?.focus();
    });
  }

  async function showProjectInGit(project: ProjectListing): Promise<void> {
    archiveError = null;
    gitRevealError = null;
    openActionError = null;
    const result = await revealProjectBranch(project.id, project.directory);
    if (result.kind === "unresolved") {
      gitRevealError = {
        projectId: project.id,
        message: "This project does not have a tracked local git branch.",
      };
    } else if (result.kind === "failed") {
      gitRevealError = {
        projectId: project.id,
        message: result.message,
      };
    }
  }

  /// Run an action that needs the project's working directory path.
  function runProjectPathAction(
    project: ProjectListing,
    action: (directory: string) => Promise<void>,
  ): void {
    runProjectOpenAction(project, () => action(project.directory));
  }

  function runProjectOpenAction(project: ProjectListing, action: () => Promise<void>): void {
    archiveError = null;
    gitRevealError = null;
    openActionError = null;
    const seq = ++openActionSeq;
    void action().catch((e: unknown) => {
      if (seq !== openActionSeq) return;
      openActionError = {
        projectId: project.id,
        message: e instanceof Error ? e.message : String(e),
      };
      console.error("[switchboard] project open action failed", e);
    });
  }
</script>

<SidebarPanel
  side="left"
  width={draftWidth ?? layout.projectsSidebarWidth}
  testid="projects-sidebar"
>
  <ResizeHandle
    value={() => draftWidth ?? layout.projectsSidebarWidth}
    min={SIDEBAR_MIN_WIDTH}
    max={sidebarMaxWidth}
    label="Resize projects sidebar"
    testid="projects-sidebar-resizer"
    class="hover:bg-focus absolute inset-y-0 right-0 z-10 w-1 transition-colors"
    onDraft={(px) => (draftWidth = px)}
    onCommit={(px) => {
      layout.projectsSidebarWidth = px;
      draftWidth = null;
    }}
    onReset={() => {
      layout.projectsSidebarWidth = PROJECTS_SIDEBAR_DEFAULT_WIDTH;
      draftWidth = null;
    }}
  />
  <div
    class="flex h-11 shrink-0 items-center justify-end px-3"
    data-tauri-drag-region
    use:windowDragRegion
  >
    <SettingsButton
      pressed={settingsOpen}
      testid="settings-button"
      class="hover:bg-raised"
      onclick={onOpenSettings}
    />
    <SidebarToggleButton
      side="left"
      expanded={true}
      label="Hide projects sidebar"
      testid="projects-sidebar-toggle"
      class="hover:bg-raised"
      onclick={onToggleSidebar}
    />
  </div>

  {#if !workspace.persistable}
    <div
      class="border-warning/30 bg-warning-soft text-warning border-b px-3 py-2 text-xs"
      data-testid="not-persistable-banner"
    >
      Couldn't read your saved workspace — archive choices won't be saved this session.
    </div>
  {/if}

  <SidebarSection title="Projects">
    {#snippet action()}
      <div class="flex items-center gap-1.5">
        <div
          class={cn(SEGMENTED_MAIN_CONTAINER_CLASS, "flex")}
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
          class={addProjectClass}
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
        <!-- "Has this project finished" — the spinner, the checkmark, and the
             archive guard — routes through the one shared predicate, so the row
             cannot drift from the background-completed observer. `liveSends` is
             passed through rather than recomputed inside it: that scan walks every
             streaming agent's transcript and this runs on the streaming hot path.

             Delete deliberately does NOT use it. `delete_project` cancels the
             project's runs under a deadline and proceeds regardless, so deletion
             with a stuck run is a supported operation; gating it on `idle` made the
             UI stricter than that guarantee and — since Stop only *requests*
             cancellation and never waits — left a wedged run with no disposal path
             at all (there is no directory-removal affordance in the UI). Archive
             keeps the guard: it is reversible, and hiding a working project is
             merely confusing rather than a dead end. -->
        {@const idle = projectIsIdle(project.id, liveSends)}
        {@const completed = idle && project.id in backgroundCompletedProjectIds}
        <!-- Workflow state comes from `workflowRuns`, not from send liveness: a
             workflow between steps, before its first dispatch, or in the
             failed-held state has no live send and would otherwise look idle.

             The running run is selected explicitly rather than taken as `[0]` so
             this row tracks the predicate structurally instead of depending on the
             backend invariant holding forever. That invariant — a `running` run
             never coexists with a held failed/interrupted one — is enforced at the
             invoke guard, which rejects both a second active run and a launch while
             a held run awaits dismissal. -->
        {@const projectRuns = workflowRuns[project.id] ?? []}
        {@const workflowRun =
          projectRuns.find((run) => run.status === "running") ?? projectRuns[0] ?? null}
        {@const workflowRunning = workflowRun?.status === "running"}
        {@const workflowFailedOrInterrupted =
          workflowRun !== null &&
          (workflowRun.status === "failed" || workflowRun.status === "interrupted")}
        {@const editing = editingProjectId === project.id}
        {@const highlighted =
          project.id === (selection.loadingProjectId ?? selection.activeProjectId)}
        {@const actionsOpen = openProjectActionsId === project.id}
        {@const quickDeleteArmed =
          pendingDelete?.projectId === project.id && pendingDelete.via === "quick"}
        {@const projectDeleting = project.id in projectDeletions.pending}
        <div
          class={cn(
            // Documented exception to the panel-context hover idiom (elsewhere a
            // hover target on `panel` brightens to `raised` — see
            // ui-conventions.md). Here the *selected* row is already `raised`
            // (white), so a `raised` hover would be indistinguishable from
            // selection. The row instead lightens to `surface`, the off-white
            // step between the panel sidebar and the white selected state — a
            // visible hover that stays distinct from selected, using an existing
            // token. (`bg-hover`, tuned for white rows, vanishes on this panel base.)
            "group hover:bg-surface flex w-full flex-col rounded-md",
            (highlighted || actionsOpen) && "bg-raised hover:bg-raised",
          )}
          data-testid="project-row"
          data-project-id={project.id}
          data-active={highlighted}
          data-actions-open={actionsOpen}
          role="group"
          aria-label={project.name}
          onpointerleave={() => {
            if (projectDeleting || !quickDeleteArmed) return;
            pendingDelete = null;
          }}
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
                    autocorrect="off"
                    autocapitalize="off"
                    spellcheck="false"
                    class={cn(
                      "text-fg border-border bg-panel h-6 min-w-0 flex-1 rounded border px-1.5 text-[13px] font-semibold",
                      "focus-visible:ring-focus focus-visible:ring-1 focus-visible:outline-none",
                      renameMessage && "border-status-failed",
                    )}
                    aria-label="Project name"
                    aria-invalid={!renameValidation.ok}
                    aria-describedby={renameError
                      ? `project-rename-error-${project.id}`
                      : undefined}
                    data-testid="project-rename-input"
                    onkeydown={(event) => onRenameKeydown(event, project)}
                    onblur={cancelEdit}
                  />
                  <Tooltip label="Save" side="bottom">
                    {#snippet trigger(props)}
                      <button
                        {...props}
                        type="button"
                        class={cn(
                          projectRowActionClass,
                          "shrink-0 disabled:cursor-not-allowed disabled:opacity-50",
                        )}
                        disabled={!canSave}
                        aria-label="Save name"
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
                    {/snippet}
                  </Tooltip>
                </div>
                <div class="text-muted flex w-full items-center gap-1 text-xs leading-4">
                  <Tooltip
                    label={directoryLabel(project)}
                    delayDuration={SUPPLEMENTAL_TOOLTIP_DELAY}
                    side="right"
                  >
                    {#snippet trigger(props)}
                      <span {...props} class="truncate">{directoryBasename(project)}</span>
                    {/snippet}
                  </Tooltip>
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
              <Tooltip
                label={directoryLabel(project)}
                delayDuration={SUPPLEMENTAL_TOOLTIP_DELAY}
                side="right"
              >
                {#snippet trigger(props)}
                  <button
                    {...props}
                    type="button"
                    class="flex min-w-0 flex-1 flex-col items-start gap-0.5 px-2.5 py-2 text-left"
                    data-testid="project-select"
                    onclick={(event) => onProjectButtonClick(event, project)}
                    onmousedown={(event) => onProjectButtonMouseDown(event, project)}
                    ondblclick={(event) => {
                      event.preventDefault();
                      startEdit(project);
                    }}
                  >
                    <div class="flex w-full items-center gap-2">
                      <span class="text-fg truncate text-[13px] font-semibold">
                        {project.name}
                      </span>
                      {#if !projectIsAvailable(project)}
                        <Badge class="ml-auto shrink-0" testid="project-unavailable"
                          >unavailable</Badge
                        >
                      {/if}
                    </div>
                    <div class="text-muted flex w-full items-center gap-1 text-xs leading-4">
                      <span class="truncate">{directoryBasename(project)}</span>
                      <span>·</span>
                      <span class="shrink-0"
                        >{relativeTime(project.last_activity, new Date(relativeNow))}</span
                      >
                    </div>
                  </button>
                {/snippet}
              </Tooltip>
              <div class="flex shrink-0 items-center gap-0.5 pr-1.5">
                {#if !completed || archivedView === "archived"}
                  <div
                    data-testid="project-row-actions"
                    class={cn(
                      // One, two, or three 28px action slots: menu alone;
                      // menu + quick delete; menu + cancel + confirm.
                      "pointer-events-none flex max-w-0 items-center justify-end gap-0.5 overflow-hidden opacity-0 transition-[max-width,opacity]",
                      quickDeleteArmed
                        ? "pointer-events-auto max-w-[84px] opacity-100"
                        : archivedView === "archived"
                          ? "group-focus-within:pointer-events-auto group-focus-within:max-w-[56px] group-focus-within:opacity-100 group-hover:pointer-events-auto group-hover:max-w-[56px] group-hover:opacity-100"
                          : "group-focus-within:pointer-events-auto group-focus-within:max-w-[28px] group-focus-within:opacity-100 group-hover:pointer-events-auto group-hover:max-w-[28px] group-hover:opacity-100",
                      actionsOpen &&
                        (archivedView === "archived"
                          ? "pointer-events-auto max-w-[56px] opacity-100"
                          : "pointer-events-auto max-w-[28px] opacity-100"),
                    )}
                  >
                    <DropdownMenu
                      open={actionsOpen}
                      onOpenChange={(open) => {
                        if (open && pendingDelete?.via === "quick") pendingDelete = null;
                        openProjectActionsId = open
                          ? project.id
                          : openProjectActionsId === project.id
                            ? null
                            : openProjectActionsId;
                      }}
                      triggerLabel={`Actions for ${project.name}`}
                      triggerTestid="project-actions-trigger"
                      triggerTabindex={0}
                      triggerClass={cn(projectRowActionClass, "shrink-0")}
                      contentTestid="project-actions-menu"
                    >
                      {#snippet trigger()}
                        <MoreHorizontal size={14} strokeWidth={1.8} aria-hidden="true" />
                      {/snippet}
                      {#if pendingDelete?.projectId === project.id && pendingDelete.via === "menu"}
                        <DropdownMenuItem
                          onSelect={() => cancelDelete(project.id)}
                          closeOnSelect={false}
                          class="gap-2"
                          data-testid="project-delete-cancel"
                        >
                          <X
                            size={14}
                            strokeWidth={1.8}
                            class="text-muted shrink-0"
                            aria-hidden="true"
                          />
                          Cancel delete
                        </DropdownMenuItem>
                        <DropdownMenuItem
                          onSelect={() => void confirmDelete(project)}
                          disabled={projectDeleting}
                          class="text-status-failed gap-2"
                          data-testid="project-delete-confirm"
                        >
                          <Check size={14} strokeWidth={1.8} class="shrink-0" aria-hidden="true" />
                          Confirm delete
                        </DropdownMenuItem>
                      {:else}
                        <DropdownMenuItem
                          onSelect={() => void showProjectInGit(project)}
                          class="gap-2"
                          data-testid="project-action-show-git"
                        >
                          <GitBranch
                            size={14}
                            strokeWidth={1.8}
                            class="text-muted shrink-0"
                            aria-hidden="true"
                          />
                          Show in Git view
                        </DropdownMenuItem>
                        <DropdownMenuItem
                          onSelect={() =>
                            runProjectPathAction(project, (directory) => openInEditor(directory))}
                          class="gap-2"
                          data-testid="project-action-editor"
                        >
                          <Code2
                            size={14}
                            strokeWidth={1.8}
                            class="text-muted shrink-0"
                            aria-hidden="true"
                          />
                          Open in editor
                        </DropdownMenuItem>
                        <DropdownMenuItem
                          onSelect={() =>
                            runProjectPathAction(project, (directory) => openInTerminal(directory))}
                          class="gap-2"
                          data-testid="project-action-terminal"
                        >
                          <Terminal
                            size={14}
                            strokeWidth={1.8}
                            class="text-muted shrink-0"
                            aria-hidden="true"
                          />
                          Open in terminal
                        </DropdownMenuItem>
                        <DropdownMenuItem
                          onSelect={() =>
                            runProjectPathAction(project, (directory) => revealInFinder(directory))}
                          class="gap-2"
                          data-testid="project-action-reveal"
                        >
                          <FolderOpen
                            size={14}
                            strokeWidth={1.8}
                            class="text-muted shrink-0"
                            aria-hidden="true"
                          />
                          Reveal in Finder
                        </DropdownMenuItem>
                        <DropdownMenuItem
                          onSelect={() => void toggleArchive(project)}
                          disabled={!idle}
                          class="gap-2"
                          data-testid="project-action-archive"
                        >
                          {#if project.archived}
                            <ArchiveRestore
                              size={14}
                              strokeWidth={1.8}
                              class="text-muted shrink-0"
                              aria-hidden="true"
                            />
                            Unarchive project
                          {:else}
                            <Archive
                              size={14}
                              strokeWidth={1.8}
                              class="text-muted shrink-0"
                              aria-hidden="true"
                            />
                            Archive project
                          {/if}
                        </DropdownMenuItem>
                        <DropdownMenuItem
                          onSelect={() => startDelete(project)}
                          closeOnSelect={false}
                          disabled={busy || projectDeleting}
                          class="text-status-failed gap-2"
                          data-testid="project-action-delete"
                          tooltip={`${PROJECT_DELETE_TOOLTIP} Works even if the project's folder no longer exists.${workflowRunning ? " The running workflow is cancelled first." : ""}`}
                        >
                          <Trash2 size={14} strokeWidth={1.8} class="shrink-0" aria-hidden="true" />
                          Delete project
                        </DropdownMenuItem>
                      {/if}
                    </DropdownMenu>
                    {#if archivedView === "archived"}
                      {#if quickDeleteArmed}
                        <Tooltip label="Cancel delete" side="bottom" reopen="fresh-hover">
                          {#snippet trigger(props)}
                            <button
                              {...props}
                              type="button"
                              class={cn(projectRowActionClass, "text-muted shrink-0")}
                              aria-label={`Cancel deleting ${project.name}`}
                              data-testid="project-quick-delete-cancel"
                              disabled={projectDeleting}
                              onclick={() => cancelQuickDelete(project.id)}
                            >
                              <X size={14} strokeWidth={1.8} aria-hidden="true" />
                            </button>
                          {/snippet}
                        </Tooltip>
                        <Tooltip label="Confirm delete" side="bottom" reopen="fresh-hover">
                          {#snippet trigger(props)}
                            <button
                              {...props}
                              type="button"
                              class={cn(
                                projectRowActionClass,
                                "text-status-failed shrink-0 disabled:cursor-not-allowed disabled:opacity-50",
                              )}
                              aria-label={`Confirm deleting ${project.name}`}
                              data-testid="project-quick-delete-confirm"
                              disabled={projectDeleting}
                              onclick={() => void confirmDelete(project)}
                            >
                              {#if projectDeleting}
                                <Spinner class="h-4 w-4" />
                              {:else}
                                <Check size={14} strokeWidth={1.8} aria-hidden="true" />
                              {/if}
                            </button>
                          {/snippet}
                        </Tooltip>
                      {:else}
                        <Tooltip
                          label={`${PROJECT_DELETE_TOOLTIP}${workflowRunning ? " The running workflow is cancelled first." : ""}`}
                          side="bottom"
                          reopen="fresh-hover"
                        >
                          {#snippet trigger(props)}
                            <button
                              {...props}
                              type="button"
                              class={cn(
                                projectRowActionClass,
                                "text-muted hover:text-status-failed shrink-0 disabled:cursor-not-allowed disabled:opacity-50",
                              )}
                              aria-label={`Delete ${project.name}`}
                              data-testid="project-quick-delete"
                              disabled={busy || projectDeleting}
                              onclick={() => startQuickDelete(project)}
                            >
                              <Trash2 size={14} strokeWidth={1.8} aria-hidden="true" />
                            </button>
                          {/snippet}
                        </Tooltip>
                      {/if}
                    {/if}
                  </div>
                {/if}
                {#if workflowFailedOrInterrupted}
                  <!-- The only surviving signal for a *background* workflow
                       failure (the global header indicator was removed): persists
                       until the user opens the project and dismisses the held
                       failed view (= abandon, which drops it from `workflowRuns`). -->
                  <div
                    class="flex h-[26px] w-[26px] items-center justify-center"
                    data-testid="project-workflow-failed"
                  >
                    <StatusDot
                      status="failed"
                      label="Workflow failed — open to dismiss"
                      class="h-2 w-2"
                    />
                  </div>
                {:else if !idle}
                  <button
                    type="button"
                    class="group/cancel text-muted hover:bg-status-failed-soft/70 hover:text-status-failed focus-visible:ring-focus focus-visible:bg-status-failed-soft/70 focus-visible:text-status-failed inline-flex h-[26px] w-[26px] items-center justify-center rounded-full transition-colors focus-visible:ring-1 focus-visible:outline-none"
                    aria-label={workflowRunning ? "Stop workflow" : "Cancel all running agents"}
                    data-testid="project-cancel"
                    onclick={() =>
                      workflowRunning && workflowRun !== null
                        ? stopWorkflowRun(project.id, workflowRun.run_id)
                        : cancelAllForProject(project.id)}
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
          {#if gitRevealError?.projectId === project.id}
            <div class="text-status-failed px-2.5 pb-2 text-xs" data-testid="project-git-error">
              Couldn't open Git view: {gitRevealError.message}
            </div>
          {/if}
          {#if openActionError?.projectId === project.id}
            <div class="text-status-failed px-2.5 pb-2 text-xs" data-testid="project-open-error">
              Couldn't open project: {openActionError.message}
            </div>
          {/if}
          {#if workflowStopError?.projectId === project.id}
            <div
              class="text-status-failed px-2.5 pb-2 text-xs"
              data-testid="project-workflow-stop-error"
            >
              {workflowStopError.message}
            </div>
          {/if}
        </div>
      {/each}
    </div>
  </SidebarSection>
</SidebarPanel>
