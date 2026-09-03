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
  import PinsSidebar from "$lib/components/PinsSidebar.svelte";
  import TranscriptNavigator from "$lib/components/TranscriptNavigator.svelte";
  import PaneTabStrip from "$lib/components/PaneTabStrip.svelte";
  import type { HeaderPaneState } from "$lib/components/PaneTabStrip.types";
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
  import { SUPPLEMENTAL_TOOLTIP_DELAY } from "$lib/components/ui/tooltip";
  import ExpandCollapseIcon from "$lib/components/ui/ExpandCollapseIcon.svelte";
  import { ICON_BUTTON_CLASS, ICON_SIZE } from "$lib/components/ui/iconButton";
  import { BookOpen, FolderOpen, GitBranch, Pin, Plus, UsersRound } from "@lucide/svelte";
  import {
    hasOverrides,
    normalizeProjectCompact,
    stateFor,
  } from "$lib/state/transcriptPreview.svelte";
  import {
    assignAgentToFirstVisibleEmptyPane,
    createEmptyPane,
    expandAllPanes,
    layoutFor,
    paneToCycleTo,
    revealPane,
    type TranscriptPane,
  } from "$lib/state/transcriptPanes.svelte";
  import { selectionFor, targetRecipients } from "$lib/state/recipientSelection.svelte";
  import { layout, RIGHT_SIDEBAR_DEFAULT_MODE, type RightSidebarMode } from "$lib/layout.svelte";
  import { navigatorState, toggleNavigator, openNavigator } from "$lib/state/transcriptJump.svelte";
  import {
    dismissPinMutationError,
    loadMessagePins,
    pinLoadError,
    pinMutationError,
    pinsLoaded,
    pinsFor,
    reconcileMessagePinIdentities,
  } from "$lib/state/messagePins.svelte";
  import DevIndicator from "$lib/components/ui/DevIndicator.svelte";
  import { installDevTranscriptSeed } from "$lib/dev/seedTranscript";
  import { windowDragRegion } from "$lib/windowDrag";
  import {
    agentIsWorking,
    hydrateAgent,
    registerAgent,
    runtimes,
    transcripts,
  } from "$lib/state/index.svelte";
  import { buildUnifiedRows } from "$lib/state/unified";
  import { messageIdentityForRow, type PinnableMessageIdentity } from "$lib/messageIdentity";
  import {
    activateProject,
    addAgentToProjectRoster,
    agentCreationFailures,
    agentsByProject,
    conversations,
    deleteProject,
    dismissProjectDeletionError,
    dismissAgentCreationFailure,
    dismissSeedPathUnresolved,
    loadWorkspace,
    nextUnreadCompletedProjectId,
    projects,
    projectDeletions,
    installRegistryStalenessRefresh,
    refreshProjectRegistry,
    retryProjectHydration,
    seedPathUnresolved,
    selection,
    setProjectArchived,
    installForkHistoryRefresh,
    startProjectActivityObserver,
    workspace,
  } from "$lib/state/workspace.svelte";
  import { PROJECT_DELETE_TOOLTIP } from "$lib/projectDeletion";
  import {
    contributedCommands,
    palette,
    togglePalette,
    type Command,
  } from "$lib/state/commandPalette.svelte";
  import type { AgentRecord, HarnessAvailability, HarnessKind, ProjectId } from "$lib/types";
  import { projectIsAvailable } from "$lib/types";
  import { ALL_HARNESSES, HARNESS_LABEL } from "$lib/harnessDisplay";
  import { harnessAvailability, refreshHarnessAvailability } from "$lib/harnessAvailability.svelte";
  import { loadPreferences, preferences } from "$lib/preferences.svelte";
  import { isReadingMode, toggleReadingMode } from "$lib/state/readingMode.svelte";
  import GitView from "$lib/components/GitView.svelte";
  import {
    view,
    setViewMode,
    enterGitView,
    revealProjectBranch,
    cancelProjectBranchReveal,
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
  import { shortcut } from "$lib/platform";

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
  let settingsOpen = $state<boolean>(false);
  let settingsInitialSection = $state<"prompts" | null>(null);
  let editorShortcutError = $state<string | null>(null);
  let editorShortcutSeq = 0;
  let commandError = $state<string | null>(null);
  let projectViewResumePending = $state<boolean>(false);
  let projectViewResumeSeq = 0;
  let gitViewResumePending = $state<boolean>(false);
  let gitViewResumeSeq = 0;
  const activeRightSidebarMode = $derived.by<RightSidebarMode>(() => {
    const projectId = selection.activeProjectId;
    return projectId === null ? RIGHT_SIDEBAR_DEFAULT_MODE : layout.rightSidebarModeFor(projectId);
  });

  function isComposerShortcutTarget(target: EventTarget | null): boolean {
    return (
      target instanceof HTMLElement && target.closest('[data-shortcut-scope="composer"]') !== null
    );
  }

  function selectRightSidebarMode(mode: RightSidebarMode): void {
    if (mode === "agents" && activeAgents.length === 0) return;
    const projectId = selection.activeProjectId;
    if (projectId === null) return;
    layout.setRightSidebarMode(projectId, mode);
    layout.rightSidebarOpen = true;
  }

  function toggleRightSidebarMode(): void {
    selectRightSidebarMode(activeRightSidebarMode === "agents" ? "pins" : "agents");
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

    // ⌘F opens the transcript navigator (find a message). Handled before the
    // editable-target guard so it works from the compose box too — there is no
    // native find in the webview to preserve. Only in a project transcript.
    if (
      (event.metaKey || event.ctrlKey) &&
      !event.shiftKey &&
      !event.altKey &&
      event.key.toLowerCase() === "f"
    ) {
      if (canOpenNavigator) {
        event.preventDefault();
        toggleNavigator();
      }
      return;
    }
    // The open navigator is a focus-trapped modal; suppress other window chords
    // while it owns the keyboard, mirroring the palette above.
    if (navigatorState.open) return;

    if (isEditableShortcutTarget(event.target) && !isComposerShortcutTarget(event.target)) return;

    const command = event.metaKey || event.ctrlKey;
    if (!command) return;

    const key = event.key.toLowerCase();
    if (event.altKey) {
      if (!event.shiftKey && event.code === "KeyP" && showRightSidebarControls) {
        event.preventDefault();
        toggleRightSidebarMode();
      } else if (event.code === "KeyB") {
        event.preventDefault();
        layout.projectsSidebarOpen = !layout.projectsSidebarOpen;
        layout.rightSidebarOpen = !layout.rightSidebarOpen;
      } else if (/^Digit[1-9]$/.test(event.code)) {
        // ⌘⌥1..N targets pane N (leftmost = 1): replace the compose recipient
        // set with that pane's members. `event.code`, not `event.key` — Option
        //+number on macOS produces a different character in `key`. Inert with
        // a single pane (nothing to disambiguate); ⌘1..9 (no Alt) stays the
        // per-agent chip toggle in ComposeBar.
        if (selection.activeProjectId === null || settingsOpen || view.mode === "git") return;
        const rosterIds = activeAgents.map((a) => a.id);
        const paneLayout = layoutFor(selection.activeProjectId, rosterIds);
        if (paneLayout.panes.length < 2) return;
        const pane = paneLayout.panes[Number(event.code.slice(5)) - 1];
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
    } else if (key === "r" && event.shiftKey) {
      // ⌘⇧R toggles reading mode. Shifted deliberately: bare ⌘R is the Git
      // view's refresh, and it is also the universal reload chord. Stays live
      // *while reading mode is on* — with the compose box gone, this is the
      // fastest way back out, so it must not be gated on the composer existing.
      if (readingModeAvailable) {
        event.preventDefault();
        toggleReadingModeForActiveProject();
      }
    } else if (key === "b" && event.shiftKey) {
      event.preventDefault();
      layout.rightSidebarOpen = !layout.rightSidebarOpen;
    } else if (key === "b") {
      event.preventDefault();
      layout.projectsSidebarOpen = !layout.projectsSidebarOpen;
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
    // `directory` is null when the project's directory identity resolves to no
    // single path — there is nothing to open, and the backend refuses to guess.
    if (activeProject.directory === null) {
      commandError = "This project's working directory could not be resolved.";
      return;
    }
    try {
      await api.openInTerminal(activeProject.directory);
    } catch (e) {
      commandError = e instanceof Error ? e.message : String(e);
    }
  }

  async function revealActiveProjectInFinder(): Promise<void> {
    if (activeProject === null) return;
    commandError = null;
    if (activeProject.directory === null) {
      commandError = "This project's working directory could not be resolved.";
      return;
    }
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
    settingsInitialSection = null;
    settingsOpen = true;
  }

  function openPromptSettings(): void {
    settingsInitialSection = "prompts";
    settingsOpen = true;
  }

  function closeSettings(): void {
    settingsOpen = false;
    settingsInitialSection = null;
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
    cancelProjectBranchReveal();
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
    if (activeProject === null || activeProject.directory === null) return;
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
    // A forked agent's branch point only materializes when its first turn runs,
    // so its inherited history has to be read back afterwards. No-op for every
    // non-fork agent.
    installForkHistoryRefresh();
    // The startup probe races the backend's PATH capture, so its answer can be
    // provisional; the store awaits its own completion-listener registration
    // before issuing any probe, and arms its own backstop for a lost event.
    void refreshHarnessAvailability();
    void loadPreferences();
    void loadWorkspace()
      .then(() => {
        // A closed sidebar is a legitimate device-local preference while the
        // app is running, but a launch that finds existing projects behind a
        // closed sidebar is a trap: the sidebar is the only picker, and the
        // reopen toggle only appears once you already know to look for it.
        // Force it open exactly once, at startup, whenever there's something
        // in it to pick from — never mid-session, so a deliberate close later
        // is respected.
        if (projects.list.length > 0) layout.projectsSidebarOpen = true;
      })
      .catch((err) => {
        dirError = err instanceof Error ? err.message : String(err);
      });

    window.addEventListener("keydown", handleGlobalKeydown);
    // Directory availability is only known at list time (see
    // `refreshProjectRegistry`), so re-list whenever the user comes back, and
    // whenever a send fails before its turn starts.
    const uninstallStalenessRefresh = installRegistryStalenessRefresh();
    const refreshOnReturn = (): void => void refreshProjectRegistry();
    const refreshWhenVisible = (): void => {
      if (document.visibilityState === "visible") refreshOnReturn();
    };
    window.addEventListener("focus", refreshOnReturn);
    document.addEventListener("visibilitychange", refreshWhenVisible);
    const removeDevSeed = installDevTranscriptSeed(() => activeAgents);
    return () => {
      stopProjectActivityObserver();
      window.removeEventListener("keydown", handleGlobalKeydown);
      window.removeEventListener("focus", refreshOnReturn);
      document.removeEventListener("visibilitychange", refreshWhenVisible);
      uninstallStalenessRefresh();
      removeDevSeed();
    };
  });

  // The displayed project's row when its working directory can't be used: the
  // folder is gone, or the catalog can't say which folder it is. Either way no
  // agent can run there, so the compose bar is gated (see ComposeBar) and this
  // banner says why. It clears on its own once a registry refresh sees the
  // folder again.
  const activeDirectoryProblem = $derived.by(() => {
    const projectId = selection.activeProjectId;
    if (projectId === null) return null;
    const listing = projects.list.find((candidate) => candidate.id === projectId);
    if (listing === undefined || projectIsAvailable(listing)) return null;
    return listing;
  });
  const activeDirectoryProblemMessage = $derived.by(() => {
    const listing = activeDirectoryProblem;
    if (listing === null) return "";
    return listing.directory_status === "resolved_path_unavailable" && listing.directory !== null
      ? `This project's folder no longer exists at ${listing.directory}, so its agents can't run. If it was moved, put it back at that location; Switchboard checks again when you return to the window.`
      : "Switchboard can't tell which folder this project belongs to, so it can't be used. Repairing its folder record isn't possible from Switchboard yet.";
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
  $effect(() => {
    const projectId = selection.activeProjectId;
    if (projectId === null || !rosterLoaded) return;
    void loadMessagePins(projectId);
  });
  const activePins = $derived(
    selection.activeProjectId === null ? [] : pinsFor(selection.activeProjectId),
  );
  const activePinLoadError = $derived(
    selection.activeProjectId === null ? null : pinLoadError(selection.activeProjectId),
  );
  const activePinMutationError = $derived(
    selection.activeProjectId === null ? null : pinMutationError(selection.activeProjectId),
  );
  const activePaneLayout = $derived(
    selection.activeProjectId !== null
      ? layoutFor(selection.activeProjectId, activeRosterIds)
      : null,
  );
  const canCyclePanes = $derived(
    (activePaneLayout?.panes.filter((pane) => pane.members.length > 0).length ?? 0) > 1,
  );
  const headerPaneEntries = $derived.by(() => {
    if (activePaneLayout === null || activePaneLayout.panes.length < 2) return [];
    return activePaneLayout.panes.map((pane) => {
      let state: HeaderPaneState = "visible";
      if (activePaneLayout.maximized === pane.id) {
        state = "visible";
      } else if (activePaneLayout.maximized !== null) {
        state = activePaneLayout.minimized.includes(pane.id) ? "minimized" : "behind_maximized";
      } else if (activePaneLayout.minimized.includes(pane.id)) {
        state = "minimized";
      }
      return { pane, state };
    });
  });
  const hiddenHeaderPaneEntries = $derived(
    headerPaneEntries.filter((entry) => entry.state !== "visible"),
  );
  const activeConvo = $derived(
    selection.activeProjectId !== null ? conversations[selection.activeProjectId] : undefined,
  );
  const activePinIdentities = $derived.by(() => {
    const turns = activeAgents.flatMap((agent) => transcripts[agent.id] ?? []);
    const rows = buildUnifiedRows(
      turns,
      activeConvo?.items ?? [],
      new Set(activeAgents.map((agent) => agent.id)),
    );
    const harnesses = new Map(activeAgents.map((agent) => [agent.id, agent.harness]));
    const identities: PinnableMessageIdentity[] = [];
    for (const row of rows) {
      if (row.kind !== "user" && row.kind !== "agent") continue;
      const identity = messageIdentityForRow(
        row,
        row.kind === "agent" ? harnesses.get(row.turn.agent_id) : undefined,
      );
      if (identity.kind === "pinnable") identities.push(identity);
    }
    return identities;
  });
  $effect(() => {
    const projectId = selection.activeProjectId;
    if (
      projectId === null ||
      !pinsLoaded(projectId) ||
      !activePins.some((pin) => pin.key.startsWith("agent:send:"))
    )
      return;
    reconcileMessagePinIdentities(projectId, activePinIdentities);
  });
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
    layout.projectsSidebarOpen && projectsSidebarHasContent && view.mode !== "git",
  );
  const showPaneHeaderControls = $derived(
    !settingsOpen && view.mode !== "git" && selection.activeProjectId !== null && rosterLoaded,
  );
  const readingModeActive = $derived(
    selection.activeProjectId !== null && isReadingMode(selection.activeProjectId),
  );
  // One predicate for the header toggle, the palette entry, and the ⌘⇧R chord,
  // so the three can't disagree about whether the mode is available.
  //
  // **This gates the way *out* of reading mode as well as the way in, so every
  // condition in it must be transient-by-navigation.** A persistent one strands
  // the user in a mode with no exit: `readingModeActive` has no matching
  // condition, so it survives whatever turns this false. Settings, the Git view
  // and a mid-load roster all qualify — each resolves by navigating back. An
  // earlier `activeAgents.length > 0` clause did not, and stranded the mode when
  // a quiet project's last agent was removed. Adding a clause here without that
  // property reproduces the bug and no existing test will catch it.
  const readingModeAvailable = $derived(showPaneHeaderControls);
  const readingModeLabel = $derived(
    readingModeActive
      ? "Turn off reading mode"
      : "Reading mode: hide the compose box and stay notified about this project",
  );
  function toggleReadingModeForActiveProject(): void {
    if (selection.activeProjectId === null) return;
    toggleReadingMode(selection.activeProjectId);
  }
  // Reading mode's whole point is being notified about the project you're
  // watching, and that rides on a preference which defaults *off* — so a user on
  // defaults who turns reading mode on would get nothing and reasonably conclude
  // it is broken. Say which setting is in the way, where they just acted.
  // Deliberately **not** an override: silently ignoring a setting the user chose
  // is worse than explaining it.
  const readingModeNotifyHint = $derived(
    !readingModeActive
      ? null
      : !preferences.notify_on_completion
        ? "Notifications are turned off, so nothing will alert you while you read. Turn them on in Settings."
        : !preferences.notify_while_focused
          ? "To be alerted about this project while you read it, turn on “Also notify me about other projects while I'm using Switchboard” in Settings."
          : null,
  );
  // What the notification gate treats as "already on screen". Derived, not
  // mirrored: any future navigation path that changes one of these inputs flows
  // through automatically, whereas pushing from each navigation handler would be
  // a second copy of this state that every new path has to remember to update.
  //
  // `rosterLoaded` matters — a project mid-activation is not yet readable, so its
  // completion should still notify.
  //
  // Reading mode is the whole of its notification behavior: reporting the project
  // as not-on-screen makes the Rust gate fall through to its
  // "different project finished" branch, which is exactly the requested
  // semantics. The gate is untouched, and there is no second gate here.
  const visibleProjectId = $derived(
    settingsOpen || view.mode === "git" || !rosterLoaded || readingModeActive
      ? null
      : selection.activeProjectId,
  );
  // Monotonic so a slow IPC write can't land after a newer view and overwrite it.
  //
  // **Known limitation — the entry race, and it predates reading mode.** This
  // push is fire-and-forget, so a completion landing inside the single IPC
  // round-trip is gated against the *previous* view. The identical window exists
  // on every navigation into and out of a project; `seq` orders visibility writes
  // against each other, not against `notify`. Reading mode inherits it rather
  // than introducing it, and an arming protocol for reading mode alone would fix
  // one instance and leave the rest — if it is ever worth closing, the fix
  // belongs here, for all navigation paths. Cost is near zero in practice: the
  // user is holding the pointer and the result is on screen. (The *exit* side is
  // ordered — see `sendCompletion.ts`, which awaits `notify` before clearing.)
  let visibleProjectSeq = 0;
  $effect(() => {
    const id = visibleProjectId;
    const seq = ++visibleProjectSeq;
    void api.setVisibleProject(id, seq).catch((e: unknown) => {
      // Display-only state; a failed write costs at most one mis-gated
      // notification, never a dropped turn.
      console.error("[switchboard] set visible project failed", e);
    });
  });

  const showRightSidebarControls = $derived(
    showPaneHeaderControls &&
      (activeAgents.length > 0 || activePins.length > 0 || layout.rightSidebarOpen),
  );
  // The navigator needs a project transcript with messages to navigate — the
  // same condition as its header button being shown.
  const canOpenNavigator = $derived(showPaneHeaderControls && activeAgents.length > 0);

  // The navigator's open flag is global (so ⌘F and the palette can drive it),
  // but the component that can close it only mounts while a project transcript
  // shows. Without this, switching to Git/settings/no-project unmounts the
  // navigator with the flag still set — the modal vanishes but the "navigator
  // owns the keyboard" guard keeps swallowing shortcuts, and returning to the
  // project resurrects it. Close it at the owning (app) level instead.
  $effect(() => {
    if (!canOpenNavigator && navigatorState.open) navigatorState.open = false;
  });

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
    const paneLayout = activePaneLayout;
    if (projectId === null || paneLayout === null) return;
    const projectPrefix = `${projectId}:`;
    const paneKeys = paneLayout.panes.map((pane) => paneTabKey(projectId, pane.id));
    const tabEntries = hiddenHeaderPaneEntries.map(({ pane }) => ({
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
    const paneId = pane.id;
    const key = paneTabKey(projectId, paneId);
    delete paneTabCompleted[key];
    paneTabWasActive = paneTabWasActive.filter((id) => id !== key);
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
      // Whether a pane was maximized, and this pane's live membership, are
      // both read live rather than trusted from before the defer — a
      // same-project gesture (another tab click, pane cycling, a membership
      // edit) can change either while this was pending.
      const live = layoutFor(projectId, rosterIds);
      const livePane = live.panes.find((p) => p.id === paneId);
      // Reveal only after a required retarget succeeds — mirrors `cyclePane`'s
      // hidden-target branch. `targetRecipients` refuses while a send is
      // rendering; revealing anyway would swap the maximized view to this pane
      // while the compose selection silently failed to follow, leaving the
      // user looking at a pane their next send won't actually reach. A reveal
      // that doesn't need to retarget (nothing maximized, or nothing to
      // target) is never a selection risk, so it stays unconditional.
      if (live.maximized !== null && livePane !== undefined && livePane.members.length > 0) {
        if (!targetRecipients(projectId, [...livePane.members])) return;
      }
      revealPane(projectId, rosterIds, paneId);
    });
  }

  function targetVisibleHeaderPane(pane: TranscriptPane): void {
    const projectId = selection.activeProjectId;
    if (projectId === null || pane.members.length === 0) return;
    if (targetRecipients(projectId, [...pane.members])) composeFocusRequest += 1;
  }

  /// Cycle the targeted pane by position (⌘⇧[ = -1, ⌘⇧] = +1). A visible pane
  /// only needs a recipient update; routing it through the persisted layout
  /// store would invalidate every transcript derivation for a no-op reveal. A
  /// hidden target still uses the reveal path so it is restored atomically.
  function cyclePane(direction: 1 | -1): void {
    const projectId = selection.activeProjectId;
    if (projectId === null || settingsOpen || view.mode === "git") return;
    const rosterIds = activeAgents.map((a) => a.id);
    const pane = paneToCycleTo(projectId, rosterIds, selectionFor(projectId), direction);
    if (pane === null) return;
    const paneId = pane.id;
    // Cycling onto a hidden pane (minimized, or any pane while another is
    // maximized) remounts its transcript, so show the spinner first — exactly
    // like clicking a header tab. An already-visible target just re-targets, so
    // it runs immediately with no spurious spinner.
    const paneLayout = activePaneLayout;
    const targetHidden =
      paneLayout !== null &&
      (paneLayout.maximized !== null
        ? paneLayout.maximized !== paneId
        : paneLayout.minimized.includes(paneId));
    if (targetHidden) {
      void withTranscriptBusy(() => {
        if (selection.activeProjectId !== projectId) return;
        // Re-read live rather than trust `pane` from before the defer — a
        // same-project gesture can change membership (or remove the pane)
        // while this was pending.
        const livePane = layoutFor(projectId, rosterIds).panes.find((p) => p.id === paneId);
        if (livePane === undefined || livePane.members.length === 0) return;
        if (targetRecipients(projectId, [...livePane.members])) {
          revealPane(projectId, rosterIds, paneId);
        }
      });
    } else {
      targetRecipients(projectId, [...pane.members]);
    }
  }

  function addEmptyPane(): void {
    const projectId = selection.activeProjectId;
    if (projectId === null) return;
    createEmptyPane(projectId, activeRosterIds, selectionFor(projectId));
  }

  function restoreAllPanes(): void {
    const projectId = selection.activeProjectId;
    if (projectId === null) return;
    const rosterIds = [...activeRosterIds];
    // Restoring remounts every previously-minimized/maximized pane in one flush
    // — show the spinner first, like the other pane-layout gestures.
    void withTranscriptBusy(() => {
      if (selection.activeProjectId !== projectId) return;
      expandAllPanes(projectId, rosterIds);
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

  // A monotonic counter bumped when a pane Cmd+click asks the composer to take
  // focus (see TranscriptPanes.onRequestComposeFocus). Owned here rather than in
  // a module store because it's a transient one-shot signal between two children
  // App already renders, not per-project state anything derives from.
  let composeFocusRequest = $state(0);

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
    activationDeleteProjectId = null;
    if (selection.activeProjectId !== null) void activateProject(selection.activeProjectId);
  }

  // Verbatim-error dialog for the project-open failure (the center-pane
  // activation-error state). Mirrors the in-transcript Details affordance so a
  // user can copy the exact error into a bug report regardless of which
  // failure surface they hit.
  let activationDetailsOpen = $state<boolean>(false);
  let activationDeleteProjectId = $state<ProjectId | null>(null);
  let activationDeleteContextId: ProjectId | null = null;
  let activationCanDelete = $derived.by(() => {
    const projectId = selection.activeProjectId;
    const failure = selection.activationFailure;
    if (projectId === null || failure === null) return false;
    if (failure.type === "project_locked") return false;
    const project = projects.list.find((candidate) => candidate.id === projectId);
    return (
      (project !== undefined && !projectIsAvailable(project)) ||
      failure.type === "project_not_loaded"
    );
  });
  let activationDeleting = $derived(
    selection.activeProjectId !== null && selection.activeProjectId in projectDeletions.pending,
  );

  $effect(() => {
    const failedProjectId = selection.activationFailure === null ? null : selection.activeProjectId;
    untrack(() => {
      if (activationDeleteContextId === failedProjectId) return;
      activationDeleteContextId = failedProjectId;
      activationDeleteProjectId = null;
    });
  });

  async function confirmActivationDelete(): Promise<void> {
    const projectId = selection.activeProjectId;
    if (projectId === null || activationDeleteProjectId !== projectId) return;
    try {
      await deleteProject(projectId);
      activationDeleteProjectId = null;
    } catch {
      activationDeleteProjectId = null;
    }
  }

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

  /// Create or attach an agent, register its listeners, and add it to the
  /// roster named by the returned record. Attach kicks off per-agent hydration
  /// so the brought-in harness session's history appears.
  async function createOrAttachAndRegister(submission: AgentFormSubmit): Promise<AgentRecord> {
    const agent =
      submission.mode === "create"
        ? await api.createAgent(
            submission.name,
            submission.harness,
            submission.primary.model ?? undefined,
            submission.primary.effort ?? undefined,
            submission.secondary,
          )
        : await api.attachAgent(submission.name, submission.harness, submission.existingSessionId);
    await registerAgent(agent);
    addAgentToProjectRoster(agent);
    const rosterIds = (agentsByProject[agent.project_id] ?? []).map((item) => item.id);
    assignAgentToFirstVisibleEmptyPane(agent.project_id, rosterIds, agent.id);
    if (submission.mode === "attach") {
      void hydrateAgent(agent.id);
    }
    return agent;
  }

  // First-agent form (center, when the active project has no agents).
  let firstAgentBusy = $state<boolean>(false);
  let firstAgentError = $state<string | null>(null);

  async function handleCreateFirstAgent(submission: AgentFormSubmit): Promise<void> {
    firstAgentError = null;
    firstAgentBusy = true;
    try {
      const agent = await createOrAttachAndRegister(submission);
      targetRecipients(agent.project_id, [agent.id]);
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
      title: layout.projectsSidebarOpen ? "Hide projects sidebar" : "Show projects sidebar",
      group: "Navigation",
      shortcut: ["mod", "B"],
      run: () => {
        layout.projectsSidebarOpen = !layout.projectsSidebarOpen;
      },
    });
    cmds.push({
      id: "nav.toggle-agents-sidebar",
      title: layout.rightSidebarOpen ? "Hide right sidebar" : "Show right sidebar",
      group: "Navigation",
      shortcut: ["mod", "shift", "B"],
      run: () => {
        layout.rightSidebarOpen = !layout.rightSidebarOpen;
      },
    });
    cmds.push({
      id: "nav.toggle-right-sidebar-mode",
      title:
        activeRightSidebarMode === "agents" ? "Switch to Pins sidebar" : "Switch to Agents sidebar",
      group: "Navigation",
      shortcut: ["mod", "alt", "P"],
      keywords: "agents pins right sidebar toggle switch",
      disabled: !showRightSidebarControls,
      run: () => toggleRightSidebarMode(),
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
        id: "project.find-message",
        title: "Find message…",
        group: "Project",
        shortcut: ["mod", "F"],
        keywords: "navigate jump search transcript scroll",
        disabled: !canOpenNavigator,
        run: () => openNavigator(),
      });
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
        id: "project.next-pane",
        title: "Next pane",
        group: "Project",
        shortcut: ["mod", "shift", "]"],
        keywords: "switch cycle right forward tab",
        disabled: !canCyclePanes,
        run: () => cyclePane(1),
      });
      cmds.push({
        id: "project.previous-pane",
        title: "Previous pane",
        group: "Project",
        shortcut: ["mod", "shift", "["],
        keywords: "switch cycle left back tab",
        disabled: !canCyclePanes,
        run: () => cyclePane(-1),
      });
      // Titled by current state, not by the action alone: reading mode can outlive
      // the work that prompted it, so the way out has to be findable without
      // remembering what was turned on.
      cmds.push({
        id: "project.reading-mode",
        title: readingModeActive ? "Turn off reading mode" : "Turn on reading mode",
        group: "Project",
        shortcut: ["mod", "shift", "R"],
        keywords: "read watch hide compose notify focus",
        disabled: !readingModeAvailable,
        run: () => toggleReadingModeForActiveProject(),
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
          onToggleSidebar={() => (layout.projectsSidebarOpen = false)}
          {settingsOpen}
        />
      {/if}
    {/snippet}

    {#snippet center()}
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
        {#if projectsSidebarHasContent && !layout.projectsSidebarOpen}
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
            onclick={() => (layout.projectsSidebarOpen = true)}
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
            <Tooltip
              label={activeProject.directory ?? "working directory unresolved"}
              delayDuration={SUPPLEMENTAL_TOOLTIP_DELAY}
              focusable={false}
              side="bottom"
            >
              {#snippet trigger(props)}
                <div {...props} class="text-muted truncate text-xs">
                  {activeProject.directory}
                </div>
              {/snippet}
            </Tooltip>
          </div>
        {:else}
          <div class="flex-1"></div>
        {/if}

        {#if showPaneHeaderControls}
          <div class="flex min-w-0 shrink items-center gap-1" data-tauri-no-drag>
            <PaneTabStrip
              entries={headerPaneEntries}
              {paneIsActive}
              paneIsCompleted={paneTabIsCompleted}
              onSelectVisible={targetVisibleHeaderPane}
              onOpenHidden={selectHeaderPane}
            />
            <!-- Shown whenever more than one pane is hidden — minimized into the
                 tab strip, or hidden behind a maximized pane. -->
            {#if hiddenHeaderPaneEntries.length > 1}
              <button
                type="button"
                class="text-muted hover:text-fg hover:bg-hover inline-flex h-6.5 shrink-0 items-center rounded-full px-2 text-xs"
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
                  class={cn(ICON_BUTTON_CLASS, "shrink-0")}
                  aria-label="Add empty pane"
                  data-testid="app-pane-add"
                  onclick={addEmptyPane}
                >
                  <Plus size={ICON_SIZE} aria-hidden="true" />
                </button>
              {/snippet}
            </Tooltip>
            <!-- Latched on-state is load-bearing, not decoration: reading mode
                 is allowed to stay on after the project goes quiet, so this is
                 the only thing distinguishing a deliberate mode from a lost
                 compose box.
                 `warning`, not `accent` or `status-failed`: the mode takes a
                 capability away, so it needs the caution role rather than the
                 green `accent` (which means *finished* here, the very thing
                 reading mode reports on) — and it is a state the user chose,
                 not an error, so the red failure vocabulary would be wrong.
                 Sized below `ICON_SIZE`: the book is a closed shape spanning
                 its full viewBox, so at 18 it reads visibly heavier than the
                 thin-stroke glyphs beside it. **Even**, because the 26px button
                 centres by flex — an odd size leaves a half pixel on each side,
                 which is a whole device pixel at 2x and reads as off-centre. -->
            <Tooltip
              label={readingModeLabel}
              shortcut={shortcut("mod", "shift", "R")}
              side="bottom"
              reopen="fresh-hover"
            >
              {#snippet trigger(props)}
                <button
                  {...props}
                  type="button"
                  onclick={toggleReadingModeForActiveProject}
                  aria-label={readingModeLabel}
                  aria-pressed={readingModeActive}
                  data-testid="reading-mode-toggle"
                  data-tauri-no-drag
                  class={cn(
                    ICON_BUTTON_CLASS,
                    "shrink-0",
                    readingModeActive && "text-warning bg-warning-soft",
                  )}
                >
                  <BookOpen size={14} aria-hidden="true" />
                </button>
              {/snippet}
            </Tooltip>
            {#if activeAgents.length > 0}
              <Tooltip label={compactLabel} side="bottom" reopen="fresh-hover">
                {#snippet trigger(props)}
                  <button
                    {...props}
                    type="button"
                    onclick={toggleCompactTranscript}
                    aria-label={compactLabel}
                    data-testid="transcript-compact-toggle"
                    data-tauri-no-drag
                    class={cn(ICON_BUTTON_CLASS, "shrink-0")}
                  >
                    <ExpandCollapseIcon expanded={!compactEnabled} size={ICON_SIZE} />
                  </button>
                {/snippet}
              </Tooltip>
              <TranscriptNavigator
                projectId={selection.activeProjectId!}
                agents={activeAgents}
                overlay={activeConvo?.items ?? []}
              />
            {/if}
          </div>
          <!-- Hairline between the project-transcript controls (panes,
               navigator, compact) and the app-level view switcher. A border,
               not a bg fill — `border` is a line token (see token-ramp scan). -->
          <div class="border-border h-4 shrink-0 border-l" aria-hidden="true"></div>
        {/if}

        {#if showRightSidebarControls}
          <div
            class={cn(SEGMENTED_MAIN_CONTAINER_CLASS, "flex shrink-0")}
            role="radiogroup"
            aria-label="Right sidebar"
          >
            <Tooltip
              label={activeAgents.length === 0 ? "No agents in this project" : "Agents"}
              shortcut={shortcut("mod", "alt", "P")}
              side="bottom"
              reopen="fresh-hover"
            >
              {#snippet trigger(props)}
                <button
                  {...props}
                  type="button"
                  role="radio"
                  class={cn(
                    SEGMENTED_MAIN_ITEM_CLASS,
                    activeRightSidebarMode === "agents"
                      ? SEGMENTED_MAIN_ITEM_ACTIVE_CLASS
                      : SEGMENTED_MAIN_ITEM_INACTIVE_CLASS,
                  )}
                  aria-label="Show agents sidebar"
                  aria-checked={activeRightSidebarMode === "agents"}
                  aria-disabled={activeAgents.length === 0}
                  data-testid="right-sidebar-mode-agents"
                  class:opacity-40={activeAgents.length === 0}
                  onclick={() => selectRightSidebarMode("agents")}
                >
                  <UsersRound size={14} aria-hidden="true" />
                </button>
              {/snippet}
            </Tooltip>
            <Tooltip
              label="Pins"
              shortcut={shortcut("mod", "alt", "P")}
              side="bottom"
              reopen="fresh-hover"
            >
              {#snippet trigger(props)}
                <button
                  {...props}
                  type="button"
                  role="radio"
                  class={cn(
                    SEGMENTED_MAIN_ITEM_CLASS,
                    activeRightSidebarMode === "pins"
                      ? SEGMENTED_MAIN_ITEM_ACTIVE_CLASS
                      : SEGMENTED_MAIN_ITEM_INACTIVE_CLASS,
                  )}
                  aria-label="Show pins sidebar"
                  aria-checked={activeRightSidebarMode === "pins"}
                  data-testid="right-sidebar-mode-pins"
                  onclick={() => selectRightSidebarMode("pins")}
                >
                  <Pin
                    size={14}
                    fill={activeRightSidebarMode === "pins" ? "currentColor" : "none"}
                    aria-hidden="true"
                  />
                </button>
              {/snippet}
            </Tooltip>
          </div>
          <SidebarToggleButton
            side="right"
            expanded={layout.rightSidebarOpen}
            label={layout.rightSidebarOpen
              ? `Hide ${activeRightSidebarMode} sidebar`
              : `Show ${activeRightSidebarMode} sidebar`}
            testid="agents-sidebar-toggle"
            onclick={() => (layout.rightSidebarOpen = !layout.rightSidebarOpen)}
          />
          <div
            class="border-border h-4 shrink-0 border-l"
            aria-hidden="true"
            data-testid="right-sidebar-command-divider"
          ></div>
        {/if}
        <CommandPaletteButton testid="command-palette-button" onclick={() => togglePalette()} />
        <!-- Top-level Projects / Git view switch. Icon-only is an intentional
             compact-header trial; tooltips and accessible names keep both
             destinations explicit. -->
        <div
          class={cn(SEGMENTED_MAIN_CONTAINER_CLASS, "flex shrink-0")}
          role="radiogroup"
          aria-label="View"
        >
          <Tooltip
            label="Projects"
            shortcut={shortcut("mod", "shift", "G")}
            side="bottom"
            reopen="fresh-hover"
          >
            {#snippet trigger(props)}
              <button
                {...props}
                type="button"
                role="radio"
                class={cn(
                  SEGMENTED_MAIN_ITEM_CLASS,
                  !settingsOpen && view.mode === "projects"
                    ? SEGMENTED_MAIN_ITEM_ACTIVE_CLASS
                    : SEGMENTED_MAIN_ITEM_INACTIVE_CLASS,
                )}
                aria-label="Projects"
                aria-checked={!settingsOpen && view.mode === "projects"}
                data-testid="view-toggle-projects"
                onclick={() => selectView("projects")}
              >
                <FolderOpen size={14} aria-hidden="true" />
              </button>
            {/snippet}
          </Tooltip>
          <Tooltip
            label="Git"
            shortcut={shortcut("mod", "shift", "G")}
            side="bottom"
            reopen="fresh-hover"
          >
            {#snippet trigger(props)}
              <button
                {...props}
                type="button"
                role="radio"
                class={cn(
                  SEGMENTED_MAIN_ITEM_CLASS,
                  !settingsOpen && view.mode === "git"
                    ? SEGMENTED_MAIN_ITEM_ACTIVE_CLASS
                    : SEGMENTED_MAIN_ITEM_INACTIVE_CLASS,
                )}
                aria-label="Git"
                aria-checked={!settingsOpen && view.mode === "git"}
                data-testid="view-toggle-git"
                onclick={() => selectView("git")}
              >
                <GitBranch size={14} aria-hidden="true" />
              </button>
            {/snippet}
          </Tooltip>
        </div>
      </div>

      {#if seedPathUnresolved.value}
        <Banner
          message="Couldn't finish detecting your installed CLIs, so this project may be missing an agent. Use + to add one, or open Settings → Supported CLIs and press Refresh."
          testid="banner-seed-path-unresolved"
          onDismiss={dismissSeedPathUnresolved}
        />
      {/if}
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
      {#each Object.entries(projectDeletions.errors) as [projectId, error] (projectId)}
        <Banner
          message={`Couldn't delete ${projects.list.find((project) => project.id === projectId)?.name ?? "project"}: ${error}`}
          testid={`banner-project-delete-failed-${projectId}`}
          onDismiss={() => dismissProjectDeletionError(projectId)}
        />
      {/each}
      {#if commandError !== null}
        <Banner
          message={commandError}
          testid="banner-command-failed"
          onDismiss={() => (commandError = null)}
        />
      {/if}
      {#if selection.activeProjectId !== null && activePinLoadError !== null}
        <Banner
          message={`Couldn't load pins: ${activePinLoadError}`}
          testid="banner-pins-load-failed"
          actionLabel="Retry"
          onAction={() => void loadMessagePins(selection.activeProjectId!, true)}
        />
      {/if}
      {#if selection.activeProjectId !== null && activePinMutationError !== null}
        <Banner
          message={`Pin change wasn't saved: ${activePinMutationError}`}
          testid="banner-pins-save-failed"
          onDismiss={() => dismissPinMutationError(selection.activeProjectId!)}
        />
      {/if}
      {#if activeDirectoryProblem !== null}
        <Banner
          message={activeDirectoryProblemMessage}
          testid="banner-project-directory-unavailable"
          actionLabel={activeDirectoryProblem.directory_status === "resolved_path_unavailable"
            ? "Check again"
            : undefined}
          onAction={() => void refreshProjectRegistry()}
        />
      {/if}

      <div class="flex min-h-0 flex-1 flex-col overflow-hidden">
        {#if settingsOpen}
          <SettingsView onClose={closeSettings} initialSection={settingsInitialSection} />
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
        {:else if selection.activationFailure !== null}
          <EmptyState
            testid="activation-error"
            tone="error"
            title="Couldn't open this project."
            description={selection.activationFailure.message}
          >
            {#snippet action()}
              <div class="flex items-center gap-2">
                <Button
                  variant="secondary"
                  size="sm"
                  data-testid="activation-retry"
                  disabled={activationDeleting}
                  onclick={retryActivation}
                >
                  Retry
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  data-testid="activation-details"
                  disabled={activationDeleting}
                  onclick={() => (activationDetailsOpen = true)}
                >
                  Details
                </Button>
                {#if activationDeleteProjectId === selection.activeProjectId}
                  <Button
                    variant="ghost"
                    size="sm"
                    data-testid="activation-delete-cancel"
                    disabled={activationDeleting}
                    onclick={() => (activationDeleteProjectId = null)}
                  >
                    Cancel
                  </Button>
                  <Tooltip label="Confirm delete" side="bottom" reopen="fresh-hover">
                    {#snippet trigger(props)}
                      <Button
                        {...props}
                        variant="danger"
                        size="sm"
                        data-testid="activation-delete-confirm"
                        disabled={activationDeleting}
                        onclick={() => void confirmActivationDelete()}
                      >
                        {activationDeleting ? "Deleting…" : "Confirm delete"}
                      </Button>
                    {/snippet}
                  </Tooltip>
                {:else if activationCanDelete}
                  <Tooltip label={PROJECT_DELETE_TOOLTIP} side="bottom" reopen="fresh-hover">
                    {#snippet trigger(props)}
                      <Button
                        {...props}
                        variant="danger"
                        size="sm"
                        data-testid="activation-delete"
                        onclick={() => {
                          activationDeleteProjectId = selection.activeProjectId;
                        }}
                      >
                        Delete project
                      </Button>
                    {/snippet}
                  </Tooltip>
                {/if}
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
            {#if layout.rightSidebarOpen}
              <SidebarPanel
                side="right"
                widthProfile={activeRightSidebarMode === "pins" ? "reading" : "rail"}
                width={activeRightSidebarMode === "pins"
                  ? layout.pinsSidebarWidth
                  : layout.agentsSidebarWidth}
                testid="project-loading-sidebar-shell"
              >
                <div></div>
              </SidebarPanel>
            {/if}
          </div>
        {:else if activeAgents.length === 0}
          <div class="flex min-h-0 flex-1 overflow-hidden">
            <div class="flex min-w-0 flex-1 flex-col overflow-y-auto">
              <CreateAgentForm
                busy={firstAgentBusy}
                error={firstAgentError}
                onSubmit={handleCreateFirstAgent}
                roster={activeAgents}
                {availability}
              />
            </div>
            {#if layout.rightSidebarOpen && activeRightSidebarMode === "pins"}
              <PinsSidebar
                projectId={selection.activeProjectId!}
                agents={activeAgents}
                {rosterLoaded}
                overlay={activeConvo?.items ?? []}
              />
            {/if}
          </div>
        {:else}
          <div class="flex min-h-0 flex-1 overflow-hidden">
            <div class="relative flex min-w-0 flex-1 flex-col overflow-hidden">
              {#if transcriptBusy}
                <div
                  class="absolute inset-0 z-50 flex items-center justify-center backdrop-blur-sm"
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
                onAddAgent={openAddAgent}
                onRequestComposeFocus={() => (composeFocusRequest += 1)}
              />
              <!-- Remount per project: besides re-seeding the per-project
                   draft/recipient state, this resets sendError, the @-menu, and
                   focus so one project's compose state can't bleed into another. -->
              {#key selection.activeProjectId}
                <ComposeBar
                  projectId={selection.activeProjectId!}
                  agents={activeAgents}
                  focusOnMount={true}
                  focusRequest={composeFocusRequest}
                  onConfigurePrompts={openPromptSettings}
                />
              {/key}
              {#if readingModeNotifyHint !== null}
                <!-- Sits where the compose box was, because that is where the
                     user just acted. Reading mode's payoff is the alert; without
                     it the feature looks broken rather than misconfigured. -->
                <div class="bg-raised px-4 pb-3">
                  <p class="text-muted text-xs leading-relaxed" data-testid="reading-mode-hint">
                    {readingModeNotifyHint}
                    <button
                      type="button"
                      class="text-accent hover:underline"
                      onclick={openSettings}
                    >
                      Open settings
                    </button>
                  </p>
                </div>
              {/if}
            </div>
            {#if layout.rightSidebarOpen}
              {#if activeRightSidebarMode === "pins"}
                <PinsSidebar
                  projectId={selection.activeProjectId!}
                  agents={activeAgents}
                  {rosterLoaded}
                  overlay={activeConvo?.items ?? []}
                />
              {:else}
                <Sidebar
                  projectId={selection.activeProjectId!}
                  agents={activeAgents}
                  onAddAgent={openAddAgent}
                />
              {/if}
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
    details={selection.activationFailure?.message ?? "No error detail was reported."}
  />

  <CommandPalette bind:open={palette.open} commands={paletteCommands} />
</main>
