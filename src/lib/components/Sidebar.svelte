<script lang="ts">
  import {
    ArrowDown,
    ArrowUp,
    Check,
    ChevronsUpDown,
    Columns2,
    Eye,
    EyeOff,
    FileText,
    Gauge,
    GripVertical,
    MoreHorizontal,
    Pencil,
    Plug,
    SlidersHorizontal,
    Square,
    Terminal,
    Trash2,
    X,
    Zap,
  } from "@lucide/svelte";
  import { flip } from "svelte/animate";
  import type { AgentRecord, AgentId, ProjectId } from "$lib/types";
  import { retryAgentHydration, runtimes, stopAgent, transcripts } from "$lib/state/index.svelte";
  import {
    removeAgent,
    renameAgent,
    reorderAgents,
    setAgentModel,
    setAgentEffort,
  } from "$lib/state/workspace.svelte";
  import { DRAG_SLOP_PX, dropIndexForPointer, movedOrder } from "$lib/agentReorder";
  import { shortcut } from "$lib/platform";
  import { SUPPORTS_EFFORT_SELECTION, SUPPORTS_MODEL_SELECTION } from "$lib/harnessDisplay";
  import {
    DEFAULT_EFFORT,
    DEFAULT_MODEL,
    MODEL_OPTIONS,
    MODEL_PRESENTATION,
    effortOptionsFor,
    type SelectionOption,
  } from "$lib/agentSelection";
  import {
    AGENTS_SIDEBAR_DEFAULT_WIDTH,
    layout,
    SIDEBAR_MIN_WIDTH,
    sidebarMaxWidth,
  } from "$lib/layout.svelte";
  import DropdownMenu from "$lib/components/ui/DropdownMenu.svelte";
  import DropdownMenuItem from "$lib/components/ui/DropdownMenuItem.svelte";
  import SelectionPicker from "$lib/components/ui/SelectionPicker.svelte";
  import Button from "$lib/components/ui/Button.svelte";
  import {
    agentSessionInfo,
    openSessionFile as apiOpenSessionFile,
    type AgentSessionInfo,
  } from "$lib/api";
  import { normalizeAgentName, validateAgentName, type NameValidation } from "$lib/agentName";
  import { cn, relativeTime } from "$lib/utils";
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
  import { selectAgent } from "$lib/state/recipientSelection.svelte";
  import HarnessIcon from "$lib/components/ui/HarnessIcon.svelte";
  import PlusIcon from "$lib/components/ui/PlusIcon.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import Dialog from "$lib/components/ui/Dialog.svelte";
  import ErrorDetailsDialog from "$lib/components/ui/ErrorDetailsDialog.svelte";
  import CopyButton from "$lib/components/ui/CopyButton.svelte";
  import { ICON_BUTTON_CLASS, ICON_BUTTON_ON_RAISED_CLASS } from "$lib/components/ui/iconButton";

  /// Cap the per-tooltip warning rows so a session with a long tail
  /// (50+) doesn't render a wall of text. Anything beyond is summarized
  /// as "+ N more"; the underlying `parse_warnings` array is untouched.
  const WARNING_ROW_CAP = 10;

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
  /// Verbatim hydration-error dialog (per-agent "history failed to load"). The
  /// failure lives on `runtime.hydration_error`; this just tracks which agent's
  /// error is currently shown.
  let hydrationDetailsOpen = $state(false);
  let hydrationDetailsName = $state("");
  let hydrationDetailsError = $state("");
  let removeConfirmAgentId = $state<AgentId | null>(null);
  let removingAgentId = $state<AgentId | null>(null);
  let removeError = $state<{ agentId: AgentId; message: string } | null>(null);

  /// Per-agent model/effort change editor. `editing` names which agent + axis is
  /// open; `editValue` is the picker's current (always concrete) value. Mirrors
  /// the resume/rename editors — errors surface inline and keep the dialog open.
  let editing = $state<{ agentId: AgentId; axis: "model" | "effort" } | null>(null);
  let editValue = $state<string>("");
  let editBusy = $state<boolean>(false);
  let editError = $state<string | null>(null);

  const editingAgent = $derived(
    editing === null ? null : (agents.find((a) => a.id === editing!.agentId) ?? null),
  );
  function withCurrentOption(
    options: SelectionOption[],
    current: string | null | undefined,
  ): SelectionOption[] {
    if (current == null || current === "" || options.some((o) => o.value === current))
      return options;
    return [{ label: current, value: current }, ...options];
  }

  /// The curated list for the axis being edited (concrete values only — picking
  /// a model/effort is always a concrete choice). If the agent already carries a
  /// value that is no longer in the curated list, keep it selectable so the
  /// dialog honestly reflects persisted state.
  const editOptions = $derived<SelectionOption[]>(
    editing === null || editingAgent === null
      ? []
      : withCurrentOption(
          editing.axis === "model"
            ? MODEL_OPTIONS[editingAgent.harness]
            : effortOptionsFor(editingAgent.harness, editingAgent.model ?? undefined),
          editing.axis === "model" ? editingAgent.model : editingAgent.effort,
        ),
  );

  /// True when the model dialog must show a persisted model that isn't in the
  /// curated list (an attached session, or a since-removed id). Its label is
  /// vendor-shaped and unbounded, so a segmented pill would truncate it — drop
  /// to a dropdown for that case (only the model axis; effort values are always
  /// short enum tokens).
  const editModelOffCatalog = $derived(
    editing?.axis === "model" &&
      editingAgent !== null &&
      editingAgent.model != null &&
      editingAgent.model !== "" &&
      !MODEL_OPTIONS[editingAgent.harness].some((o) => o.value === editingAgent.model),
  );

  /// Presentation for the change dialog: effort is always segmented; the model
  /// axis follows the shared `MODEL_PRESENTATION` map (dropdown for Gemini),
  /// falling back to a dropdown when an off-catalog value is injected.
  const editPresentation = $derived<"segmented" | "dropdown">(
    editing?.axis === "model" &&
      editingAgent !== null &&
      (MODEL_PRESENTATION[editingAgent.harness] === "dropdown" || editModelOffCatalog)
      ? "dropdown"
      : "segmented",
  );

  function canChangeModel(agent: AgentRecord): boolean {
    return SUPPORTS_MODEL_SELECTION[agent.harness];
  }
  function canChangeEffort(agent: AgentRecord): boolean {
    return SUPPORTS_EFFORT_SELECTION[agent.harness];
  }

  function openChange(agent: AgentRecord, axis: "model" | "effort"): void {
    editing = { agentId: agent.id, axis };
    // Seed with the agent's current value, or the harness default when it pins
    // nothing yet (e.g. an attached agent) — the menu only opens for harnesses
    // that have a default on this axis, so the fallback is always concrete.
    const current = axis === "model" ? agent.model : agent.effort;
    const fallback =
      axis === "model" ? DEFAULT_MODEL[agent.harness] : DEFAULT_EFFORT[agent.harness];
    editValue = current ?? fallback ?? "";
    editError = null;
    editBusy = false;
  }

  function closeChange(): void {
    editing = null;
    editError = null;
    editBusy = false;
  }

  async function submitChange(): Promise<void> {
    if (editing === null) return;
    const { agentId, axis } = editing;
    const value = editValue;
    const stillEditingSubmittedTarget = (): boolean =>
      editing?.agentId === agentId && editing.axis === axis;
    editBusy = true;
    editError = null;
    try {
      if (axis === "model") {
        await setAgentModel(agentId, value);
      } else {
        await setAgentEffort(agentId, value);
      }
      if (stillEditingSubmittedTarget()) closeChange();
    } catch (err) {
      if (stillEditingSubmittedTarget()) {
        editError = err instanceof Error ? err.message : String(err);
        editBusy = false;
      }
    }
  }

  const resumeAgent = $derived(
    resumeAgentId === null ? null : (agents.find((agent) => agent.id === resumeAgentId) ?? null),
  );
  const resumeInfo = $derived(
    resumeAgentId === null ? null : (sessionInfoByAgent[resumeAgentId] ?? null),
  );

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
    if (resumeAgentId !== null && !ids.has(resumeAgentId)) {
      resumeAgentId = null;
      resumeOpen = false;
    }
    if (editing !== null && !ids.has(editing.agentId)) closeChange();
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
    // Alt+Arrow reorders the focused card (the menu items advertise the
    // chord). Skipped while typing — Alt+Arrow is a text-caret motion inside
    // the rename input.
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

  // --- Roster reordering -------------------------------------------------
  // Roster order is the canonical display order app-wide (these cards, the
  // compose chips and their ⌘1..9 numbering, pane columns), so all reorder
  // gestures funnel into one commit path. Two gestures: Move up/down (menu
  // items + Alt+Arrow), and dragging the far-right hover grip.

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

  /// Context utilization — `(context_input_tokens + output_tokens) /
  /// context_window` from the most recent completed agent turn. Forward-looking
  /// signal ("how full will the next turn's context be").
  ///
  /// `context_input_tokens` is the harness-reconciled input-side occupancy
  /// (see `TurnUsage`): for Claude it sums the disjoint cache fields (cached +
  /// cache-creation, which `input_tokens` alone excludes — the cause of the
  /// near-0% bug), for Codex the adapter substitutes the session file's
  /// per-turn `last_token_usage` because the stream reports thread-cumulative
  /// totals (the cause of the >900% bug). Consuming the pre-reconciled
  /// value keeps this formula harness-agnostic — do not re-add per-harness
  /// token summation here. Both it and `context_window` must be present;
  /// otherwise the bar is hidden.
  function contextUtilization(agentId: AgentId): number | undefined {
    const turns = transcripts[agentId] ?? [];
    for (let i = turns.length - 1; i >= 0; i--) {
      const turn = turns[i];
      if (turn?.role !== "agent" || turn.usage === undefined) continue;
      const window = turn.usage.context_window;
      if (window === undefined || window === null || window === 0) continue;
      const contextInput = turn.usage.context_input_tokens;
      if (contextInput === undefined || contextInput === null) continue;
      const outputTokens = turn.usage.output_tokens ?? 0;
      return (contextInput + outputTokens) / window;
    }
    return undefined;
  }

  /// Label for a Codex rate-limit window, from its `window_minutes` duration
  /// (300 = the ~5-hour primary, 10080 = the weekly secondary). Unknown/absent
  /// durations fall back to "quota" — which preserves the prior single-cell
  /// copy "quota used: N%" for payloads that carry only a bare `used_percent`.
  function codexWindowLabel(windowMinutes: unknown): string {
    if (windowMinutes === 300) return "5-hour";
    if (windowMinutes === 10080) return "weekly";
    return "quota";
  }

  /// Defensive read of Codex's opaque `last_rate_limit` into its independent
  /// windows (`primary` + `secondary`). Each is a usage gauge (`used_percent`)
  /// with a duration (`window_minutes`) and optional reset (`resets_at`, unix
  /// epoch seconds). Same reset-passed rule as the Claude reader: a window
  /// whose reset is in the past is dropped (it has cycled, so the % is from a
  /// stale window); a window with no `resets_at` is kept (can't prove it
  /// stale — older Codex shapes and minimal fixtures omit it). Codex
  /// rate-limit is session-file-backed (class B), so there's no snapshot-age
  /// qualifier. Returns `[]` when nothing is displayable.
  function codexRateLimitView(
    payload: unknown,
    nowMs: number,
  ): Array<{ label: string; usedPercent: number; resetsAtMs: number | null }> {
    if (typeof payload !== "object" || payload === null) return [];
    const windows: Array<{ label: string; usedPercent: number; resetsAtMs: number | null }> = [];
    for (const key of ["primary", "secondary"] as const) {
      const w = (payload as Record<string, unknown>)[key];
      if (typeof w !== "object" || w === null) continue;
      const ww = w as { used_percent?: unknown; resets_at?: unknown; window_minutes?: unknown };
      if (typeof ww.used_percent !== "number") continue;
      let resetsAtMs: number | null = null;
      if (typeof ww.resets_at === "number") {
        const ms = ww.resets_at * 1000;
        if (ms <= nowMs) continue; // reset-passed → window cycled, % is stale
        resetsAtMs = ms;
      }
      windows.push({
        label: codexWindowLabel(ww.window_minutes),
        usedPercent: ww.used_percent,
        resetsAtMs,
      });
    }
    return windows;
  }

  /// Human label for Claude's primary rate-limit window, derived from the
  /// payload's `rateLimitType` rather than hardcoded: the event tells us the
  /// window kind (observed: `"five_hour"`; other plans/tiers may differ), so
  /// we don't assert a duration the event could contradict. Unknown/absent
  /// types fall back to a generic "rate limit".
  function rateLimitLabel(rateLimitType: unknown): string {
    return rateLimitType === "five_hour" ? "5-hour limit" : "rate limit";
  }

  /// Defensive read of Claude's opaque `last_rate_limit` payload into the two
  /// **independent** signals the Sidebar shows. Each is gated on its own reset
  /// being in the *future*: a reset is an absolute timestamp, so it stays
  /// accurate however old the snapshot is — right until `nowMs` passes it, at
  /// which point the window has cycled and we no longer have its new reset, so
  /// that signal is dropped (showing a past "resets at" would be plainly
  /// wrong). This replaces an age-based staleness heuristic with a certainty.
  ///
  /// - `window`: the primary rate-limit window (`resetsAt` + `rateLimitType`
  ///   label) — emitted on every turn, **independent of overage**.
  /// - `overage`: the "using credits" escalation (`isUsingOverage`), with its
  ///   own credit/overage window (`overageResetsAt`, which can be days out so
  ///   it lives in the tooltip, not the inline clock). A null overage reset
  ///   ("flag set, no window time") is still shown — we can't prove it stale.
  ///
  /// Returns `null` when nothing is currently displayable.
  function rateLimitView(
    payload: unknown,
    nowMs: number,
  ): {
    window: { label: string; resetsAtMs: number } | null;
    overage: { resetsAtMs: number | null } | null;
  } | null {
    if (typeof payload !== "object" || payload === null) return null;
    const p = payload as {
      rateLimitType?: unknown;
      resetsAt?: unknown;
      isUsingOverage?: unknown;
      overageResetsAt?: unknown;
    };

    let window: { label: string; resetsAtMs: number } | null = null;
    if (typeof p.resetsAt === "number") {
      const resetsAtMs = p.resetsAt * 1000;
      if (resetsAtMs > nowMs) {
        window = { label: rateLimitLabel(p.rateLimitType), resetsAtMs };
      }
    }

    let overage: { resetsAtMs: number | null } | null = null;
    if (p.isUsingOverage === true) {
      if (typeof p.overageResetsAt === "number") {
        const overageMs = p.overageResetsAt * 1000;
        overage = overageMs > nowMs ? { resetsAtMs: overageMs } : null;
      } else {
        overage = { resetsAtMs: null };
      }
    }

    if (window === null && overage === null) return null;
    return { window, overage };
  }

  /// Compact clock time for an inline window line (the 5-hour window resets
  /// within hours, so same-day clock reads cleanly). Milliseconds since epoch.
  /// Display-only — never parsed back or scheduled against (no auto-retry).
  function formatResetTime(ms: number): string {
    return new Date(ms).toLocaleTimeString(undefined, {
      hour: "numeric",
      minute: "2-digit",
    });
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

  /// Per-agent collapsed state. Default expanded; ephemeral (resets on reload).
  let collapsed = $state<Record<string, boolean>>({});

  function toggleCollapsed(agentId: AgentId): void {
    collapsed[agentId] = !collapsed[agentId];
  }

  const allExpanded = $derived(agents.every((a) => !(collapsed[a.id] ?? false)));

  function toggleAll(): void {
    if (allExpanded) {
      for (const a of agents) collapsed[a.id] = true;
    } else {
      for (const a of agents) delete collapsed[a.id];
    }
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
          <button
            type="button"
            class="text-muted hover:text-fg shrink-0 px-1 text-[11px] hover:underline"
            title="Show all agents"
            aria-label={`${hiddenAgentCount} hidden — show all agents`}
            data-testid="sidebar-show-all-agents"
            onclick={() => showAllAgents(projectId, rosterIds)}
          >
            {hiddenAgentCount} hidden
          </button>
        {/if}
        {#if agents.length > 0}
          <button
            type="button"
            class={ICON_BUTTON_CLASS}
            aria-label={allExpanded ? "Collapse all agents" : "Expand all agents"}
            title={allExpanded ? "Collapse all" : "Expand all"}
            data-testid="sidebar-toggle-all"
            onclick={toggleAll}
          >
            <svg
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
              class="h-4 w-4"
              aria-hidden="true"
            >
              {#if allExpanded}
                <path d="m17 11-5-5-5 5" />
                <path d="m17 18-5-5-5 5" />
              {:else}
                <path d="m7 6 5 5 5-5" />
                <path d="m7 13 5 5 5-5" />
              {/if}
            </svg>
          </button>
        {/if}
        {#if onAddAgent}
          <button
            type="button"
            class={ICON_BUTTON_CLASS}
            title="Add agent"
            aria-label="Add agent"
            data-testid="sidebar-add-agent"
            onclick={onAddAgent}
          >
            <PlusIcon />
          </button>
        {/if}
      </div>
    {/snippet}

    {#if agents.length === 0}
      <p class="text-muted px-3 py-3 text-xs">No agents in this project yet.</p>
    {/if}
    <div class="flex flex-col gap-1.5 px-2 pt-1 pb-2" bind:this={agentListEl}>
      {#each displayAgents as agent (agent.id)}
        {@const runtime = runtimes[agent.id]}
        {@const util = contextUtilization(agent.id)}
        {@const codexWindows =
          agent.harness === "codex" ? codexRateLimitView(runtime?.last_rate_limit, Date.now()) : []}
        <!-- `Date.now()` read once per render for the reset-in-the-future gate.
             Non-reactive: a reset that elapses while the app sits open won't
             auto-hide until the next render, which a new turn (or reopen)
             triggers — acceptable for a passive status cell. -->
        {@const rlView =
          agent.harness === "claude_code"
            ? rateLimitView(runtime?.last_rate_limit, Date.now())
            : null}
        {@const overageAsOf = runtime?.last_rate_limit_as_of}
        {@const isCollapsed = collapsed[agent.id] ?? false}
        {@const active = isActive(agent.id)}
        {@const sessionInfo = sessionInfoByAgent[agent.id]}
        {@const confirmingRemove = removeConfirmAgentId === agent.id}
        <!-- A native button cannot contain the card's controls. The focusable
             composite surface mirrors its pointer toggle on Enter/Space, while
             each nested control keeps its own semantics. -->
        <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
        <div
          class={cn(
            "group bg-raised hover:ring-active focus-visible:ring-focus cursor-pointer rounded-md px-2.5 py-2 transition-shadow hover:shadow-sm hover:ring-1 focus-visible:ring-1 focus-visible:outline-none",
            dragState?.started === true &&
              dragState.agentId === agent.id &&
              "ring-accent/60 relative z-10 shadow-lg ring-1",
          )}
          data-testid="sidebar-agent"
          data-agent-id={agent.id}
          data-collapsed={isCollapsed}
          tabindex="0"
          aria-label={`${agent.name}, ${isCollapsed ? "collapsed" : "expanded"}. Press Enter or Space to toggle details.`}
          use:agentRowPointerActions={agent.id}
          animate:flip={{ duration: dragState?.started === true ? 0 : 150 }}
        >
          <div class="flex items-center justify-between gap-2">
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
                title={renameMessage ?? undefined}
                data-testid="agent-rename-input"
                onkeydown={(event) => onRenameKeydown(event, agent)}
                onblur={cancelEdit}
              />
              <button
                type="button"
                class={cn(
                  ICON_BUTTON_ON_RAISED_CLASS,
                  "shrink-0 disabled:cursor-not-allowed disabled:opacity-50",
                )}
                disabled={!canSave}
                aria-label="Save name"
                title="Save"
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
            {:else}
              {@const agentHidden = isAgentHidden(projectId, rosterIds, agent.id)}
              <div class="flex min-h-7 min-w-0 flex-1 items-center px-1.5 text-left">
                <span
                  class="text-fg cursor-text truncate text-[13px] font-semibold"
                  data-testid="agent-name"
                  title="Double-click to rename"
                >
                  {agent.name}
                </span>
              </div>
              <div class="flex shrink-0 items-center gap-0.5">
                <Tooltip
                  label={agentHidden ? `Show ${agent.name}` : `Hide ${agent.name} (⌥-click: solo)`}
                  delayDuration={800}
                >
                  {#snippet trigger(props)}
                    <button
                      {...props}
                      type="button"
                      class={cn(
                        ICON_BUTTON_ON_RAISED_CLASS,
                        "shrink-0",
                        // The eye stays visible while the agent is hidden (it's
                        // the state indicator); otherwise it appears on hover
                        // like the actions trigger. `hidden`, not `opacity-0`:
                        // an invisible button still reserves its width, and
                        // that reserved gutter is what was truncating names —
                        // the name takes the full row until the icons reveal.
                        agentHidden
                          ? "text-muted"
                          : "hidden group-focus-within:inline-flex group-hover:inline-flex",
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
                    ICON_BUTTON_ON_RAISED_CLASS,
                    "shrink-0",
                    "hidden group-focus-within:inline-flex group-hover:inline-flex data-[state=open]:inline-flex",
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
                      <ChevronsUpDown
                        size={14}
                        strokeWidth={1.8}
                        class="text-muted shrink-0"
                        aria-hidden="true"
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
                    {#if sessionInfo?.resume_command}
                      <DropdownMenuItem
                        onSelect={() => {
                          resumeAgentId = agent.id;
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
                    {#if canChangeModel(agent)}
                      <DropdownMenuItem
                        onSelect={() => openChange(agent, "model")}
                        class="gap-2"
                        data-testid="agent-change-model"
                      >
                        <SlidersHorizontal
                          size={14}
                          strokeWidth={1.8}
                          class="text-muted shrink-0"
                          aria-hidden="true"
                        />
                        Change model
                      </DropdownMenuItem>
                    {/if}
                    {#if canChangeEffort(agent)}
                      <DropdownMenuItem
                        onSelect={() => openChange(agent, "effort")}
                        class="gap-2"
                        data-testid="agent-change-effort"
                      >
                        <Gauge
                          size={14}
                          strokeWidth={1.8}
                          class="text-muted shrink-0"
                          aria-hidden="true"
                        />
                        Change effort
                      </DropdownMenuItem>
                    {/if}
                    <!-- Roster reorder. Disabled (not hidden) at the ends so the
                         boundary reads as "can't move further", not a missing
                         feature. closeOnSelect={false}: moving several positions
                         is one menu trip. -->
                    {#if agents.length > 1}
                      {@const rosterIndex = rosterIds.indexOf(agent.id)}
                      <DropdownMenuItem
                        onSelect={() => void moveAgentBy(agent.id, -1)}
                        closeOnSelect={false}
                        disabled={rosterIndex === 0}
                        class="gap-2"
                        data-testid="agent-move-up"
                      >
                        <ArrowUp
                          size={14}
                          strokeWidth={1.8}
                          class="text-muted shrink-0"
                          aria-hidden="true"
                        />
                        Move up
                        <span class="text-muted ml-auto font-mono text-xs"
                          >{shortcut("alt", "↑")}</span
                        >
                      </DropdownMenuItem>
                      <DropdownMenuItem
                        onSelect={() => void moveAgentBy(agent.id, 1)}
                        closeOnSelect={false}
                        disabled={rosterIndex === agents.length - 1}
                        class="gap-2"
                        data-testid="agent-move-down"
                      >
                        <ArrowDown
                          size={14}
                          strokeWidth={1.8}
                          class="text-muted shrink-0"
                          aria-hidden="true"
                        />
                        Move down
                        <span class="text-muted ml-auto font-mono text-xs"
                          >{shortcut("alt", "↓")}</span
                        >
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
                        title="Deletes Switchboard's files for this agent; underlying session files are kept, and its responses are removed from the conversation."
                      >
                        <Trash2 size={14} strokeWidth={1.8} class="shrink-0" aria-hidden="true" />
                        Delete agent
                      </DropdownMenuItem>
                    {/if}
                  {/if}
                </DropdownMenu>
                <HarnessIcon harness={agent.harness} size="md" testid="agent-harness-icon" />
                {#if agents.length > 1}
                  <span
                    class={cn(
                      "text-muted h-3 w-3 shrink-0 cursor-grab touch-none items-center justify-center active:cursor-grabbing",
                      dragState?.agentId === agent.id
                        ? "inline-flex"
                        : "hidden group-focus-within:inline-flex group-hover:inline-flex",
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
          {#if !isCollapsed}
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
            {#if runtime?.parse_warnings && runtime.parse_warnings.length > 0}
              {@const warnings = runtime.parse_warnings}
              {@const visible = warnings.slice(0, WARNING_ROW_CAP)}
              {@const extra = warnings.length - visible.length}
              <Tooltip side="right">
                {#snippet trigger(props)}
                  <!-- tabindex=0: the indicator is purely hover/focus-driven
                       (no click). bits-ui Trigger spreads handler props but
                       doesn't make a <div> focusable on its own, so without
                       this keyboard-only users couldn't reach the warning
                       detail. Not promoted to <button> because a button
                       implies a click action that doesn't exist. -->
                  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
                  <div
                    {...props}
                    tabindex="0"
                    class="text-warning mt-1 cursor-default text-xs"
                    data-testid="agent-parse-warnings"
                  >
                    ⚠ {warnings.length} transcript warning{warnings.length === 1 ? "" : "s"}
                  </div>
                {/snippet}
                <ul class="max-w-md space-y-1 text-[13px]" data-testid="agent-parse-warnings-list">
                  {#each visible as warning (warning.line_number + ":" + warning.reason)}
                    <li class="flex gap-2" data-testid="agent-parse-warnings-row">
                      <span class="text-primary-fg/70 shrink-0 font-mono"
                        >line {warning.line_number}:</span
                      >
                      <span>{warning.reason}</span>
                    </li>
                  {/each}
                  {#if extra > 0}
                    <li
                      class="text-primary-fg/70 pt-0.5 text-xs"
                      data-testid="agent-parse-warnings-overflow"
                    >
                      + {extra} more
                    </li>
                  {/if}
                </ul>
              </Tooltip>
            {/if}
            <!-- Selected model/effort (intent), shown first. When the user
                 hasn't chosen a model (Antigravity, or an attached agent, which
                 pins nothing) we fall back to the harness-observed model from
                 `runtime.meta` so the line isn't blank when a model is known.
                 Effort is selection-only — no observed source. The per-turn
                 transcript footer carries the actual runtime history (which may
                 show a resolved id even when intent is an alias). -->
            {#if agent.model || runtime?.meta?.model || agent.effort}
              <!-- One secondary line, `opus · high` — configuration is context,
                   not a table of key: value pairs. An observed (session-derived)
                   model keeps its explanatory tooltip instead of a label. -->
              <div
                class="text-muted mt-1.5 truncate text-xs leading-4"
                data-testid="agent-selection"
              >
                {#if agent.model}
                  <span title={agent.model} data-testid="agent-selected-model">{agent.model}</span>
                {:else if runtime?.meta?.model}
                  <span
                    title={`${runtime.meta.model} — observed from the session (no model selected)`}
                    data-testid="agent-observed-model">{runtime.meta.model}</span
                  >
                {/if}
                {#if (agent.model || runtime?.meta?.model) && agent.effort}
                  <span aria-hidden="true"> · </span>
                {/if}
                {#if agent.effort}
                  <span data-testid="agent-selected-effort">{agent.effort}</span>
                {/if}
              </div>
            {/if}
            {#if runtime?.meta && (runtime.meta.mcp_servers.length > 0 || runtime.meta.skills.length > 0)}
              <div class="mt-1.5 flex items-center gap-1" data-testid="agent-meta">
                {#if runtime.meta.mcp_servers.length > 0}
                  {@const mcpCount = runtime.meta.mcp_servers.length}
                  <span
                    class="bg-panel text-muted inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px]"
                    title={`${mcpCount} MCP server${mcpCount === 1 ? "" : "s"}`}
                    data-testid="agent-mcp-chip"
                  >
                    <Plug size={11} strokeWidth={1.8} aria-hidden="true" />{mcpCount}
                  </span>
                {/if}
                {#if runtime.meta.skills.length > 0}
                  {@const skillCount = runtime.meta.skills.length}
                  <span
                    class="bg-panel text-muted inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px]"
                    title={`${skillCount} skill${skillCount === 1 ? "" : "s"}`}
                    data-testid="agent-skills-chip"
                  >
                    <Zap size={11} strokeWidth={1.8} aria-hidden="true" />{skillCount}
                  </span>
                {/if}
              </div>
            {/if}
            <!-- Per-turn cost is deliberately NOT shown on the card — it
                 renders inline per-message in the transcript (real-spend turns
                 only). There is no per-agent cost total (system-design §2): the
                 old accumulating `$` figure read as a running total but wasn't
                 one. Do not re-add it. The current overage *status* below stays
                 (Bucket-A "as of now" state). -->
            {#if rlView !== null}
              <!-- Claude rate-limit surface — two independent signals, each
                   shown only while its own reset is still in the future (a past
                   "resets at" would be wrong, so it clean-hides instead). The
                   primary window (neutral) is emitted on every turn, regardless
                   of overage; the overage escalation (amber `warning` token) is
                   layered on top only when billing to credits. One always-present
                   tooltip carries full dates (a window can be days out) and the
                   snapshot age when rehydrated. Survives restart via the
                   metadata sidecar. -->
              <Tooltip side="right">
                {#snippet trigger(props)}
                  <!-- tabindex=0 so keyboard users can open the tooltip; a <div>
                       (no click action) isn't focusable on its own. Mirrors the
                       parse-warnings indicator. -->
                  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
                  <div
                    {...props}
                    tabindex="0"
                    class="mt-1.5 cursor-default space-y-0.5 text-xs"
                    data-testid="agent-rate-limit-claude"
                  >
                    {#if rlView.window !== null}
                      <div class="text-fg" data-testid="agent-rate-window">
                        {rlView.window.label} resets {formatResetTime(rlView.window.resetsAtMs)}
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
                <div class="max-w-xs space-y-1 text-[13px]" data-testid="agent-rate-detail">
                  {#if rlView.window !== null}
                    <p>
                      {rlView.window.label} resets {formatResetDateTime(rlView.window.resetsAtMs)}
                    </p>
                  {/if}
                  {#if rlView.overage !== null}
                    <p>
                      Spending usage credits{rlView.overage.resetsAtMs !== null
                        ? ` — overage window resets ${formatResetDateTime(rlView.overage.resetsAtMs)}`
                        : "."}
                    </p>
                  {/if}
                  {#if overageAsOf != null}
                    <p class="text-primary-fg/70" data-testid="agent-rate-snapshot">
                      Snapshot from {relativeTime(overageAsOf)} — send a message to refresh.
                    </p>
                  {/if}
                </div>
              </Tooltip>
            {/if}
            {#if codexWindows.length > 0}
              <!-- Codex rate-limit: one neutral gauge line per independent
                   window (primary ~5-hour + secondary weekly), with full reset
                   dates in the tooltip (the weekly window is days out, beyond a
                   bare clock). Session-file-backed (class B, durable) — no
                   snapshot-age qualifier, unlike Claude's stream-only payload. -->
              <Tooltip side="right">
                {#snippet trigger(props)}
                  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
                  <div
                    {...props}
                    tabindex="0"
                    class="text-fg mt-1.5 cursor-default space-y-0.5 text-xs"
                    data-testid="agent-rate-limit"
                  >
                    {#each codexWindows as w, i (i)}
                      <div>{w.label} used: {w.usedPercent.toFixed(0)}%</div>
                    {/each}
                  </div>
                {/snippet}
                <div class="max-w-xs space-y-1 text-[13px]" data-testid="agent-rate-limit-detail">
                  {#each codexWindows as w, i (i)}
                    <p>
                      {w.label}: {w.usedPercent.toFixed(0)}% used{w.resetsAtMs !== null
                        ? ` · resets ${formatResetDateTime(w.resetsAtMs)}`
                        : ""}
                    </p>
                  {/each}
                </div>
              </Tooltip>
            {/if}
            <!-- Clean-hide convention: every metadata cell above and below is
                 presence-gated, so a value a harness never reports simply never
                 renders — no blank label, no empty bar, no "—" placeholder. These
                 absences are correct, not gaps: Gemini exposes no `context_window`
                 (the bar below never renders for it), and Antigravity reports no
                 cost / quota / context at all. A transient absence (a fresh agent
                 pre-first-turn) hides identically to a permanent one; that's the
                 intended behavior, not a case to distinguish. -->
            {#if util !== undefined}
              <div class="mt-1.5" data-testid="agent-context-bar">
                <div class="text-muted mb-0.5 text-[11px]">
                  context after last turn: {(util * 100).toFixed(0)}%
                </div>
                <div class="bg-active h-1 w-full overflow-hidden rounded">
                  <div
                    class="bg-fg h-full"
                    style:width="{Math.min(util * 100, 100).toFixed(1)}%"
                  ></div>
                </div>
              </div>
            {/if}
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

<Dialog
  bind:open={resumeOpen}
  onClose={() => (resumeAgentId = null)}
  title="Resume in terminal"
  contentClass="max-w-lg"
>
  <div class="space-y-3" data-testid="resume-panel">
    <p class="text-muted text-xs">
      Run this in your terminal to resume this session interactively.
    </p>
    <div class="flex items-center gap-2">
      <code
        class="bg-panel text-fg min-w-0 flex-1 overflow-x-auto rounded-md px-2.5 py-2 font-mono text-xs whitespace-pre"
        data-testid="resume-command">{resumeInfo?.resume_command ?? ""}</code
      >
      <CopyButton
        text={resumeInfo?.resume_command ?? ""}
        label="Copy command"
        testid="resume-copy"
        class="shrink-0"
      />
    </div>
    {#if resumeAgent !== null && isActive(resumeAgent.id)}
      <p class="text-status-failed text-xs" data-testid="resume-warning-active">
        ⚠ Switchboard is currently driving this session — stop the agent before running this
        command, or two processes will write one session file and corrupt it.
      </p>
    {:else}
      <p class="text-muted text-xs" data-testid="resume-warning">
        ⚠ Don't run this while the agent is active in Switchboard — two processes writing one
        session file will corrupt it.
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

<!-- Change model / effort. Reuses the shared selection picker (concrete values
     only). Applies on the agent's next send. -->
<Dialog
  open={editing !== null}
  onClose={closeChange}
  title={editing?.axis === "effort" ? "Change effort" : "Change model"}
  contentClass="max-w-lg"
  dismissible={!editBusy}
>
  <div class="space-y-3" data-testid="change-selection-panel">
    <label class="block space-y-1">
      <span class="text-muted text-xs">
        {editing?.axis === "effort" ? "Reasoning effort" : "Model"}
      </span>
      <SelectionPicker
        bind:value={editValue}
        options={editOptions}
        disabled={editBusy}
        testid="change-select"
        ariaLabel={editing?.axis === "effort" ? "Reasoning effort" : "Model"}
        presentation={editPresentation}
      />
    </label>
    <p class="text-muted text-xs leading-relaxed">Takes effect on the next message.</p>
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
        disabled={editBusy}
        onclick={() => void submitChange()}
      >
        {editBusy ? "Saving…" : "Save"}
      </Button>
    </div>
  </div>
</Dialog>
