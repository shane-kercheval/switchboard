<script lang="ts">
  import { onMount, tick, untrack } from "svelte";
  import * as api from "$lib/api";
  import Banner from "$lib/components/Banner.svelte";
  import ComposeBar from "$lib/components/ComposeBar.svelte";
  import AddAgentModal from "$lib/components/AddAgentModal.svelte";
  import CreateAgentForm from "$lib/components/CreateAgentForm.svelte";
  import type { AgentFormSubmit } from "$lib/components/CreateAgentForm.types";
  import CreateProjectForm from "$lib/components/CreateProjectForm.svelte";
  import CommandPalette from "$lib/components/CommandPalette.svelte";
  import ProjectsSidebar from "$lib/components/ProjectsSidebar.svelte";
  import SettingsView from "$lib/components/SettingsView.svelte";
  import Sidebar from "$lib/components/Sidebar.svelte";
  import TranscriptPanes from "$lib/components/TranscriptPanes.svelte";
  import WelcomeScreen from "$lib/components/WelcomeScreen.svelte";
  import Dialog from "$lib/components/ui/Dialog.svelte";
  import Button from "$lib/components/ui/Button.svelte";
  import AppShell from "$lib/components/ui/AppShell.svelte";
  import EmptyState from "$lib/components/ui/EmptyState.svelte";
  import SidebarPanel from "$lib/components/ui/SidebarPanel.svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import ErrorDetailsDialog from "$lib/components/ui/ErrorDetailsDialog.svelte";
  import SettingsButton from "$lib/components/ui/SettingsButton.svelte";
  import CommandPaletteButton from "$lib/components/ui/CommandPaletteButton.svelte";
  import SidebarToggleButton from "$lib/components/ui/SidebarToggleButton.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { ICON_BUTTON_CLASS, ICON_SIZE } from "$lib/components/ui/iconButton";
  import { ChevronsDownUp, ChevronsUpDown, CircleCheck, Plus } from "@lucide/svelte";
  import {
    hasOverrides,
    normalizeProjectCompact,
    stateFor,
  } from "$lib/state/transcriptPreview.svelte";
  import {
    createEmptyPane,
    layoutFor,
    paneToCycleTo,
    restoreMaximizedPane,
    revealPane,
    type TranscriptPane,
  } from "$lib/state/transcriptPanes.svelte";
  import { selectionFor, targetRecipients } from "$lib/state/recipientSelection.svelte";
  import DevIndicator from "$lib/components/ui/DevIndicator.svelte";
  import { installDevTranscriptSeed } from "$lib/dev/seedTranscript";
  import { windowDragRegion } from "$lib/windowDrag";
  import { agentIsWorking, hydrateAgent, registerAgent, runtimes } from "$lib/state/index.svelte";
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
    setProjectArchived,
    startProjectActivityObserver,
    workspace,
  } from "$lib/state/workspace.svelte";
  import {
    contributedCommands,
    palette,
    togglePalette,
    type Command,
  } from "$lib/state/commandPalette.svelte";
  import type { AgentRecord, HarnessAvailability, HarnessKind, ProjectId } from "$lib/types";
  import { ALL_HARNESSES, HARNESS_LABEL } from "$lib/harnessDisplay";
  import { harnessAvailability, refreshHarnessAvailability } from "$lib/harnessAvailability.svelte";
  import { loadPreferences } from "$lib/preferences.svelte";
  import GitView from "$lib/components/GitView.svelte";
  import {
    view,
    setViewMode,
    enterGitView,
    revealProjectBranch,
    selectedWorktreePathForEditor,
  } from "$lib/state/gitView.svelte";
  import {
    SEGMENTED_MAIN_CONTAINER_CLASS,
    SEGMENTED_MAIN_ITEM_ACTIVE_CLASS,
    SEGMENTED_MAIN_ITEM_CLASS,
    SEGMENTED_MAIN_ITEM_INACTIVE_CLASS,
  } from "$lib/components/ui/segmentedControl";
  import { cn } from "$lib/utils";
  import { isEditableShortcutTarget } from "$lib/keyboard";

  // One availability map keyed by harness, derived from the shared
  // `harnessAvailability` store (one probe also feeding the Supported-CLIs
  // list), so the status list and create-form gating read the same source — and
  // a new harness needs no per-harness wiring here, just its entry in
  // `ALL_HARNESSES`. Auth is deliberately not tracked here: a logged-out harness
  // is discovered reactively on send, surfaced as an actionable transcript turn.
  const availability = $derived(
    Object.fromEntries(
      ALL_HARNESSES.map((h) => [h, harnessAvailability.availability(h)]),
    ) as Record<HarnessKind, HarnessAvailability>,
  );

  let dirError = $state<string | null>(null);
  let projectsSidebarOpen = $state<boolean>(true);
  let agentsSidebarOpen = $state<boolean>(true);
  let settingsOpen = $state<boolean>(false);
  let editorShortcutError = $state<string | null>(null);
  let editorShortcutSeq = 0;
  let commandError = $state<string | null>(null);
  let projectViewResumePending = $state<boolean>(false);
  let projectViewResumeSeq = 0;
  let gitViewResumePending = $state<boolean>(false);
  let gitViewResumeSeq = 0;

  function isComposerShortcutTarget(target: EventTarget | null): boolean {
    return (
      target instanceof HTMLElement && target.closest('[data-shortcut-scope="composer"]') !== null
    );
  }

  function handleGlobalKeydown(event: KeyboardEvent): void {
    // ⌘⇧P opens/closes the command palette from anywhere, including inside an
    // input — it's the one shortcut that must override the editable-target guard
    // so it's always reachable.
    if (
      (event.metaKey || event.ctrlKey) &&
      event.shiftKey &&
      !event.altKey &&
      event.key.toLowerCase() === "p"
    ) {
      event.preventDefault();
      togglePalette();
      return;
    }
    // While the palette is open it owns the keyboard (its own input handles
    // navigation/Escape); suppress every other window-level shortcut so a chord
    // typed into the palette doesn't also fire its global action.
    if (palette.open) return;

    if (isEditableShortcutTarget(event.target) && !isComposerShortcutTarget(event.target)) return;

    const command = event.metaKey || event.ctrlKey;
    if (!command) return;

    const key = event.key.toLowerCase();
    if (event.altKey) {
      if (event.code === "KeyB") {
        event.preventDefault();
        projectsSidebarOpen = !projectsSidebarOpen;
        agentsSidebarOpen = !agentsSidebarOpen;
      } else if (/^Digit[1-9]$/.test(event.code)) {
        // ⌘⌥1..N targets pane N (leftmost = 1): replace the compose recipient
        // set with that pane's members. `event.code`, not `event.key` — Option
        //+number on macOS produces a different character in `key`. Inert with
        // a single pane (nothing to disambiguate); ⌘1..9 (no Alt) stays the
        // per-agent chip toggle in ComposeBar.
        if (selection.activeProjectId === null || settingsOpen || view.mode === "git") return;
        const rosterIds = activeAgents.map((a) => a.id);
        const layout = layoutFor(selection.activeProjectId, rosterIds);
        if (layout.panes.length < 2) return;
        const pane = layout.panes[Number(event.code.slice(5)) - 1];
        // An empty pane keeps its positional number but is not a send target
        // (targeting it could only clear the recipient set, silently).
        if (pane === undefined || pane.members.length === 0) return;
        event.preventDefault();
        // Targeting also reveals: a minimized (or maximized-over) pane would
        // otherwise receive the send invisibly. Reveal is gated on the target
        // write so the gesture is atomic under the prompt-render targeting
        // lock — a refused chord must not change pane visibility either.
        if (targetRecipients(selection.activeProjectId, [...pane.members])) {
          revealPane(selection.activeProjectId, rosterIds, pane.id);
        }
      }
      return;
    }

    if (event.shiftKey && (event.code === "BracketLeft" || event.code === "BracketRight")) {
      // ⌘⇧[ / ⌘⇧] cycle the targeted pane by position (left/right, wrapping),
      // like switching browser/terminal tabs. `event.code`, not `event.key` —
      // Shift+bracket produces "{"/"}" in `key`.
      if (selection.activeProjectId === null || settingsOpen || view.mode === "git") return;
      event.preventDefault();
      cyclePane(event.code === "BracketRight" ? 1 : -1);
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
    } else if (key === "e" && event.shiftKey) {
      event.preventDefault();
      void openSelectionInEditor();
    } else if (key === "b" && event.shiftKey) {
      event.preventDefault();
      agentsSidebarOpen = !agentsSidebarOpen;
    } else if (key === "b") {
      event.preventDefault();
      projectsSidebarOpen = !projectsSidebarOpen;
    } else if (key === "n" && event.shiftKey) {
      event.preventDefault();
      if (hasActiveProject) openAddAgent();
    } else if (key === "n") {
      // ⌘N is contextual. While the Git view is showing it adds a repo (handled
      // by GitView's own keydown handler); everywhere else it adds a project.
      if (!(view.mode === "git" && !settingsOpen)) {
        event.preventDefault();
        openProjectDialog();
      }
    }
  }

  async function openSelectionInEditor(): Promise<void> {
    const seq = ++editorShortcutSeq;
    editorShortcutError = null;
    const path =
      view.mode === "git" ? selectedWorktreePathForEditor() : (activeProject?.directory ?? null);
    if (path === null) return;
    try {
      await api.openInEditor(path);
    } catch (e) {
      if (seq !== editorShortcutSeq) return;
      editorShortcutError = e instanceof Error ? e.message : String(e);
      console.warn("[switchboard] open in editor shortcut failed", e);
    }
  }

  async function openActiveProjectInTerminal(): Promise<void> {
    if (activeProject === null) return;
    commandError = null;
    try {
      await api.openInTerminal(activeProject.directory);
    } catch (e) {
      commandError = e instanceof Error ? e.message : String(e);
    }
  }

  async function revealActiveProjectInFinder(): Promise<void> {
    if (activeProject === null) return;
    commandError = null;
    try {
      await api.revealInFinder(activeProject.directory);
    } catch (e) {
      commandError = e instanceof Error ? e.message : String(e);
    }
  }

  async function toggleArchiveActiveProject(): Promise<void> {
    if (activeProject === null) return;
    commandError = null;
    try {
      await setProjectArchived(activeProject.id, !activeProject.archived);
    } catch (e) {
      commandError = e instanceof Error ? e.message : String(e);
    }
  }

  function switchToProject(projectId: ProjectId): void {
    settingsOpen = false;
    if (view.mode === "git") selectView("projects");
    void activateProject(projectId);
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
      // Entering Git renders the full repos→branches tree in one synchronous
      // flush, which blocks the paint after the toggle (the old view appears to
      // hang). Show a spinner shell for one paint first, mirroring the project
      // side below, so the switch is felt immediately.
      if (view.mode !== "git") showGitViewLoadingForNextPaint();
      void enterGitView();
    } else {
      gitViewResumePending = false;
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

  function showGitViewLoadingForNextPaint(): void {
    const seq = ++gitViewResumeSeq;
    gitViewResumePending = true;
    const clear = (): void => {
      if (seq === gitViewResumeSeq) gitViewResumePending = false;
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
    const removeDevSeed = installDevTranscriptSeed(() => activeAgents);
    return () => {
      stopProjectActivityObserver();
      window.removeEventListener("keydown", handleGlobalKeydown);
      removeDevSeed();
    };
  });

  // The displayed project's roster + hydrated conversation. `rosterLoaded`
  // distinguishes "roster still loading on first activation" (key absent) from
  // "loaded and genuinely empty" (key present, length 0) so the first-agent
  // prompt doesn't flash before the roster resolves.
  const activeAgents = $derived<AgentRecord[]>(
    selection.activeProjectId !== null ? (agentsByProject[selection.activeProjectId] ?? []) : [],
  );
  const activeRosterIds = $derived(activeAgents.map((a) => a.id));
  const rosterLoaded = $derived(
    selection.activeProjectId !== null && selection.activeProjectId in agentsByProject,
  );
  const activePaneLayout = $derived(
    selection.activeProjectId !== null
      ? layoutFor(selection.activeProjectId, activeRosterIds)
      : null,
  );
  const activeMaximizedPane = $derived(
    activePaneLayout?.maximized === null || activePaneLayout === null
      ? null
      : (activePaneLayout.panes.find((pane) => pane.id === activePaneLayout.maximized) ?? null),
  );
  const headerTabPanes = $derived(
    activePaneLayout === null
      ? []
      : activePaneLayout.panes.filter((pane) =>
          activeMaximizedPane !== null
            ? pane.id !== activeMaximizedPane.id
            : activePaneLayout.minimized.includes(pane.id),
        ),
  );
  const activeConvo = $derived(
    selection.activeProjectId !== null ? conversations[selection.activeProjectId] : undefined,
  );
  const activeProject = $derived(
    projects.list.find((p) => p.id === selection.activeProjectId) ?? null,
  );
  // Single enablement predicate shared by the ⌘⇧N keyboard guard and the
  // "Add agent" palette command's `disabled`, so the two can't disagree on
  // whether the action is available. (The command registry and the keyboard
  // handler remain parallel dispatch paths for now — undecided whether the
  // registry becomes the canonical dispatch surface later.)
  const hasActiveProject = $derived(activeProject !== null);
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
  const showPaneHeaderControls = $derived(
    !settingsOpen &&
      view.mode !== "git" &&
      selection.activeProjectId !== null &&
      rosterLoaded &&
      activeAgents.length > 0,
  );

  // Compact-transcript header control. The action is a normalize, not a blind
  // invert: with manual per-unit overrides present it resets (enable compact +
  // clear overrides); otherwise it inverts the project's compact mode. Label and
  // icon reflect that so the control reads as reset / compact / expand.
  const compactEnabled = $derived(
    selection.activeProjectId !== null && stateFor(selection.activeProjectId).enabled,
  );
  const compactHasOverrides = $derived(
    selection.activeProjectId !== null && hasOverrides(selection.activeProjectId),
  );
  const compactLabel = $derived(
    compactHasOverrides
      ? "Reset compact transcript"
      : compactEnabled
        ? "Expand transcript"
        : "Compact transcript",
  );

  function paneIsActive(pane: TranscriptPane): boolean {
    return pane.members.some((id) => agentIsWorking(runtimes[id]));
  }

  // Previous-frame bookkeeping only; rendered state lives in paneTabCompleted.
  // Entries for inactive projects intentionally remain until that project is
  // active again, so background pane completions survive project switches.
  let paneTabWasActive: string[] = [];
  let paneTabCompleted = $state<Record<string, true>>({});

  function paneTabKey(projectId: ProjectId, paneId: string): string {
    return `${projectId}:${paneId}`;
  }

  $effect(() => {
    const projectId = selection.activeProjectId;
    const layout = activePaneLayout;
    if (projectId === null || layout === null) return;
    const projectPrefix = `${projectId}:`;
    const paneKeys = layout.panes.map((pane) => paneTabKey(projectId, pane.id));
    const tabEntries = headerTabPanes.map((pane) => ({
      key: paneTabKey(projectId, pane.id),
      active: paneIsActive(pane),
    }));
    const tabKeys = tabEntries.map((entry) => entry.key);

    untrack(() => {
      for (const key of paneTabWasActive) {
        if (key.startsWith(projectPrefix) && (!paneKeys.includes(key) || !tabKeys.includes(key))) {
          paneTabWasActive = paneTabWasActive.filter((id) => id !== key);
        }
      }
      for (const key of Object.keys(paneTabCompleted)) {
        if (key.startsWith(projectPrefix) && (!paneKeys.includes(key) || !tabKeys.includes(key))) {
          delete paneTabCompleted[key];
        }
      }
      for (const entry of tabEntries) {
        if (entry.active) {
          if (!paneTabWasActive.includes(entry.key))
            paneTabWasActive = [...paneTabWasActive, entry.key];
          delete paneTabCompleted[entry.key];
        } else if (paneTabWasActive.includes(entry.key)) {
          paneTabWasActive = paneTabWasActive.filter((id) => id !== entry.key);
          paneTabCompleted[entry.key] = true;
        }
      }
    });
  });

  function paneTabIsCompleted(pane: TranscriptPane): boolean {
    return (
      selection.activeProjectId !== null &&
      paneTabCompleted[paneTabKey(selection.activeProjectId, pane.id)] === true
    );
  }

  function selectHeaderPane(pane: TranscriptPane): void {
    const projectId = selection.activeProjectId;
    if (projectId === null) return;
    const key = paneTabKey(projectId, pane.id);
    delete paneTabCompleted[key];
    paneTabWasActive = paneTabWasActive.filter((id) => id !== key);
    const wasMaximized = activePaneLayout?.maximized !== null;
    // Capture the roster alongside `projectId`: the reveal is deferred two
    // animation frames (below), and `activeRosterIds` is a live derivation, so
    // reading it inside the closure would pair the old project with whatever
    // roster is active when the frames land. `reconcileLayout` prunes pane
    // membership against the roster it's handed and persists, so a stale read
    // would corrupt the original project's saved layout.
    const rosterIds = [...activeRosterIds];
    // Revealing a pane remounts its `UnifiedTranscript` (and re-derives every
    // render block) in one synchronous flush — perceptible lag with no feedback
    // on a long transcript. Reuse the transcript-busy overlay so the switch
    // shows a spinner first, then runs the remount once it has painted.
    void withTranscriptBusy(() => {
      // The user navigated away before the deferred reveal ran — drop it rather
      // than mutate a project's layout they're no longer looking at.
      if (selection.activeProjectId !== projectId) return;
      revealPane(projectId, rosterIds, pane.id);
      if (wasMaximized && pane.members.length > 0) {
        targetRecipients(projectId, [...pane.members]);
      }
    });
  }

  /// Cycle the targeted pane by position (⌘⇧[ = -1, ⌘⇧] = +1). Reuses the
  /// ⌘⌥N reveal-on-target path so a maximized pane re-maximizes, a hidden one is
  /// restored, and a refused target (prompt-render lock) changes nothing.
  function cyclePane(direction: 1 | -1): void {
    const projectId = selection.activeProjectId;
    if (projectId === null || settingsOpen || view.mode === "git") return;
    const rosterIds = activeAgents.map((a) => a.id);
    const pane = paneToCycleTo(projectId, rosterIds, selectionFor(projectId), direction);
    if (pane === null) return;
    const reveal = (): void => {
      if (targetRecipients(projectId, [...pane.members])) {
        revealPane(projectId, rosterIds, pane.id);
      }
    };
    // Cycling onto a hidden pane (minimized, or any pane while another is
    // maximized) remounts its transcript, so show the spinner first — exactly
    // like clicking a header tab. An already-visible target just re-targets, so
    // it runs immediately with no spurious spinner.
    const layout = activePaneLayout;
    const targetHidden =
      layout !== null &&
      (layout.maximized !== null
        ? layout.maximized !== pane.id
        : layout.minimized.includes(pane.id));
    if (targetHidden) {
      void withTranscriptBusy(() => {
        if (selection.activeProjectId !== projectId) return;
        reveal();
      });
    } else {
      reveal();
    }
  }

  function addEmptyPane(): void {
    if (selection.activeProjectId === null) return;
    createEmptyPane(selection.activeProjectId, activeRosterIds);
  }

  function restoreAllPanes(): void {
    const projectId = selection.activeProjectId;
    if (projectId === null) return;
    const rosterIds = [...activeRosterIds];
    // Restoring remounts every previously-minimized/maximized pane in one flush
    // — show the spinner first, like the other pane-layout gestures.
    void withTranscriptBusy(() => {
      if (selection.activeProjectId !== projectId) return;
      restoreMaximizedPane(projectId, rosterIds);
    });
  }

  /// Expand/collapse-all over a long conversation re-renders every block in
  /// one synchronous flush — perceptible lag with zero feedback. Cover the
  /// center pane with a blur+spinner, let it PAINT first (two rAFs: the first
  /// resolves before the next paint, the second after it has happened), then
  /// run the mutation and drop the overlay once the re-render has flushed. The
  /// spinner keeps animating through the blocked main thread because
  /// `animate-spin` is a compositable transform animation.
  let transcriptBusy = $state(false);

  async function withTranscriptBusy(action: () => void): Promise<void> {
    transcriptBusy = true;
    await new Promise(requestAnimationFrame);
    await new Promise(requestAnimationFrame);
    action();
    await tick();
    transcriptBusy = false;
  }

  function toggleCompactTranscript(): void {
    const projectId = selection.activeProjectId;
    if (projectId === null) return;
    void withTranscriptBusy(() => normalizeProjectCompact(projectId));
  }

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

  // The context-aware command list for the palette: always-available navigation,
  // active-project actions while in the Projects view, the flat project switcher,
  // and whatever the active view contributed (the Git view registers its own).
  //
  // This rebuilds on its reactive deps even while the palette is closed (the
  // palette is always mounted). It's cheap and fires only on app-state changes,
  // not in a hot path — if it ever shows up in a profile, gate construction on
  // `palette.open` (the keyboard shortcuts don't read this list, so that's safe).
  const paletteCommands = $derived.by<Command[]>(() => {
    const cmds: Command[] = [];
    const inProjects = view.mode === "projects" && !settingsOpen;
    const hasActive = hasActiveProject;

    cmds.push({
      id: "nav.toggle-view",
      title: view.mode === "git" ? "Switch to Projects view" : "Switch to Git view",
      group: "Navigation",
      shortcut: ["mod", "shift", "G"],
      keywords: "projects git toggle view",
      run: () => selectView(view.mode === "git" ? "projects" : "git"),
    });
    cmds.push({
      id: "nav.settings",
      title: settingsOpen ? "Close settings" : "Open settings",
      group: "Navigation",
      shortcut: ["mod", ","],
      keywords: "preferences",
      run: () => toggleSettings(),
    });
    cmds.push({
      id: "nav.toggle-projects-sidebar",
      title: projectsSidebarOpen ? "Hide projects sidebar" : "Show projects sidebar",
      group: "Navigation",
      shortcut: ["mod", "B"],
      run: () => {
        projectsSidebarOpen = !projectsSidebarOpen;
      },
    });
    cmds.push({
      id: "nav.toggle-agents-sidebar",
      title: agentsSidebarOpen ? "Hide agents sidebar" : "Show agents sidebar",
      group: "Navigation",
      shortcut: ["mod", "shift", "B"],
      run: () => {
        agentsSidebarOpen = !agentsSidebarOpen;
      },
    });
    cmds.push({
      id: "nav.add-project",
      title: "Add project",
      group: "Navigation",
      shortcut: ["mod", "N"],
      keywords: "new create",
      run: () => openProjectDialog(),
    });

    if (inProjects) {
      cmds.push({
        id: "project.next-ready",
        title: "Switch to next ready project",
        group: "Project",
        shortcut: ["mod", "G"],
        keywords: "next ready completed unread",
        disabled: nextUnreadCompletedProjectId() === null,
        run: () => selectNextUnreadCompletedProject(),
      });
      cmds.push({
        id: "project.add-agent",
        title: "Add agent",
        group: "Project",
        shortcut: ["mod", "shift", "N"],
        keywords: "new harness",
        disabled: !hasActive,
        run: () => openAddAgent(),
      });
      cmds.push({
        id: "project.open-editor",
        title: "Open project in editor",
        group: "Project",
        shortcut: ["mod", "shift", "E"],
        disabled: !hasActive,
        run: () => void openSelectionInEditor(),
      });
      cmds.push({
        id: "project.open-terminal",
        title: "Open project in terminal",
        group: "Project",
        disabled: !hasActive,
        run: () => void openActiveProjectInTerminal(),
      });
      cmds.push({
        id: "project.reveal-finder",
        title: "Reveal project in Finder",
        group: "Project",
        disabled: !hasActive,
        run: () => void revealActiveProjectInFinder(),
      });
      cmds.push({
        id: "project.show-in-git",
        title: "Show project in Git view",
        group: "Project",
        shortcut: ["mod", "shift", "F"],
        keywords: "git branch reveal",
        disabled: !hasActive,
        run: () => void openActiveProjectInGit(),
      });
      cmds.push({
        id: "project.archive",
        title: activeProject?.archived === true ? "Unarchive project" : "Archive project",
        group: "Project",
        disabled: !hasActive,
        run: () => void toggleArchiveActiveProject(),
      });
    }

    for (const project of projects.list) {
      const isActive =
        project.id === selection.activeProjectId && view.mode === "projects" && !settingsOpen;
      cmds.push({
        id: `switch.${project.id}`,
        title: project.name,
        group: "Switch to project",
        keywords: `${project.directory}${project.archived ? " archived" : ""}`,
        disabled: isActive,
        run: () => switchToProject(project.id),
      });
    }

    return [...cmds, ...contributedCommands()];
  });
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

        {#if showPaneHeaderControls}
          <div class="flex min-w-0 shrink items-center gap-1" data-tauri-no-drag>
            <div
              class="flex min-w-0 shrink items-center gap-1 overflow-hidden"
              data-testid="app-pane-tab-strip"
            >
              {#each headerTabPanes as pane (pane.id)}
                {@const active = paneIsActive(pane)}
                {@const completed = paneTabIsCompleted(pane)}
                <button
                  type="button"
                  class="border-border bg-panel text-fg hover:bg-raised inline-flex h-6.5 max-w-36 min-w-0 shrink items-center gap-1.5 rounded-full border px-2 text-xs"
                  data-testid="app-pane-minimized-tab"
                  data-pane-id={pane.id}
                  onclick={() => selectHeaderPane(pane)}
                >
                  {#if active}
                    <span
                      class="inline-flex shrink-0 items-center justify-center"
                      role="status"
                      aria-label={`${pane.name} has running agents`}
                      data-testid="app-pane-tab-activity"
                    >
                      <Spinner class="h-3.5 w-3.5" />
                    </span>
                  {:else if completed}
                    <span
                      class="text-accent inline-flex shrink-0 items-center justify-center"
                      role="status"
                      aria-label={`${pane.name} activity ended`}
                      data-testid="app-pane-tab-completed"
                    >
                      <CircleCheck size={14} strokeWidth={1.8} aria-hidden="true" />
                    </span>
                  {/if}
                  <span class="truncate font-medium">{pane.name}</span>
                </button>
              {/each}
            </div>
            {#if activeMaximizedPane !== null && headerTabPanes.length > 1}
              <button
                type="button"
                class="text-muted hover:text-fg hover:bg-border/60 inline-flex h-6.5 shrink-0 items-center rounded-full px-2 text-xs"
                data-testid="app-pane-restore-all"
                onclick={restoreAllPanes}
              >
                Restore all
              </button>
            {/if}
            <Tooltip label="Add empty pane" side="bottom">
              {#snippet trigger(props)}
                <button
                  {...props}
                  type="button"
                  class={cn(ICON_BUTTON_CLASS, "hover:bg-panel shrink-0")}
                  aria-label="Add empty pane"
                  data-testid="app-pane-add"
                  onclick={addEmptyPane}
                >
                  <Plus size={ICON_SIZE} aria-hidden="true" />
                </button>
              {/snippet}
            </Tooltip>
            <Tooltip label={compactLabel} side="bottom">
              {#snippet trigger(props)}
                <button
                  {...props}
                  type="button"
                  onclick={toggleCompactTranscript}
                  aria-label={compactLabel}
                  data-testid="transcript-compact-toggle"
                  data-tauri-no-drag
                  class={cn(ICON_BUTTON_CLASS, "hover:bg-panel shrink-0")}
                >
                  {#if compactEnabled}
                    <ChevronsUpDown size={ICON_SIZE} aria-hidden="true" />
                  {:else}
                    <ChevronsDownUp size={ICON_SIZE} aria-hidden="true" />
                  {/if}
                </button>
              {/snippet}
            </Tooltip>
          </div>
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
        <CommandPaletteButton
          testid="command-palette-button"
          onclick={() => togglePalette()}
          class="hover:bg-panel"
        />
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

      {#each agentCreationFailures as failure (failure.harness)}
        <Banner
          message={`Couldn't create the ${HARNESS_LABEL[failure.harness]} agent: ${failure.error}`}
          testid={`banner-agent-create-failed-${failure.harness}`}
          onDismiss={() => dismissAgentCreationFailure(failure.harness)}
        />
      {/each}
      {#if editorShortcutError !== null}
        <Banner
          message={`Couldn't open editor: ${editorShortcutError}`}
          testid="banner-open-editor-failed"
          onDismiss={() => (editorShortcutError = null)}
        />
      {/if}
      {#if commandError !== null}
        <Banner
          message={commandError}
          testid="banner-command-failed"
          onDismiss={() => (commandError = null)}
        />
      {/if}

      <div class="flex min-h-0 flex-1 flex-col overflow-hidden">
        {#if settingsOpen}
          <SettingsView onClose={closeSettings} />
        {:else if view.mode === "git"}
          {#if gitViewResumePending}
            <div class="flex min-h-0 flex-1 flex-col overflow-hidden">
              <EmptyState testid="git-view-loading" title="Loading repositories…" spinner />
            </div>
          {:else}
            <GitView />
          {/if}
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
          <!-- Mirror the loaded view's layout (center pane + compose-bar shell
               + agents-sidebar shell) so the spinner doesn't jump — sideways or
               vertically — when the roster resolves and the next loading state
               ("Loading history…") renders inside the real layout. The compose
               shell reuses ComposeBar's chrome classes (outer strip, rounded
               box, min-h-16 content) so its height tracks the real empty
               compose bar; a multi-agent project adds a chips row the shell
               can't predict — a small accepted residual. -->
          <div class="flex min-h-0 flex-1 overflow-hidden">
            <div class="flex min-w-0 flex-1 flex-col overflow-hidden">
              <EmptyState testid="project-loading" title="Loading project…" spinner />
              <div class="bg-raised px-4 pt-2 pb-4" data-testid="project-loading-compose-shell">
                <div class="border-border bg-raised rounded-xl border p-2.5">
                  <!-- Stand-ins for the compose box's two rows: the header row
                       (h-6 buttons + mb-1.5, chips-less) and the textarea at
                       its initial 3-row autosize. A real (inert) textarea with
                       the same rows/font/padding inherits the exact height
                       from the same CSS instead of hand-copying a pixel
                       value. -->
                  <div class="mb-1.5 h-6"></div>
                  <textarea
                    rows="3"
                    disabled
                    aria-hidden="true"
                    tabindex="-1"
                    class="pointer-events-none block w-full resize-none border-0 bg-transparent p-1 text-sm"
                  ></textarea>
                </div>
              </div>
            </div>
            {#if agentsSidebarOpen}
              <SidebarPanel side="right" width="w-60" testid="project-loading-sidebar-shell">
                <div></div>
              </SidebarPanel>
            {/if}
          </div>
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
            <div class="relative flex min-w-0 flex-1 flex-col overflow-hidden">
              {#if transcriptBusy}
                <div
                  class="bg-surface/30 absolute inset-0 z-50 flex items-center justify-center backdrop-blur-sm"
                  data-testid="transcript-busy-overlay"
                >
                  <Spinner class="h-8 w-8" />
                </div>
              {/if}
              <TranscriptPanes
                projectId={selection.activeProjectId!}
                agents={activeAgents}
                overlay={activeConvo?.items ?? []}
                loadStatus={activeConvo?.status ?? "complete"}
                loadError={activeConvo?.error}
                runWithBusy={withTranscriptBusy}
                onRetryLoad={() => {
                  if (selection.activeProjectId !== null)
                    void retryProjectHydration(selection.activeProjectId);
                }}
              />
              <!-- Remount per project: besides re-seeding the per-project
                   draft/recipient state, this resets sendError, the @-menu, and
                   focus so one project's compose state can't bleed into another. -->
              {#key selection.activeProjectId}
                <ComposeBar
                  projectId={selection.activeProjectId!}
                  agents={activeAgents}
                  focusOnMount={true}
                />
              {/key}
            </div>
            {#if agentsSidebarOpen}
              <Sidebar
                projectId={selection.activeProjectId!}
                agents={activeAgents}
                onAddAgent={openAddAgent}
              />
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

  <CommandPalette bind:open={palette.open} commands={paletteCommands} />
</main>
