<script lang="ts">
  import { onMount } from "svelte";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import * as api from "$lib/api";
  import Banner from "$lib/components/Banner.svelte";
  import ComposeBar from "$lib/components/ComposeBar.svelte";
  import AddAgentModal from "$lib/components/AddAgentModal.svelte";
  import CreateAgentForm from "$lib/components/CreateAgentForm.svelte";
  import type { AgentFormSubmit } from "$lib/components/CreateAgentForm.types";
  import ProjectsSidebar from "$lib/components/ProjectsSidebar.svelte";
  import SettingsView from "$lib/components/SettingsView.svelte";
  import Sidebar from "$lib/components/Sidebar.svelte";
  import UnifiedTranscript from "$lib/components/UnifiedTranscript.svelte";
  import WelcomeScreen from "$lib/components/WelcomeScreen.svelte";
  import Dialog from "$lib/components/ui/Dialog.svelte";
  import Input from "$lib/components/ui/Input.svelte";
  import Button from "$lib/components/ui/Button.svelte";
  import {
    SEGMENTED_CONTAINER_CLASS,
    SEGMENTED_ITEM_CLASS,
    SEGMENTED_ITEM_ACTIVE_CLASS,
    SEGMENTED_ITEM_INACTIVE_CLASS,
  } from "$lib/components/ui/segmentedControl";
  import AppShell from "$lib/components/ui/AppShell.svelte";
  import EmptyState from "$lib/components/ui/EmptyState.svelte";
  import SettingsButton from "$lib/components/ui/SettingsButton.svelte";
  import SidebarToggleButton from "$lib/components/ui/SidebarToggleButton.svelte";
  import { windowDragRegion } from "$lib/windowDrag";
  import { hydrateAgent, registerAgent } from "$lib/state/index.svelte";
  import {
    activateProject,
    addAgentToActiveProject,
    addDirectory,
    agentCreationFailures,
    agentsByProject,
    conversations,
    createProjectAndActivate,
    dismissAgentCreationFailure,
    loadWorkspace,
    projects,
    selection,
    workspace,
  } from "$lib/state/workspace.svelte";
  import type {
    AgentRecord,
    HarnessAvailability,
    HarnessBanner,
    HarnessKind,
    ProjectSummary,
  } from "$lib/types";
  import { bannerCopy, bannerTestid } from "$lib/harnessAvailability";
  import { ALL_HARNESSES, HARNESS_LABEL } from "$lib/harnessDisplay";
  import { harnessAvailability, refreshHarnessAvailability } from "$lib/harnessAvailability.svelte";
  import { basename, cn } from "$lib/utils";

  // One availability map keyed by harness, derived from the shared
  // `harnessAvailability` store (one probe also feeding the Supported-CLIs
  // list), so the banner stack and the create-form gating read the same source
  // — and a new harness needs no per-harness wiring here, just its entry in
  // `ALL_HARNESSES`. Auth is deliberately not tracked here: a logged-out harness
  // is discovered reactively on send, surfaced as an actionable transcript turn.
  const availability = $derived(
    Object.fromEntries(
      ALL_HARNESSES.map((h) => [h, harnessAvailability.availability(h)]),
    ) as Record<HarnessKind, HarnessAvailability>,
  );

  const banners = $derived.by((): HarnessBanner[] =>
    ALL_HARNESSES.map((h) => availability[h])
      .filter((a) => a.binary === "missing")
      .map((a) => ({ kind: "binary_missing" as const, harness: a.harness })),
  );

  let dirError = $state<string | null>(null);
  let projectsSidebarOpen = $state<boolean>(true);
  let agentsSidebarOpen = $state<boolean>(true);
  let settingsOpen = $state<boolean>(false);

  function isEditableShortcutTarget(target: EventTarget | null): boolean {
    if (!(target instanceof HTMLElement)) return false;
    return (
      target.isContentEditable ||
      target.tagName === "INPUT" ||
      target.tagName === "TEXTAREA" ||
      target.tagName === "SELECT"
    );
  }

  function handleGlobalKeydown(event: KeyboardEvent): void {
    if (isEditableShortcutTarget(event.target)) return;

    const command = event.metaKey || event.ctrlKey;
    if (!command) return;

    const key = event.key.toLowerCase();
    if (event.altKey) {
      if (event.code === "KeyB") {
        event.preventDefault();
        projectsSidebarOpen = !projectsSidebarOpen;
        agentsSidebarOpen = !agentsSidebarOpen;
      }
      return;
    }

    if (key === "," && !event.shiftKey) {
      event.preventDefault();
      toggleSettings();
    } else if (key === "b" && event.shiftKey) {
      event.preventDefault();
      agentsSidebarOpen = !agentsSidebarOpen;
    } else if (key === "b") {
      event.preventDefault();
      projectsSidebarOpen = !projectsSidebarOpen;
    }
  }

  function openSettings(): void {
    settingsOpen = true;
  }

  function closeSettings(): void {
    settingsOpen = false;
  }

  function toggleSettings(): void {
    if (settingsOpen) {
      closeSettings();
    } else {
      openSettings();
    }
  }

  // Startup: kick off the harness install probe (the store writes each slice as
  // it resolves — no barrier) and eagerly load the workspace registry
  // (directory list + flat project list). Per-project rosters/hydration stay
  // lazy. Auth probes intentionally not called here — see the
  // `harnessAvailability` comment above.
  onMount(() => {
    void refreshHarnessAvailability();
    void loadWorkspace().catch((err) => {
      dirError = err instanceof Error ? err.message : String(err);
    });

    window.addEventListener("keydown", handleGlobalKeydown);
    return () => {
      window.removeEventListener("keydown", handleGlobalKeydown);
    };
  });

  // The displayed project's roster + hydrated conversation. `rosterLoaded`
  // distinguishes "roster still loading on first activation" (key absent) from
  // "loaded and genuinely empty" (key present, length 0) so the first-agent
  // prompt doesn't flash before the roster resolves.
  const activeAgents = $derived<AgentRecord[]>(
    selection.activeProjectId !== null ? (agentsByProject[selection.activeProjectId] ?? []) : [],
  );
  const rosterLoaded = $derived(
    selection.activeProjectId !== null && selection.activeProjectId in agentsByProject,
  );
  const activeConvo = $derived(
    selection.activeProjectId !== null ? conversations[selection.activeProjectId] : undefined,
  );
  const activeProject = $derived(
    projects.list.find((p) => p.id === selection.activeProjectId) ?? null,
  );
  // The projects sidebar is a project picker — with no projects there's
  // nothing to pick, so it (and its re-open toggle) hide entirely; the
  // welcome screen carries the New/Add affordances and Settings moves to the
  // title bar. Exception: a workspace-persistability warning lives in the
  // sidebar, so keep it visible when that needs surfacing even with no
  // projects.
  const projectsSidebarHasContent = $derived(projects.list.length > 0 || !workspace.persistable);
  const projectsSidebarVisible = $derived(projectsSidebarOpen && projectsSidebarHasContent);

  function retryActivation(): void {
    if (selection.activeProjectId !== null) void activateProject(selection.activeProjectId);
  }

  // Folder picking is the one place a directory enters the model — an internal
  // detail of "new project" (where it lives) and "add existing" (where to look),
  // never a managed object in the UI.
  async function pickFolder(): Promise<string | null> {
    const result = await openDialog({ directory: true, multiple: false });
    return typeof result === "string" ? result : null;
  }

  // Combined "Add project" dialog — toggle between new and existing modes.
  let projectDialogOpen = $state<boolean>(false);
  let projectDialogMode = $state<"new" | "existing">("new");

  // New-project sub-state
  let newProjectFolder = $state<string | null>(null);
  let newProjectName = $state<string>("");
  let newProjectBusy = $state<boolean>(false);
  let newProjectError = $state<string | null>(null);

  // Add-existing sub-state
  let addExistingFolder = $state<string | null>(null);
  // `null` until a folder has been chosen; then the projects discovered in it
  // (empty array = none found). This is a *preview* read via the read-only
  // `pick_directory` probe — nothing is registered until "Add" commits.
  let addExistingFound = $state<ProjectSummary[] | null>(null);
  let addExistingBusy = $state<boolean>(false);
  let addExistingError = $state<string | null>(null);

  function openProjectDialog(): void {
    projectDialogMode = "new";
    newProjectFolder = null;
    newProjectName = "";
    newProjectError = null;
    addExistingFolder = null;
    addExistingFound = null;
    addExistingError = null;
    projectDialogOpen = true;
  }

  function switchProjectDialogMode(next: "new" | "existing"): void {
    if (next === projectDialogMode) return;
    projectDialogMode = next;
    // Reset the outgoing mode's state so stale results don't linger.
    if (next === "new") {
      addExistingFolder = null;
      addExistingFound = null;
      addExistingError = null;
    } else {
      newProjectFolder = null;
      newProjectName = "";
      newProjectError = null;
    }
  }

  const projectTabNewClass = $derived(
    cn(
      SEGMENTED_ITEM_CLASS,
      "flex-1",
      projectDialogMode === "new" ? SEGMENTED_ITEM_ACTIVE_CLASS : SEGMENTED_ITEM_INACTIVE_CLASS,
    ),
  );
  const projectTabExistingClass = $derived(
    cn(
      SEGMENTED_ITEM_CLASS,
      "flex-1",
      projectDialogMode === "existing"
        ? SEGMENTED_ITEM_ACTIVE_CLASS
        : SEGMENTED_ITEM_INACTIVE_CLASS,
    ),
  );

  async function chooseNewProjectFolder(): Promise<void> {
    const folder = await pickFolder();
    if (folder === null) return;
    newProjectFolder = folder;
    if (newProjectName.trim() === "") newProjectName = basename(folder);
  }

  const newProjectValid = $derived(newProjectFolder !== null && newProjectName.trim() !== "");

  async function submitNewProject(): Promise<void> {
    if (!newProjectValid || newProjectFolder === null) return;
    newProjectError = null;
    newProjectBusy = true;
    try {
      await createProjectAndActivate(newProjectName.trim(), newProjectFolder);
      projectDialogOpen = false;
      settingsOpen = false;
    } catch (err) {
      newProjectError = err instanceof Error ? err.message : String(err);
    } finally {
      newProjectBusy = false;
    }
  }

  // Pick a folder and *preview* the projects it holds via the read-only
  // `pick_directory` probe — nothing is registered or written to disk yet. The
  // commit happens in `confirmAddExisting` when the user presses "Add".
  async function chooseAddExistingFolder(): Promise<void> {
    const folder = await pickFolder();
    if (folder === null) return;
    // Discard any prior preview up front, so a failed probe leaves a clean
    // "nothing to add" state (Add disabled) rather than stranding the button on
    // the previously-picked folder.
    addExistingError = null;
    addExistingFolder = null;
    addExistingFound = null;
    addExistingBusy = true;
    try {
      const info = await api.pickDirectory(folder);
      // Use the canonical path the probe resolved, so "Add" commits the same
      // directory identity the backend keys on.
      addExistingFolder = info.path;
      addExistingFound = info.projects;
    } catch (err) {
      addExistingError = err instanceof Error ? err.message : String(err);
    } finally {
      addExistingBusy = false;
    }
  }

  // Commit the previewed folder: register it in the workspace (its projects join
  // the flat list) and close the dialog. Only reachable once the preview holds
  // at least one project.
  async function confirmAddExisting(): Promise<void> {
    if (addExistingFolder === null) return;
    addExistingError = null;
    addExistingBusy = true;
    try {
      await addDirectory(addExistingFolder);
      projectDialogOpen = false;
    } catch (err) {
      addExistingError = err instanceof Error ? err.message : String(err);
    } finally {
      addExistingBusy = false;
    }
  }

  /// Create or attach an agent into the active project, register its listeners,
  /// and add it to the active roster. Attach kicks off per-agent hydration so
  /// the brought-in harness session's history appears.
  async function createOrAttachAndRegister(submission: AgentFormSubmit): Promise<void> {
    const agent =
      submission.mode === "create"
        ? await api.createAgent(submission.name, submission.harness)
        : await api.attachAgent(submission.name, submission.harness, submission.existingSessionId);
    await registerAgent(agent);
    addAgentToActiveProject(agent);
    if (submission.mode === "attach") {
      void hydrateAgent(agent.id);
    }
  }

  // First-agent form (center, when the active project has no agents).
  let firstAgentBusy = $state<boolean>(false);
  let firstAgentError = $state<string | null>(null);

  async function handleCreateFirstAgent(submission: AgentFormSubmit): Promise<void> {
    firstAgentError = null;
    firstAgentBusy = true;
    try {
      await createOrAttachAndRegister(submission);
    } catch (err) {
      firstAgentError = err instanceof Error ? err.message : String(err);
    } finally {
      firstAgentBusy = false;
    }
  }

  // Add-agent modal (from the right sidebar, when agents already exist).
  let addAgentOpen = $state<boolean>(false);
  let addAgentError = $state<string | null>(null);
  let addAgentBusy = $state<boolean>(false);

  async function handleAddAgent(submission: AgentFormSubmit): Promise<void> {
    addAgentError = null;
    addAgentBusy = true;
    try {
      await createOrAttachAndRegister(submission);
      addAgentOpen = false;
    } catch (err) {
      addAgentError = err instanceof Error ? err.message : String(err);
    } finally {
      addAgentBusy = false;
    }
  }

  function openAddAgent(): void {
    addAgentError = null;
    addAgentOpen = true;
  }

  function handleAddAgentCancel(): void {
    addAgentOpen = false;
    addAgentError = null;
  }
</script>

{#snippet projectDialogBody()}
  <div class="space-y-4" data-testid="project-dialog">
    <div class={cn(SEGMENTED_CONTAINER_CLASS, "flex")} role="tablist">
      <button
        type="button"
        class={projectTabNewClass}
        role="tab"
        aria-selected={projectDialogMode === "new"}
        data-testid="project-dialog-mode-new"
        onclick={() => switchProjectDialogMode("new")}
        disabled={newProjectBusy || addExistingBusy}
      >
        New project
      </button>
      <button
        type="button"
        class={projectTabExistingClass}
        role="tab"
        aria-selected={projectDialogMode === "existing"}
        data-testid="project-dialog-mode-existing"
        onclick={() => switchProjectDialogMode("existing")}
        disabled={newProjectBusy || addExistingBusy}
      >
        Add existing
      </button>
    </div>

    {#if projectDialogMode === "new"}
      <div class="space-y-4" data-testid="new-project-form">
        <p class="text-muted text-sm leading-relaxed">
          Choose the folder you want to work in — typically your repo or working directory.
          Switchboard will initialize it as a new project there.
        </p>
        <div class="space-y-1.5">
          <span class="text-muted block text-xs">Folder</span>
          <Button
            variant="secondary"
            size="sm"
            data-testid="new-project-choose-folder"
            onclick={chooseNewProjectFolder}
          >
            Choose folder…
          </Button>
          {#if newProjectFolder}
            <p
              class="text-muted bg-panel truncate rounded px-2 py-1.5 font-mono text-xs"
              title={newProjectFolder}
            >
              {newProjectFolder}
            </p>
          {/if}
        </div>
        <div class="space-y-1.5">
          <label for="new-project-name" class="text-muted block text-xs">Name</label>
          <Input
            id="new-project-name"
            data-testid="new-project-name"
            placeholder="project name"
            bind:value={newProjectName}
            disabled={newProjectBusy}
            onkeydown={(e: KeyboardEvent) => {
              if (e.key === "Enter") void submitNewProject();
            }}
          />
        </div>
        {#if newProjectError}
          <p class="text-status-failed text-xs" data-testid="new-project-error">
            {newProjectError}
          </p>
        {/if}
        <div class="flex justify-end gap-2">
          <Button
            variant="secondary"
            size="sm"
            class="w-24"
            data-testid="new-project-cancel"
            disabled={newProjectBusy}
            onclick={() => (projectDialogOpen = false)}
          >
            Cancel
          </Button>
          <Button
            size="sm"
            class="w-24"
            data-testid="new-project-submit"
            disabled={!newProjectValid || newProjectBusy}
            onclick={submitNewProject}
          >
            Create
          </Button>
        </div>
      </div>
    {:else}
      <div class="space-y-4" data-testid="add-existing-form">
        <p class="text-muted text-sm leading-relaxed">
          Choose a folder you've already used with Switchboard — your repo or working directory (the
          one that contains a
          <code class="bg-panel text-fg rounded px-1 font-mono text-xs">.switchboard/</code>
          folder). Switchboard looks there for projects you've created before.
        </p>
        <Button
          variant="secondary"
          size="sm"
          data-testid="add-existing-choose-folder"
          disabled={addExistingBusy}
          onclick={chooseAddExistingFolder}
        >
          Choose folder…
        </Button>
        {#if addExistingError}
          <p class="text-status-failed text-xs" data-testid="add-existing-error">
            {addExistingError}
          </p>
        {:else if addExistingFound !== null}
          {#if addExistingFound.length > 0}
            <div class="space-y-1.5" data-testid="add-existing-found">
              <p class="text-fg text-sm">
                Found {addExistingFound.length}
                {addExistingFound.length === 1 ? "project" : "projects"} — these will be added:
              </p>
              <ul class="text-muted space-y-0.5 text-xs">
                {#each addExistingFound as found (found.id)}
                  <li class="truncate">{found.name}</li>
                {/each}
              </ul>
            </div>
          {:else}
            <p class="text-warning text-xs leading-relaxed" data-testid="add-existing-none">
              No Switchboard projects found in
              <span class="font-mono" title={addExistingFolder ?? ""}>{addExistingFolder}</span>.
              Make sure you picked the working directory that contains a
              <code class="bg-panel text-fg rounded px-1 font-mono">.switchboard/</code>
              folder — or switch to "New project" to create one there.
            </p>
          {/if}
        {/if}
        <div class="flex justify-end gap-2">
          <Button
            variant="secondary"
            size="sm"
            class="w-24"
            data-testid="add-existing-cancel"
            disabled={addExistingBusy}
            onclick={() => (projectDialogOpen = false)}
          >
            Cancel
          </Button>
          <Button
            size="sm"
            class="w-24"
            data-testid="add-existing-add"
            disabled={addExistingBusy || addExistingFound === null || addExistingFound.length === 0}
            onclick={confirmAddExisting}
          >
            {addExistingBusy ? "Adding…" : "Add"}
          </Button>
        </div>
      </div>
    {/if}
  </div>
{/snippet}

<main class="bg-surface text-fg flex h-full flex-col">
  <AppShell centerTestid="workspace-main">
    {#snippet left()}
      {#if projectsSidebarVisible}
        <ProjectsSidebar
          onAddProject={openProjectDialog}
          onOpenSettings={toggleSettings}
          onProjectSelect={() => (settingsOpen = false)}
          onToggleSidebar={() => (projectsSidebarOpen = false)}
          {settingsOpen}
        />
      {/if}
    {/snippet}

    {#snippet center()}
      {@const showAgentsToggle =
        !settingsOpen &&
        selection.activeProjectId !== null &&
        rosterLoaded &&
        activeAgents.length > 0}
      <!--
        One title bar spanning the center pane, draggable. When the projects
        sidebar is collapsed there is no left column, so this bar absorbs the
        traffic-light clearance + the re-open and settings controls — the title bar
        then extends edge-to-edge like a native window. `pl-20` clears the macOS
        traffic lights positioned at {x:16} in tauri.conf.json; keep the two in
        sync if that position changes.
      -->
      <div
        class="border-border/80 bg-raised flex h-11 shrink-0 items-center gap-2 border-b pr-3 {projectsSidebarVisible
          ? 'pl-4'
          : 'pl-20'}"
        data-tauri-drag-region
        use:windowDragRegion
      >
        <!-- Title-bar Settings + re-open toggle appear only when the sidebar
             has content but is collapsed. In the no-project state there's no
             sidebar at all, so neither shows — the welcome screen stays clean. -->
        {#if projectsSidebarHasContent && !projectsSidebarOpen}
          <SettingsButton
            pressed={settingsOpen}
            testid="settings-button"
            onclick={toggleSettings}
          />
          <SidebarToggleButton
            side="left"
            expanded={false}
            label="Show projects sidebar"
            testid="projects-sidebar-toggle"
            onclick={() => (projectsSidebarOpen = true)}
          />
        {/if}
        {#if settingsOpen}
          <div class="flex min-w-0 flex-1 items-center gap-2" data-testid="breadcrumb">
            <div class="text-fg truncate text-sm font-semibold">Settings</div>
          </div>
        {:else if activeProject}
          <div class="flex min-w-0 flex-1 items-center gap-2" data-testid="breadcrumb">
            <div class="text-fg truncate text-sm font-semibold">{activeProject.name}</div>
            <div class="text-muted shrink-0 text-xs">·</div>
            <div class="text-muted truncate text-xs" title={activeProject.directory}>
              {activeProject.directory}
            </div>
          </div>
        {:else}
          <div class="flex-1"></div>
        {/if}
        {#if showAgentsToggle}
          <SidebarToggleButton
            side="right"
            expanded={agentsSidebarOpen}
            label={agentsSidebarOpen ? "Hide agents sidebar" : "Show agents sidebar"}
            testid="agents-sidebar-toggle"
            onclick={() => (agentsSidebarOpen = !agentsSidebarOpen)}
            class="hover:bg-panel"
          />
        {/if}
      </div>

      {#each banners as banner (bannerTestid(banner))}
        <Banner message={bannerCopy(banner)} testid={bannerTestid(banner)} />
      {/each}

      {#each agentCreationFailures as failure (failure.harness)}
        <Banner
          message={`Couldn't create the ${HARNESS_LABEL[failure.harness]} agent: ${failure.error}`}
          testid={`banner-agent-create-failed-${failure.harness}`}
          onDismiss={() => dismissAgentCreationFailure(failure.harness)}
        />
      {/each}

      <div class="flex min-h-0 flex-1 flex-col overflow-hidden">
        {#if settingsOpen}
          <SettingsView onClose={closeSettings} />
        {:else if selection.activeProjectId === null}
          <!-- Every no-project state shows the same orientation surface
               (what Switchboard is, the project/agent explainer, the CTAs, and
               the harness panel). When projects already exist they remain in
               the sidebar list as the selection affordance. -->
          <div class="flex h-full flex-col items-center overflow-y-auto px-8 pt-6 pb-8">
            <div class="w-full max-w-2xl pb-6">
              <WelcomeScreen onAddProject={openProjectDialog} />
            </div>
          </div>
        {:else if selection.activationError !== null}
          <EmptyState
            testid="activation-error"
            tone="error"
            title="Couldn't open this project."
            description={selection.activationError}
          >
            {#snippet action()}
              <Button
                variant="secondary"
                size="sm"
                data-testid="activation-retry"
                onclick={retryActivation}
              >
                Retry
              </Button>
            {/snippet}
          </EmptyState>
        {:else if !rosterLoaded}
          <EmptyState testid="project-loading" title="Loading project…" />
        {:else if activeAgents.length === 0}
          <div class="flex flex-1 flex-col overflow-y-auto">
            <CreateAgentForm
              busy={firstAgentBusy}
              error={firstAgentError}
              onSubmit={handleCreateFirstAgent}
              roster={activeAgents}
              {availability}
            />
          </div>
        {:else}
          <div class="flex min-h-0 flex-1 overflow-hidden">
            <div class="flex min-w-0 flex-1 flex-col overflow-hidden">
              <UnifiedTranscript
                agents={activeAgents}
                overlay={activeConvo?.items ?? []}
                loadStatus={activeConvo?.status ?? "complete"}
              />
              {#key selection.activeProjectId}
                <ComposeBar agents={activeAgents} />
              {/key}
            </div>
            {#if agentsSidebarOpen}
              <Sidebar agents={activeAgents} onAddAgent={openAddAgent} />
            {/if}
          </div>
        {/if}
      </div>
    {/snippet}
  </AppShell>

  {#if dirError}
    <p class="border-border text-status-failed border-t px-4 py-2 text-xs" data-testid="error">
      {dirError}
    </p>
  {/if}

  <Dialog
    bind:open={projectDialogOpen}
    title="Add project"
    dismissible={!newProjectBusy && !addExistingBusy}
    onClose={() => (projectDialogOpen = false)}
  >
    {@render projectDialogBody()}
  </Dialog>

  <AddAgentModal
    bind:open={addAgentOpen}
    busy={addAgentBusy}
    error={addAgentError}
    roster={activeAgents}
    {availability}
    onSubmit={handleAddAgent}
    onCancel={handleAddAgentCancel}
  />
</main>
