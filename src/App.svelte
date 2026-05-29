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
    agentsByProject,
    conversations,
    createProjectAndActivate,
    loadWorkspace,
    projects,
    selection,
    workspace,
  } from "$lib/state/workspace.svelte";
  import type {
    AgentRecord,
    BinaryState,
    HarnessAvailability,
    HarnessBanner,
    ProjectListing,
  } from "$lib/types";
  import { bannerCopy, bannerTestid } from "$lib/harnessAvailability";
  import { basename } from "$lib/utils";

  // Per-harness binary-presence probes. Auth is deliberately not tracked
  // in this surface — a logged-out harness is discovered reactively when
  // the user sends, and the failed turn carries an authored actionable
  // message in the transcript. The backend `check_*_auth` Tauri commands
  // exist for the getting-started surface (no-project state) to consume,
  // not the working UI.
  let claudeBinary = $state<BinaryState>("checking");
  let codexBinary = $state<BinaryState>("checking");
  let geminiBinary = $state<BinaryState>("checking");
  let antigravityBinary = $state<BinaryState>("checking");

  const claudeAvailability = $derived<HarnessAvailability>({
    harness: "claude_code",
    binary: claudeBinary,
  });
  const codexAvailability = $derived<HarnessAvailability>({
    harness: "codex",
    binary: codexBinary,
  });
  const geminiAvailability = $derived<HarnessAvailability>({
    harness: "gemini",
    binary: geminiBinary,
  });
  const antigravityAvailability = $derived<HarnessAvailability>({
    harness: "antigravity",
    binary: antigravityBinary,
  });

  const banners = $derived.by((): HarnessBanner[] =>
    [claudeAvailability, codexAvailability, geminiAvailability, antigravityAvailability]
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

  // Startup: kick off the per-harness binary probes (each writes its own slice
  // as it resolves — no barrier) and eagerly load the workspace registry
  // (directory list + flat project list). Per-project rosters/hydration stay
  // lazy. Auth probes intentionally not called here — see `claudeBinary` /
  // `codexBinary` / etc. comment above.
  onMount(() => {
    api.checkClaudeBinary().then(
      () => (claudeBinary = "available"),
      () => (claudeBinary = "missing"),
    );
    api.checkCodexBinary().then(
      () => (codexBinary = "available"),
      () => (codexBinary = "missing"),
    );
    api.checkGeminiBinary().then(
      () => (geminiBinary = "available"),
      () => (geminiBinary = "missing"),
    );
    api.checkAntigravityBinary().then(
      () => (antigravityBinary = "available"),
      () => (antigravityBinary = "missing"),
    );
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

  // Add existing: a dialog first explains *what* to select (the working
  // directory, not the `.switchboard/` folder), then reports what was found so
  // a no-match doesn't look like a silent success.
  let addExistingOpen = $state<boolean>(false);
  let addExistingFolder = $state<string | null>(null);
  // `null` until a folder has been chosen; then the projects discovered in it
  // (empty array = none found).
  let addExistingFound = $state<ProjectListing[] | null>(null);
  let addExistingBusy = $state<boolean>(false);
  let addExistingError = $state<string | null>(null);

  function openAddExisting(): void {
    addExistingFolder = null;
    addExistingFound = null;
    addExistingError = null;
    addExistingOpen = true;
  }

  async function chooseAddExistingFolder(): Promise<void> {
    const folder = await pickFolder();
    if (folder === null) return;
    addExistingError = null;
    addExistingBusy = true;
    try {
      await addDirectory(folder);
      // `addDirectory` refreshes the workspace; surface what now lives in the
      // chosen folder straight from the flat list (same source as the sidebar).
      addExistingFolder = folder;
      addExistingFound = projects.list.filter((p) => p.directory === folder);
    } catch (err) {
      addExistingError = err instanceof Error ? err.message : String(err);
    } finally {
      addExistingBusy = false;
    }
  }

  // New project: a small modal collects the target folder + a name.
  let newProjectOpen = $state<boolean>(false);
  let newProjectFolder = $state<string | null>(null);
  let newProjectName = $state<string>("");
  let newProjectBusy = $state<boolean>(false);
  let newProjectError = $state<string | null>(null);

  function openNewProject(): void {
    newProjectFolder = null;
    newProjectName = "";
    newProjectError = null;
    newProjectOpen = true;
  }

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
      newProjectOpen = false;
      settingsOpen = false;
    } catch (err) {
      newProjectError = err instanceof Error ? err.message : String(err);
    } finally {
      newProjectBusy = false;
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

<main class="bg-surface text-fg flex h-full flex-col">
  <AppShell centerTestid="workspace-main">
    {#snippet left()}
      {#if projectsSidebarVisible}
        <ProjectsSidebar
          onNewProject={openNewProject}
          onAddExisting={openAddExisting}
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
              <WelcomeScreen onNewProject={openNewProject} onAddExisting={openAddExisting} />
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
              {claudeAvailability}
              {codexAvailability}
              {geminiAvailability}
              {antigravityAvailability}
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

  <Dialog bind:open={newProjectOpen} title="New project" onClose={() => (newProjectOpen = false)}>
    <div class="space-y-4" data-testid="new-project-form">
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
        <p class="text-status-failed text-xs" data-testid="new-project-error">{newProjectError}</p>
      {/if}
      <div class="flex justify-end">
        <Button
          size="sm"
          data-testid="new-project-submit"
          disabled={!newProjectValid || newProjectBusy}
          onclick={submitNewProject}
        >
          Create
        </Button>
      </div>
    </div>
  </Dialog>

  <Dialog
    bind:open={addExistingOpen}
    title="Add an existing project"
    onClose={() => (addExistingOpen = false)}
  >
    <div class="space-y-4" data-testid="add-existing-form">
      <p class="text-muted text-sm leading-relaxed">
        Choose a folder you've already used with Switchboard — your repo or working directory (the
        one that contains a
        <code class="bg-panel text-fg rounded px-1 font-mono text-xs">.switchboard/</code>
        folder), not the
        <code class="bg-panel text-fg rounded px-1 font-mono text-xs">.switchboard/</code> folder itself.
        Switchboard looks there for projects you've created before.
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
              {addExistingFound.length === 1 ? "project" : "projects"} — added to your list.
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
            <span class="font-mono" title={addExistingFolder}>{addExistingFolder}</span>. Make sure
            you picked the working directory that contains a
            <code class="bg-panel text-fg rounded px-1 font-mono">.switchboard/</code>
            folder — or start a new project there with “New project”.
          </p>
        {/if}
      {/if}

      <div class="flex justify-end">
        <Button
          variant="secondary"
          size="sm"
          data-testid="add-existing-done"
          disabled={addExistingBusy || (addExistingFound === null && addExistingError === null)}
          onclick={() => (addExistingOpen = false)}
        >
          Done
        </Button>
      </div>
    </div>
  </Dialog>

  <AddAgentModal
    bind:open={addAgentOpen}
    busy={addAgentBusy}
    error={addAgentError}
    {claudeAvailability}
    {codexAvailability}
    {geminiAvailability}
    {antigravityAvailability}
    onSubmit={handleAddAgent}
    onCancel={handleAddAgentCancel}
  />
</main>
