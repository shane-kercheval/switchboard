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
  } from "$lib/state/workspace.svelte";
  import type { AgentRecord, BinaryState, HarnessAvailability, HarnessBanner } from "$lib/types";
  import { bannerCopy, bannerTestid } from "$lib/harnessAvailability";
  import { basename } from "$lib/utils";

  // --- Per-harness availability probes (unchanged from the single-directory
  // model; see the flat-field rationale below). ---
  let claudeBinary = $state<BinaryState>("checking");
  let codexBinary = $state<BinaryState>("checking");
  let codexAuth = $state<"available" | "missing" | "checking">("checking");
  let geminiBinary = $state<BinaryState>("checking");
  let geminiAuth = $state<"available" | "missing" | "checking">("checking");
  let antigravityBinary = $state<BinaryState>("checking");
  let antigravityAuth = $state<"available" | "missing" | "checking">("checking");

  const claudeAvailability = $derived<HarnessAvailability>({
    harness: "claude_code",
    binary: claudeBinary,
    auth: "unsupported",
  });
  const codexAvailability = $derived<HarnessAvailability>({
    harness: "codex",
    binary: codexBinary,
    auth: codexAuth,
  });
  const geminiAvailability = $derived<HarnessAvailability>({
    harness: "gemini",
    binary: geminiBinary,
    auth: geminiAuth,
  });
  const antigravityAvailability = $derived<HarnessAvailability>({
    harness: "antigravity",
    binary: antigravityBinary,
    auth: antigravityAuth,
  });

  const banners = $derived.by((): HarnessBanner[] => {
    const result: HarnessBanner[] = [];
    for (const a of [
      claudeAvailability,
      codexAvailability,
      geminiAvailability,
      antigravityAvailability,
    ]) {
      if (a.binary === "missing") {
        result.push({ kind: "binary_missing", harness: a.harness });
      } else if (
        a.auth === "missing" &&
        (a.harness === "codex" || a.harness === "gemini" || a.harness === "antigravity")
      ) {
        result.push({ kind: "auth_missing", harness: a.harness });
      }
    }
    return result;
  });

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
    if (!command || event.altKey) return;

    const key = event.key.toLowerCase();
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

  // Startup: kick off the harness probes (each writes its own slice as it
  // resolves — no barrier) and eagerly load the workspace registry (directory
  // list + flat project list). Per-project rosters/hydration stay lazy.
  onMount(() => {
    api.checkClaudeBinary().then(
      () => (claudeBinary = "available"),
      () => (claudeBinary = "missing"),
    );
    api.checkCodexBinary().then(
      () => (codexBinary = "available"),
      () => (codexBinary = "missing"),
    );
    api.checkCodexAuth().then(
      () => (codexAuth = "available"),
      () => (codexAuth = "missing"),
    );
    api.checkGeminiBinary().then(
      () => (geminiBinary = "available"),
      () => (geminiBinary = "missing"),
    );
    api.checkGeminiAuth().then(
      () => (geminiAuth = "available"),
      () => (geminiAuth = "missing"),
    );
    api.checkAntigravityBinary().then(
      () => (antigravityBinary = "available"),
      () => (antigravityBinary = "missing"),
    );
    api.checkAntigravityAuth().then(
      () => (antigravityAuth = "available"),
      () => (antigravityAuth = "missing"),
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

  // Add existing: point at a folder; every Switchboard project already in it
  // flows into the flat list.
  async function handleAddExisting(): Promise<void> {
    dirError = null;
    const folder = await pickFolder();
    if (folder === null) return;
    try {
      await addDirectory(folder);
    } catch (err) {
      dirError = err instanceof Error ? err.message : String(err);
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
      {#if projectsSidebarOpen}
        <ProjectsSidebar
          onNewProject={openNewProject}
          onAddExisting={handleAddExisting}
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
        class="border-border/80 bg-raised flex h-11 shrink-0 items-center gap-2 border-b pr-3 {projectsSidebarOpen
          ? 'pl-4'
          : 'pl-20'}"
        data-tauri-drag-region
        use:windowDragRegion
      >
        {#if !projectsSidebarOpen}
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
          {#if projects.list.length === 0}
            <WelcomeScreen onNewProject={openNewProject} onAddExisting={handleAddExisting} />
          {:else}
            <EmptyState title="Select a project." />
          {/if}
        {:else if selection.activationError !== null}
          <EmptyState
            testid="activation-error"
            tone="error"
            title="Couldn't open this project."
            description={selection.activationError}
          >
            {#snippet action()}
              <Button variant="secondary" data-testid="activation-retry" onclick={retryActivation}>
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
          data-testid="new-project-submit"
          disabled={!newProjectValid || newProjectBusy}
          onclick={submitNewProject}
        >
          Create
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
