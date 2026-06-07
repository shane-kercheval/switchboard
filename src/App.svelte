<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "$lib/api";
  import Banner from "$lib/components/Banner.svelte";
  import ComposeBar from "$lib/components/ComposeBar.svelte";
  import AddAgentModal from "$lib/components/AddAgentModal.svelte";
  import CreateAgentForm from "$lib/components/CreateAgentForm.svelte";
  import type { AgentFormSubmit } from "$lib/components/CreateAgentForm.types";
  import CreateProjectForm from "$lib/components/CreateProjectForm.svelte";
  import ProjectsSidebar from "$lib/components/ProjectsSidebar.svelte";
  import SettingsView from "$lib/components/SettingsView.svelte";
  import Sidebar from "$lib/components/Sidebar.svelte";
  import UnifiedTranscript from "$lib/components/UnifiedTranscript.svelte";
  import WelcomeScreen from "$lib/components/WelcomeScreen.svelte";
  import Dialog from "$lib/components/ui/Dialog.svelte";
  import Button from "$lib/components/ui/Button.svelte";
  import AppShell from "$lib/components/ui/AppShell.svelte";
  import EmptyState from "$lib/components/ui/EmptyState.svelte";
  import ErrorDetailsDialog from "$lib/components/ui/ErrorDetailsDialog.svelte";
  import SettingsButton from "$lib/components/ui/SettingsButton.svelte";
  import SidebarToggleButton from "$lib/components/ui/SidebarToggleButton.svelte";
  import DevIndicator from "$lib/components/ui/DevIndicator.svelte";
  import { windowDragRegion } from "$lib/windowDrag";
  import { hydrateAgent, registerAgent } from "$lib/state/index.svelte";
  import {
    activateProject,
    addAgentToActiveProject,
    agentCreationFailures,
    agentsByProject,
    conversations,
    dismissAgentCreationFailure,
    loadWorkspace,
    nextUnreadCompletedProjectId,
    projects,
    retryProjectHydration,
    selection,
    startProjectActivityObserver,
    workspace,
  } from "$lib/state/workspace.svelte";
  import type { AgentRecord, HarnessAvailability, HarnessBanner, HarnessKind } from "$lib/types";
  import { bannerCopy, bannerTestid } from "$lib/harnessAvailability";
  import { ALL_HARNESSES, HARNESS_LABEL } from "$lib/harnessDisplay";
  import { harnessAvailability, refreshHarnessAvailability } from "$lib/harnessAvailability.svelte";
  import { loadPreferences } from "$lib/preferences.svelte";
  import GitView from "$lib/components/GitView.svelte";
  import { view, setViewMode, enterGitView, revealProjectBranch } from "$lib/state/gitView.svelte";
  import {
    SEGMENTED_MAIN_CONTAINER_CLASS,
    SEGMENTED_MAIN_ITEM_ACTIVE_CLASS,
    SEGMENTED_MAIN_ITEM_CLASS,
    SEGMENTED_MAIN_ITEM_INACTIVE_CLASS,
  } from "$lib/components/ui/segmentedControl";
  import { cn } from "$lib/utils";

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
  let projectViewResumePending = $state<boolean>(false);
  let projectViewResumeSeq = 0;

  function isEditableShortcutTarget(target: EventTarget | null): boolean {
    if (!(target instanceof HTMLElement)) return false;
    return (
      target.isContentEditable ||
      target.tagName === "INPUT" ||
      target.tagName === "TEXTAREA" ||
      target.tagName === "SELECT"
    );
  }

  function isComposerShortcutTarget(target: EventTarget | null): boolean {
    return (
      target instanceof HTMLElement && target.closest('[data-shortcut-scope="composer"]') !== null
    );
  }

  function handleGlobalKeydown(event: KeyboardEvent): void {
    if (isEditableShortcutTarget(event.target) && !isComposerShortcutTarget(event.target)) return;

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
    } else if (key === "g" && event.shiftKey) {
      // ⌘⇧G toggles the top-level Projects ↔ Git view.
      event.preventDefault();
      selectView(view.mode === "git" ? "projects" : "git");
    } else if (key === "g") {
      event.preventDefault();
      selectNextUnreadCompletedProject();
    } else if (key === "f" && event.shiftKey) {
      event.preventDefault();
      void openActiveProjectInGit();
    } else if (key === "b" && event.shiftKey) {
      event.preventDefault();
      agentsSidebarOpen = !agentsSidebarOpen;
    } else if (key === "b") {
      event.preventDefault();
      projectsSidebarOpen = !projectsSidebarOpen;
    }
  }

  function selectNextUnreadCompletedProject(): void {
    const projectId = nextUnreadCompletedProjectId();
    if (projectId === null) return;
    settingsOpen = false;
    if (view.mode === "git") selectView("projects");
    void activateProject(projectId);
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

  // Switch the top-level view. Entering Git runs the staleness-gated refresh;
  // Settings is closed so the toggle always lands on the chosen view. Session-
  // only — never persisted (the app always opens to Projects).
  function selectView(mode: "projects" | "git"): void {
    settingsOpen = false;
    if (mode === "git") {
      projectViewResumePending = false;
      void enterGitView();
    } else {
      if (view.mode === "git" && selection.activeProjectId !== null) {
        showProjectViewLoadingForNextPaint();
      }
      setViewMode("projects");
    }
  }

  function showProjectViewLoadingForNextPaint(): void {
    const seq = ++projectViewResumeSeq;
    projectViewResumePending = true;
    const clear = (): void => {
      if (seq === projectViewResumeSeq) projectViewResumePending = false;
    };
    if (typeof requestAnimationFrame !== "function") {
      setTimeout(clear, 0);
      return;
    }
    requestAnimationFrame(() => setTimeout(clear, 0));
  }

  async function openActiveProjectInGit(): Promise<void> {
    if (activeProject === null) return;
    settingsOpen = false;
    const result = await revealProjectBranch(activeProject.id, activeProject.directory);
    if (result.kind === "failed") {
      console.warn("[switchboard] project git shortcut failed", result.message);
    } else if (result.kind === "unresolved") {
      console.warn("[switchboard] project git shortcut could not resolve a local branch");
    }
  }

  // Startup: kick off the harness install probe (the store writes each slice as
  // it resolves — no barrier) and eagerly load the workspace registry
  // (directory list + flat project list). Per-project rosters/hydration stay
  // lazy. Auth probes intentionally not called here — see the
  // `harnessAvailability` comment above.
  onMount(() => {
    const stopProjectActivityObserver = startProjectActivityObserver();
    void refreshHarnessAvailability();
    void loadPreferences();
    void loadWorkspace().catch((err) => {
      dirError = err instanceof Error ? err.message : String(err);
    });

    window.addEventListener("keydown", handleGlobalKeydown);
    return () => {
      stopProjectActivityObserver();
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
  const projectSwitching = $derived(
    selection.activeProjectId !== null && selection.loadingProjectId === selection.activeProjectId,
  );
  // The projects sidebar is a project picker — with no projects there's
  // nothing to pick, so it (and its re-open toggle) hide entirely; the
  // welcome screen carries the New/Add affordances and Settings moves to the
  // title bar. Exception: a workspace-persistability warning lives in the
  // sidebar, so keep it visible when that needs surfacing even with no
  // projects.
  const projectsSidebarHasContent = $derived(projects.list.length > 0 || !workspace.persistable);
  // The Git view is a full-width center-pane takeover (decision D1) — the
  // Projects sidebar hides while it's active and returns on toggle back.
  const projectsSidebarVisible = $derived(
    projectsSidebarOpen && projectsSidebarHasContent && view.mode !== "git",
  );

  function retryActivation(): void {
    if (selection.activeProjectId !== null) void activateProject(selection.activeProjectId);
  }

  // Verbatim-error dialog for the project-open failure (the center-pane
  // activation-error state). Mirrors the in-transcript Details affordance so a
  // user can copy the exact error into a bug report regardless of which
  // failure surface they hit.
  let activationDetailsOpen = $state<boolean>(false);

  // "Add project" dialog. The form (`CreateProjectForm`) owns both modes' state
  // and commits; App only tracks open/close and a `busy` flag the form drives so
  // the modal stays non-dismissible while a commit (esp. new-project agent
  // seeding) is in flight. The form remounts fresh on each open (Dialog unmounts
  // its body when closed), so there's no state to reset here.
  let projectDialogOpen = $state<boolean>(false);
  let projectDialogBusy = $state<boolean>(false);

  function openProjectDialog(): void {
    projectDialogOpen = true;
  }

  /// Create or attach an agent into the active project, register its listeners,
  /// and add it to the active roster. Attach kicks off per-agent hydration so
  /// the brought-in harness session's history appears.
  async function createOrAttachAndRegister(submission: AgentFormSubmit): Promise<void> {
    const agent =
      submission.mode === "create"
        ? await api.createAgent(
            submission.name,
            submission.harness,
            submission.model,
            submission.effort,
          )
        : await api.attachAgent(
            submission.name,
            submission.harness,
            submission.existingSessionId,
            submission.model,
            submission.effort,
          );
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
        view.mode !== "git" &&
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
        <!-- Dev-only build indicator, pinned to the far left of the header so
             it's visible in every state — including the no-project welcome,
             where there is no sidebar to host it. Renders nothing in
             production builds (self-gated on import.meta.env.DEV). -->
        <DevIndicator />
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
        {:else if view.mode === "git"}
          <div class="flex min-w-0 flex-1 items-center gap-2" data-testid="breadcrumb">
            <div class="text-fg truncate text-sm font-semibold">Git</div>
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

        <!-- Top-level view toggle: Projects | Git (⌘⇧G). Session-only; settings
             is a modal-over, so its toggle press lands on the chosen view. -->
        <div
          class={cn(SEGMENTED_MAIN_CONTAINER_CLASS, "flex shrink-0")}
          role="radiogroup"
          aria-label="View"
        >
          <button
            type="button"
            role="radio"
            class={cn(
              SEGMENTED_MAIN_ITEM_CLASS,
              !settingsOpen && view.mode === "projects"
                ? SEGMENTED_MAIN_ITEM_ACTIVE_CLASS
                : SEGMENTED_MAIN_ITEM_INACTIVE_CLASS,
            )}
            aria-checked={!settingsOpen && view.mode === "projects"}
            data-testid="view-toggle-projects"
            title="Projects (⌘⇧G)"
            onclick={() => selectView("projects")}
          >
            Projects
          </button>
          <button
            type="button"
            role="radio"
            class={cn(
              SEGMENTED_MAIN_ITEM_CLASS,
              !settingsOpen && view.mode === "git"
                ? SEGMENTED_MAIN_ITEM_ACTIVE_CLASS
                : SEGMENTED_MAIN_ITEM_INACTIVE_CLASS,
            )}
            aria-checked={!settingsOpen && view.mode === "git"}
            data-testid="view-toggle-git"
            title="Git (⌘⇧G)"
            onclick={() => selectView("git")}
          >
            Git
          </button>
        </div>
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
        {:else if view.mode === "git"}
          <GitView />
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
              <div class="flex items-center gap-2">
                <Button
                  variant="secondary"
                  size="sm"
                  data-testid="activation-retry"
                  onclick={retryActivation}
                >
                  Retry
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  data-testid="activation-details"
                  onclick={() => (activationDetailsOpen = true)}
                >
                  Details
                </Button>
              </div>
            {/snippet}
          </EmptyState>
        {:else if projectViewResumePending || projectSwitching || !rosterLoaded}
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
                loadError={activeConvo?.error}
                onRetryLoad={() => {
                  if (selection.activeProjectId !== null)
                    void retryProjectHydration(selection.activeProjectId);
                }}
              />
              <!-- Remount per project: besides re-seeding the per-project
                   draft/recipient state, this resets sendError, the @-menu, and
                   focus so one project's compose state can't bleed into another. -->
              {#key selection.activeProjectId}
                <ComposeBar projectId={selection.activeProjectId!} agents={activeAgents} />
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
    dismissible={!projectDialogBusy}
    onClose={() => (projectDialogOpen = false)}
  >
    <CreateProjectForm
      bind:busy={projectDialogBusy}
      onClose={() => (projectDialogOpen = false)}
      onCreated={() => (settingsOpen = false)}
    />
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

  <ErrorDetailsDialog
    bind:open={activationDetailsOpen}
    title="Couldn't open this project"
    message="Opening this project failed. The exact error is below — copy it into a bug report."
    details={selection.activationError ?? "No error detail was reported."}
  />
</main>
