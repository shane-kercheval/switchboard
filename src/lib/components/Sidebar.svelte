<script lang="ts">
  import {
    Check,
    ChartPie,
    ChevronDown,
    ChevronRight,
    Columns2,
    Eye,
    EyeOff,
    FileText,
    GripVertical,
    MoreHorizontal,
    Pencil,
    RotateCcw,
    SlidersHorizontal,
    Square,
    Terminal,
    Trash2,
    X,
  } from "@lucide/svelte";
  import { flip } from "svelte/animate";
  import type { AgentSelection, AgentRecord, AgentId, ProjectId } from "$lib/types";
  import {
    dispatchCompaction,
    dispatchContextReport,
    retryAgentHydration,
    runtimes,
    stopAgent,
    transcripts,
  } from "$lib/state/index.svelte";
  import { supportsContextReport, supportsManualCompaction } from "$lib/harnessCapabilities";
  import {
    removeAgent,
    renameAgent,
    reorderAgents,
    setAgentSelection,
  } from "$lib/state/workspace.svelte";
  import { DRAG_SLOP_PX, dropIndexForPointer, movedOrder } from "$lib/agentReorder";
  import ExpandCollapseIcon from "$lib/components/ui/ExpandCollapseIcon.svelte";
  import { SUPPORTS_EFFORT_SELECTION, SUPPORTS_MODEL_SELECTION } from "$lib/harnessDisplay";
  import { effortSupportFor, selectionIsValid } from "$lib/agentSelection";
  import { claudeRateLimitView, codexRateLimitView, type UsageWindow } from "$lib/usageWindows";
  import { preferences } from "$lib/preferences.svelte";
  import {
    AGENTS_SIDEBAR_DEFAULT_WIDTH,
    layout,
    sidebarMaxWidth,
    SIDEBAR_MIN_WIDTH,
  } from "$lib/layout.svelte";
  import DropdownMenu from "$lib/components/ui/DropdownMenu.svelte";
  import DropdownMenuItem from "$lib/components/ui/DropdownMenuItem.svelte";
  import AgentSelectionChip from "$lib/components/AgentSelectionChip.svelte";
  import AgentSelectionEditor from "$lib/components/AgentSelectionEditor.svelte";
  import Button from "$lib/components/ui/Button.svelte";
  import {
    agentSessionInfo,
    openSessionFile as apiOpenSessionFile,
    resumeAgentInTerminal,
    type AgentSessionInfo,
  } from "$lib/api";
  import { normalizeAgentName, validateAgentName, type NameValidation } from "$lib/agentName";
  import {
    cn,
    formatResetCountdown,
    formatTokens,
    formatUsedPercent,
    relativeTime,
  } from "$lib/utils";
  import ResizeHandle from "$lib/components/ui/ResizeHandle.svelte";
  import SidebarPanel from "$lib/components/ui/SidebarPanel.svelte";
  import SidebarSection from "$lib/components/ui/SidebarSection.svelte";
  import {
    hiddenCount,
    isAgentHidden,
    layoutFor,
    moveAgentToNewPane,
    moveAgentToPane,
    paneOfAgent,
    showAllAgents,
    soloAgent,
    toggleAgentHidden,
  } from "$lib/state/transcriptPanes.svelte";
  import { selectAgent, selectionFor } from "$lib/state/recipientSelection.svelte";
  import { workflowRuns } from "$lib/state/workflows.svelte";
  import HarnessIcon from "$lib/components/ui/HarnessIcon.svelte";
  import PlusIcon from "$lib/components/ui/PlusIcon.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import TruncatedText from "$lib/components/ui/TruncatedText.svelte";
  import Dialog from "$lib/components/ui/Dialog.svelte";
  import ErrorDetailsDialog from "$lib/components/ui/ErrorDetailsDialog.svelte";
  import CopyButton from "$lib/components/ui/CopyButton.svelte";
  import Meter from "$lib/components/ui/Meter.svelte";
  import AgentEnvironment from "$lib/components/AgentEnvironment.svelte";
  import ContextBreakdown from "$lib/components/ContextBreakdown.svelte";
  import { ICON_BUTTON_CLASS, ICON_BUTTON_ON_PANEL_CLASS } from "$lib/components/ui/iconButton";

  /// An agent is "active" — currently driving work — when its turn is in-flight
  /// (run_status) or it still has queued sends. Gates the "Stop agent" action and
  /// the resume panel's stronger collision warning.
  function isActive(agentId: AgentId): boolean {
    const rt = runtimes[agentId];
    if (rt === undefined) return false;
    return (
      rt.run_status === "starting" ||
      rt.run_status === "processing" ||
      (rt.pending_sends ?? []).length > 0
    );
  }

  /// `onAddAgent` is the "+ Add agent" entry point in the sidebar header.
  /// Optional so existing callers + tests that don't pass it continue
  /// rendering; when absent, the button isn't shown.
  let {
    projectId,
    agents,
    onAddAgent,
  }: { projectId: ProjectId; agents: AgentRecord[]; onAddAgent?: () => void } = $props();

  // Pane membership + visibility for this project's roster. The eye toggle and
  // the move-to-pane actions both key off the same optional-membership model.
  const rosterIds = $derived(agents.map((a) => a.id));
  const paneLayout = $derived(layoutFor(projectId, rosterIds));
  const hiddenAgentCount = $derived(hiddenCount(projectId, rosterIds));
  const recipientSelection = $derived(selectionFor(projectId));
  const workflowActive = $derived((workflowRuns[projectId]?.length ?? 0) > 0);

  /// Eye toggle: plain click hides/shows the agent within its pane; Alt-click
  /// solos it (show only this agent in its pane; Alt-click again restores) —
  /// the mixer/layer-tool gesture.
  function onVisibilityClick(agent: AgentRecord, event: MouseEvent): void {
    if (event.altKey) {
      soloAgent(projectId, rosterIds, agent.id);
    } else {
      toggleAgentHidden(projectId, rosterIds, agent.id);
    }
  }

  /// Live width during a resize drag; the store commits on pointer-up.
  let draftWidth = $state<number | null>(null);

  let sessionInfoByAgent = $state<Record<AgentId, AgentSessionInfo | null>>({});
  let sessionInfoStarted = $state<Record<AgentId, boolean>>({});
  let sessionInfoInFlight = $state<Record<AgentId, boolean>>({});
  let sessionInfoError = $state<{ agentId: AgentId; message: string } | null>(null);
  let resumeAgentId = $state<AgentId | null>(null);
  let resumeOpen = $state(false);
  let resumeLaunchSequence = 0;
  let resumeLaunchOperation = $state<{ id: number; agentId: AgentId } | null>(null);
  const resumeLaunching = $derived(resumeLaunchOperation !== null);
  let resumeLaunchError = $state<string | null>(null);
  /// Verbatim hydration-error dialog (per-agent "history failed to load"). The
  /// failure lives on `runtime.hydration_error`; this just tracks which agent's
  /// error is currently shown.
  let hydrationDetailsOpen = $state(false);
  let hydrationDetailsName = $state("");
  let hydrationDetailsError = $state("");
  let removeConfirmAgentId = $state<AgentId | null>(null);
  /// The agent whose context-bar compact button is armed — one click arms, a
  /// second runs it. Same two-step as delete, and for the same reason: the
  /// button sits inline with no menu around it, so a stray click would spend a
  /// turn's worth of tokens with nothing to undo it.
  let compactConfirmAgentId = $state<AgentId | null>(null);
  /// Holds the armed button's confirm tooltip open. One boolean, because only
  /// one agent is ever armed, and it is bound only by the armed branch.
  let compactConfirmTooltipOpen = $state(false);
  let removingAgentId = $state<AgentId | null>(null);
  let removeError = $state<{ agentId: AgentId; message: string } | null>(null);

  /// Selection writes replace the complete configuration. One agent-level busy
  /// flag prevents independently rendered controls from submitting stale peers.
  let selectionEditingAgentId = $state<AgentId | null>(null);
  let editSelection = $state<AgentSelection>({
    model: null,
    effort: null,
    model_choices: [],
    effort_choices: [],
  });
  let editBusy = $state<boolean>(false);
  let editError = $state<string | null>(null);
  let selectionSaving = $state<Record<AgentId, boolean>>({});
  let selectionSaveErrors = $state<Record<AgentId, string>>({});

  const editingAgent = $derived(
    selectionEditingAgentId === null
      ? null
      : (agents.find((a) => a.id === selectionEditingAgentId) ?? null),
  );
  function canConfigureSelection(agent: AgentRecord): boolean {
    return SUPPORTS_MODEL_SELECTION[agent.harness] || SUPPORTS_EFFORT_SELECTION[agent.harness];
  }

  function selectionForAgent(agent: AgentRecord): AgentSelection {
    return {
      model: agent.model,
      effort: agent.effort,
      model_choices: agent.model_choices,
      effort_choices: agent.effort_choices,
    };
  }

  function selectionBusy(agentId: AgentId): boolean {
    return selectionSaving[agentId] === true;
  }

  function openSelectionSettings(agent: AgentRecord): void {
    selectionEditingAgentId = agent.id;
    editSelection = selectionForAgent(agent);
    editError = null;
    editBusy = false;
  }

  function closeChange(): void {
    selectionEditingAgentId = null;
    editError = null;
    editBusy = false;
  }

  async function submitChange(): Promise<void> {
    if (selectionEditingAgentId === null || editingAgent === null) return;
    const agentId = selectionEditingAgentId;
    if (!selectionIsValid(editSelection, editingAgent.harness)) return;
    editBusy = true;
    selectionSaving[agentId] = true;
    editError = null;
    try {
      await setAgentSelection(agentId, $state.snapshot(editSelection));
      delete selectionSaveErrors[agentId];
      if (selectionEditingAgentId === agentId) closeChange();
    } catch (err) {
      if (selectionEditingAgentId === agentId) {
        editError = err instanceof Error ? err.message : String(err);
        editBusy = false;
      }
    } finally {
      delete selectionSaving[agentId];
    }
  }

  async function activateSelection(agent: AgentRecord, selection: AgentSelection): Promise<void> {
    if (selectionBusy(agent.id)) return;
    selectionSaving[agent.id] = true;
    delete selectionSaveErrors[agent.id];
    try {
      await setAgentSelection(agent.id, selection);
    } catch (err) {
      selectionSaveErrors[agent.id] = err instanceof Error ? err.message : String(err);
    } finally {
      delete selectionSaving[agent.id];
    }
  }

  const resumeAgent = $derived(
    resumeAgentId === null ? null : (agents.find((agent) => agent.id === resumeAgentId) ?? null),
  );
  const resumeInfo = $derived(
    resumeAgentId === null ? null : (sessionInfoByAgent[resumeAgentId] ?? null),
  );

  function closeResume(): void {
    resumeAgentId = null;
    resumeLaunchError = null;
  }

  async function launchResumeInTerminal(): Promise<void> {
    if (resumeAgent === null || resumeLaunching || isActive(resumeAgent.id)) return;
    const agentId = resumeAgent.id;
    const operationId = ++resumeLaunchSequence;
    resumeLaunchOperation = { id: operationId, agentId };
    resumeLaunchError = null;
    try {
      await resumeAgentInTerminal(agentId);
      if (resumeLaunchOperation?.id !== operationId) return;
      resumeLaunchOperation = null;
      if (resumeAgentId === agentId) {
        resumeOpen = false;
        closeResume();
      }
    } catch (err) {
      if (resumeLaunchOperation?.id !== operationId) return;
      resumeLaunchOperation = null;
      if (resumeAgentId === agentId) {
        resumeLaunchError = err instanceof Error ? err.message : String(err);
      }
    }
  }

  function hasSessionActions(info: AgentSessionInfo | null | undefined): boolean {
    return Boolean(info?.session_file || info?.resume_command);
  }

  function refreshAgentSessionInfo(agentId: AgentId, force = false): void {
    if (sessionInfoInFlight[agentId] === true) return;
    if (!force && sessionInfoStarted[agentId] === true) return;
    if (force && hasSessionActions(sessionInfoByAgent[agentId])) return;

    sessionInfoStarted[agentId] = true;
    sessionInfoInFlight[agentId] = true;
    void agentSessionInfo(agentId)
      .then((info) => {
        sessionInfoByAgent[agentId] = info;
        if (sessionInfoError?.agentId === agentId) sessionInfoError = null;
      })
      .catch((err: unknown) => {
        sessionInfoByAgent[agentId] = null;
        sessionInfoError = {
          agentId,
          message: err instanceof Error ? err.message : String(err),
        };
      })
      .finally(() => {
        sessionInfoInFlight[agentId] = false;
      });
  }

  $effect(() => {
    const ids = new Set(agents.map((agent) => agent.id));
    for (const agent of agents) {
      refreshAgentSessionInfo(agent.id);
    }

    for (const id of Object.keys(sessionInfoByAgent)) {
      if (!ids.has(id)) delete sessionInfoByAgent[id];
    }
    for (const id of Object.keys(sessionInfoStarted)) {
      if (!ids.has(id)) delete sessionInfoStarted[id];
    }
    for (const id of Object.keys(sessionInfoInFlight)) {
      if (!ids.has(id)) delete sessionInfoInFlight[id];
    }
    if (removeConfirmAgentId !== null && !ids.has(removeConfirmAgentId))
      removeConfirmAgentId = null;
    if (compactConfirmAgentId !== null && !ids.has(compactConfirmAgentId))
      compactConfirmAgentId = null;
    if (resumeAgentId !== null && !ids.has(resumeAgentId)) {
      resumeAgentId = null;
      resumeOpen = false;
    }
    if (selectionEditingAgentId !== null && !ids.has(selectionEditingAgentId)) closeChange();
    for (const id of Object.keys(selectionSaving)) {
      if (!ids.has(id)) delete selectionSaving[id];
    }
    for (const id of Object.keys(selectionSaveErrors)) {
      if (!ids.has(id)) delete selectionSaveErrors[id];
    }
    if (reorderError !== null && !ids.has(reorderError.agentId)) reorderError = null;
    if (dragState !== null && !ids.has(dragState.agentId)) dragState = null;
    if (hoveredAgentId !== null && !ids.has(hoveredAgentId)) hoveredAgentId = null;
  });

  function openSessionFile(agent: AgentRecord): void {
    if (!sessionInfoByAgent[agent.id]?.session_file) return;
    void apiOpenSessionFile(agent.id).catch((err: unknown) => {
      console.error("[switchboard] open session file failed", err);
    });
  }

  function startRemove(agent: AgentRecord): void {
    removeError = null;
    removeConfirmAgentId = agent.id;
  }

  function cancelRemove(agentId: AgentId): void {
    if (removeConfirmAgentId === agentId) removeConfirmAgentId = null;
  }

  function cardClickToggles(event: MouseEvent): boolean {
    if (!(event.target instanceof Element)) return false;
    if (event.target.closest('[data-testid="agent-name"]') !== null) return false;
    const interactive = event.target.closest(
      "button, a, input, textarea, select, [role], [tabindex], [data-agent-card-control]",
    );
    if (interactive !== null && interactive !== event.currentTarget) return false;
    const selection = window.getSelection();
    return selection === null || selection.isCollapsed;
  }

  function onAgentCardClick(agentId: AgentId, event: MouseEvent): void {
    if (cardClickToggles(event)) toggleCollapsed(agentId);
  }

  function onAgentCardDoubleClick(agent: AgentRecord, event: MouseEvent): void {
    if (!(event.target instanceof Element)) return;
    if (event.target.closest('[data-testid="agent-name"]') === null) return;
    event.preventDefault();
    startEdit(agent);
  }

  function onAgentCardKeydown(agentId: AgentId, event: KeyboardEvent): void {
    if (event.target !== event.currentTarget || (event.key !== "Enter" && event.key !== " "))
      return;
    event.preventDefault();
    toggleCollapsed(agentId);
  }

  function agentRowPointerActions(node: HTMLElement, agentId: AgentId): { destroy: () => void } {
    const handlePointerEnter = (): void => {
      hoveredAgentId = agentId;
      refreshAgentSessionInfo(agentId, true);
    };
    const handlePointerLeave = (): void => {
      if (hoveredAgentId === agentId) hoveredAgentId = null;
      cancelRemove(agentId);
    };
    const handleClick = (event: MouseEvent): void => onAgentCardClick(agentId, event);
    const handleDoubleClick = (event: MouseEvent): void => {
      const agent = agents.find((candidate) => candidate.id === agentId);
      if (agent !== undefined) onAgentCardDoubleClick(agent, event);
    };
    // Alt+Arrow reorders the focused card. Skipped while typing — Alt+Arrow is
    // a text-caret motion inside the rename input.
    const handleKeydown = (event: KeyboardEvent): void => {
      onAgentCardKeydown(agentId, event);
      if (event.defaultPrevented) return;
      if (!event.altKey || (event.key !== "ArrowUp" && event.key !== "ArrowDown")) return;
      if (event.target instanceof HTMLInputElement || event.target instanceof HTMLTextAreaElement)
        return;
      event.preventDefault();
      event.stopPropagation();
      void moveAgentBy(agentId, event.key === "ArrowUp" ? -1 : 1);
    };
    node.addEventListener("pointerenter", handlePointerEnter);
    node.addEventListener("pointerleave", handlePointerLeave);
    node.addEventListener("click", handleClick);
    node.addEventListener("dblclick", handleDoubleClick);
    node.addEventListener("keydown", handleKeydown);
    return {
      destroy: () => {
        node.removeEventListener("pointerenter", handlePointerEnter);
        node.removeEventListener("pointerleave", handlePointerLeave);
        node.removeEventListener("click", handleClick);
        node.removeEventListener("dblclick", handleDoubleClick);
        node.removeEventListener("keydown", handleKeydown);
      },
    };
  }

  // --- Hover-revealed card controls ---------------------------------------
  // These give their width back when hidden rather than standing as an
  // invisible gutter. Measured in WebKit at the 280px default: the gutter held
  // 59px of empty space beside a name clipped by 39px, so reserving it cost
  // more room than the name was short — and the card's most-read text lost to
  // controls that were not on screen. Reclaiming it fits names that previously
  // could not fit at any sidebar width.
  //
  // Width and margin transition, so revealing the cluster slides the harness
  // icon rather than snapping it; that motion is what the reserved gutter was
  // trading the name's width to avoid. Same treatment the pane-member chips
  // use for their remove control.
  //
  // The reveal conditions have to be spelled out per variant: Tailwind scans
  // for literal class strings, so a composed `${variant}:w-[26px]` would never
  // be generated.
  const CONTROL_COLLAPSED =
    "pointer-events-none w-0 overflow-hidden opacity-0 transition-[width,margin,opacity]";
  const CONTROL_REVEAL_ICON_BUTTON = [
    "group-hover:pointer-events-auto group-hover:ml-0.5 group-hover:w-[26px] group-hover:opacity-100",
    "group-focus-visible:pointer-events-auto group-focus-visible:ml-0.5 group-focus-visible:w-[26px] group-focus-visible:opacity-100",
    "group-has-[:focus-visible]:pointer-events-auto group-has-[:focus-visible]:ml-0.5 group-has-[:focus-visible]:w-[26px] group-has-[:focus-visible]:opacity-100",
    "group-has-[[data-state=open]]:pointer-events-auto group-has-[[data-state=open]]:ml-0.5 group-has-[[data-state=open]]:w-[26px] group-has-[[data-state=open]]:opacity-100",
  ].join(" ");
  const CONTROL_REVEAL_GRIP = [
    "group-hover:pointer-events-auto group-hover:ml-0.5 group-hover:w-3 group-hover:opacity-100",
    "group-focus-visible:pointer-events-auto group-focus-visible:ml-0.5 group-focus-visible:w-3 group-focus-visible:opacity-100",
    "group-has-[:focus-visible]:pointer-events-auto group-has-[:focus-visible]:ml-0.5 group-has-[:focus-visible]:w-3 group-has-[:focus-visible]:opacity-100",
    "group-has-[[data-state=open]]:pointer-events-auto group-has-[[data-state=open]]:ml-0.5 group-has-[[data-state=open]]:w-3 group-has-[[data-state=open]]:opacity-100",
  ].join(" ");

  // --- Roster reordering -------------------------------------------------
  // Roster order is the canonical display order app-wide (these cards, the
  // compose chips and their ⌘1..9 numbering, pane columns), so all reorder
  // gestures funnel into one commit path: Alt+Arrow and dragging the leading-edge
  // hover grip.

  let reorderError = $state<{ agentId: AgentId; message: string } | null>(null);

  /// In-flight grip drag. `order` is the local preview the cards render from
  /// while dragging; the store is only touched on drop. `started` gates the
  /// slop threshold — an un-started drag is just a pressed grip and has no
  /// effect.
  let dragState = $state<{
    agentId: AgentId;
    pointerId: number;
    started: boolean;
    startX: number;
    startY: number;
    order: AgentId[];
  } | null>(null);

  /// Card currently under the pointer — the target of the Alt+Arrow reorder
  /// chord. Hover, not focus, because macOS WebKit does not focus buttons on
  /// click, so a focus-scoped chord would be unreachable by mouse. Mirrors
  /// `hoveredPaneId` in TranscriptPanes.
  let hoveredAgentId = $state<AgentId | null>(null);

  let agentListEl: HTMLElement | null = null;

  const displayAgents = $derived.by(() => {
    if (dragState === null || !dragState.started) return agents;
    const byId = new Map(agents.map((a) => [a.id, a]));
    const preview = dragState.order.flatMap((id) => byId.get(id) ?? []);
    return preview.length === agents.length ? preview : agents;
  });

  async function commitOrder(agentId: AgentId, order: AgentId[]): Promise<void> {
    if (order.length !== rosterIds.length || order.every((id, i) => id === rosterIds[i])) return;
    reorderError = null;
    try {
      await reorderAgents(projectId, order);
    } catch (err) {
      reorderError = {
        agentId,
        message: err instanceof Error ? err.message : String(err),
      };
    }
  }

  async function moveAgentBy(agentId: AgentId, delta: -1 | 1): Promise<void> {
    const from = rosterIds.indexOf(agentId);
    await commitOrder(agentId, movedOrder(rosterIds, from, from + delta));
  }

  /// Recompute the preview order from the rendered cards' midpoints. Reads
  /// geometry from the DOM (display order) rather than tracking it, so the
  /// math stays correct under collapsed/expanded cards of different heights.
  function updateDragOrder(drag: NonNullable<typeof dragState>, pointerY: number): void {
    if (agentListEl === null) return;
    const others: AgentId[] = [];
    const midpoints: number[] = [];
    for (const card of agentListEl.querySelectorAll<HTMLElement>("[data-agent-id]")) {
      const id = card.dataset.agentId;
      if (id === undefined || id === drag.agentId) continue;
      const rect = card.getBoundingClientRect();
      others.push(id);
      midpoints.push(rect.top + rect.height / 2);
    }
    const at = dropIndexForPointer(midpoints, pointerY);
    const next = [...others.slice(0, at), drag.agentId, ...others.slice(at)];
    if (!next.every((id, i) => id === drag.order[i])) drag.order = next;
  }

  /// Nudge the nearest scrollable ancestor while the drag pointer hugs its
  /// edge. Advances per pointermove (no rAF loop) — continuing to scroll
  /// requires wiggling the pointer, an accepted simplification for a sidebar
  /// roster.
  function dragAutoScroll(pointerY: number): void {
    for (let el = agentListEl; el !== null; el = el.parentElement) {
      if (el.scrollHeight <= el.clientHeight + 1) continue;
      const overflowY = getComputedStyle(el).overflowY;
      if (overflowY !== "auto" && overflowY !== "scroll") continue;
      const rect = el.getBoundingClientRect();
      if (pointerY < rect.top + 28) el.scrollTop -= 10;
      else if (pointerY > rect.bottom - 28) el.scrollTop += 10;
      return;
    }
  }

  /// Swallow the click the browser synthesizes right after pointerup, so
  /// dropping (or Escape-releasing) a drag never also toggles a card's
  /// collapse. Capture-phase, self-removing: the click fires synchronously
  /// after pointerup, so if none arrives by the next macrotask there is
  /// nothing to swallow and the trap is disarmed.
  function swallowNextClick(): void {
    const swallow = (event: MouseEvent): void => {
      event.preventDefault();
      event.stopPropagation();
    };
    window.addEventListener("click", swallow, { capture: true });
    setTimeout(() => window.removeEventListener("click", swallow, { capture: true }), 0);
  }

  /// Drag session: pointerdown on the grip arms it; move/up/cancel/Escape are
  /// window-level for the drag's lifetime. Window, NOT element listeners with
  /// pointer capture: the keyed {#each} moves the dragged card's DOM node on
  /// every preview reorder, and re-inserting a node silently releases its
  /// pointer capture — the drag would freeze mid-gesture (same pattern as the
  /// pane-gutter resize in TranscriptPanes).
  function beginDrag(agentId: AgentId, event: PointerEvent): void {
    if (agents.length < 2 || event.button !== 0 || dragState !== null) return;
    // Suppress text selection / focus side effects; the grip's click (collapse
    // toggle) still fires if the press never passes the slop threshold.
    event.preventDefault();
    const pointerId = event.pointerId;
    let cancelled = false;
    dragState = {
      agentId,
      pointerId,
      started: false,
      startX: event.clientX,
      startY: event.clientY,
      order: agents.map((a) => a.id),
    };
    const onMove = (e: PointerEvent): void => {
      if (e.pointerId !== pointerId || cancelled) return;
      const drag = dragState;
      if (drag === null) return;
      if (!drag.started) {
        if (Math.hypot(e.clientX - drag.startX, e.clientY - drag.startY) < DRAG_SLOP_PX) return;
        drag.started = true;
      }
      dragAutoScroll(e.clientY);
      updateDragOrder(drag, e.clientY);
    };
    const onUp = (e: PointerEvent): void => {
      if (e.pointerId !== pointerId) return;
      const drag = dragState;
      const started = cancelled || drag?.started === true;
      cleanup();
      dragState = null;
      if (started) swallowNextClick();
      if (cancelled || drag === null || !drag.started) return;
      void commitOrder(drag.agentId, drag.order);
    };
    const onCancel = (e: PointerEvent): void => {
      if (e.pointerId !== pointerId) return;
      cleanup();
      dragState = null;
    };
    // Escape reverts the preview immediately but keeps the listeners armed
    // until the actual pointerup, whose synthesized click still needs
    // swallowing.
    const onKey = (e: KeyboardEvent): void => {
      if (e.key !== "Escape" || cancelled || dragState?.started !== true) return;
      e.preventDefault();
      cancelled = true;
      dragState = null;
    };
    const cleanup = (): void => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onCancel);
      window.removeEventListener("keydown", onKey, { capture: true });
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", onCancel);
    window.addEventListener("keydown", onKey, { capture: true });
  }

  function gripDrag(node: HTMLElement, agentId: AgentId): { destroy: () => void } {
    const onPointerDown = (event: PointerEvent): void => beginDrag(agentId, event);
    node.addEventListener("pointerdown", onPointerDown);
    return {
      destroy: () => node.removeEventListener("pointerdown", onPointerDown),
    };
  }

  /// Alt+Arrow reorders the card under the pointer. The per-card focus-within
  /// handler (see `agentRowPointerActions`) takes precedence via
  /// stopPropagation when keyboard focus is inside a card.
  function onWindowKeydown(event: KeyboardEvent): void {
    if (!event.altKey || (event.key !== "ArrowUp" && event.key !== "ArrowDown")) return;
    if (hoveredAgentId === null) return;
    if (event.target instanceof HTMLInputElement || event.target instanceof HTMLTextAreaElement)
      return;
    event.preventDefault();
    void moveAgentBy(hoveredAgentId, event.key === "ArrowUp" ? -1 : 1);
  }

  async function confirmRemove(agent: AgentRecord): Promise<void> {
    removingAgentId = agent.id;
    removeError = null;
    try {
      await removeAgent(agent.id);
      layout.removeAgentCardState(projectId, agent.id);
      if (removeConfirmAgentId === agent.id) removeConfirmAgentId = null;
    } catch (err) {
      removeConfirmAgentId = null;
      removeError = {
        agentId: agent.id,
        message: err instanceof Error ? err.message : String(err),
      };
    } finally {
      removingAgentId = null;
    }
  }

  /// Start a manual compaction for `agentId`. The `send_id` is minted here, the
  /// same way the compose bar mints one per send, so the queued row has
  /// something to cancel with before any `turn_start` carries the id back.
  /// A tooltip trigger's own handler for `name`, so a trigger that needs its
  /// own handler can run both. Spreading the trigger props and then declaring
  /// the same handler on the element silently REPLACES the tooltip's — for
  /// `onpointerleave` that means it never closes, and hovering the next
  /// trigger stacks a second one behind it.
  function triggerHandler(
    props: Record<string, unknown>,
    name: string,
  ): ((event: Event) => void) | undefined {
    const handler = props[name];
    return typeof handler === "function" ? (handler as (event: Event) => void) : undefined;
  }

  function disarmCompaction(): void {
    compactConfirmAgentId = null;
    compactConfirmTooltipOpen = false;
  }

  async function startCompaction(agentId: AgentId): Promise<void> {
    disarmCompaction();
    await dispatchCompaction(agentId, crypto.randomUUID());
  }

  /// The agent whose breakdown panel is open, or `null`. One at a time — the
  /// panel is a modal.
  let breakdownAgentId = $state<AgentId | null>(null);
  const breakdownAgent = $derived(
    breakdownAgentId === null ? undefined : agents.find((a) => a.id === breakdownAgentId),
  );

  /// Whether this agent's own context report is the thing occupying it. The
  /// entry points below gate on the agent being idle, which would otherwise
  /// lock the user out of the panel their own report is filling: the dispatch
  /// makes the agent busy, so closing the dialog mid-run would leave no way
  /// back to the spinner.
  function contextReportInFlight(agentId: AgentId): boolean {
    const phase = runtimes[agentId]?.context_report_request?.phase;
    return phase === "queued" || phase === "running";
  }

  /// A breakdown can only be asked for while the agent is idle. The report
  /// goes through the same per-agent FIFO as sends, so on a busy agent it
  /// waits out the in-flight turn *and* every queued send — leaving the panel
  /// a featureless spinner for minutes, since opening is what dispatches.
  /// Refusing at the entry point keeps the wait to the ~1s an idle report
  /// takes, which is the wait the spinner-only panel was designed around.
  ///
  /// An agent with no runtime yet reads as idle, not busy: `run_status` is the
  /// dispatch lifecycle, so its absence means nothing has ever been dispatched.
  /// Defaulting the other way would disable the breakdown on exactly the agents
  /// most likely to be asked about — the ones just restored on project open.
  function canOpenContextBreakdown(agentId: AgentId): boolean {
    return (runtimes[agentId]?.run_status ?? "idle") === "idle" || contextReportInFlight(agentId);
  }

  /// Open first so feedback is immediate, then dispatch. **Opening is the
  /// refresh** — the panel carries no button, so every entry point asks for a
  /// current breakdown, and re-opening is how the user asks for another one
  /// (including after a failure). Cheap enough to do unconditionally: the
  /// report runs locally and bills nothing.
  ///
  /// The one exception is a request already queued or running, which would
  /// orphan the first request's correlation — that slot is single-occupancy, so
  /// closing and reopening mid-run rides the run already in flight.
  function openContextBreakdown(agentId: AgentId): void {
    breakdownAgentId = agentId;
    if (contextReportInFlight(agentId)) return;
    void startContextReport(agentId);
  }

  /// Unlike the compact button, no arm-then-confirm step: a report costs
  /// nothing, changes nothing, and is the thing the user just asked for.
  async function startContextReport(agentId: AgentId): Promise<void> {
    await dispatchContextReport(agentId, crypto.randomUUID());
  }

  type ContextOccupancy = { usedTokens: number; windowTokens: number; fraction: number };

  /// Context occupancy — `context_tokens_after_turn` over `context_window`
  /// from the most recent completed agent turn. Forward-looking signal ("how
  /// full will the next turn's context be"). The raw operands come back beside
  /// the fraction because the meter shows both, and rescanning the transcript
  /// for them could disagree with the fraction on a mid-render update.
  ///
  /// `context_input_tokens` is the harness-reconciled input-side occupancy
  /// (see `TurnUsage`): for Claude it sums the disjoint cache fields (cached +
  /// cache-creation, which `input_tokens` alone excludes — the cause of the
  /// near-0% bug), for Codex the adapter substitutes the session file's
  /// per-turn `last_token_usage` because the stream reports thread-cumulative
  /// totals (the cause of the >900% bug). Consuming the pre-reconciled
  /// value keeps this formula harness-agnostic — do not re-add per-harness
  /// token summation here. `context_tokens_after_turn` also keeps Claude's
  /// whole-dispatch billing output separate from the final parent call's
  /// occupancy. Both it and `context_window` must be present; otherwise the bar
  /// is hidden. An impossible occupancy is hidden rather than clamped, so bad
  /// telemetry never masquerades as a plausible 100%.
  function contextOccupancy(agentId: AgentId): ContextOccupancy | undefined {
    const turns = transcripts[agentId] ?? [];
    for (let i = turns.length - 1; i >= 0; i--) {
      const turn = turns[i];
      // A streaming turn has no terminal usage yet, so keep showing the last
      // completed state while it runs. Once a terminal usage record exists it
      // is authoritative: missing/invalid operands clean-hide rather than
      // falling back to an older, stale percentage.
      if (turn?.role !== "agent" || turn.status === "streaming" || turn.usage === undefined)
        continue;
      const window = turn.usage.context_window;
      if (window === undefined || window === null || window === 0) return undefined;
      const occupancy = turn.usage.context_tokens_after_turn;
      if (occupancy === undefined || occupancy === null) return undefined;
      if (occupancy > window) return undefined;
      return { usedTokens: occupancy, windowTokens: window, fraction: occupancy / window };
    }
    return undefined;
  }

  /// Full date+time for the tooltip's reset windows — a window (esp. the
  /// overage window) can be days out, so the tooltip carries the date the
  /// inline clock omits. Milliseconds since epoch. Display-only.
  function formatResetDateTime(ms: number): string {
    return new Date(ms).toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "numeric",
      minute: "2-digit",
    });
  }

  function toggleCollapsed(agentId: AgentId): void {
    layout.setAgentCardCollapsed(
      projectId,
      agentId,
      !layout.agentCardCollapsedFor(projectId, agentId),
    );
  }

  const allExpanded = $derived(
    agents.every((agent) => !layout.agentCardCollapsedFor(projectId, agent.id)),
  );

  function toggleAll(): void {
    layout.setAllAgentCardsCollapsed(
      projectId,
      agents.map((agent) => agent.id),
      allExpanded,
    );
  }

  /// Inline rename editor. Only one card edits at a time, so a single
  /// `editingAgentId` + `draftName` suffices; `renameError` holds a backend
  /// rejection (the live format/uniqueness check is `renameValidation`, the
  /// frontend mirror of the backend rules — the backend stays authoritative).
  let editingAgentId = $state<AgentId | null>(null);
  let draftName = $state<string>("");
  let renaming = $state<boolean>(false);
  let renameError = $state<string | null>(null);

  /// Validate the draft against the live roster, excluding the agent being
  /// edited so re-saving its own (or a case/hyphen-variant) name isn't a false
  /// duplicate. `renameMessage` suppresses the `empty` reason so an emptied
  /// field disables save without nagging mid-edit (mirrors the create form).
  const renameValidation = $derived<NameValidation>(
    editingAgentId === null ? { ok: true } : validateAgentName(draftName, agents, editingAgentId),
  );
  const renameMessage = $derived(
    renameValidation.ok || renameValidation.reason === "empty" ? null : renameValidation.message,
  );
  const canSave = $derived(renameValidation.ok && !renaming);

  function startEdit(agent: AgentRecord): void {
    editingAgentId = agent.id;
    draftName = agent.name;
    renameError = null;
  }

  function cancelEdit(): void {
    editingAgentId = null;
    renameError = null;
  }

  /// Commit the draft. An unchanged verbatim name skips the round-trip (a no-op
  /// rename just exits edit mode). On success the roster updates and we leave
  /// edit mode; on a backend rejection we stay in edit mode and surface it.
  async function commitEdit(agent: AgentRecord): Promise<void> {
    // Same gate the save button uses (`!canSave`), so the Enter path can't
    // double-submit while a rename is already in flight. Preserves the
    // unchanged-name skip: validation ok + not renaming → proceeds → the
    // `next === agent.name` branch exits without a round-trip.
    if (!canSave) return;
    const next = normalizeAgentName(draftName);
    if (next === agent.name) {
      cancelEdit();
      return;
    }
    renaming = true;
    renameError = null;
    try {
      await renameAgent(agent.id, next);
      editingAgentId = null;
    } catch (err) {
      renameError = err instanceof Error ? err.message : String(err);
    } finally {
      renaming = false;
    }
  }

  function onRenameKeydown(event: KeyboardEvent, agent: AgentRecord): void {
    if (event.key === "Enter") {
      event.preventDefault();
      void commitEdit(agent);
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
</script>

<!-- One description for one action, rendered by both affordances that trigger
     it: the agent-menu item and the context-bar button. Second line muted — a
     mechanical footnote to the first, not a second instruction. The armed
     button says only "Confirm compaction?"; at that point the user has already
     read this and is being asked one question. -->
<!-- One meter per usage window, for both harnesses. The inline detail is the
     countdown; the full reset date lives in the tooltip, where there is room
     for it. -->
{#snippet usageMeters(windows: UsageWindow[])}
  {#each windows as w (w.key)}
    <Meter
      label={w.label}
      value={w.usedFraction}
      detail={w.resetsAtMs === null ? undefined : formatResetCountdown(w.resetsAtMs)}
      separateDetail
      alignPercentage
      tone={w.surpassedThreshold === undefined ? "neutral" : "warning"}
      testid="agent-usage-window"
    />
  {/each}
{/snippet}

<!-- Tooltip rows for the same windows: the percentage spelled out and the full
     reset date the inline countdown compresses. The meter's amber tone carries
     the harness's threshold warning without repeating its internal cutoff. -->
{#snippet usageWindowDetail(windows: UsageWindow[])}
  <div class="min-w-64 space-y-2.5">
    {#each windows as w (w.key)}
      <section class="space-y-1">
        <div class="flex items-baseline gap-4">
          <span class="min-w-0 font-medium">{w.label}</span>
          <span class="ml-auto shrink-0 tabular-nums">{formatUsedPercent(w.usedFraction)} used</span
          >
        </div>
        {#if w.resetsAtMs !== null}
          <div class="text-primary-fg/70 grid grid-cols-[auto_1fr] gap-4 text-[12px]">
            <span>Resets</span>
            <span class="text-right tabular-nums">{formatResetDateTime(w.resetsAtMs)}</span>
          </div>
        {/if}
      </section>
    {/each}
  </div>
{/snippet}

{#snippet compactTooltipContent()}
  <div class="max-w-xs space-y-1 text-[13px]">
    <p class="font-medium">Compact the conversation</p>
    <p class="text-primary-fg/70">Runs as a turn; queues if the agent is busy.</p>
  </div>
{/snippet}

<svelte:window onkeydown={onWindowKeydown} />

<SidebarPanel side="right" width={draftWidth ?? layout.agentsSidebarWidth} testid="sidebar">
  <ResizeHandle
    value={() => draftWidth ?? layout.agentsSidebarWidth}
    min={SIDEBAR_MIN_WIDTH}
    max={sidebarMaxWidth}
    edge="start"
    label="Resize agents sidebar"
    testid="agents-sidebar-resizer"
    class="hover:bg-focus absolute inset-y-0 left-0 z-10 w-1 transition-colors"
    onDraft={(px) => (draftWidth = px)}
    onCommit={(px) => {
      layout.agentsSidebarWidth = px;
      draftWidth = null;
    }}
    onReset={() => {
      layout.agentsSidebarWidth = AGENTS_SIDEBAR_DEFAULT_WIDTH;
      draftWidth = null;
    }}
  />
  <SidebarSection title="Agents">
    {#snippet action()}
      <div class="flex items-center gap-0.5">
        {#if hiddenAgentCount > 0}
          <Tooltip label="Show all agents" side="bottom">
            {#snippet trigger(props)}
              <button
                {...props}
                type="button"
                class="text-muted hover:text-fg shrink-0 px-1 text-[11px] hover:underline"
                aria-label={`${hiddenAgentCount} hidden — show all agents`}
                data-testid="sidebar-show-all-agents"
                onclick={() => showAllAgents(projectId, rosterIds)}
              >
                {hiddenAgentCount} hidden
              </button>
            {/snippet}
          </Tooltip>
        {/if}
        {#if agents.length > 0}
          <Tooltip label={allExpanded ? "Collapse all" : "Expand all"} side="bottom">
            {#snippet trigger(props)}
              <button
                {...props}
                type="button"
                class={ICON_BUTTON_ON_PANEL_CLASS}
                aria-label={allExpanded ? "Collapse all agents" : "Expand all agents"}
                data-testid="sidebar-toggle-all"
                onclick={toggleAll}
              >
                <ExpandCollapseIcon expanded={allExpanded} size={14} />
              </button>
            {/snippet}
          </Tooltip>
        {/if}
        {#if onAddAgent}
          <Tooltip label="Add agent" side="bottom">
            {#snippet trigger(props)}
              <button
                {...props}
                type="button"
                class={ICON_BUTTON_ON_PANEL_CLASS}
                aria-label="Add agent"
                data-testid="sidebar-add-agent"
                onclick={onAddAgent}
              >
                <PlusIcon />
              </button>
            {/snippet}
          </Tooltip>
        {/if}
      </div>
    {/snippet}

    {#if agents.length === 0}
      <p class="text-muted px-3 py-3 text-xs">No agents in this project yet.</p>
    {/if}
    <div class="flex flex-col gap-1.5 px-2 pt-1 pb-2" bind:this={agentListEl}>
      {#each displayAgents as agent (agent.id)}
        {@const runtime = runtimes[agent.id]}
        {@const context = contextOccupancy(agent.id)}
        {@const codexWindows =
          agent.harness === "codex" ? codexRateLimitView(runtime?.last_rate_limit, Date.now()) : []}
        <!-- `Date.now()` read once per render for the reset-in-the-future gate.
             Non-reactive: a reset that elapses while the app sits open won't
             auto-hide until the next render, which a new turn (or reopen)
             triggers — acceptable for a passive status cell. -->
        {@const rlView =
          agent.harness === "claude_code"
            ? claudeRateLimitView(
                runtime?.last_rate_limit,
                Date.now(),
                runtime?.last_rate_limit_model,
              )
            : null}
        {@const overageAsOf = runtime?.last_rate_limit_as_of}
        <!-- At most one window is ever flagged: the payload names a single
             `rateLimitType`, and `claudeRateLimitView` stamps the threshold on
             that window alone. `find` is the shape of that invariant — if a
             future CLI reports a threshold per window, it is `usageWindows.ts`
             that has to change first. -->
        {@const usageWarning = rlView?.windows.find(
          (window) => window.surpassedThreshold !== undefined,
        )}
        {@const agentSelection = selectionForAgent(agent)}
        {@const effortSupport = effortSupportFor(agent.harness, agent.model)}
        {@const emptySelection =
          agent.model === null &&
          agent.effort === null &&
          agent.model_choices.length === 0 &&
          agent.effort_choices.length === 0}
        {@const isCollapsed = layout.agentCardCollapsedFor(projectId, agent.id)}
        {@const active = isActive(agent.id)}
        {@const recipientSelected = !workflowActive && recipientSelection.includes(agent.id)}
        {@const sessionInfo = sessionInfoByAgent[agent.id]}
        {@const confirmingRemove = removeConfirmAgentId === agent.id}
        <!-- A native button cannot contain the card's controls. The focusable
             composite surface mirrors its pointer toggle on Enter/Space, while
             each nested control keeps its own semantics. -->
        <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
        <div
          class={cn(
            "group bg-raised hover:ring-active focus-visible:ring-focus relative cursor-pointer rounded-lg px-2.5 py-2 transition-shadow hover:shadow-sm hover:ring-1 focus-visible:ring-1 focus-visible:outline-none",
            recipientSelected && "ring-accent hover:ring-accent ring-1",
            dragState?.started === true &&
              dragState.agentId === agent.id &&
              "ring-accent/60 relative z-10 shadow-lg ring-1",
          )}
          data-testid="sidebar-agent"
          data-agent-id={agent.id}
          data-collapsed={isCollapsed}
          data-recipient-selected={recipientSelected}
          tabindex="0"
          aria-label={`${agent.name}, ${isCollapsed ? "collapsed" : "expanded"}. Press Enter or Space to toggle details.`}
          use:agentRowPointerActions={agent.id}
          animate:flip={{ duration: dragState?.started === true ? 0 : 150 }}
        >
          <div class="flex items-center justify-between gap-1">
            {#if editingAgentId === agent.id}
              <!-- Edit mode swaps the whole left side: an <input> can't nest in
                   the collapse-toggle <button>, and the harness icon becomes a
                   save (check) button. Blur cancels (never persist on blur); the
                   save button's mousedown-preventDefault keeps focus so its click
                   commits before blur-cancel can fire. -->
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
                aria-label="Agent name"
                aria-invalid={!renameValidation.ok}
                aria-describedby={renameError ? `agent-rename-error-${agent.id}` : undefined}
                data-testid="agent-rename-input"
                onkeydown={(event) => onRenameKeydown(event, agent)}
                onblur={cancelEdit}
              />
              <Tooltip label="Save" side="bottom">
                {#snippet trigger(props)}
                  <button
                    {...props}
                    type="button"
                    class={cn(
                      ICON_BUTTON_CLASS,
                      "shrink-0 disabled:cursor-not-allowed disabled:opacity-50",
                    )}
                    disabled={!canSave}
                    aria-label="Save name"
                    data-testid="agent-rename-save"
                    onmousedown={(event) => event.preventDefault()}
                    onclick={() => void commitEdit(agent)}
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
              {@const agentHidden = isAgentHidden(projectId, rosterIds, agent.id)}
              <button
                type="button"
                class="text-muted hover:text-fg hover:bg-control-hover focus-visible:ring-focus inline-flex h-6 w-5 shrink-0 items-center justify-center rounded-full focus-visible:ring-1 focus-visible:outline-none"
                aria-label={isCollapsed ? `Expand ${agent.name}` : `Collapse ${agent.name}`}
                aria-expanded={!isCollapsed}
                data-testid="agent-collapse-toggle"
                onclick={() => toggleCollapsed(agent.id)}
              >
                {#if isCollapsed}
                  <ChevronRight size={13} strokeWidth={1.8} aria-hidden="true" />
                {:else}
                  <ChevronDown size={13} strokeWidth={1.8} aria-hidden="true" />
                {/if}
              </button>
              <!-- The reserved action gutter narrows this column, so a long
                   name clips and needs a recovery path. `TruncatedText` is that
                   path and only that path: it measures actual clipping, so a
                   name that fits raises no tooltip repeating text the user can
                   already read. Keyboard users get the full name from the
                   card's own `aria-label`, which is why the tooltip is not
                   focusable. `data-testid` stays on the text span itself —
                   `cardClickToggles` and double-click-to-rename both resolve
                   through `closest('[data-testid="agent-name"]')`. -->
              <div class="flex min-h-7 min-w-0 flex-1 items-center text-left">
                <TruncatedText
                  text={agent.name}
                  class="text-fg cursor-text text-[13px] font-semibold"
                  testid="agent-name"
                />
              </div>
              <!-- No `gap`: a flex gap is charged for a zero-width child too,
                   which would leave a residual gutter exactly like the one this
                   cluster stopped reserving. Each control carries its own
                   `ml-0.5`, applied only when it is revealed. -->
              <div class="flex shrink-0 items-center">
                <Tooltip
                  label={agentHidden ? `Show ${agent.name}` : `Hide ${agent.name} (⌥-click: solo)`}
                  delayDuration={800}
                  reopen="fresh-hover"
                >
                  {#snippet trigger(props)}
                    <button
                      {...props}
                      type="button"
                      class={cn(
                        ICON_BUTTON_CLASS,
                        "shrink-0",
                        // The eye stays visible while the agent is hidden (it's
                        // the state indicator, and a hidden agent's card must
                        // say so without being hovered); otherwise it collapses
                        // to nothing like the rest of the cluster.
                        agentHidden
                          ? "text-muted ml-0.5"
                          : cn(CONTROL_COLLAPSED, CONTROL_REVEAL_ICON_BUTTON),
                      )}
                      aria-label={agentHidden ? `Show ${agent.name}` : `Hide ${agent.name}`}
                      aria-pressed={agentHidden}
                      data-testid="agent-visibility-toggle"
                      onclick={(event) => onVisibilityClick(agent, event)}
                    >
                      {#if agentHidden}
                        <EyeOff size={14} strokeWidth={1.8} aria-hidden="true" />
                      {:else}
                        <Eye size={14} strokeWidth={1.8} aria-hidden="true" />
                      {/if}
                    </button>
                  {/snippet}
                </Tooltip>
                <DropdownMenu
                  triggerClass={cn(
                    ICON_BUTTON_CLASS,
                    "shrink-0",
                    CONTROL_COLLAPSED,
                    CONTROL_REVEAL_ICON_BUTTON,
                  )}
                  triggerLabel={`Actions for ${agent.name}`}
                  triggerTestid="agent-actions-trigger"
                  contentTestid="agent-actions-menu"
                >
                  {#snippet trigger()}
                    <MoreHorizontal size={14} strokeWidth={1.8} aria-hidden="true" />
                  {/snippet}
                  {#if confirmingRemove}
                    <DropdownMenuItem
                      onSelect={() => cancelRemove(agent.id)}
                      closeOnSelect={false}
                      class="gap-2"
                      data-testid="agent-remove-cancel"
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
                      onSelect={() => void confirmRemove(agent)}
                      disabled={removingAgentId === agent.id}
                      class="text-status-failed gap-2"
                      data-testid="agent-remove-confirm"
                    >
                      <Check size={14} strokeWidth={1.8} class="shrink-0" aria-hidden="true" />
                      Confirm delete
                    </DropdownMenuItem>
                  {:else}
                    <DropdownMenuItem
                      onSelect={() => startEdit(agent)}
                      class="gap-2"
                      data-testid="agent-action-rename"
                    >
                      <Pencil
                        size={14}
                        strokeWidth={1.8}
                        class="text-muted shrink-0"
                        aria-hidden="true"
                      />
                      Rename
                    </DropdownMenuItem>
                    <DropdownMenuItem
                      onSelect={() => toggleCollapsed(agent.id)}
                      class="gap-2"
                      data-testid="agent-action-collapse"
                    >
                      <ExpandCollapseIcon
                        expanded={!isCollapsed}
                        size={14}
                        strokeWidth={1.8}
                        class="text-muted shrink-0"
                      />
                      {isCollapsed ? "Expand" : "Collapse"}
                    </DropdownMenuItem>
                    {#if active}
                      <DropdownMenuItem
                        onSelect={() => stopAgent(agent.id)}
                        class="text-status-failed gap-2"
                        data-testid="agent-action-stop"
                      >
                        <Square size={14} strokeWidth={1.8} class="shrink-0" aria-hidden="true" />
                        Stop agent
                      </DropdownMenuItem>
                    {/if}
                    {#if supportsContextReport(agent.harness)}
                      <!-- Gated on the same rule as the card's icon: the menu
                           is a second door to one action, and a report queued
                           from here would sit behind the agent's backlog just
                           the same. -->
                      <DropdownMenuItem
                        onSelect={() => openContextBreakdown(agent.id)}
                        disabled={!canOpenContextBreakdown(agent.id)}
                        class="gap-2"
                        data-testid="agent-action-context-breakdown"
                      >
                        <ChartPie
                          size={14}
                          strokeWidth={1.8}
                          class="text-muted shrink-0"
                          aria-hidden="true"
                        />
                        Context breakdown…
                      </DropdownMenuItem>
                    {/if}
                    {#if supportsManualCompaction(agent.harness)}
                      <!-- Never disabled while busy: a compaction queues behind
                           the running turn like any other work, so greying it out
                           would refuse something the backend accepts. -->
                      <DropdownMenuItem
                        onSelect={() => void startCompaction(agent.id)}
                        class="gap-2"
                        tooltipContent={compactTooltipContent}
                        data-testid="agent-action-compact"
                      >
                        <RotateCcw
                          size={14}
                          strokeWidth={1.8}
                          class="text-muted shrink-0"
                          aria-hidden="true"
                        />
                        Compact context
                      </DropdownMenuItem>
                    {/if}
                    {#if sessionInfo?.resume_command}
                      <DropdownMenuItem
                        onSelect={() => {
                          resumeAgentId = agent.id;
                          resumeLaunchError = null;
                          resumeOpen = true;
                        }}
                        class="gap-2"
                        data-testid="agent-action-resume"
                      >
                        <Terminal
                          size={14}
                          strokeWidth={1.8}
                          class="text-muted shrink-0"
                          aria-hidden="true"
                        />
                        Resume in terminal
                      </DropdownMenuItem>
                    {/if}
                    {#if sessionInfo?.session_file}
                      <DropdownMenuItem
                        onSelect={() => openSessionFile(agent)}
                        class="gap-2"
                        data-testid="agent-action-open-session"
                      >
                        <FileText
                          size={14}
                          strokeWidth={1.8}
                          class="text-muted shrink-0"
                          aria-hidden="true"
                        />
                        Open session file
                      </DropdownMenuItem>
                    {/if}
                    {#if canConfigureSelection(agent)}
                      <DropdownMenuItem
                        onSelect={() => openSelectionSettings(agent)}
                        disabled={selectionBusy(agent.id)}
                        class="gap-2"
                        data-testid="agent-selection-settings"
                      >
                        <SlidersHorizontal
                          size={14}
                          strokeWidth={1.8}
                          class="text-muted shrink-0"
                          aria-hidden="true"
                        />
                        Model settings…
                      </DropdownMenuItem>
                    {/if}
                    <!-- Pane assignment. Move, never copy: an agent can belong
                         to at most one pane. Listed flat (no submenu): a
                         project realistically has a handful of panes. -->
                    {#if paneLayout.panes.length > 1}
                      {@const ownPaneId = paneOfAgent(projectId, rosterIds, agent.id)?.id}
                      {#each paneLayout.panes.filter((p) => p.id !== ownPaneId) as pane (pane.id)}
                        <DropdownMenuItem
                          onSelect={() => {
                            moveAgentToPane(projectId, rosterIds, agent.id, pane.id);
                            selectAgent(projectId, agent.id);
                          }}
                          class="gap-2"
                          data-testid={`agent-move-to-pane-${pane.id}`}
                        >
                          <Columns2
                            size={14}
                            strokeWidth={1.8}
                            class="text-muted shrink-0"
                            aria-hidden="true"
                          />
                          Move to {pane.name}
                        </DropdownMenuItem>
                      {/each}
                    {/if}
                    {#if agents.length > 1}
                      <DropdownMenuItem
                        onSelect={() => {
                          moveAgentToNewPane(projectId, rosterIds, agent.id);
                          selectAgent(projectId, agent.id);
                        }}
                        class="gap-2"
                        data-testid="agent-move-to-new-pane"
                      >
                        <Columns2
                          size={14}
                          strokeWidth={1.8}
                          class="text-muted shrink-0"
                          aria-hidden="true"
                        />
                        Move to new pane
                      </DropdownMenuItem>
                    {/if}
                    {#if !active}
                      <DropdownMenuItem
                        onSelect={() => startRemove(agent)}
                        closeOnSelect={false}
                        class="text-status-failed gap-2"
                        data-testid="agent-action-remove"
                        tooltip="Deletes Switchboard's files for this agent; underlying session files are kept, and its responses are removed from the conversation."
                      >
                        <Trash2 size={14} strokeWidth={1.8} class="shrink-0" aria-hidden="true" />
                        Delete agent
                      </DropdownMenuItem>
                    {/if}
                  {/if}
                </DropdownMenu>
                <HarnessIcon
                  harness={agent.harness}
                  size="md"
                  class="ml-0.5"
                  testid="agent-harness-icon"
                />
                {#if agents.length > 1}
                  <span
                    class={cn(
                      "text-muted flex h-4 w-3 shrink-0 cursor-grab touch-none items-center justify-center active:cursor-grabbing",
                      // A grip mid-drag stays put: collapsing the control the
                      // pointer is holding would yank the card out from under
                      // the gesture.
                      dragState?.agentId === agent.id
                        ? "ml-0.5"
                        : cn(CONTROL_COLLAPSED, CONTROL_REVEAL_GRIP),
                    )}
                    data-testid="agent-drag-grip"
                    data-agent-card-control
                    aria-hidden="true"
                    use:gripDrag={agent.id}
                  >
                    <GripVertical size={12} strokeWidth={1.8} />
                  </span>
                {/if}
              </div>
            {/if}
          </div>
          {#if sessionInfoError?.agentId === agent.id}
            <div class="text-status-failed mt-1 text-xs" data-testid="agent-actions-error">
              Couldn't read session state: {sessionInfoError.message}
            </div>
          {/if}
          {#if editingAgentId === agent.id && renameError}
            <div
              id={`agent-rename-error-${agent.id}`}
              class="text-status-failed mt-1 text-xs"
              data-testid="agent-rename-error"
            >
              {renameError}
            </div>
          {/if}
          {#if runtime?.hydration_error}
            <div class="mt-1 space-y-1" data-testid="agent-hydration-error">
              <!-- Clamp the inline reason to two lines: it keeps an
                     at-a-glance "why" without a long path-bearing error (the
                     `LoadTranscriptError::Io` message now names the session
                     file) ballooning the narrow card. The full verbatim text
                     stays available via Details. -->
              <div class="text-status-failed line-clamp-2 text-xs break-words">
                history failed to load: {runtime.hydration_error}
              </div>
              <div class="flex items-center gap-3 text-xs">
                <button
                  type="button"
                  class="text-accent hover:underline"
                  data-testid="agent-hydration-retry"
                  onclick={() => void retryAgentHydration(agent.id)}
                >
                  Retry
                </button>
                <button
                  type="button"
                  class="text-muted hover:text-fg hover:underline"
                  data-testid="agent-hydration-details"
                  onclick={() => {
                    hydrationDetailsName = agent.name;
                    hydrationDetailsError = runtime.hydration_error ?? "";
                    hydrationDetailsOpen = true;
                  }}
                >
                  Details
                </button>
              </div>
            </div>
          {/if}
          <!-- Selected model/effort is future-send intent, never observed
                 runtime history. The transcript footer owns the actual model
                 used by each completed turn. -->
          {#if canConfigureSelection(agent)}
            <div
              class="mt-1.5 flex min-w-0 flex-wrap items-center gap-1"
              data-testid="agent-selection"
            >
              {#if emptySelection}
                <span class="text-muted text-xs" data-testid="agent-selection-default">
                  Harness/session default
                </span>
              {:else if agent.model_choices.length > 0}
                <AgentSelectionChip
                  axis="model"
                  harness={agent.harness}
                  selection={agentSelection}
                  busy={selectionBusy(agent.id)}
                  onActivate={(selection) => void activateSelection(agent, selection)}
                />
              {:else if agent.model === null && SUPPORTS_MODEL_SELECTION[agent.harness]}
                <span class="text-muted text-xs" data-testid="agent-model-default">
                  Model: Harness/session default
                </span>
              {/if}
              {#if agent.effort_choices.length > 0 && effortSupport.kind !== "none"}
                <AgentSelectionChip
                  axis="effort"
                  harness={agent.harness}
                  selection={agentSelection}
                  busy={selectionBusy(agent.id)}
                  onActivate={(selection) => void activateSelection(agent, selection)}
                />
              {:else if !emptySelection && agent.effort === null && effortSupport.kind !== "none" && SUPPORTS_EFFORT_SELECTION[agent.harness]}
                <span class="text-muted text-xs" data-testid="agent-effort-default">
                  Effort: Harness/session default
                </span>
              {/if}
            </div>
          {/if}
          {#if selectionSaveErrors[agent.id]}
            <p class="text-status-failed mt-1 text-xs" data-testid="agent-selection-save-error">
              {selectionSaveErrors[agent.id]}
            </p>
          {/if}
          <!-- Clean-hide convention: every metadata cell above and below is
                 presence-gated, so a value a harness never reports simply never
                 renders — no blank label, no empty bar, no "—" placeholder. These
                 absences are correct, not gaps: some harnesses expose no `context_window`
                 (the bar below never renders for it), and Antigravity reports no
                 cost / quota / context at all. A transient absence (a fresh agent
                 pre-first-turn) hides identically to a permanent one; that's the
                 intended behavior, not a case to distinguish. -->
          {#if context !== undefined}
            <div class="mt-1.5 flex items-end gap-1.5" data-testid="agent-context-bar">
              <!-- "Context", not "Context used": the row carries two buttons
                     beside the meter and the longer label no longer fits — it
                     clipped by 11px at the default sidebar width, measured in
                     WebKit. Nothing is lost, since the detail beside it already
                     reads "121.1k / 1M · 12%". Pinned by
                     `tests/browser/agent-context-row-fit.browser.test.ts`. -->
              <Meter
                label="Context"
                value={context.fraction}
                detail="{formatTokens(context.usedTokens)} / {formatTokens(context.windowTokens)}"
                class="flex-1"
              />
              {#if supportsContextReport(agent.harness)}
                <!-- The meter says how full; this asks for a fresh breakdown
                     and opens the result panel immediately. -->
                {@const breakdownAvailable = canOpenContextBreakdown(agent.id)}
                <Tooltip
                  label={breakdownAvailable
                    ? "Context breakdown"
                    : "Context breakdown — available when the agent is idle"}
                  side="top"
                >
                  {#snippet trigger(props)}
                    <button
                      {...props}
                      type="button"
                      class={cn(
                        ICON_BUTTON_CLASS,
                        "-mb-0.5 h-5 w-5",
                        "disabled:opacity-40 disabled:hover:bg-transparent",
                      )}
                      aria-label="Context breakdown"
                      disabled={!breakdownAvailable}
                      data-testid="agent-context-breakdown-button"
                      onclick={() => openContextBreakdown(agent.id)}
                    >
                      <ChartPie size={14} strokeWidth={1.8} aria-hidden="true" />
                    </button>
                  {/snippet}
                </Tooltip>
              {/if}
              {#if supportsManualCompaction(agent.harness)}
                {@const armed = compactConfirmAgentId === agent.id}
                {#snippet compactButton(props: Record<string, unknown>, armed: boolean)}
                  {@const closeOnLeave = triggerHandler(props, "onpointerleave")}
                  {@const closeOnBlur = triggerHandler(props, "onblur")}
                  <button
                    {...props}
                    type="button"
                    class={armed
                      ? cn(ICON_BUTTON_CLASS, "text-accent -mb-0.5 h-5 w-5")
                      : cn(ICON_BUTTON_CLASS, "-mb-0.5 h-5 w-5")}
                    aria-label={armed ? "Compact now" : "Compact context"}
                    data-armed={armed ? "true" : "false"}
                    data-testid="agent-compact-button"
                    onclick={() => {
                      if (armed) {
                        void startCompaction(agent.id);
                      } else {
                        compactConfirmAgentId = agent.id;
                        compactConfirmTooltipOpen = true;
                      }
                    }}
                    onpointerleave={(event) => {
                      closeOnLeave?.(event);
                      disarmCompaction();
                    }}
                    onblur={(event) => {
                      closeOnBlur?.(event);
                      disarmCompaction();
                    }}
                  >
                    {#if armed}
                      <Check size={13} strokeWidth={2.2} aria-hidden="true" />
                    {:else}
                      <RotateCcw size={14} strokeWidth={1.8} aria-hidden="true" />
                    {/if}
                  </button>
                {/snippet}
                <!-- Two instances, not one with a swapped label. The primitive
                       closes a tooltip when its trigger is clicked and latches it
                       shut until a fresh pointer-enter — right for an ordinary
                       button, wrong here, where the click is exactly what needs
                       explaining and the pointer never leaves. Arming mounts a
                       second, unsuppressed tooltip already open. Disarms on
                       pointer leave rather than on a timer or an outside click:
                       the button is the only thing that armed it, so leaving it
                       is the clearest "I didn't mean that". -->
                {#if armed}
                  <Tooltip
                    label="Confirm compaction?"
                    side="top"
                    bind:open={compactConfirmTooltipOpen}
                  >
                    {#snippet trigger(props)}
                      {@render compactButton(props, true)}
                    {/snippet}
                  </Tooltip>
                {:else}
                  <Tooltip side="top" reopen="fresh-hover">
                    {#snippet trigger(props)}
                      {@render compactButton(props, false)}
                    {/snippet}
                    {@render compactTooltipContent()}
                  </Tooltip>
                {/if}
              {/if}
            </div>
          {/if}
          {#if isCollapsed && (usageWarning !== undefined || (rlView?.overage ?? null) !== null)}
            <div
              class="text-warning mt-1.5 space-y-0.5 text-[11px]"
              data-testid="agent-compact-warnings"
            >
              {#if usageWarning !== undefined}
                <p>{usageWarning.label} · {formatUsedPercent(usageWarning.usedFraction)} used</p>
              {/if}
              {#if (rlView?.overage ?? null) !== null}
                <p>⚡ using credits</p>
              {/if}
            </div>
          {/if}
          {#if !isCollapsed}
            <!-- Per-turn cost is deliberately NOT shown on the card — it
                 renders inline per-message in the transcript (real-spend turns
                 only). There is no per-agent cost total (system-design §2): the
                 old accumulating `$` figure read as a running total but wasn't
                 one. Do not re-add it. The current overage *status* below stays
                 (Bucket-A "as of now" state). -->
            {#if rlView !== null}
              <!-- Claude usage windows — one meter per window the payload
                   reports, each shown only while its own reset is still in the
                   future (a past "resets at" would be wrong, so it clean-hides
                   instead). A window the CLI itself flagged past a threshold
                   fills amber. The overage escalation is a separate signal
                   about billing, layered beneath the meters only when spending
                   credits. One always-present tooltip carries full reset dates
                   (a weekly window is days out, beyond the inline countdown),
                   and the snapshot age when rehydrated.
                   Stream-only, so it survives restart via the metadata
                   sidecar. -->
              <div class="text-muted mt-2 text-[10px] font-medium tracking-wide uppercase">
                Usage limits
              </div>
              <Tooltip side="right">
                {#snippet trigger(props)}
                  <!-- tabindex=0 so keyboard users can open the tooltip; a <div>
                       (no click action) isn't focusable on its own. Mirrors the
                       parse-warnings indicator. -->
                  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
                  <div
                    {...props}
                    tabindex="0"
                    class="mt-1.5 cursor-default space-y-1 text-xs"
                    data-testid="agent-rate-limit-claude"
                  >
                    {@render usageMeters(rlView.windows)}
                    {#if rlView.fallback !== null}
                      <div class="text-fg" data-testid="agent-rate-window">
                        {rlView.fallback.label} resets {formatResetCountdown(
                          rlView.fallback.resetsAtMs,
                        )}
                      </div>
                    {/if}
                    {#if rlView.overage !== null}
                      <!-- -ml-1 offsets the ⚡ glyph's left-side bearing so it
                           aligns with the text column above (the emoji box
                           carries a few px of transparent left padding). -->
                      <div class="text-warning -ml-1" data-testid="agent-overage">
                        ⚡ using credits
                      </div>
                    {/if}
                  </div>
                {/snippet}
                <div class="space-y-2.5 text-[13px]" data-testid="agent-rate-detail">
                  <p class="font-medium">Usage details</p>
                  {@render usageWindowDetail(rlView.windows)}
                  {#if rlView.fallback !== null}
                    <div class="grid grid-cols-[auto_1fr] gap-4">
                      <span>{rlView.fallback.label}</span>
                      <span class="text-right tabular-nums">
                        Resets {formatResetDateTime(rlView.fallback.resetsAtMs)}
                      </span>
                    </div>
                  {/if}
                  {#if rlView.overage !== null}
                    <div class="text-warning border-primary-fg/20 border-t pt-2">
                      <p class="font-medium">Spending usage credits</p>
                      {#if rlView.overage.resetsAtMs !== null}
                        <p class="mt-0.5 text-[12px]">
                          Overage window resets {formatResetDateTime(rlView.overage.resetsAtMs)}
                        </p>
                      {/if}
                    </div>
                  {/if}
                  {#if overageAsOf != null}
                    <p
                      class="text-primary-fg/70 border-primary-fg/20 border-t pt-2 text-[12px]"
                      data-testid="agent-rate-snapshot"
                    >
                      Snapshot from {relativeTime(overageAsOf)} — send a message to refresh.
                    </p>
                  {/if}
                </div>
              </Tooltip>
            {/if}
            {#if codexWindows.length > 0}
              <!-- Codex usage windows — the same meters with the same labels as
                   Claude's. Session-file-backed (class B, durable), so no
                   snapshot-age qualifier, and Codex reports no threshold flag,
                   so no window here ever warns. -->
              <div class="text-muted mt-2 text-[10px] font-medium tracking-wide uppercase">
                Usage limits
              </div>
              <Tooltip side="right">
                {#snippet trigger(props)}
                  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
                  <div
                    {...props}
                    tabindex="0"
                    class="mt-1.5 cursor-default space-y-1 text-xs"
                    data-testid="agent-rate-limit"
                  >
                    {@render usageMeters(codexWindows)}
                  </div>
                {/snippet}
                <div class="space-y-2.5 text-[13px]" data-testid="agent-rate-limit-detail">
                  <p class="font-medium">Usage details</p>
                  {@render usageWindowDetail(codexWindows)}
                </div>
              </Tooltip>
            {/if}
            <AgentEnvironment inventory={runtime?.meta?.inventory} asOf={runtime?.meta_as_of} />
          {/if}
          {#if removeError?.agentId === agent.id}
            <div class="text-status-failed mt-1 text-xs" data-testid="agent-remove-error">
              Couldn't delete agent: {removeError.message}
            </div>
          {/if}
          {#if reorderError?.agentId === agent.id}
            <div class="text-status-failed mt-1 text-xs" data-testid="agent-reorder-error">
              Couldn't reorder agents: {reorderError.message}
            </div>
          {/if}
        </div>
      {/each}
    </div>
  </SidebarSection>
</SidebarPanel>

<ContextBreakdown
  open={breakdownAgent !== undefined}
  onClose={() => (breakdownAgentId = null)}
  agentName={breakdownAgent?.name ?? ""}
  report={breakdownAgentId === null ? undefined : runtimes[breakdownAgentId]?.last_context_report}
  at={breakdownAgentId === null ? null : runtimes[breakdownAgentId]?.last_context_report_at}
  request={breakdownAgentId === null
    ? undefined
    : runtimes[breakdownAgentId]?.context_report_request}
/>

<Dialog
  bind:open={resumeOpen}
  onClose={closeResume}
  title="Resume in terminal"
  contentClass="max-w-lg"
>
  <div class="space-y-3" data-testid="resume-panel">
    <p class="text-muted text-xs">
      Run this in your terminal to resume this session interactively.
    </p>
    <div class="flex items-center gap-2">
      <code
        class="bg-panel text-fg min-w-0 flex-1 rounded-md px-2.5 py-2 font-mono text-xs leading-5 break-all whitespace-pre-wrap"
        data-testid="resume-command">{resumeInfo?.resume_command ?? ""}</code
      >
      <CopyButton
        text={resumeInfo?.resume_command ?? ""}
        label="Copy command"
        testid="resume-copy"
        class="shrink-0"
      />
      <Tooltip label={`Run in ${preferences.terminal_app}`} side="top">
        {#snippet trigger(props)}
          <button
            {...props}
            type="button"
            class={cn(ICON_BUTTON_CLASS, "shrink-0")}
            disabled={resumeLaunching || (resumeAgent !== null && isActive(resumeAgent.id))}
            aria-label={`Run in ${preferences.terminal_app}`}
            data-testid="resume-run-terminal"
            onclick={() => void launchResumeInTerminal()}
          >
            <Terminal size={16} strokeWidth={1.5} aria-hidden="true" />
          </button>
        {/snippet}
      </Tooltip>
    </div>
    {#if resumeLaunchError !== null}
      <p class="text-status-failed text-xs" data-testid="resume-launch-error">
        Couldn't open terminal: {resumeLaunchError}
      </p>
    {/if}
    {#if resumeAgent !== null && isActive(resumeAgent.id)}
      <p class="text-status-failed text-xs" data-testid="resume-warning-active">
        ⚠ Switchboard is currently driving this session — stop the agent before running this
        command, or two processes will write one session file and corrupt it.
      </p>
    {:else}
      <p class="text-muted text-xs" data-testid="resume-warning">
        ⚠ While this terminal session is open, don't send to this agent in Switchboard — two
        processes writing one session file can corrupt it.
      </p>
    {/if}
  </div>
</Dialog>

<ErrorDetailsDialog
  bind:open={hydrationDetailsOpen}
  title={`Couldn't load ${hydrationDetailsName}'s history`}
  message="This agent's history failed to load. The exact error is below — copy it into a bug report."
  details={hydrationDetailsError}
/>

<!-- Selection changes apply to future sends; queued sends retain the values
     they captured when submitted. -->
<Dialog
  open={editingAgent !== null}
  onClose={closeChange}
  title="Model settings"
  contentClass="max-w-[612px]"
  dismissible={!editBusy}
>
  <div class="space-y-3" data-testid="change-selection-panel">
    {#if editingAgent !== null}
      <AgentSelectionEditor
        harness={editingAgent.harness}
        selection={editSelection}
        context="current"
        onChange={(selection) => (editSelection = selection)}
        disabled={editBusy}
        testidPrefix="change-selection"
      />
    {/if}
    <p class="text-muted text-xs leading-relaxed">
      The active configuration applies to new messages. Already queued messages keep their original
      configuration.
    </p>
    {#if editError}
      <p class="text-status-failed text-xs" data-testid="change-error">{editError}</p>
    {/if}
    <div class="flex justify-end gap-2">
      <Button
        variant="secondary"
        size="sm"
        class="w-24"
        data-testid="change-cancel"
        disabled={editBusy}
        onclick={closeChange}
      >
        Cancel
      </Button>
      <Button
        size="sm"
        class="w-24"
        data-testid="change-save"
        disabled={editBusy ||
          (editingAgent !== null && !selectionIsValid(editSelection, editingAgent.harness))}
        onclick={() => void submitChange()}
      >
        {editBusy ? "Saving…" : "Save"}
      </Button>
    </div>
  </div>
</Dialog>
