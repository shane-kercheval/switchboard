<script lang="ts">
  import {
    agentIsWorking,
    cancelSend,
    dispatchUserTurn,
    noteLocalSend,
    failSendStart,
    recordSendAccepted,
    runtimes,
    transcripts,
  } from "$lib/state/index.svelte";
  import { createReachableFork, type ReachableFork } from "$lib/state/workspace.svelte";
  import { HARNESS_LABEL } from "$lib/harnessDisplay";
  import {
    addHeldForward,
    removeHeldForward,
    forwardSourceKey,
    expandForwardSources,
    forwardSourceIds,
    refreshForeignSourceLabels,
    sourceReadinessFor,
    forwardSourceForAgent,
    forwardSourceAgentsForPane,
    agentReadinessFor,
    reconcileForwardSources,
    reconcileForwardSourceMap,
    type ForwardReadiness,
    type ForwardSource,
    type ForwardSourceRef,
  } from "$lib/state/heldForwards.svelte";
  import { buildLiveSendsMap } from "$lib/state/liveSends";
  import {
    clearCompose,
    composeContentMatches,
    emptyForwards,
    flush,
    getCompose,
    setAttachments,
    setContent,
    setForwards,
    setSelection,
    type ComposeContent,
    type ComposeForwards,
    type ComposeSnapshot,
    type PromptContent,
    type WorkflowContent,
  } from "$lib/state/composeStore";
  import {
    abandonAwaitingUserOperation,
    beginOperation,
    clearOutcome,
    composerConsumedCount,
    finishOperation,
    markComposerConsumed,
    operationFor,
    outcomeFor,
    ownsOperation,
    setOperationPhase,
    takeOutcome,
  } from "$lib/state/composeOperations.svelte";
  import { projects, recordProjectsActivityLocally } from "$lib/state/workspace.svelte";
  import {
    selectionFor,
    setRecipients,
    targetRecipients,
  } from "$lib/state/recipientSelection.svelte";
  import {
    isAgentHidden,
    layoutFor,
    revealPane,
    type TranscriptPane,
  } from "$lib/state/transcriptPanes.svelte";
  import * as api from "$lib/api";
  import type {
    AgentId,
    AgentRecord,
    Attachment,
    ProjectId,
    Prompt,
    WorkflowFormDescriptor,
    WorkflowInputValue,
    WorkflowListing,
  } from "$lib/types";
  import { classifyKind, nextLabel } from "$lib/attachments";
  import { registerSend } from "$lib/state/sendCompletion";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import {
    buildRenderArgs,
    combinePromptMessage,
    missingRequiredArgs,
    promptDisplayName,
  } from "$lib/prompt";
  import Textarea from "$lib/components/ui/Textarea.svelte";
  import StopIcon from "$lib/components/ui/StopIcon.svelte";
  import HarnessIcon from "$lib/components/ui/HarnessIcon.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { SUPPLEMENTAL_TOOLTIP_DELAY } from "$lib/components/ui/tooltip";
  import ClearIcon from "$lib/components/ui/ClearIcon.svelte";
  import { COMPOSER_ACTION_BUTTON_CLASS, ICON_BUTTON_CLASS } from "$lib/components/ui/iconButton";
  import PromptMenu from "$lib/components/PromptMenu.svelte";
  import PromptComposer from "$lib/components/PromptComposer.svelte";
  import WorkflowMenu from "$lib/components/WorkflowMenu.svelte";
  import WorkflowComposer from "$lib/components/WorkflowComposer.svelte";
  import WorkflowSteps from "$lib/components/WorkflowSteps.svelte";
  import { workflowRuns, cancelRun, abandonRun, refreshRuns } from "$lib/state/workflows.svelte";
  import Button from "$lib/components/ui/Button.svelte";
  import ForwardSourceChip from "$lib/components/ui/ForwardSourceChip.svelte";
  import ForwardSourcePicker from "$lib/components/ui/ForwardSourcePicker.svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import { basename, cn, currentIsoTimestamp } from "$lib/utils";
  import { copyText } from "$lib/native";
  import { workflowAuthoringPrompt } from "$lib/workflowAuthoring";
  import { shortcut } from "$lib/platform";
  import { isEditableShortcutTarget } from "$lib/keyboard";
  import { onDestroy, onMount, tick, untrack } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";

  let {
    projectId,
    agents,
    focusOnMount = false,
    focusRequest = 0,
    onConfigurePrompts,
  }: {
    projectId: ProjectId;
    agents: AgentRecord[];
    focusOnMount?: boolean;
    /// A monotonic counter the parent bumps to pull focus into the composer
    /// (a pane Cmd+click). Not project state — a transient one-shot signal
    /// owned by `App` and delivered as a prop; see the watching effect.
    focusRequest?: number;
    /// Open Settings at the prompt-source section.
    onConfigurePrompts?: () => void;
  } = $props();

  // The compose bar is remounted per project (App.svelte's `{#key}`), and the
  // parent only mounts it once the roster is loaded and non-empty — so the
  // saved snapshot can be applied synchronously here, against a populated
  // roster, with no first-render-empty window to guard against. `projectId` and
  // the mount-time roster are constant for this component's life; `untrack`
  // states that the initial read is deliberate, not a missed dependency.
  const saved = untrack(() => getCompose(projectId));

  // Plain-mode draft text. (Prompt-mode content lives in the prompt-mode state
  // below.) A saved prompt-mode draft starts as empty plain text until its
  // prompt is resolved against the loaded cache (`tryRestorePrompt`).
  let draft = $state<string>(saved.content.kind === "plain" ? saved.content.draft : "");
  let sendError = $state<string | null>(null);
  let composeEl = $state<HTMLDivElement | undefined>(undefined);
  let textareaEl = $state<HTMLTextAreaElement | undefined>(undefined);

  // ── Attachments ─────────────────────────────────────────────────────────────
  // Dropped files staged on the backend; each chip carries the wire `Attachment`
  // fields plus a local `id` for list keying / removal.
  //
  // **The compose snapshot is authoritative, not this component.** Chips persist
  // across a project switch and a Git-view toggle (both of which unmount this bar).
  // Two consequences worth stating: a staging copy that finishes after this bar
  // unmounts still belongs to the project it began under and lands in that
  // project's snapshot; and the project loader must declare these paths
  // (`draftAttachmentPaths`) or its GC reclaims the files behind chips we're still
  // showing.
  //
  // **Exactly two sanctioned write paths — do not invent a third.**
  //   1. While mounted: mutate `attachmentChips`; the `$effect` below mirrors it to
  //      `setAttachments`. `commitChips` is the convenience wrapper for a replace.
  //   2. After unmount (a staging copy that outlived this instance): `addAttachmentChip`
  //      writes the store directly, because no `$effect` runs on a dead component.
  // A direct `attachmentChips = …` assignment that skips both is the exact bug this
  // section already shipped once — a restore path that updated the UI but not the
  // store, so the file was GC'd on the next load. The effect is the backstop that
  // makes that class of mistake structurally impossible.
  type AttachmentChip = Attachment & { id: string };
  let attachmentChips = $state<AttachmentChip[]>(restoreChips(saved.attachments ?? []));
  let dragOver = $state(false);
  // Whether keyboard focus is anywhere inside the compose box. Drives the card's
  // focus border (a border-color change, intentionally not a ring/glow — kept as
  // thin as possible): the compose bar is the app's default keyboard target, so
  // an always-on highlight says nothing — one that appears on focus and clears
  // when focus moves to the Git view's keyboard nav, a dialog, or elsewhere is
  // the real signal. Tracked at the container (not the plain textarea) so it also
  // lights for prompt- and workflow-mode fields, which render inside this box.
  // A focus-trapping menu opened from here (bits-ui portals its content out of
  // the DOM subtree) counts as focus leaving — consistent with "a dialog took
  // focus"; the ring returns when the menu closes.
  let composeFocused = $state(false);
  // Bumped only on send-clear. A staging result captures the generation it began
  // under and is discarded if it has since moved on, so a slow copy can't
  // resurrect a chip into a composer whose contents were already dispatched.
  // Unmount deliberately does *not* bump: the result is still wanted, just in the
  // snapshot rather than in this (dead) component's chip list.
  let sendGeneration = 0;
  let unmounted = false;

  function restoreChips(attachments: Attachment[]): AttachmentChip[] {
    return attachments.map((attachment) => ({ ...attachment, id: crypto.randomUUID() }));
  }

  function chipsToAttachments(chips: AttachmentChip[]): Attachment[] {
    return chips.map(({ label, kind, path, original_name }) => ({
      label,
      kind,
      path,
      original_name,
    }));
  }

  /// Set the chip list and write it through. The only path that mutates chips
  /// while this bar is mounted.
  function commitChips(next: AttachmentChip[]): void {
    attachmentChips = next;
    setAttachments(projectId, chipsToAttachments(next));
  }

  /// Append a freshly staged file. Reads the snapshot (not `attachmentChips`) as
  /// the base so it stays correct after this bar has unmounted.
  function addAttachmentChip(staged: { path: string; original_name: string }): void {
    const kind = classifyKind(staged.original_name);
    const existing = getCompose(projectId).attachments ?? [];
    const attachment: Attachment = {
      label: nextLabel(kind, existing),
      kind,
      path: staged.path,
      original_name: staged.original_name,
    };
    setAttachments(projectId, [...existing, attachment]);
    if (!unmounted) {
      attachmentChips = [...attachmentChips, { ...attachment, id: crypto.randomUUID() }];
    }
  }

  function removeAttachmentChip(id: string): void {
    commitChips(attachmentChips.filter((chip) => chip.id !== id));
  }

  /// Drop restored chips whose staged file no longer exists (a cleaned
  /// `.switchboard/`, or an older build's GC). Only the *restored* paths are
  /// candidates — a file attached while this check was in flight was never at risk
  /// and must not be pruned by a stale answer.
  async function pruneMissingAttachments(restored: Attachment[]): Promise<void> {
    const candidates = new Set(restored.map((a) => a.path));
    try {
      const alive = new Set(await api.existingAttachmentPaths(projectId, [...candidates]));
      if (unmounted) return;
      const next = attachmentChips.filter(
        (chip) => !candidates.has(chip.path) || alive.has(chip.path),
      );
      if (next.length !== attachmentChips.length) commitChips(next);
    } catch {
      // Leave the chips: a failed probe is not evidence the files are gone, and a
      // genuinely missing file still surfaces as a send error.
    }
  }

  // Forward sources: agents whose latest output is forwarded into this send (the
  // §7 manual cross-agent forward). Picked from the `@`-menu's "Forward from"
  // entries; a pane entry expands to its members at pick time. A send with ≥1
  // forward source dispatches via `forward_message` instead of the normal send path.
  //
  // Persisted (all four families below), so a project switch or Git-view toggle —
  // both of which unmount this bar — doesn't silently discard a forward the user
  // set up. Restored sources are reconciled against the live roster: an agent
  // removed since the draft was written is dropped, a renamed one takes its
  // current name.
  // `untrack`: the mount-time roster is constant for this component's life (see
  // the `saved` snapshot above), so reconciling against it once is deliberate, not
  // a missed dependency.
  const savedForwards = saved.forwards ?? emptyForwards();
  let forwardSources = $state<ForwardSource[]>(
    untrack(() => reconcileForwardSources(savedForwards.message, agents, projectId)),
  );

  /// Other projects offered in the Forward picker's `Projects` section.
  /// Restricted to **available** directories: an unavailable one can't have its
  /// journal read or its agent dispatched against, so offering it would produce a
  /// source that fails at send. (M2's central store lifts this — the journal
  /// moves out of the working directory — but that is not this milestone.)
  const otherForwardProjects = $derived(
    projects.list
      .filter((p) => p.id !== projectId && p.available && !p.archived)
      .map((p) => ({ id: p.id, name: p.name, directory: p.directory })),
  );

  /// The **shared half** of cross-project sourcing: what to browse, how to read a
  /// roster, how to open and validate. Each consumer spreads this and adds its own
  /// `onPickForeign`, which `CrossProjectConfig` requires — where a picked source
  /// lands differs per surface, and one shared commit closure is exactly how
  /// prompt- and workflow-field picks once landed in this bar's plain-message list.
  ///
  /// `activate` **opens the project before any chip is added**. Browsing is a read,
  /// but committing has to prove the project is usable: otherwise one locked by
  /// another window looks selectable and the user finds out only after composing a
  /// whole message. The backend still opens at dispatch — that path serves chips
  /// restored cold from a draft, which never passed through here.
  const crossProjectBase = $derived({
    projects: otherForwardProjects,
    loadAgents: (id: ProjectId) =>
      api.listProjectAgentsReadonly(
        id,
        otherForwardProjects.find((p) => p.id === id)?.directory ?? "",
      ),
    activate: async (id: ProjectId) => {
      await api.openProject(id);
    },
  });

  function addForwardSource(source: ForwardSource): void {
    if (forwardSources.some((s) => forwardSourceKey(s) === forwardSourceKey(source))) return;
    forwardSources = [...forwardSources, source];
  }

  /// Add every (live) member of a pane as its own agent source — a pane is a
  /// selection shortcut, not a stored entity, so it expands to agent chips here.
  function addPaneForwardSources(pane: TranscriptPane): void {
    for (const source of forwardSourceAgentsForPane(pane, agents)) addForwardSource(source);
  }

  function removeForwardSource(key: string): void {
    // Focus the textarea *before* the reactive flush unmounts the chip's X, so
    // focus never falls to <body> and the focus border never blinks off. (The
    // X's own `onmousedown` preventDefault already keeps focus put for mouse
    // clicks; this covers keyboard removal, where the X held focus.)
    textareaEl?.focus();
    forwardSources = forwardSources.filter((s) => forwardSourceKey(s) !== key);
  }

  /// Whether an agent's on-disk history has been read into `transcripts`.
  ///
  /// Load-bearing wherever an *absent* transcript is read as evidence: every
  /// agent is seeded with an empty one at registration, and a failed read leaves
  /// it empty until the user retries hydration — so before this holds, "empty"
  /// and "has months of history" are indistinguishable.
  function isHydrated(agentId: AgentId): boolean {
    return runtimes[agentId]?.hydration_status === "complete";
  }

  /// What an agent would contribute if forwarded from right now. The single source
  /// of truth for every surface that flags a forward source — the chips, the
  /// `@`-menu rows, and the per-field pickers in the prompt/workflow composers —
  /// so they cannot disagree about the same agent.
  function agentReadiness(agentId: AgentId): ForwardReadiness {
    return agentReadinessFor(transcripts[agentId], runtimes[agentId]);
  }

  /// Readiness for a chip. Must go through the *source*, not the bare id:
  /// `transcripts` holds only this project's agents, so a foreign id yields
  /// `undefined`, and `forwardReadiness(undefined)` is `"empty"` — a "this will
  /// block your send" warning that is false for a healthy foreign source.
  function sourceReadiness(source: ForwardSource): ForwardReadiness {
    return sourceReadinessFor(
      source,
      projectId,
      (id) => transcripts[id],
      (id) => runtimes[id],
    );
  }

  /// The current chips as the `Attachment` wire shape (drops the local `id`),
  /// snapshotted once per send so every fan-out recipient gets the same list.
  function snapshotAttachments(): Attachment[] {
    return attachmentChips.map((chip) => ({
      label: chip.label,
      kind: chip.kind,
      path: chip.path,
      original_name: chip.original_name,
    }));
  }

  /// Stage each dropped OS file path on the backend (copy into the project's
  /// attachments dir) and add a chip for it. A per-file failure surfaces in the
  /// send-error line and skips that file rather than aborting the rest.
  async function stageDroppedPaths(paths: string[]): Promise<void> {
    const gen = sendGeneration;
    for (const path of paths) {
      try {
        const staged = await api.stageAttachment(projectId, path);
        // The drop's compose session may have been *sent* while the copy was in
        // flight; if so, discard rather than resurrecting a chip into a cleared
        // composer. An unmount is not a discard — `addAttachmentChip` writes to
        // the originating project's snapshot either way.
        if (gen !== sendGeneration) return;
        addAttachmentChip(staged);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        showError(`Couldn't attach ${basename(path)}: ${message}`);
      }
    }
  }

  // OS file drops do NOT raise HTML5 `drop` events while Tauri's `dragDropEnabled`
  // is on, so the webview drag-drop event is the only signal. It is window-global,
  // but the compose bar is the only file-drop target in the app, so a drop
  // anywhere in the window attaches — no position hit-test (its physical↔CSS
  // coordinate mapping is platform/DPR-fragile and bought nothing for a single
  // drop target).
  onMount(() => {
    // Restored chips are shown immediately (no flicker) and reconciled against
    // disk in the background — a staged file can vanish out-of-band.
    const restored = saved.attachments ?? [];
    if (restored.length > 0) void pruneMissingAttachments(restored);

    // Guarded: `getCurrentWebview()` throws outside a Tauri webview (tests, any
    // non-Tauri host), where drag-drop simply isn't available.
    let dropSub: Promise<() => void> | undefined;
    try {
      dropSub = getCurrentWebview().onDragDropEvent((event) => {
        const payload = event.payload;
        if (payload.type === "enter" || payload.type === "over") {
          dragOver = true;
        } else if (payload.type === "leave") {
          dragOver = false;
        } else if (payload.type === "drop") {
          dragOver = false;
          // Ignore drops while a send is rendering: the attachment set is frozen
          // for that send (see the `sending`-gated remove button too).
          if (!composerBusy) void stageDroppedPaths(payload.paths);
        }
      });
      void dropSub.catch((e) => console.error("[attachments] onDragDropEvent failed", e));
    } catch {
      dropSub = undefined;
    }
    // Await the subscription promise before unlistening, so an unmount that beats
    // the promise still tears the listener down. A bare `unlisten?.()` would
    // no-op in that race and leak a global listener that keeps staging into a
    // stale project context.
    return () => void dropSub?.then((u) => u()).catch(() => {});
  });

  // ── Prompt mode ────────────────────────────────────────────────────────────
  // `mode` swaps the compose area between the plain textarea, the structured
  // prompt composer, and the workflow invocation form. In workflow mode the
  // compose bar's own To field + message-level forward affordances are hidden:
  // a workflow parameterizes its recipients via its declared agent inputs, so
  // the workflow owns routing (the prompt-vs-workflow routing distinction).
  let mode = $state<"plain" | "prompt" | "workflow">("plain");
  let selectedPrompt = $state<Prompt | null>(null);
  let promptArgs = $state<Record<string, string>>({});
  // Per-argument forward sources (live-UI-only, like `forwardSources`). A
  // prompt-mode send with any entry here — or in `promptAppendedSources` — routes
  // through the forward-prompt path.
  let promptArgSources = $state<Record<string, ForwardSource[]>>(
    untrack(() => reconcileForwardSourceMap(savedForwards.promptArgs, agents, projectId)),
  );
  // Forward sources for the appended-text field (the appended text is just
  // another forwardable field; the backend composes it into the appended tail).
  let promptAppendedSources = $state<ForwardSource[]>(
    untrack(() => reconcileForwardSources(savedForwards.promptAppended, agents, projectId)),
  );
  let appendedText = $state<string>("");
  let promptMenuOpen = $state(false);
  let promptMenuAllowsLiteralInsert = $state(false);
  let prompts = $state<Prompt[]>([]);
  let focusPromptFieldOnMount = $state(false);
  let promptMenuSyncing = $state(false);
  // Whether the cache has been read at least once, so the picker can show a
  // "loading" row instead of momentarily claiming there are no prompts.
  let promptsLoaded = $state(false);

  // The provider whose browser sign-in this send is currently waiting on —
  // auto-launched when a render reports needs-sign-in (the user just tried to
  // use that provider's prompt, so intent is unambiguous). Drives the
  // compose-bar waiting line; `sending` stays true across the wait.
  // Informational (non-error) line in the send-feedback slot — used when a
  // mid-send sign-in succeeded but the composer context changed during the
  // browser wait, so the send was deliberately not dispatched.
  let sendNotice = $state<string | null>(null);

  // Workflow invocation state (live-UI-only). The menu lists the project's
  // workflows; picking an invocable one enters workflow mode with a per-input
  // form. Not persisted across reloads (a half-filled invocation is transient).
  let workflowMenuOpen = $state(false);
  let workflows = $state<WorkflowListing[]>([]);
  let workflowsLoaded = $state(false);
  let selectedWorkflow = $state<WorkflowListing | null>(null);
  // The resolved invocation form for the picked workflow (declared inputs +
  // auto-derived prompt-argument fields + compatibility). Fetched per-pick via
  // `describe_workflow_form`; independently completed prompt syncs reclassify it
  // through a cache-only command. Null while the initial fetch is pending or has
  // failed; those two states render explicitly rather than falling through to
  // plain compose.
  let workflowForm = $state<WorkflowFormDescriptor | null>(null);
  let workflowFormLoading = $state(false);
  let workflowFormError = $state<string | null>(null);
  // Monotonic token: each pick/re-fetch bumps it; a fetch ignores its reply if a
  // newer one superseded it (name alone isn't a workflow's identity — a built-in
  // and a same-named copied user workflow share a name).
  let workflowFormGen = 0;
  // The generation of the network-owning fresh resolver, when one is in flight.
  // Sync events arriving during it are coalesced after its response; cache-only
  // refreshes may supersede one another so the newest event wins.
  let workflowFreshGen: number | null = null;
  // Monotonic receipt count for authoritative prompt-resolution changes. A fresh
  // request records its starting value and, if the count advances, follows its
  // accepted response with one cache-only refresh against the latest snapshot.
  let promptResolutionEventGen = 0;
  let workflowInputs = $state<Record<string, WorkflowInputValue>>({});
  // Per-field forward sources for the workflow's fillable single-text fields,
  // keyed by field name. Persisted with the other forward families; reset whenever
  // the workflow changes (a field name means nothing across workflows).
  let workflowForwardSources = $state<Record<string, ForwardSource[]>>(
    untrack(() => reconcileForwardSourceMap(savedForwards.workflowFields, agents, projectId)),
  );

  /// Refresh restored foreign chips' display names — **once per mount**.
  ///
  /// `onMount`, not `$effect`, and the distinction is load-bearing: the body reads
  /// all four source families and writes them back, so as an effect every chip add
  /// and removal would re-run it and re-read each referenced project's registry.
  /// (It wouldn't loop — `refreshForeignSourceLabels` returns the same array
  /// reference when nothing changed — but the reads were unbounded behind a
  /// comment claiming a bound.)
  ///
  /// Once-per-mount is the right scope because `App.svelte` wraps this component
  /// in `{#key selection.activeProjectId}`, so a project switch remounts it and
  /// gets a fresh pass. **If that key is ever removed, this stops refreshing on a
  /// project switch and must become an effect keyed on `projectId`.**
  ///
  /// Bounded by what the draft references — the distinct *foreign* projects named
  /// by saved chips, which is none in the common case. Read-only roster calls (no
  /// load, no lock), so the browse/activate split holds. This is the only place
  /// the refresh can happen: restore is synchronous, and the picker's cache is
  /// empty until the user browses that project — the one path where a stale name
  /// isn't on screen. Failures keep the stored label rather than mutating or
  /// dropping the chip.
  onMount(() => {
    // A plain record, not a Map: local bookkeeping, never rendered — the same
    // reasoning as `seenSendSeq` in the transcript, and why the reactive-Map lint
    // doesn't apply.
    const foreign: Record<ProjectId, string> = {};
    for (const family of [
      forwardSources,
      promptAppendedSources,
      ...Object.values(promptArgSources),
      ...Object.values(workflowForwardSources),
    ]) {
      for (const source of family) {
        if (source.projectId !== undefined && source.projectId !== projectId) {
          const dir = projects.list.find((p) => p.id === source.projectId)?.directory;
          if (dir !== undefined) foreign[source.projectId] = dir;
        }
      }
    }
    const pending = Object.entries(foreign);
    if (pending.length === 0) return;
    let cancelled = false;
    for (const [id, dir] of pending) {
      void api
        .listProjectAgentsReadonly(id, dir)
        .then((roster) => {
          if (cancelled) return;
          const name = projects.list.find((p) => p.id === id)?.name;
          forwardSources = refreshForeignSourceLabels(forwardSources, id, roster, name);
          promptAppendedSources = refreshForeignSourceLabels(
            promptAppendedSources,
            id,
            roster,
            name,
          );
          for (const [field, list] of Object.entries(promptArgSources)) {
            promptArgSources[field] = refreshForeignSourceLabels(list, id, roster, name);
          }
          for (const [field, list] of Object.entries(workflowForwardSources)) {
            workflowForwardSources[field] = refreshForeignSourceLabels(list, id, roster, name);
          }
        })
        .catch(() => {
          // Unreadable project: keep the stored labels.
        });
    }
    return () => {
      cancelled = true;
    };
  });
  let invokingWorkflow = $state(false);
  let workflowSigningInProvider = $state<string | null>(null);
  let workflowSignInGen = 0;
  // A saved prompt-mode draft to restore once the cache loads; consumed when
  // restoration settles. Null when the saved draft was plain.
  let pendingRestore = $state<PromptContent | null>(
    saved.content.kind === "prompt" ? saved.content : null,
  );
  let promptRestoreRequestGen = 0;
  let latestPromptResolutionGeneration = 0;
  let promptRestoreIssue = $state<
    "confirmed_missing" | "not_configured" | "temporarily_unavailable" | "request_failed" | null
  >(null);
  // A saved workflow-mode invocation to restore once the workflow list loads.
  let pendingWorkflowRestore = $state<WorkflowContent | null>(
    saved.content.kind === "workflow" ? saved.content : null,
  );
  // True while a saved prompt- or workflow-mode draft is still being resolved
  // against its source. Gates the persist effect so the not-yet-restored plain
  // placeholder can't overwrite the saved snapshot before restoration settles.
  let restoring = $state(saved.content.kind === "prompt" || saved.content.kind === "workflow");
  // A workflow restore whose `list_workflows` failed. `restoring` stays true (so
  // the snapshot is preserved), and the placeholder shows a retry/discard state
  // instead of a spinner.
  let workflowRestoreFailed = $state(false);

  async function loadPrompts(): Promise<void> {
    try {
      const list = await api.listPrompts();
      prompts = Array.isArray(list) ? list : [];
    } catch {
      prompts = [];
    } finally {
      promptsLoaded = true;
    }
  }

  // Saved prompt restoration asks the backend for a provider-aware verdict from
  // one coherent snapshot. A global list miss cannot distinguish deletion from
  // auth, store, or transport failure. Workflow reclassification likewise stays
  // cache-only on authoritative resolution changes.
  onMount(() => {
    const hadDraft = pendingRestore !== null;
    let active = true;
    let listenerReady = false;
    let changedDuringRegistration = false;
    let unlisten: UnlistenFn | null = null;

    function reclassifyFromPromptSnapshot(): void {
      if (hadDraft) {
        promptRestoreIssue = null;
        void resolvePendingPrompt();
      }
      if (
        mode === "workflow" &&
        selectedWorkflow !== null &&
        workflowForm !== null &&
        workflowFreshGen === null
      ) {
        void loadWorkflowForm(selectedWorkflow, "cache_only");
      }
    }

    function onPromptChanged(event: { payload: { generation: number } }): void {
      latestPromptResolutionGeneration = Math.max(
        latestPromptResolutionGeneration,
        event.payload.generation,
      );
      promptResolutionEventGen = Math.max(promptResolutionEventGen, event.payload.generation);
      if (!listenerReady) {
        changedDuringRegistration = true;
        return;
      }
      reclassifyFromPromptSnapshot();
    }

    async function register(): Promise<void> {
      try {
        const registered = await listen<{ generation: number }>("prompts:changed", onPromptChanged);
        if (!active) {
          registered();
          return;
        }
        unlisten = registered;
        listenerReady = true;
        // One post-subscription read closes the read/subscribe gap. An event
        // received while registration was pending is deliberately coalesced
        // into this same pass rather than starting a competing request.
        if (hadDraft || changedDuringRegistration) reclassifyFromPromptSnapshot();
      } catch {
        // The initial snapshot read still settles restoration if native event
        // registration is unavailable.
      }
    }

    if (hadDraft) void resolvePendingPrompt();
    void register();
    return () => {
      active = false;
      unlisten?.();
      unlisten = null;
    };
  });

  // A saved workflow-mode draft resolves against the local workflow list, which is
  // not loaded until a menu opens — so fetch it here rather than waiting for one.
  onMount(() => {
    if (pendingWorkflowRestore === null) return;
    void loadWorkflows().then(tryRestoreWorkflow);
  });

  onMount(() => {
    if (!focusOnMount) return;
    // A pending workflow restore owns the focus decision: it fires after its
    // async list load, and focusing the textarea now would fight the form.
    if (pendingWorkflowRestore !== null) return;
    if (pendingRestore === null) {
      requestAnimationFrame(() => textareaEl?.focus());
    } else {
      focusPromptFieldOnMount = true;
    }
  });

  // A pane Cmd+click (see TranscriptPanes.onPaneClick) targets the pane and
  // then bumps `focusRequest` to take focus so the user can type immediately.
  // The effect's first run only records the baseline — so mount, and a remount
  // that inherits a prior count, never steal focus; only a later bump does.
  // Focused directly (not via rAF): the textarea already exists post-mount,
  // same as the Mod+K path — the rAF deferral is only for the mount/restore
  // paths where the element is freshly inserted. Prompt/workflow modes have no
  // textarea, so this no-ops there by design (focus assist is plain-mode only).
  let lastFocusRequest: number | null = null;
  $effect(() => {
    const requested = focusRequest;
    if (lastFocusRequest === null || requested === lastFocusRequest) {
      lastFocusRequest = requested;
      return;
    }
    lastFocusRequest = requested;
    textareaEl?.focus();
  });

  /// Resolve a saved prompt-mode draft against one coherent backend snapshot.
  /// Every unavailable verdict preserves the structured draft; only the user's
  /// explicit Start over action discards it.
  async function resolvePendingPrompt(fresh = false): Promise<void> {
    if (pendingRestore === null) return;
    const snapshot = pendingRestore;
    const request = ++promptRestoreRequestGen;
    try {
      const resolution = fresh
        ? await api.resolveSavedPromptFresh(snapshot.provider, snapshot.name)
        : await api.resolveSavedPrompt(snapshot.provider, snapshot.name);
      if (request !== promptRestoreRequestGen || pendingRestore !== snapshot) return;
      if (resolution.generation < latestPromptResolutionGeneration) return;
      latestPromptResolutionGeneration = Math.max(
        latestPromptResolutionGeneration,
        resolution.generation,
      );
      if (resolution.state === "available") {
        promptRestoreIssue = null;
        selectedPrompt = resolution.prompt;
        promptArgs = Object.fromEntries(
          resolution.prompt.arguments.map((a) => [a.name, snapshot.args[a.name] ?? ""]),
        );
        appendedText = snapshot.appendedText;
        focusPromptFieldOnMount = focusPromptFieldOnMount || focusOnMount;
        mode = "prompt";
        pendingRestore = null;
        restoring = false;
        return;
      }
      promptRestoreIssue =
        resolution.state === "confirmed_missing" || resolution.state === "not_configured"
          ? resolution.state
          : "temporarily_unavailable";
    } catch {
      // Preserve the persisted draft. A later prompt-state event or an explicit
      // retry can resolve it without destroying the user's arguments.
      if (request === promptRestoreRequestGen && pendingRestore === snapshot) {
        promptRestoreIssue = "request_failed";
      }
    }
  }

  function retryPromptRestore(): void {
    promptRestoreIssue = null;
    void resolvePendingPrompt(true);
  }

  function discardPromptRestore(): void {
    const snapshot = pendingRestore;
    draft = snapshot?.appendedText ?? "";
    pendingRestore = null;
    promptRestoreIssue = null;
    restoring = false;
  }

  function promptRestoreMessage(): string {
    switch (promptRestoreIssue) {
      case "confirmed_missing":
        return "This prompt is no longer available from its provider.";
      case "not_configured":
        return "This prompt's provider is no longer configured.";
      case "temporarily_unavailable":
        return "This prompt's provider is temporarily unavailable.";
      default:
        return "Couldn't restore your saved prompt.";
    }
  }

  /// Resolve a saved workflow-mode invocation against the loaded workflow list.
  ///
  /// `listOk` distinguishes the two outcomes that a bare `find(...) === undefined`
  /// conflates, and getting this wrong destroys user state:
  /// - **list succeeded, workflow absent** → genuinely renamed/deleted. Fall back
  ///   to plain mode; the persist effect then overwrites the saved snapshot, which
  ///   is correct because the workflow is confirmed gone.
  /// - **list failed** (transient FS/IPC error, permissions, corrupt file →
  ///   `loadWorkflows` catches all into `workflows = []`) → we simply don't know.
  ///   Keep the snapshot pending and `restoring` true (which gates the content
  ///   persist effect, so plain content never overwrites the saved workflow), and
  ///   surface a retry/discard state. Never let a failure erase the draft.
  ///
  /// Unlike `tryRestorePrompt`, the *absent* case is one-shot with no cold-cache
  /// grace period: a successful local list that lacks the workflow is authoritative.
  ///
  /// Saved field values are installed *before* `loadWorkflowForm`, whose seeding is
  /// additive — restored values survive and any field the workflow gained since is
  /// seeded empty.
  function tryRestoreWorkflow(listOk: boolean): void {
    const snapshot = pendingWorkflowRestore;
    if (snapshot === null) return;
    if (!listOk) {
      workflowRestoreFailed = true;
      return; // snapshot + `restoring` stay put; the draft is preserved.
    }
    pendingWorkflowRestore = null;
    workflowRestoreFailed = false;
    restoring = false;
    const found = workflows.find(
      (w) => w.name === snapshot.name && w.is_builtin === snapshot.isBuiltin,
    );
    if (found === undefined) {
      if (focusOnMount) requestAnimationFrame(() => textareaEl?.focus());
      return;
    }
    selectedWorkflow = found;
    workflowForm = null;
    workflowInputs = { ...snapshot.inputs };
    mode = "workflow";
    void loadWorkflowForm(found);
  }

  /// Retry a workflow restore that failed to list. Re-lists and re-resolves.
  function retryWorkflowRestore(): void {
    workflowRestoreFailed = false;
    void loadWorkflows().then(tryRestoreWorkflow);
  }

  /// Give up on a failed workflow restore and start fresh in plain mode. This is
  /// the user's explicit choice, so releasing the snapshot (letting the persist
  /// effect overwrite it) is intended here, unlike the transient-failure path.
  function discardWorkflowRestore(): void {
    pendingWorkflowRestore = null;
    workflowRestoreFailed = false;
    restoring = false;
  }

  /// The current compose content as a persistable snapshot. Single definition so
  /// the persist effect and the explicit send-clear persist agree. `content.kind`
  /// is the mode, so each mode round-trips through its own variant.
  function currentContent(): ComposeContent {
    if (mode === "prompt" && selectedPrompt !== null) {
      return {
        kind: "prompt",
        provider: selectedPrompt.provider,
        name: selectedPrompt.name,
        args: { ...promptArgs },
        appendedText,
      };
    }
    if (mode === "workflow" && selectedWorkflow !== null) {
      return {
        kind: "workflow",
        name: selectedWorkflow.name,
        // Part of the identity: a built-in and a same-named copied user workflow
        // are different workflows, so restore needs both to re-resolve the listing.
        isBuiltin: selectedWorkflow.is_builtin,
        // List-valued inputs are arrays; copy them out of reactive state so the
        // store never aliases live `$state`.
        inputs: Object.fromEntries(
          Object.entries(workflowInputs).map(([name, value]) => [
            name,
            Array.isArray(value) ? [...value] : value,
          ]),
        ),
      };
    }
    return { kind: "plain", draft };
  }

  /// The four forward-source families as a persistable snapshot. Copied out of
  /// reactive state rather than passed by reference — the store must not alias
  /// live `$state`, or it would keep mutating after this bar is gone.
  function currentForwards(): ComposeForwards {
    const copyMap = (map: Record<string, ForwardSource[]>): Record<string, ForwardSource[]> =>
      Object.fromEntries(Object.entries(map).map(([field, sources]) => [field, [...sources]]));
    return {
      message: [...forwardSources],
      promptArgs: copyMap(promptArgSources),
      promptAppended: [...promptAppendedSources],
      workflowFields: copyMap(workflowForwardSources),
    };
  }

  // Drop any selected ids whose agent disappeared (agent removed at runtime).
  $effect(() => {
    const valid = selectedIds.filter((id) => agents.some((a) => a.id === id));
    if (valid.length !== selectedIds.length) setSelectedIds(valid);
  });

  // Persist every forward-source family together — they are one field in the
  // snapshot because a mode switch hides the inapplicable ones but must preserve
  // them for the return trip. Like content, the send-clear path also writes
  // through explicitly, so a cleared set can't be overtaken by a same-frame unmount.
  $effect(() => {
    setForwards(projectId, currentForwards());
  });

  // Backstop mirroring `attachmentChips` → the snapshot (path 1 in the attachments
  // header comment). Content/forwards have had this from the start; attachments did
  // not, and a restore path that assigned chips without writing the store shipped a
  // silent file-loss bug. With this effect, any mounted mutation of the chip list
  // persists whether or not the mutating site remembered to. The synchronous
  // `commitChips` writes still matter for send-clear (`persistComposeNow` flushes in
  // the same frame, ahead of this scheduled effect); this covers everything else.
  $effect(() => {
    setAttachments(projectId, chipsToAttachments(attachmentChips));
  });

  // Persist the compose content per project (machine-local; see composeStore).
  // Plain and prompt modes are distinct persisted states. Skipped while a saved
  // prompt-mode draft is still being restored, so the pre-restore plain
  // placeholder can't overwrite (and destroy) the saved snapshot. The send-clear
  // path persists explicitly (`persistComposeNow`) so it survives a same-frame
  // unmount regardless of this effect's scheduling.
  $effect(() => {
    if (restoring) return;
    setContent(projectId, currentContent());
  });
  // The parent unmounts this bar the moment a project loses its last agent (it
  // falls back to the roster-loading / first-agent screen), so an empty roster
  // is the parent's job to gate. The `length === 0` skip is defense-in-depth:
  // it guarantees a transient empty roster can never overwrite saved chips with
  // `[]`, independent of any future change to the parent's gating.
  $effect(() => {
    if (agents.length === 0) return;
    setSelection(projectId, selectedIds);
  });

  /// Recipient set — every agent is shown as a toggle chip (click to add/drop);
  /// `@name` is the keyboard path to the same toggle. Sticky across sends, and
  /// persisted per project (across switches and restarts) via `composeStore`.
  ///
  /// The set itself lives in the shared `recipientSelection` store — the single
  /// source of truth for "who receives the send" — so pane targeting (header
  /// click, Cmd+click, Cmd+Alt+N) can write it and the pane coverage borders
  /// can derive from it. This component seeds it from the persisted snapshot at
  /// mount and persists writes back (the `setSelection` effect below), wherever
  /// they originated.
  untrack(() => setRecipients(projectId, initialSelection(saved.selectedIds, agents)));
  const selectedIds = $derived(selectionFor(projectId));
  function setSelectedIds(ids: AgentId[]): void {
    setRecipients(projectId, ids);
  }

  const rosterIds = $derived(agents.map((a) => a.id));
  const paneLayout = $derived(layoutFor(projectId, rosterIds));

  // No "dock" treatment on the compose box: an earlier iteration accented the
  // box's border whenever the recipient set exactly equaled one pane, but in
  // real use a persistent accent on the compose surface read as unexplained
  // noise. The pane's own coverage ring is the one targeting visual.

  /// Resolve the recipient set for a fresh mount.
  /// - A single-agent project shows no chips (nothing to choose), so the lone
  ///   agent is always the recipient — a saved empty/stale selection must never
  ///   leave it unsendable with no UI to recover.
  /// - A deliberate deselect-all (saved `[]`) is honored.
  /// - A saved selection whose agents were all removed falls back to the first
  ///   agent rather than stranding the composer with no recipient.
  function initialSelection(savedIds: AgentId[] | undefined, roster: AgentRecord[]): AgentId[] {
    if (roster.length === 0) return [];
    if (roster.length === 1) return [roster[0]!.id];
    if (savedIds !== undefined) {
      const valid = savedIds.filter((id) => roster.some((a) => a.id === id));
      if (valid.length > 0 || savedIds.length === 0) return valid;
    }
    return [roster[0]!.id];
  }

  // Mod+K (focus the message box) ignores the chord while a dialog is open or
  // while another editable element is focused, so it only ever pulls focus to
  // this composer's textarea.
  function hasOpenDialog(): boolean {
    return document.querySelector('[role="dialog"], [role="alertdialog"]') !== null;
  }

  // Keyboard routes to the recipient chips, working even while typing (the
  // modifier chord inserts no text). Window-level so they fire regardless of
  // focus. Mod+Shift+A selects every agent; Mod+1..9 toggles the Nth agent
  // (same order as the sidebar). Mod+Enter sends from prompt mode (plain mode's
  // textarea owns Mod+Enter so it can also suppress the newline).
  //
  // Escape also clears recipients, but — unlike the Mod chords — it carries a
  // destructive side effect and Escape is overloaded across the app, so it's
  // scoped to compose-surface focus (textarea or a chip). Outside the composer,
  // Escape is left alone for whatever else owns it.
  $effect(() => {
    function onKeydown(e: KeyboardEvent): void {
      const mod = e.metaKey || e.ctrlKey;
      if (mod && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "k") {
        if (hasOpenDialog()) return;
        if (isEditableShortcutTarget(e.target) && e.target !== textareaEl) return;
        e.preventDefault();
        textareaEl?.focus();
        return;
      }
      if (e.key === "Escape") {
        if (composeEl === undefined || !composeEl.contains(document.activeElement)) return;
        // First dismiss whichever menu is open, otherwise clear the recipient
        // set. The draft text is untouched either way.
        if (promptMenuOpen) {
          promptMenuOpen = false;
          e.preventDefault();
        } else if (menuOpen) {
          menuOpen = false;
          e.preventDefault();
        } else if (!composerBusy && selectedIds.length > 0) {
          setSelectedIds([]);
          e.preventDefault();
        }
        return;
      }
      if (!mod || e.altKey) return;
      // An open dialog (e.g. the command palette) owns the keyboard — don't let
      // a chord typed into it also toggle recipients or send. Mirrors the ⌘K
      // guard above.
      if (hasOpenDialog()) return;
      // While a workflow run replaces the compose box, the targeting chords below
      // (⌘⌃N forward, ⌘N toggle, ⌘⇧A select-all) would silently mutate the hidden
      // compose state behind the live view — so it would reappear with stray
      // recipients/forwards when the run ends. Inert them (send is already gated
      // off; ⌘K/Escape above stay live). Only the compose-targeting region.
      if (activeWorkflowRun !== null) return;
      // ⌘⌃1..9 → add pane N as a forward source, mirroring ⌘⌥1..9 ("target pane
      // N"). Both modifiers required, so it never collides with ⌘1..9 (target
      // agent N) — intercepted before that branch below. **Plain-mode only**: in
      // prompt mode forwarding is per-field, and in workflow mode the workflow
      // owns routing — the whole-message forward set is hidden in both, so this
      // must not mutate it from behind a hidden UI.
      if (e.metaKey && e.ctrlKey && !e.shiftKey && e.key >= "1" && e.key <= "9") {
        if (mode !== "plain") return;
        const pane = paneLayout.panes[Number(e.key) - 1];
        if (pane !== undefined && pane.members.length > 0) {
          e.preventDefault();
          if (!composerBusy) addPaneForwardSources(pane);
        }
        return;
      }
      if (e.key === "Enter") {
        if (composeEl?.contains(document.activeElement)) {
          // ⇧⌘↵ asks to branch. Prompt and workflow modes have no fork, and the
          // branches below would send *normally* — to the very agent the user
          // was trying to branch away from, with no indication that is what
          // happened. Intercept and explain. Plain mode never reaches here: the
          // textarea's own handler owns the chord.
          if (e.shiftKey && mode !== "plain") {
            e.preventDefault();
            handleForkSend();
          } else if (mode === "prompt") {
            e.preventDefault();
            handlePrimaryAction();
          } else if (mode === "workflow") {
            // ⌘Enter from inside the workflow form runs it (the invoke action
            // no-ops if the form isn't runnable / is already starting).
            e.preventDefault();
            void invokeWorkflowAction();
          }
        }
        return;
      }
      if (e.shiftKey) {
        if (e.key.toLowerCase() === "a") {
          e.preventDefault();
          if (composerBusy) return;
          setSelectedIds(agents.map((a) => a.id));
        }
        return;
      }
      if (e.key < "1" || e.key > "9") return;
      const agent = agents[Number(e.key) - 1];
      if (agent === undefined) return;
      e.preventDefault();
      if (composerBusy) return;
      toggleRecipient(agent.id);
    }
    window.addEventListener("keydown", onKeydown);
    return () => window.removeEventListener("keydown", onKeydown);
  });

  const selectedAgents = $derived(
    selectedIds
      .map((id) => agents.find((a) => a.id === id))
      .filter((a): a is AgentRecord => a !== undefined),
  );

  // ---- Fork: a send-time option, not a standalone action ----
  //
  // Claude has no copy-a-session operation and refuses a promptless fork, so a
  // branch can only come into existence *as a turn* (harness-behavior §3.5).
  // Modelling Fork as a send-modifier makes the UI match that: the send that
  // carries it registers the branch and dispatches its first turn as one
  // action, which is also why a parent and its not-yet-existing branch can
  // never be co-recipients.
  /// Shared so the shape check and the narrowing guard in `evaluateForkAttempt`
  /// cannot drift into saying different things about the same state.
  const NO_FORK_SOURCE = "Select an agent to branch from.";

  /// The one recipient a fork could branch from, or `null` when the selection
  /// isn't a single agent.
  const forkCandidate = $derived<AgentRecord | null>(
    selectedAgents.length === 1 ? (selectedAgents[0] ?? null) : null,
  );

  /// Whether any of the composing prompt's fields is filled from another agent —
  /// an argument or the appended text. Either one routes the send through the
  /// held forward-prompt path instead of an immediate render.
  const anyPromptFieldForwarded = $derived(
    selectedPrompt !== null &&
      (selectedPrompt.arguments.some((a) => (promptArgSources[a.name]?.length ?? 0) > 0) ||
        promptAppendedSources.length > 0),
  );

  /// Whether `agentId` looks like it has a harness session to branch from.
  ///
  /// Derived from the transcript rather than fetched: an agent with any turn
  /// has dispatched at least once, and the compose bar already holds that
  /// state. Deliberately **not** an `agent_session_info` call — the chip is
  /// advisory and re-validated by the backend, and putting an IPC behind every
  /// selection change would make the compose bar's request sequence depend on
  /// which agent is selected.
  ///
  /// **Advisory, not exact.** It errs toward offering the chip; the backend
  /// re-validates through `resolve_session_file` and returns a precise error.
  /// Known imprecision, all in the safe direction: an agent whose only turn
  /// failed before a session file was written reads as branchable, and so does
  /// a real session that parses to zero turns (an empty or housekeeping-only
  /// file). Hydration state is consulted so the *unsafe* direction — a real
  /// session reading as absent while hydration is loading or has failed —
  /// cannot happen.
  function looksLikeItHasASession(agentId: AgentId): boolean {
    // An empty transcript is evidence only once hydration has *completed*.
    // While loading, and after a failure until the user retries, it is empty for
    // an agent that may have a long history — and "send it a message first" is
    // then not merely unhelpful but false.
    if (!isHydrated(agentId)) return true;
    return (transcripts[agentId] ?? []).length > 0;
  }

  /// Why this send's *shape* rules out a fork, or `null` when it admits one.
  /// Everything except whether the recipient happens to be busy right now.
  ///
  /// **Drives visibility as a boolean, and explanation as a string.** The fork
  /// half is an unlabelled icon, so it is simply absent unless it applies —
  /// dimmed, it would be unclickable noise rather than an explanation. But the
  /// keyboard shortcut stays live in every one of these states and is then the
  /// *only* fork surface the user can reach, so the reason has to exist as real
  /// copy rather than as a comment. Silence there would swallow the keystroke.
  ///
  const forkShapeBlock = $derived.by((): string | null => {
    // A workflow is N sends; "which one branches?" has no answer.
    if (mode === "workflow") return "Fork isn't available while composing a workflow.";
    // A prompt with a forwarded field composes server-side and dispatches
    // whenever its sources settle, through `dispatchForwardPrompt` — a hold with
    // no bound. Branching into that means either a promptless agent sitting for
    // the duration or one registered against a long-stale busy-parent gate.
    if (mode === "prompt" && anyPromptFieldForwarded) {
      return "Fork isn't available while a prompt field is filled from another agent — clear the forwarded fields first.";
    }
    // Fork branches one agent, so a multi-recipient send has no single source.
    if (selectedAgents.length > 1) return "Fork branches one agent — select a single recipient.";
    const candidate = forkCandidate;
    if (candidate === null) return NO_FORK_SOURCE;
    if (mode === "plain" && forwardSources.length > 0) {
      // Both are send modifiers and the forward branch runs first, so a fork
      // would lose silently: the message would go to the parent, exactly what
      // the branch's selection swap exists to prevent.
      return "Fork branches from one agent's own history — clear the forward sources first.";
    }
    // Claude is the only harness Switchboard can branch (`supports_session_fork`).
    if (candidate.harness !== "claude_code") {
      return `Switchboard can only branch Claude Code sessions, and ${candidate.name} is ${HARNESS_LABEL[candidate.harness]}.`;
    }
    if (!looksLikeItHasASession(candidate.id)) {
      return `${candidate.name} has no session to branch from yet — send it a message first.`;
    }
    return null;
  });

  /// Why a fork can't be taken right now, or `null` when it can. Probe-measured:
  /// a branch taken mid-turn inherits a synthesized "No response requested."
  /// placeholder instead of the parent's real answer, permanently. The backend
  /// re-checks this at dispatch — here it keeps the offer off the screen and
  /// gives the shortcut something to say.
  const forkBlock = $derived.by((): string | null => {
    if (forkShapeBlock !== null) return forkShapeBlock;
    const candidate = forkCandidate;
    if (candidate !== null && agentIsWorking(runtimes[candidate.id])) {
      return `${candidate.name} is working — a branch taken now would not include its current answer. Wait for it to finish, or cancel it first.`;
    }
    return null;
  });

  const forkAvailable = $derived(forkBlock === null);

  /// What pressing the fork shortcut should do. `nothing-to-send` is deliberate
  /// silence: plain ⌘↵ on an empty composer already does nothing, and inventing
  /// an error for the fork half alone would be an inconsistency the user has to
  /// learn. Every *other* block explains itself.
  type ForkAttempt =
    | { kind: "ready"; source: AgentRecord }
    | { kind: "blocked"; reason: string }
    | { kind: "nothing-to-send" };

  function evaluateForkAttempt(): ForkAttempt {
    if (forkBlock !== null) return { kind: "blocked", reason: forkBlock };
    // Unreachable at runtime — `forkShapeBlock` already returns `NO_FORK_SOURCE`
    // for a null candidate, so the check above returns first. Kept because it is
    // what narrows `forkCandidate` to non-null for the `ready` variant below;
    // deleting it does not compile.
    const source = forkCandidate;
    if (source === null) return { kind: "blocked", reason: NO_FORK_SOURCE };
    // An empty composer has nothing to fork, so it is a no-op in every state —
    // ahead of the readiness checks below, which would otherwise explain why a
    // message the user never typed could not be sent. **Emptiness is
    // mode-specific**: prompt mode's message lives in the prompt's fields, not
    // `draft`, so the plain test would silently swallow every prompt fork.
    const nothingToSend =
      mode === "prompt"
        ? selectedPrompt === null || missingRequired.length > 0
        : draft.trim() === "" && attachmentChips.length === 0;
    if (nothingToSend) return { kind: "nothing-to-send" };
    // No `showStop` arm: it requires an empty composer and no attachments, which
    // the check above already answers. A send in flight with text typed leaves
    // `showStop` false and falls through to the checks below.
    if (composerBusy) return { kind: "blocked", reason: "Already sending." };
    if (!allRecipientsHydrated) {
      return {
        kind: "blocked",
        reason: `Still loading ${source.name}'s history — try again in a moment.`,
      };
    }
    return { kind: "ready", source };
  }

  /// `@` recipient picker: a trailing `@token` opens a typeahead of all agents;
  /// Enter / click picks one as the sole recipient and strips the token. This is
  /// the keyboard route to selecting recipients without touching the mouse.
  let menuOpen = $state(false);
  let menuEl = $state<HTMLDivElement | undefined>(undefined);
  let menuQuery = $state("");
  let fileMatches = $state<string[]>([]);
  let fileSearchState = $state<"idle" | "searching" | "ready" | "error">("idle");
  // Non-reactive cancellation state: it only invalidates pending async file searches.
  let fileSearchToken = 0;
  let fileSearchTimer: ReturnType<typeof setTimeout> | undefined = undefined;
  // The `@token` span the open menu is showing, captured at detection time so a
  // pick splices exactly what the menu offered — not whatever the live caret
  // points at (arrow keys can move the caret out of the token while the menu
  // stays open). Non-reactive: only read at pick time. `null` when no menu.
  let menuTokenSpan: { start: number; end: number } | null = null;
  let highlighted = $state(0);
  const AT_TOKEN = /(^|\s)@([^\s]*)$/;
  const FILE_MATCH_LIMIT = 12;
  const FILE_SEARCH_DEBOUNCE_MS = 180;

  const agentCandidates = $derived(
    menuOpen ? agents.filter((a) => a.name.toLowerCase().includes(menuQuery.toLowerCase())) : [],
  );

  /// The menu's navigable rows: file matches render first in their own section,
  /// then recipient actions and matching agents. **All** appears only when not
  /// everyone is selected and its keyword matches the query; **Clear** only when
  /// something is selected and its keyword matches. Even though files render
  /// first, keyboard selection prefers a matched agent when one exists.
  type FileMenuItem = {
    kind: "file";
    key: string;
    path: string;
    label: string;
    parent: string | null;
  };
  type RecipientMenuItem =
    | { kind: "all"; key: string }
    | { kind: "clear"; key: string }
    | { kind: "pane"; key: string; pane: TranscriptPane; index: number }
    | { kind: "agent"; key: string; agent: AgentRecord };
  type AttachmentMenuItem = { kind: "attachment"; key: string; chipId: string; label: string };
  type ForwardMenuItem =
    | { kind: "forward-agent"; key: string; agent: AgentRecord }
    | { kind: "forward-pane"; key: string; pane: TranscriptPane };
  type MenuItem = FileMenuItem | AttachmentMenuItem | RecipientMenuItem | ForwardMenuItem;
  // Current chips as menu rows, filtered by the `@`-query on their label (e.g.
  // `@image` narrows to `image-*`), consistent with how the file and recipient
  // sections filter.
  const attachmentItems = $derived.by<AttachmentMenuItem[]>(() => {
    if (!menuOpen || attachmentChips.length === 0) return [];
    const q = menuQuery.toLowerCase();
    return attachmentChips
      .filter((chip) => chip.label.toLowerCase().includes(q))
      .map((chip) => ({
        kind: "attachment" as const,
        key: `attachment:${chip.id}`,
        chipId: chip.id,
        label: chip.label,
      }));
  });
  const fileItems = $derived<FileMenuItem[]>(
    menuOpen
      ? fileMatches.map((path) => ({
          kind: "file",
          key: `file:${path}`,
          path,
          label: basename(path),
          parent: parentPath(path),
        }))
      : [],
  );
  const recipientItems = $derived.by<RecipientMenuItem[]>(() => {
    // Single-agent projects suppress the recipient section entirely; the lone
    // agent is already the implicit recipient, so @ is only useful for files.
    if (!menuOpen || agents.length <= 1) return [];
    const q = menuQuery.toLowerCase();
    const items: RecipientMenuItem[] = [];
    if (selectedIds.length < agents.length && "all".includes(q)) {
      items.push({ kind: "all", key: "all" });
    }
    if (selectedIds.length > 0 && "clear".includes(q)) {
      items.push({ kind: "clear", key: "clear" });
    }
    // Pane targets, ahead of individual agents — only once the user has
    // actually split (≥2 panes): with the single default pane the existing
    // `all` action already covers the only possible pane target, and a pane
    // entry would be a duplicate row in the most common state.
    if (paneLayout.panes.length > 1) {
      for (const [index, pane] of paneLayout.panes.entries()) {
        // An empty pane is not a send target (picking it could only clear
        // the recipient set); it keeps its positional ⌘⌥ number regardless.
        if (pane.members.length === 0) continue;
        if (!pane.name.toLowerCase().includes(q)) continue;
        items.push({ kind: "pane", key: `pane:${pane.id}`, pane, index });
      }
    }
    return [
      ...items,
      ...agentCandidates.map((agent) => ({
        kind: "agent" as const,
        key: agent.id,
        agent,
      })),
    ];
  });
  /// "Forward from {agent | pane}" entries — the manual cross-agent forward
  /// source picker (§7). Mirrors the recipient section's agent + pane filtering,
  /// but picking one adds a forward *source* chip rather than selecting a
  /// recipient. Suppressed in single-agent projects (nothing to forward between)
  /// and for sources already added.
  const forwardItems = $derived.by<ForwardMenuItem[]>(() => {
    // Message-level forwarding is plain-mode only (prompt mode forwards per-field;
    // workflow mode routes via its agent inputs).
    if (!menuOpen || agents.length <= 1 || mode !== "plain") return [];
    const q = menuQuery.toLowerCase();
    const items: ForwardMenuItem[] = [];
    const alreadyForwarded = forwardSourceIds(forwardSources);
    if (paneLayout.panes.length > 1) {
      for (const pane of paneLayout.panes) {
        if (!pane.name.toLowerCase().includes(q)) continue;
        // A pane resolves to its live member agents; offer it only while at least
        // one of those isn't already forwarded — otherwise picking it is a no-op
        // (the agent-only model has no pane chip of its own to add).
        const addable = forwardSourceAgentsForPane(pane, agents);
        if (addable.length === 0 || addable.every((s) => alreadyForwarded.includes(s.id))) continue;
        items.push({ kind: "forward-pane", key: `forward-pane:${pane.id}`, pane });
      }
    }
    for (const agent of agentCandidates) {
      if (alreadyForwarded.includes(agent.id)) continue;
      items.push({ kind: "forward-agent", key: `forward-agent:${agent.id}`, agent });
    }
    return items;
  });
  /// Forward entries that would resolve to nothing. Rendered (with the reason)
  /// but **not selectable**. The rule is scoped to *direct leaf rows*: a row
  /// maps one-to-one to a known-empty source, so it is disabled outright.
  /// **Panes are deliberately different** — a pane is a bulk shortcut and stays
  /// selectable even when a member is empty; its expansion produces one chip
  /// per member, and the empty member's chip carries the warning before the
  /// user submits. Each route intervenes at the point where it has a surface
  /// to say why. Keeping disabled rows out of `menuItems` — the list arrow
  /// keys walk and Enter picks from — is what makes the direct rule hold for
  /// the keyboard as well as the mouse.
  const emptyForwardKeys = $derived(
    new Set(
      forwardItems
        .filter(
          (item) => item.kind === "forward-agent" && agentReadiness(item.agent.id) === "empty",
        )
        .map((item) => item.key),
    ),
  );
  const menuItems = $derived.by<MenuItem[]>(() => {
    if (!menuOpen) return [];
    return [
      ...fileItems,
      ...attachmentItems,
      ...recipientItems,
      ...forwardItems.filter((item) => !emptyForwardKeys.has(item.key)),
    ];
  });
  const fileStatusText = $derived.by<string | null>(() => {
    if (fileItems.length > 0) return null;
    if (fileSearchState === "searching") return "Searching files...";
    if (fileSearchState === "ready") return "No matching files";
    if (fileSearchState === "error") return "File search unavailable";
    return null;
  });
  const showFileSection = $derived(fileItems.length > 0 || fileStatusText !== null);
  // `forwardItems` separately, because an unselectable forward row is still a row:
  // a query that matches only spent agents must show them, not close the menu.
  const hasMenuContent = $derived(
    menuItems.length > 0 || forwardItems.length > 0 || showFileSection,
  );

  // Every recipient's history loaded — the precondition for a send (independent
  // of run_status: a busy recipient's message queues).
  const allRecipientsHydrated = $derived(
    selectedAgents.length > 0 &&
      selectedAgents.every((a) => {
        const rt = runtimes[a.id];
        return (
          rt !== undefined &&
          (rt.hydration_status === "complete" || rt.hydration_status === "failed")
        );
      }),
  );

  const missingRequired = $derived(
    selectedPrompt === null
      ? []
      : missingRequiredArgs(selectedPrompt, promptArgs).filter(
          // A required argument with ≥1 forward source isn't blocking — the
          // forwarded output fills it (mirrors PromptComposer's `missing`).
          (name) => (promptArgSources[name]?.length ?? 0) === 0,
        ),
  );

  /// Send is gated on a recipient + every recipient's history being loaded, plus
  /// per-mode content: plain needs non-empty text; prompt needs a selected prompt
  /// Busy for *this project's composer*, not just this component instance.
  ///
  /// Read from the operation claim alone. Every send path that awaits claims the
  /// slot, so a component-local flag would be a second representation of the same
  /// fact — and the two disagreeing after a remount is the bug this feature kept
  /// reproducing. It also makes abandonment work: releasing the claim frees the
  /// composer everywhere, including the bar that started the operation.
  const composerBusy = $derived(operationFor(projectId) !== undefined);

  /// The operation is parked on a browser sign-in whose final step the backend
  /// deliberately leaves un-timed. Offer a way out rather than letting one stuck
  /// sign-in hold the project's composer until the app restarts.
  const abandonableOperation = $derived.by(() => {
    const op = operationFor(projectId);
    if (op === undefined || op.phase.name !== "awaiting_user") return undefined;
    return { id: op.id, provider: op.phase.provider };
  });

  /// A finished operation's message reaches whichever bar is mounted now, which
  /// may not be the one that started it — so it is *consumed* into this bar's own
  /// error/notice state rather than rendered from shared state. Rendering it as a
  /// standing fallback kept a stale "Fork failed" alive through every subsequent
  /// action and every revisit of the project.
  $effect(() => {
    const outcome = outcomeFor(projectId);
    if (outcome === undefined) return;
    untrack(() => {
      takeOutcome(projectId);
      if (outcome.tone === "error") showError(outcome.message);
      else showNotice(outcome.message);
    });
  });

  /// with all required arguments filled, and is blocked while a render is in
  /// flight. **Not** gated on run_status — send-while-busy queues.
  const sendDisabled = $derived(
    mode === "prompt"
      ? selectedPrompt === null ||
          missingRequired.length > 0 ||
          composerBusy ||
          !allRecipientsHydrated
      : (draft.trim() === "" && attachmentChips.length === 0 && forwardSources.length === 0) ||
          composerBusy ||
          !allRecipientsHydrated,
  );

  // Every live send across this project's agents, mapped to the agents it's
  // live for. The composer stop cancels *all* of it, not just the most recent
  // send, so one click halts everything the project's agents are running and
  // have queued. (IPC failures prune `pending_sends` via failSendStart, so
  // failed recipients drop out without extra bookkeeping.)
  const liveSends = $derived(buildLiveSendsMap(agents, runtimes, transcripts));
  // The stop-morph is a plain-mode affordance: an empty textarea means the
  // primary action is "stop the live send" rather than "send". Prompt mode never
  // morphs — its primary action is always send.
  const showStop = $derived(
    mode === "plain" &&
      liveSends.size > 0 &&
      draft.trim() === "" &&
      attachmentChips.length === 0 &&
      forwardSources.length === 0,
  );
  const primaryDisabled = $derived(showStop ? false : sendDisabled);

  function toggleRecipient(id: AgentId): void {
    if (composerBusy) return;
    setSelectedIds(
      selectedIds.includes(id) ? selectedIds.filter((x) => x !== id) : [...selectedIds, id],
    );
  }

  function preferredHighlightIndex(items: MenuItem[]): number {
    const agentIndex = items.findIndex((item) => item.kind === "agent");
    return agentIndex >= 0 ? agentIndex : 0;
  }

  function fileMenuItemsFor(paths: string[]): FileMenuItem[] {
    return paths.map((path) => ({
      kind: "file",
      key: `file:${path}`,
      path,
      label: basename(path),
      parent: parentPath(path),
    }));
  }

  function parentPath(path: string): string | null {
    const trimmed = path.endsWith("/") ? path.slice(0, -1) : path;
    const i = trimmed.lastIndexOf("/");
    return i > 0 ? trimmed.slice(0, i) : null;
  }

  /// Replace the active `@token` (at the caret, anywhere in the text) with
  /// `insert`, fixing up spacing and moving the caret to just after the inserted
  /// text. `insert === ""` strips the token (recipient picks); a non-empty
  /// `insert` is a mention and gets exactly one trailing space before any
  /// following word. No-op if there's no active token (e.g. the caret moved away
  /// before the pick landed).
  function replaceAtToken(insert: string): void {
    const span = menuTokenSpan;
    if (span === null) return;
    // Splice the span the menu actually captured, but only if the text there
    // still spells `@<menuQuery>` — guards against the caret/text drifting out
    // from under the open menu (e.g. arrow keys) producing a garbled splice.
    if (draft.slice(span.start, span.end) !== `@${menuQuery}`) return;
    const before = draft.slice(0, span.start);
    let after = draft.slice(span.end);
    let text = insert;
    if (insert === "") {
      // Removing a token that sat between two spaces would leave a double space.
      if (/\s$/.test(before) && /^\s/.test(after)) after = after.slice(1);
    } else if (after.length === 0 || !/^\s/.test(after)) {
      text = `${insert} `;
    }
    draft = `${before}${text}${after}`;
    const caret = before.length + text.length;
    void tick().then(() => {
      textareaEl?.focus();
      textareaEl?.setSelectionRange(caret, caret);
    });
  }

  function stripAtToken(): void {
    replaceAtToken("");
  }

  function markdownCodeSpan(text: string): string {
    const runs = text.match(/`+/g) ?? [];
    const longest = runs.reduce((max, run) => Math.max(max, run.length), 0);
    const fence = "`".repeat(longest + 1);
    if (text.startsWith("`") || text.endsWith("`")) {
      return `${fence} ${text} ${fence}`;
    }
    return `${fence}${text}${fence}`;
  }

  function insertFileMention(path: string): void {
    replaceAtToken(markdownCodeSpan(path));
  }

  function pickItem(item: MenuItem): void {
    if (item.kind === "file") {
      insertFileMention(item.path);
    } else if (item.kind === "attachment") {
      // Insert the chip's reference token (`` `image-1` ``) via the same
      // mechanism as a file mention — the chip set is what's sent; this just
      // lets the user write prose referring to it.
      insertFileMention(item.label);
    } else if (item.kind === "all") {
      setSelectedIds(agents.map((a) => a.id));
      stripAtToken();
    } else if (item.kind === "clear") {
      setSelectedIds([]);
      stripAtToken();
    } else if (item.kind === "pane") {
      // Replace semantics, matching `@agentname` — `@panename` makes the pane
      // the target, exactly like clicking its header (and honors the same
      // targeting freeze). Targeting also reveals a minimized or
      // maximized-over pane, like Cmd+Alt+N; the reveal is gated on the
      // target write so a freeze-refused gesture changes nothing visible.
      if (targetRecipients(projectId, [...item.pane.members])) {
        revealPane(projectId, rosterIds, item.pane.id);
      }
      stripAtToken();
    } else if (item.kind === "forward-agent") {
      addForwardSource(forwardSourceForAgent(item.agent));
      stripAtToken();
    } else if (item.kind === "forward-pane") {
      // A pane is a shortcut for its members — add one agent source per live
      // member (deduped), never a pane chip.
      addPaneForwardSources(item.pane);
      stripAtToken();
    } else {
      setSelectedIds([item.agent.id]);
      stripAtToken();
    }
    closeMentionMenu();
  }

  function clearFileSearchTimer(): void {
    if (fileSearchTimer === undefined) return;
    clearTimeout(fileSearchTimer);
    fileSearchTimer = undefined;
  }

  function scheduleFileMatchRefresh(query: string): void {
    clearFileSearchTimer();
    const token = (fileSearchToken += 1);
    fileSearchState = "searching";
    fileSearchTimer = setTimeout(() => {
      fileSearchTimer = undefined;
      void refreshFileMatches(query, token);
    }, FILE_SEARCH_DEBOUNCE_MS);
  }

  function closeMentionMenu(): void {
    menuOpen = false;
    menuTokenSpan = null;
    fileMatches = [];
    fileSearchState = "idle";
    clearFileSearchTimer();
    fileSearchToken += 1;
  }

  onDestroy(() => {
    clearFileSearchTimer();
    fileSearchToken += 1;
    // In-flight staging is deliberately *not* abandoned: the copy belongs to this
    // project's draft, and `addAttachmentChip` commits it to the snapshot whether
    // or not this bar is still around to render the chip.
    unmounted = true;
    // Flush point: a project switch remounts this bar (`{#key}`), so the
    // outgoing bar's deferred draft write must land before the next one mounts.
    flush();
  });

  async function refreshFileMatches(query: string, token: number): Promise<void> {
    try {
      const matches = await api.searchProjectFiles(projectId, query, FILE_MATCH_LIMIT);
      if (token !== fileSearchToken || !menuOpen || menuQuery !== query) return;
      fileMatches = matches;
      fileSearchState = "ready";
      highlighted = preferredHighlightIndex([
        ...fileMenuItemsFor(matches),
        ...attachmentItems,
        ...recipientItems,
      ]);
    } catch {
      if (token !== fileSearchToken || !menuOpen || menuQuery !== query) return;
      fileMatches = [];
      fileSearchState = "error";
      highlighted = preferredHighlightIndex([...attachmentItems, ...recipientItems]);
    }
  }

  /// The `@token` immediately to the left of the caret, or `null`. Caret-aware
  /// (not anchored to end-of-text) so `@` works in the middle of a message: the
  /// token is `@` + non-whitespace chars ending exactly at the caret, with the
  /// `@` at the start of the text or after whitespace. `start` is the `@`'s
  /// index; `end` is the caret. A non-collapsed selection isn't a typing caret,
  /// so it yields `null`.
  function activeAtToken(): { query: string; start: number; end: number } | null {
    const el = textareaEl;
    if (el !== undefined && el.selectionStart !== el.selectionEnd) return null;
    const caret = el?.selectionStart ?? draft.length;
    const match = AT_TOKEN.exec(draft.slice(0, caret));
    if (!match) return null;
    const query = match[2] ?? "";
    return { query, start: caret - query.length - 1, end: caret };
  }

  $effect(() => {
    if (!menuOpen) return;
    function onPointerDown(e: PointerEvent): void {
      if (menuEl?.contains(e.target as Node)) return;
      closeMentionMenu();
    }
    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  });

  // Click-outside closes the prompt / workflow pickers (their own Escape/pick
  // also close them). "Outside" is anything that isn't the menu itself or its
  // trigger button: scoping to the menu — rather than the whole compose box —
  // is what lets a click on the textarea dismiss it, and excluding the trigger
  // keeps a click there from closing-then-reopening (the trigger owns its own
  // toggle).
  function closeMenuOnOutsidePointer(
    e: PointerEvent,
    menuTestid: string,
    triggerTestid: string,
    close: () => void,
  ): void {
    const el = e.target instanceof Element ? e.target : null;
    if (el?.closest(`[data-testid="${menuTestid}"]`)) return;
    if (el?.closest(`[data-testid="${triggerTestid}"]`)) return;
    close();
  }
  $effect(() => {
    if (!promptMenuOpen) return;
    function onPointerDown(e: PointerEvent): void {
      closeMenuOnOutsidePointer(e, "prompt-menu", "compose-prompt-button", () => {
        promptMenuOpen = false;
      });
    }
    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  });
  $effect(() => {
    if (!workflowMenuOpen) return;
    function onPointerDown(e: PointerEvent): void {
      closeMenuOnOutsidePointer(e, "workflow-menu", "compose-workflow-button", () => {
        workflowMenuOpen = false;
      });
    }
    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  });

  $effect(() => {
    if (!menuOpen) return;
    if (menuItems.length === 0) return;
    if (highlighted >= menuItems.length) highlighted = menuItems.length - 1;
  });

  function onInput(): void {
    const token = activeAtToken();
    const hasAtToken = token !== null;
    const hasRecipientOptions = agents.length > 1;
    const hasAttachments = attachmentChips.length > 0;
    const shouldSearchFiles = hasAtToken && (token.query.length > 0 || agents.length === 1);
    const shouldOpenMenu =
      hasAtToken && (hasRecipientOptions || shouldSearchFiles || hasAttachments);
    if (token !== null && shouldOpenMenu) {
      promptMenuOpen = false;
      menuQuery = token.query;
      menuTokenSpan = { start: token.start, end: token.end };
      menuOpen = true;
      if (shouldSearchFiles) {
        const q = token.query.toLowerCase();
        const retainedFileMatches = fileMatches.filter((path) => path.toLowerCase().includes(q));
        fileMatches = retainedFileMatches;
        highlighted = preferredHighlightIndex([
          ...fileMenuItemsFor(retainedFileMatches),
          ...attachmentItems,
          ...recipientItems,
        ]);
        scheduleFileMatchRefresh(token.query);
      } else {
        fileMatches = [];
        highlighted = preferredHighlightIndex([...attachmentItems, ...recipientItems]);
        clearFileSearchTimer();
        fileSearchState = "idle";
        fileSearchToken += 1;
      }
    } else {
      closeMentionMenu();
    }
  }

  function handleKey(event: KeyboardEvent): void {
    if (menuOpen && menuItems.length > 0) {
      if (event.key === "ArrowDown") {
        event.preventDefault();
        highlighted = (highlighted + 1) % menuItems.length;
        return;
      }
      if (event.key === "ArrowUp") {
        event.preventDefault();
        highlighted = (highlighted - 1 + menuItems.length) % menuItems.length;
        return;
      }
      if (event.key === "Enter" && !event.metaKey) {
        event.preventDefault();
        const pick = menuItems[highlighted];
        if (pick !== undefined) pickItem(pick);
        return;
      }
    }
    // `/` on an empty textarea opens the prompt picker (instead of typing a slash).
    if (event.key === "/" && draft === "" && !promptMenuOpen) {
      event.preventDefault();
      openPromptMenu(true);
      return;
    }
    // Escape (menu dismiss / clear recipients) is handled by the window-level
    // listener above, so it works whether the textarea or a chip has focus.
    if (event.key === "Enter" && event.metaKey) {
      event.preventDefault();
      if (event.shiftKey) handleForkSend();
      else handlePrimaryAction();
    }
  }

  function openPromptMenu(allowsLiteralInsert = false): void {
    closeMentionMenu();
    workflowMenuOpen = false;
    promptMenuAllowsLiteralInsert = allowsLiteralInsert;
    void loadPrompts();
    promptMenuOpen = true;
  }

  function setEmptyDraftFromPromptSearch(message: string): void {
    draft = message;
    promptMenuOpen = false;
    void tick().then(() => {
      textareaEl?.focus();
      textareaEl?.setSelectionRange(message.length, message.length);
    });
  }

  /// Enter prompt mode (or swap the chosen prompt). Carries any text the user
  /// had into Appended text so nothing is lost; resets argument inputs for the
  /// newly chosen prompt.
  function pickPrompt(prompt: Prompt): void {
    const carried = mode === "plain" ? draft : appendedText;
    selectedPrompt = prompt;
    promptArgs = Object.fromEntries(prompt.arguments.map((a) => [a.name, ""]));
    promptArgSources = {};
    promptAppendedSources = [];
    appendedText = carried;
    focusPromptFieldOnMount = true;
    draft = "";
    mode = "prompt";
    promptMenuOpen = false;
  }

  /// Copy a read-only built-in into the user's own prompts, then refresh the
  /// cache so the owned copy appears (the backend syncs before this resolves).
  /// Keeps the menu open so the user sees their new prompt land; a name clash or
  /// write failure surfaces on the send-error line.
  async function copyPrompt(prompt: Prompt): Promise<void> {
    try {
      await api.copyBuiltinPrompt(prompt.name);
      await loadPrompts();
      clearStatus();
    } catch (err) {
      showError(`Couldn't copy prompt: ${err instanceof Error ? err.message : String(err)}`);
    }
  }

  async function syncPromptMenu(): Promise<void> {
    if (promptMenuSyncing) return;
    promptMenuSyncing = true;
    clearStatus();
    try {
      await api.syncPrompts();
      await loadPrompts();
    } catch (err) {
      showError(`Couldn't sync prompts: ${err instanceof Error ? err.message : String(err)}`);
    } finally {
      promptMenuSyncing = false;
    }
  }

  /// Leave prompt mode, carrying Appended text back into the plain textarea.
  function removePrompt(): void {
    draft = appendedText;
    mode = "plain";
    selectedPrompt = null;
    promptArgs = {};
    promptArgSources = {};
    promptAppendedSources = [];
    focusPromptFieldOnMount = false;
    appendedText = "";
  }

  // --- Workflows ------------------------------------------------------------

  /// Returns whether the list actually succeeded. The workflow-restore path needs
  /// to tell "list succeeded, workflow absent" from "list failed" — collapsing
  /// both into `workflows = []` is what let a transient failure erase a saved
  /// invocation. Other callers ignore the result.
  async function loadWorkflows(): Promise<boolean> {
    try {
      const list = await api.listWorkflows();
      workflows = Array.isArray(list) ? list : [];
      return true;
    } catch {
      workflows = [];
      return false;
    } finally {
      workflowsLoaded = true;
    }
  }

  function openWorkflowMenu(): void {
    closeMentionMenu();
    promptMenuOpen = false;
    void loadWorkflows();
    void loadPrompts();
    workflowMenuOpen = true;
  }

  /// Enter workflow mode with the picked workflow and resolve its form (declared
  /// inputs + auto-derived prompt-argument fields) via `describe_workflow_form`.
  /// The prompt is hardcoded — nothing to pre-seed/pick — so fields seed empty.
  function pickWorkflow(workflow: WorkflowListing): void {
    workflowSignInGen += 1;
    workflowSigningInProvider = null;
    selectedWorkflow = workflow;
    workflowForm = null;
    workflowFormError = null;
    workflowInputs = {};
    workflowForwardSources = {};
    mode = "workflow";
    workflowMenuOpen = false;
    void loadWorkflowForm(workflow);
  }

  /// Fetch (or re-fetch) the descriptor for the picked workflow and reconcile its
  /// draft with the accepted schema. An initial pick/manual check uses the fresh
  /// resolver; a `prompts:changed` event can only use the cache-only resolver. A
  /// monotonic generation token guards stale replies.
  function emptyWorkflowValue(
    ty: WorkflowFormDescriptor["inputs"][number]["ty"],
  ): WorkflowInputValue {
    return ty === "agent_list" || ty === "text_list" ? [] : "";
  }

  function valueMatchesWorkflowType(
    value: WorkflowInputValue | undefined,
    ty: WorkflowFormDescriptor["inputs"][number]["ty"],
  ): value is WorkflowInputValue {
    return ty === "agent_list" || ty === "text_list"
      ? Array.isArray(value)
      : typeof value === "string";
  }

  function reconcileWorkflowFormState(
    form: WorkflowFormDescriptor,
    previous: WorkflowFormDescriptor | null,
  ): {
    inputs: Record<string, WorkflowInputValue>;
    forwards: Record<string, ForwardSource[]>;
  } {
    const nextInputs: Record<string, WorkflowInputValue> = {};
    const nextForwards: Record<string, ForwardSource[]> = {};
    const previousInputs = new Map(previous?.inputs.map((input) => [input.name, input.ty]) ?? []);
    const currentDeclared = new Set(form.inputs.map((input) => input.name));

    for (const input of form.inputs) {
      const value = workflowInputs[input.name];
      const sameSemanticType =
        previous === null
          ? valueMatchesWorkflowType(value, input.ty)
          : previousInputs.get(input.name) === input.ty;
      nextInputs[input.name] =
        sameSemanticType && valueMatchesWorkflowType(value, input.ty)
          ? Array.isArray(value)
            ? [...value]
            : value
          : emptyWorkflowValue(input.ty);
      const inputForwards = workflowForwardSources[input.name];
      if (input.ty === "text" && sameSemanticType && inputForwards) {
        nextForwards[input.name] = [...inputForwards];
      }
    }

    const previousDerived = new Set(previous?.derived_args.map((argument) => argument.name) ?? []);
    if (form.compatibility.state === "ok") {
      for (const argument of form.derived_args) {
        const value = workflowInputs[argument.name];
        const previousSchemaAuthoritative = previous?.compatibility.state === "ok";
        const sameField = !previousSchemaAuthoritative || previousDerived.has(argument.name);
        nextInputs[argument.name] = sameField && typeof value === "string" ? value : "";
        const argumentForwards = workflowForwardSources[argument.name];
        if (sameField && argumentForwards) {
          nextForwards[argument.name] = [...argumentForwards];
        }
      }
    } else {
      // A provider outage can temporarily erase every derived schema. Preserve
      // those hidden draft values until a successfully resolved schema can
      // authoritatively reconcile them.
      const draftDerived = Object.keys(workflowInputs).filter((name) => !currentDeclared.has(name));
      for (const name of draftDerived) {
        const value = workflowInputs[name];
        if (!currentDeclared.has(name) && typeof value === "string") nextInputs[name] = value;
        const draftForwards = workflowForwardSources[name];
        if (!currentDeclared.has(name) && draftForwards) {
          nextForwards[name] = [...draftForwards];
        }
      }
    }

    return { inputs: nextInputs, forwards: nextForwards };
  }

  async function loadWorkflowForm(
    workflow: WorkflowListing,
    resolution: "fresh" | "cache_only" = "fresh",
  ): Promise<void> {
    const gen = ++workflowFormGen;
    const resolutionEventsAtStart = promptResolutionEventGen;
    if (resolution === "fresh") workflowFreshGen = gen;
    if (workflowForm === null) workflowFormError = null;
    workflowFormLoading = true;
    try {
      const form =
        resolution === "fresh"
          ? await api.describeWorkflowForm(workflow.name, workflow.is_builtin)
          : await api.refreshWorkflowFormFromCache(workflow.name, workflow.is_builtin);
      if (gen !== workflowFormGen) return; // superseded by a newer pick/re-fetch
      const reconciled = reconcileWorkflowFormState(form, workflowForm);
      workflowForm = form;
      workflowFormError = null;
      workflowInputs = reconciled.inputs;
      workflowForwardSources = reconciled.forwards;
    } catch (err) {
      if (gen === workflowFormGen) {
        const message = err instanceof Error ? err.message : String(err);
        if (workflowForm === null) workflowFormError = message;
        else showError(`Couldn't refresh workflow: ${message}`);
      }
    } finally {
      if (resolution === "fresh" && workflowFreshGen === gen) workflowFreshGen = null;
      if (gen === workflowFormGen) workflowFormLoading = false;
    }
    if (
      resolution === "fresh" &&
      gen === workflowFormGen &&
      promptResolutionEventGen > resolutionEventsAtStart &&
      selectedWorkflow?.name === workflow.name &&
      selectedWorkflow.is_builtin === workflow.is_builtin
    ) {
      void loadWorkflowForm(workflow, "cache_only");
    }
  }

  function removeWorkflow(): void {
    workflowSignInGen += 1;
    workflowSigningInProvider = null;
    mode = "plain";
    selectedWorkflow = null;
    workflowForm = null;
    workflowFormLoading = false;
    workflowFormError = null;
    workflowInputs = {};
    workflowForwardSources = {};
    workflowFormGen++; // invalidate any in-flight fetch for the removed workflow
  }

  function retryWorkflowForm(): void {
    if (selectedWorkflow === null || workflowFormLoading || workflowSigningInProvider !== null)
      return;
    void loadWorkflowForm(selectedWorkflow);
  }

  async function signInWorkflowProvider(provider: string): Promise<void> {
    if (selectedWorkflow === null || workflowSigningInProvider !== null) return;
    const workflow = selectedWorkflow;
    const attempt = ++workflowSignInGen;
    workflowSigningInProvider = provider;
    clearStatus();
    try {
      await api.signInMcpProvider(provider);
      if (
        unmounted ||
        attempt !== workflowSignInGen ||
        selectedWorkflow?.name !== workflow.name ||
        selectedWorkflow.is_builtin !== workflow.is_builtin
      ) {
        return;
      }
      await loadWorkflowForm(workflow);
    } catch (err) {
      if (!unmounted && attempt === workflowSignInGen) {
        showError(
          `Couldn't sign in to ${provider}: ${err instanceof Error ? err.message : String(err)}`,
        );
      }
    } finally {
      if (!unmounted && attempt === workflowSignInGen) workflowSigningInProvider = null;
    }
  }

  async function copyWorkflow(workflow: WorkflowListing): Promise<void> {
    try {
      await api.copyBuiltinWorkflow(workflow.name);
      await loadWorkflows();
      clearStatus();
    } catch (err) {
      showError(`Couldn't copy workflow: ${err instanceof Error ? err.message : String(err)}`);
    }
  }

  function openWorkflowsFolder(): void {
    void api.openWorkflowsDir().catch((err: unknown) => {
      console.error("[switchboard] open workflows folder failed", err);
    });
  }

  function openPromptsFolder(): void {
    void api.openLocalPromptsDir().catch((err: unknown) => {
      console.error("[switchboard] open prompts folder failed", err);
    });
  }

  async function copyWorkflowAuthoringPrompt(): Promise<boolean> {
    let workflowsDir: string | null = null;
    try {
      workflowsDir = await api.workflowsDir();
    } catch (err) {
      console.error("[switchboard] resolve workflows folder failed", err);
    }

    try {
      await copyText(workflowAuthoringPrompt(workflowsDir));
      clearStatus();
      return true;
    } catch (err) {
      const message = `Couldn't copy workflow prompt: ${err instanceof Error ? err.message : String(err)}`;
      showError(message);
      return false;
    }
  }

  function configurePrompts(): void {
    promptMenuOpen = false;
    onConfigurePrompts?.();
  }

  /// Whether the picked workflow is runnable: the form is resolved, invocable,
  /// compatible (prompts resolved, no drift), and every required field (declared
  /// input or derived prompt arg) is filled. Drives the invoke button's disabled
  /// state. A pending (`unresolved`) or `incompatible` form blocks Run.
  const workflowRunnable = $derived.by(() => {
    const form = workflowForm;
    if (form === null || workflowFormLoading) return false;
    if (!form.invocable || form.compatibility.state !== "ok") return false;
    // A single `text` input / derived arg also counts as filled when it carries
    // ≥1 forward source (only text/derived fields can — agent/list fields keep
    // their existing emptiness check).
    const hasForward = (name: string): boolean => (workflowForwardSources[name]?.length ?? 0) > 0;
    const inputMissing = form.inputs.some((i) => {
      if (i.optional) return false;
      const v = workflowInputs[i.name];
      if (i.ty === "agent_list" || i.ty === "text_list") return !Array.isArray(v) || v.length === 0;
      return (typeof v !== "string" || v.trim() === "") && !hasForward(i.name);
    });
    const argMissing = form.derived_args.some((a) => {
      if (!a.required) return false;
      const v = workflowInputs[a.name];
      return (typeof v !== "string" || v.trim() === "") && !hasForward(a.name);
    });
    return !inputMissing && !argMissing;
  });

  // The viewed project's single workflow run. The `[0]` relies on the
  // one-run-per-project invariant, enforced at the backend invoke guard (which
  // rejects both a second *active* run and a launch while a *held*
  // failed/interrupted run awaits dismissal) — so the array never holds more than
  // one and `[0]` is the run, not an arbitrary pick. When present it replaces the
  // compose box with the live progress view: a `running` run shows progress; a
  // `failed`/`interrupted` run is held (failed step + reason) until dismissed.
  const activeWorkflowRun = $derived(workflowRuns[projectId]?.[0] ?? null);
  // A Stop/Dismiss failure, surfaced inline in the held panel — without this a
  // failed Dismiss is a silent dead button (the run stays held with no feedback).
  let workflowRunError = $state<string | null>(null);

  async function stopWorkflowRun(): Promise<void> {
    if (activeWorkflowRun === null) return;
    workflowRunError = null;
    try {
      await cancelRun(activeWorkflowRun.run_id);
    } catch (err) {
      workflowRunError = `Couldn't stop the workflow: ${err instanceof Error ? err.message : String(err)}`;
    }
  }
  async function dismissWorkflowRun(): Promise<void> {
    if (activeWorkflowRun === null) return;
    workflowRunError = null;
    try {
      await abandonRun(projectId, activeWorkflowRun.run_id);
    } catch (err) {
      workflowRunError = `Couldn't dismiss the workflow: ${err instanceof Error ? err.message : String(err)}`;
    }
  }

  async function invokeWorkflowAction(): Promise<void> {
    if (selectedWorkflow === null || invokingWorkflow || !workflowRunnable) return;
    const workflow = selectedWorkflow;
    invokingWorkflow = true;
    clearStatus();
    try {
      // Pane-expand each field's sources to wire refs (agent + owning project);
      // omit empty fields so the map carries only fields the user actually
      // attached a forward to.
      const forwardSources: Record<string, ForwardSourceRef[]> = {};
      for (const [name, sources] of Object.entries(workflowForwardSources)) {
        if (sources.length > 0) forwardSources[name] = expandForwardSources(sources, projectId);
      }
      const runId = await api.invokeWorkflow(
        projectId,
        workflow.name,
        workflow.is_builtin,
        workflowInputs,
        forwardSources,
      );
      // Lock the UI immediately from the confirmed launch (only reached when
      // invoke *succeeded* — a validation/guard failure throws to the catch below
      // and leaves compose up so the user can retry). The optimistic row makes the
      // lockout independent of the follow-up `list_workflow_runs`, whose transient
      // failure must not let compose return while the backend run is live. It
      // carries the *declared* step snapshot; `refreshRuns` upgrades it to the
      // resolved one, and progress events preserve `steps` while advancing
      // step/status (so the row survives even if every refresh fails).
      const steps = workflowForm?.steps ?? [];
      const existing = workflowRuns[projectId] ?? [];
      if (!existing.some((r) => r.run_id === runId)) {
        workflowRuns[projectId] = [
          ...existing,
          {
            run_id: runId,
            workflow: workflow.name,
            step: 0,
            total: steps.length,
            status: "running",
            reason: null,
            steps,
          },
        ];
      }
      // Best-effort upgrade to the authoritative resolved snapshot.
      await refreshRuns(projectId);
      removeWorkflow();
    } catch (err) {
      showError(`Couldn't run workflow: ${err instanceof Error ? err.message : String(err)}`);
    } finally {
      invokingWorkflow = false;
    }
  }

  function handlePrimaryAction(): void {
    if (showStop) {
      for (const [sendId, agentIds] of liveSends) cancelSend(sendId, agentIds);
      return;
    }
    void handleSubmit();
  }

  /// Send this message to a new branch of the recipient — the send button's
  /// second half, and the only way a branch comes into existence.
  ///
  /// Guarded rather than relying on the control being hidden: the keyboard
  /// shortcut reaches this too, and availability can change between keydown and
  /// handling (the parent starts a turn). `showStop` is excluded because the
  /// button is a cancel in that state and the half is not rendered.
  function handleForkSend(): void {
    const attempt = evaluateForkAttempt();
    if (attempt.kind === "nothing-to-send") return;
    if (attempt.kind === "blocked") {
      // The shortcut is live in states where the button is hidden, so this is
      // the only place the reason can reach the user. Never fall through to a
      // normal send: that would message the agent they were branching away from.
      showError(attempt.reason);
      return;
    }
    closeMentionMenu();
    if (mode === "prompt" && selectedPrompt !== null) {
      void dispatchPromptForkSend(attempt.source, selectedPrompt);
      return;
    }
    void dispatchForkSend(attempt.source, draft.trim(), snapshotAttachments());
  }

  /// Manual cross-agent forward (§7). Seeds the held "waiting for {agent}…" entry
  /// (live-UI-only), then awaits the long-lived `forward_message`, which holds
  /// for the sources and returns the **composed body** (it does not dispatch).
  /// On `resolved` the frontend dispatches that body through the normal send path
  /// (`dispatchToRecipients`) — so the forward groups, queues, cancels, and
  /// renders exactly like any send, with the live `message_id → send_id`
  /// correlation intact (no race, no special-casing) — carrying the staged
  /// `attachments`, because a forward is still a send and the user's files ride it
  /// like any other message. On invalidate/cancel it restores the composer.
  function dispatchForward(
    body: string,
    sources: ForwardSource[],
    attachments: Attachment[],
    targets: AgentRecord[],
  ): void {
    const forwardId = crypto.randomUUID();
    const sendId = crypto.randomUUID();
    const recipients = targets.map((t) => t.id);
    // Capture the project id for the held-forward store calls: the hold can
    // outlive this ComposeBar instance (the user navigates to another project
    // while it waits — the compose bar is `{#key projectId}`-remounted, so this
    // instance is destroyed mid-await). The cleanup below must key the global
    // `heldForwards` store by *this* forward's project, not the reactive
    // `projectId` prop, which no longer resolves to it once the instance is gone
    // — otherwise the held entry is never removed and the "waiting…" row sticks.
    const forwardProjectId = projectId;
    addHeldForward(forwardProjectId, { forwardId, sendId, body, sources, recipients });
    void (async () => {
      try {
        const outcome = await api.forwardMessage(
          body,
          expandForwardSources(sources, projectId),
          forwardId,
          forwardProjectId,
        );
        removeHeldForward(forwardProjectId, forwardId);
        if (outcome.status === "resolved") {
          // Dispatch the composed body as a normal send under this forward's
          // send_id — the user message + responses render and group via the
          // existing machinery. The forward marker is derived from the body's
          // sentinel lines at render time (durable across reload). The body is
          // always complete: any empty source invalidates instead of being
          // silently skipped.
          dispatchToRecipients(outcome.body, attachments, targets, sendId, forwardProjectId);
        } else {
          // invalidated (a source failed/cancelled, or any source resolved with
          // no forwardable text) or the user cancelled — restore the composer.
          restoreForward(body, sources, attachments);
          if (outcome.status === "invalidated") showError(`Forward not sent: ${outcome.reason}`);
        }
      } catch (err) {
        removeHeldForward(forwardProjectId, forwardId);
        showError(`Forward failed: ${err instanceof Error ? err.message : String(err)}`);
        restoreForward(body, sources, attachments);
      }
    })();
  }

  /// Restore a cancelled/invalidated forward's source chips, its typed text, and
  /// its attachment chips — each only when the composer hasn't been touched since
  /// (don't clobber a new draft/attachment the user started while the forward was
  /// holding).
  ///
  /// **Known limitation (deferred):** this runs in the closure of the ComposeBar
  /// instance that submitted the forward. If the user navigates away and back
  /// while the forward is holding, that instance is unmounted on resolve, so the
  /// held entry is still cleaned up but the typed text is **not** restored to the
  /// (remounted) composer — narrow timing edge, small loss of the user's own
  /// un-sent text.
  ///
  /// This is the same root cause as the captured-id fixes elsewhere in the
  /// forward closures (held-store cleanup, the dispatch activity bump): the
  /// forward lifecycle is owned by a `{#key projectId}`-remounted component that
  /// is deliberately destroyed mid-hold, so anything the resolve closure touches
  /// on the instance is suspect. Project-keyed *global* reads were re-pinned to a
  /// captured id; instance-local reads like this restore are merely lost (no
  /// cross-project corruption). The durable fix is to hoist the forward
  /// dispatch/hold lifecycle into the project-scoped store layer (which already
  /// survives remounts — that's why the `heldForwards` store works), so neither
  /// cleanup, activity, nor restore depends on the submitting component being
  /// alive. Deferred until forward-lifecycle code is next touched.
  function restoreForward(body: string, sources: ForwardSource[], attachments: Attachment[]): void {
    for (const source of sources) addForwardSource(source);
    if (draft.trim() === "" && body !== "") {
      draft = body;
    }
    if (attachmentChips.length === 0 && attachments.length > 0) {
      commitChips(restoreChips(attachments));
    }
    persistComposeNow();
  }

  /// Manual forward into a prompt's arguments (§7) — the prompt-composer analogue
  /// of `dispatchForward`. Seeds the held "waiting for {agent}…" entry, then awaits
  /// `forward_prompt`, which holds for every argument's sources, composes each
  /// argument (typed lead + forwarded blocks), renders the prompt, and returns the
  /// **rendered body**. On `resolved` the frontend appends the user's appended text
  /// and dispatches through the normal send path (so it groups/queues/cancels like
  /// any send) — carrying the staged `attachments`, because a prompt forward is a
  /// prompt send (one argument is just forward-sourced), so it carries files like
  /// any prompt send. On invalidate/cancel it restores the prompt composer and its
  /// attachment chips.
  function dispatchForwardPrompt(
    prompt: Prompt,
    typedArgs: Record<string, string>,
    appended: string,
    argSources: Record<string, ForwardSource[]>,
    appendedSources: ForwardSource[],
    attachments: Attachment[],
    targets: AgentRecord[],
  ): void {
    const forwardId = crypto.randomUUID();
    const sendId = crypto.randomUUID();
    const recipients = targets.map((t) => t.id);
    // Dedupe sources across every argument *and* the appended text for the held
    // entry's "waiting for…" label (one agent/pane can feed several fields).
    const allSources: ForwardSource[] = [];
    for (const list of [...Object.values(argSources), appendedSources]) {
      for (const source of list) {
        if (allSources.some((s) => forwardSourceKey(s) === forwardSourceKey(source))) continue;
        allSources.push(source);
      }
    }
    // Capture the project id for the held-forward store calls — see
    // `dispatchForward`: this hold can outlive the `{#key projectId}`-remounted
    // ComposeBar instance, so the cleanup must key the global store by *this*
    // forward's project, not the now-stale reactive `projectId` prop.
    const forwardProjectId = projectId;
    // body "" — a prompt forward composes server-side (render after fill), so
    // there's no pre-composed body to show; the held row names the prompt
    // instead so the wait isn't entirely content-free.
    addHeldForward(forwardProjectId, {
      forwardId,
      sendId,
      body: "",
      sources: allSources,
      recipients,
      promptName: promptDisplayName(prompt),
    });
    const forwardArgs: api.ForwardArg[] = prompt.arguments
      .filter((a) => (argSources[a.name]?.length ?? 0) > 0)
      .map((a) => ({
        name: a.name,
        sources: expandForwardSources(argSources[a.name] ?? [], projectId),
        required: a.required,
      }));
    void (async () => {
      try {
        // The backend renders the prompt, composes the appended text (typed +
        // forwarded blocks), and returns the **already-combined** body — so the
        // appended sources resolve in the same hold (one invalidation domain) and
        // the frontend dispatches verbatim, no client-side combine.
        const outcome = await api.forwardPrompt(
          prompt.provider,
          prompt.name,
          buildRenderArgs(prompt, typedArgs),
          forwardArgs,
          appended,
          expandForwardSources(appendedSources, projectId),
          forwardId,
          forwardProjectId,
        );
        removeHeldForward(forwardProjectId, forwardId);
        if (outcome.status === "resolved") {
          dispatchToRecipients(outcome.body, attachments, targets, sendId, forwardProjectId);
        } else {
          restoreForwardPrompt(
            prompt,
            typedArgs,
            appended,
            argSources,
            appendedSources,
            attachments,
          );
          if (outcome.status === "invalidated") showError(`Forward not sent: ${outcome.reason}`);
        }
      } catch (err) {
        removeHeldForward(forwardProjectId, forwardId);
        showError(`Forward failed: ${err instanceof Error ? err.message : String(err)}`);
        restoreForwardPrompt(prompt, typedArgs, appended, argSources, appendedSources, attachments);
      }
    })();
  }

  /// Restore a cancelled/invalidated prompt forward — but only into a pristine
  /// plain composer, so a new prompt, draft, or attachment the user started while
  /// the forward was holding is never clobbered. Same deferred navigate-away
  /// limitation as `restoreForward`. The attachment chips are rebuilt from the
  /// snapshot (the staged files persist on disk), preserving their original
  /// labels.
  function restoreForwardPrompt(
    prompt: Prompt,
    typedArgs: Record<string, string>,
    appended: string,
    argSources: Record<string, ForwardSource[]>,
    appendedSources: ForwardSource[],
    attachments: Attachment[],
  ): void {
    if (
      mode !== "plain" ||
      selectedPrompt !== null ||
      draft.trim() !== "" ||
      attachmentChips.length > 0
    )
      return;
    selectedPrompt = prompt;
    promptArgs = { ...typedArgs };
    promptArgSources = { ...argSources };
    promptAppendedSources = [...appendedSources];
    appendedText = appended;
    commitChips(restoreChips(attachments));
    mode = "prompt";
    persistComposeNow();
  }

  /// Dispatch `text` to `targets` under one send_id. Shared by the plain, prompt,
  /// and forward paths — the prompt path renders first, then calls this with the
  /// finished text and the recipients captured at click time (so toggling chips
  /// mid-render can't redirect the send); the forward path passes the
  /// backend-composed body and its own `sendId` (so it can key the forward
  /// caption to the dispatched send).
  ///
  /// `dispatchProjectId` is the project this send belongs to, passed explicitly
  /// rather than read from the `projectId` prop: the forward paths call this from
  /// a closure that can outlive the `{#key projectId}`-remounted instance (the
  /// user navigates away mid-hold), so the ambient prop may no longer point at the
  /// submitting project — see `dispatchForward`. The live submit paths pass the
  /// prop, which is correct there.
  function dispatchToRecipients(
    text: string,
    attachments: Attachment[],
    targets: AgentRecord[],
    sendId: string = crypto.randomUUID(),
    dispatchProjectId: ProjectId = projectId,
  ): void {
    // Bump this project's local last-activity so it sorts/reads as active right
    // away, before any turn event round-trips. Once per send action.
    recordProjectsActivityLocally([dispatchProjectId], currentIsoTimestamp());
    // Announce the send to this project's transcript so it follows the
    // response, once per send action rather than per recipient.
    noteLocalSend(dispatchProjectId, sendId);
    // Register the whole recipient set *before* any IPC call, so one recipient's
    // rejection can't erase an agent that was supposed to be in the send — and so
    // the completion tracker can join sends queued onto the same busy agents into
    // one queue-drained notification rather than notifying between turns. Only
    // sends dispatched here are registered, which is what keeps workflow steps
    // from notifying individually.
    registerSend(
      sendId,
      dispatchProjectId,
      projects.list.find((p) => p.id === dispatchProjectId)?.name ?? "Switchboard",
      targets.map((a) => ({ id: a.id, name: a.name })),
    );
    for (const agent of targets) {
      const userTurnId = crypto.randomUUID();
      // Every recipient gets the SAME snapshotted attachment list (one shared
      // staged file per attachment), so hydration groups the fan-out's chips
      // once and no recipient can drift to a different set.
      dispatchUserTurn(agent.id, userTurnId, text, attachments, sendId);
      // Per-recipient, fire-and-forget: an idle recipient starts immediately, a
      // busy one queues. A single recipient's IPC failure fails only its bubble.
      void (async () => {
        try {
          const messageId = await api.sendMessage(agent.id, text, sendId, attachments);
          recordSendAccepted(agent.id, userTurnId, messageId);
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          showError(`Send failed: ${message}`);
          failSendStart(agent.id, userTurnId, { message, kind: "adapter_failure" });
        }
      })();
    }
  }

  /// Branch `source` and send this message as the branch's first turn.
  ///
  /// Registration must land *before* the send: the branch's session does not
  /// exist until a turn dispatches into it, so there is no agent to send to
  /// until `forkAgentIntoOwnPane` resolves. That await is also why this is the
  /// one send path that can fail before any optimistic turn exists — a fork
  /// rejection (parent busy, no session, wrong harness) leaves the compose bar
  /// exactly as the user left it, text and all, so they can retry after
  /// waiting or cancelling.
  ///
  /// Once registered, the message goes through the **normal** send path: same
  /// journaling, same queueing, same dispatch. The branch is an ordinary agent
  /// from that moment on.
  /// Give back a send that never dispatched, without destroying anything newer.
  ///
  /// **Merges against the store, not this instance's locals.** Two things can
  /// have moved on during the await: the user can type here (the textarea is not
  /// disabled — typing while a fork registers is expected), and a project switch
  /// can destroy this bar and mount a replacement for the same project. Assigning
  /// `draft = text` handled neither: the first silently discarded whichever
  /// message the user didn't get back, and the second wrote a dead instance's
  /// state over a live one. Reading the store, merging, and writing back is
  /// lossless in both cases regardless of which instance is running this.
  function restoreCapturedSend(text: string, carried: AttachmentChip[]): void {
    const stored = getCompose(projectId);
    const current = stored.content.kind === "plain" ? stored.content.draft : "";
    const merged = current.trim() === "" ? text : `${text}\n\n${current}`;
    setContent(projectId, { kind: "plain", draft: merged });
    // Union by id: the sent chips come back, anything staged since survives, and
    // nothing is duplicated.
    const storedAttachments = stored.attachments ?? [];
    const seen = new Set(storedAttachments.map((a) => a.path));
    setAttachments(projectId, [
      ...carried.filter((chip) => !seen.has(chip.path)),
      ...storedAttachments,
    ]);
    if (!unmounted) {
      draft = merged;
      commitChips([...carried.filter((chip) => !seen.has(chip.path)), ...attachmentChips]);
    }
    flush();
    // A bar mounted since this send started reads the store once and pushes its
    // own locals down, so the write above is invisible to it without this — the
    // message would come back only on the *next* mount.
    if (unmounted) markComposerConsumed(projectId);
  }

  async function dispatchForkSend(
    source: AgentRecord,
    text: string,
    attachments: Attachment[],
  ): Promise<void> {
    // **Single-flight, project-scoped.** Two submits across this await would each
    // register a branch and each dispatch the same text — two agents, the message
    // sent twice, quota spent twice. Claiming rather than setting a local flag is
    // what makes that hold across a project switch: a replacement bar reads its
    // busy state from the claim, so it cannot start a second operation of any
    // kind while this one registers.
    const opId = beginOperation(projectId, { kind: "plain_fork", sourceId: source.id });
    if (opId === null) return;
    // Clear any error from a previous send/forward, as `handleSubmit` does —
    // otherwise a stale failure sits on screen through a successful fork.
    clearStatus();
    // **Clear before the await, not after.** This is the only send path with an
    // await between submit and dispatch, and everything downstream of that await
    // is a hazard: a project switch destroys this bar mid-flight, and `onDestroy`
    // flushes whatever the compose state holds at that moment. Clearing
    // afterwards means the flush persists the message the user already sent, and
    // they return to find it sitting in the box addressed to the parent. Guarding
    // the post-await writes instead just makes that permanent. Clearing up front
    // removes the window: the flush persists cleared state, and anything typed
    // during the await is new content this send never captured.
    const carried = attachmentChips;
    draft = "";
    commitChips([]);
    persistComposeNow();
    const capturedRecipients = [...selectedIds];
    // The claim is held through the whole operation, not just the await: the
    // selection swap, the restore, and the dispatch below are the part a second
    // operation must not interleave with.
    let outcome: { message: string; tone: "error" | "notice" } | undefined;
    try {
      let created: ReachableFork;
      try {
        created = await createReachableFork(source.id);
      } catch (err) {
        outcome = {
          message: `Fork failed: ${err instanceof Error ? err.message : String(err)}`,
          tone: "error",
        };
        restoreCapturedSend(text, carried);
        return;
      }
      selectForkIfRecipientsUnchanged(capturedRecipients, created.fork.id);
      // **Committed is not the same as reachable.** Hand the message back — the
      // branch stays visible with its retry, and the next send materializes it,
      // which is the ordinary self-healing path for a fork whose first turn never
      // ran.
      if (created.kind === "unsubscribed") {
        outcome = { message: created.message, tone: "error" };
        restoreCapturedSend(text, carried);
        return;
      }
      // Otherwise the send always completes, even if this bar is gone. Abandoning
      // the first message would leave a promptless fork — the one state this
      // design exists to make impossible, since Claude refuses a promptless fork
      // and the branch would never materialize.
      dispatchToRecipients(text, attachments, [created.fork]);
      sendGeneration += 1;
    } finally {
      // Published through the claim so a failure raised after this bar is gone
      // still reaches whichever composer the user is looking at.
      finishOperation(projectId, opId, outcome);
    }
  }

  /// Move the composer onto the branch, unless the user has moved on.
  ///
  /// Selection normally follows the branch: the conversation continues there, and
  /// leaving the parent selected would send the next message — including the
  /// retry after an unreachable branch — to the agent they just branched away
  /// from. But this runs after an await, so the composer it retargets may be a
  /// *replacement* instance for the same project whose recipients the user has
  /// since chosen deliberately. Overwriting those would silently redirect their
  /// next message to an agent they never picked.
  ///
  /// **Both copies, not just the live one.** The persisted selection is normally
  /// synced by a scheduled `$effect` that only runs while a bar is mounted — so a
  /// fork completing while the user is in another project moved the live store
  /// and left the saved one naming the parent. Remounting then *actively* wrote
  /// that stale value back over the live one (`initialSelection` seeds from the
  /// snapshot at mount), so the user returned to a composer addressed at the
  /// agent they had just branched away from, and their next message went there.
  function selectForkIfRecipientsUnchanged(captured: AgentId[], forkId: AgentId): void {
    if (!recipientsUnchanged(captured)) return;
    setSelectedIds([forkId]);
    setSelection(projectId, [forkId]);
    flush();
  }

  /// Publish a status. The two channels supersede each other: a composer showing
  /// a stale "Already sending" above a fresh "Sent to alice-fork" is describing
  /// two states at once. Every status write goes through these.
  function showError(message: string | null): void {
    sendError = message;
    sendNotice = null;
  }

  function showNotice(message: string | null): void {
    sendNotice = message;
    sendError = null;
  }

  function clearStatus(): void {
    sendError = null;
    sendNotice = null;
  }

  /// Whether both the composed content and the live recipient set still match
  /// what a send captured — asked where a fork's *precondition* must still hold
  /// (a single, unchanged recipient), not where consumed content is retired.
  function composeUnchangedSince(snapshot: ComposeSnapshot): boolean {
    return (
      composeContentMatches(projectId, snapshot) && recipientsUnchanged(snapshot.selectedIds ?? [])
    );
  }

  /// Recipients come from `recipientSelection`, not the persisted copy in the
  /// compose snapshot: that copy is written by a *scheduled* effect and lags a
  /// selection change by a frame, which would report "unchanged" for a change
  /// that has already happened.
  function recipientsUnchanged(captured: AgentId[]): boolean {
    const current = selectionFor(projectId);
    if (current.length !== captured.length) return false;
    return !current.some((id, i) => id !== captured[i]);
  }

  /// Return the composer to plain mode after a prompt send. Shared by the
  /// ordinary send and the fork so the two cannot drift into clearing different
  /// subsets — a stale forward set or a leftover argument resurfacing on the next
  /// send is invisible until it rides a message the user didn't mean to send.
  function clearPromptComposer(): void {
    selectedPrompt = null;
    promptArgs = {};
    promptArgSources = {};
    promptAppendedSources = [];
    // A completed send is a fresh start: drop any plain-mode forward set that was
    // hidden during prompt mode, so it can't silently resurface on a later send.
    forwardSources = [];
    focusPromptFieldOnMount = false;
    appendedText = "";
    draft = "";
    // Chips clear optimistically with the text (the optimistic user turn already
    // renders them); the staged files persist on disk for the send.
    commitChips([]);
    mode = "plain";
    persistComposeNow();
  }

  /// Retire the compose state a dispatched send consumed — but only if it is
  /// still exactly what that send captured.
  ///
  /// A prompt fork can outlive its own ComposeBar (a project switch remounts the
  /// bar; the continuation runs on regardless), so "clear the composer" cannot
  /// mean "assign my locals." It means: compare the *store* against the captured
  /// snapshot, and clear only on an exact match. A match proves the composer on
  /// screen is still the one that submitted; any difference means a remounted or
  /// edited composer owns that slot and must be left alone.
  ///
  /// `markComposerConsumed` is the other half. Clearing the store is invisible to
  /// an already-mounted composer — it read the store once at mount and pushes its
  /// locals *down* — so without a signal it would keep displaying, and then
  /// re-persist, a prompt that has already been sent.
  function retireConsumedCompose(snapshot: ComposeSnapshot): boolean {
    // **Content, not recipients.** Three different questions get asked about a
    // snapshot and they need three different comparisons. Whether to *clear* asks
    // only whether the composed message is still the one that was consumed —
    // recipients are sticky across sends and are reconciled separately. Folding
    // them in here means adding a recipient during the sign-in window leaves the
    // just-sent prompt sitting in the composer, ready to be sent twice.
    if (!composeContentMatches(projectId, snapshot)) return false;
    if (unmounted) {
      clearCompose(projectId);
      flush();
    } else {
      clearPromptComposer();
    }
    markComposerConsumed(projectId);
    return true;
  }

  /// Render `prompt`, branch `source`, and send the rendered text as the branch's
  /// first turn.
  ///
  /// **Render first.** A fork is a registry append, so branching before the render
  /// and then failing it puts a visibly empty agent in the roster that received
  /// nothing. Rendering first means a render failure — including a refused or
  /// abandoned MCP sign-in — leaves no durable trace at all.
  ///
  /// **Nothing is cleared until the send has dispatched.** The prompt stays intact
  /// (and visibly busy) across both awaits, so every failure path preserves it by
  /// doing nothing at all — no restore, and no window in which a rejected fork can
  /// cost the user a filled-in prompt.
  ///
  /// **Divergence means different things on either side of registration.** Before
  /// it, nothing is committed, so a composer that changed under the render aborts:
  /// dispatching text the user has since replaced would create a branch carrying
  /// content they abandoned. After it, the branch exists and dispatch is
  /// mandatory — abandoning it leaves the promptless fork this whole design exists
  /// to prevent — so the send goes and the newer composer is preserved instead.
  async function dispatchPromptForkSend(source: AgentRecord, prompt: Prompt): Promise<void> {
    // The whole composer, not just the prompt: an attachment staged or a forward
    // configured since submit has to count as divergence, or finalizing would
    // discard it.
    const snapshot: ComposeSnapshot = {
      content: currentContent(),
      selectedIds: [...selectedIds],
      attachments: snapshotAttachments(),
      forwards: currentForwards(),
    };
    const renderArgs = buildRenderArgs(prompt, promptArgs);
    const attachments = snapshot.attachments ?? [];

    // One claim per project, held by whichever send path awaits. A replacement bar
    // reads its busy state from this, so coming back mid-fork cannot submit a
    // second one.
    const opId = beginOperation(projectId, { kind: "prompt_fork", sourceId: source.id });
    if (opId === null) return;
    clearStatus();
    clearOutcome(projectId);
    promptMenuOpen = false;
    closeMentionMenu();
    let outcome: { message: string; tone: "error" | "notice" } | undefined;
    let abandoned = false;
    try {
      let finalText: string;
      let signedInMidSend = false;
      try {
        let rendered = await api.renderPrompt(prompt.provider, prompt.name, renderArgs);
        if (!ownsOperation(projectId, opId)) {
          abandoned = true;
          return;
        }
        if (rendered.kind === "needs_sign_in") {
          // The wait is unbounded — the backend's credential commit is
          // deliberately un-timed — so the composer offers a way out of it. The
          // phase change also unfreezes targeting, which must not span the wait.
          setOperationPhase(projectId, opId, {
            name: "awaiting_user",
            provider: rendered.provider,
          });
          await api.signInMcpProvider(rendered.provider);
          if (!ownsOperation(projectId, opId)) {
            abandoned = true;
            return;
          }
          setOperationPhase(projectId, opId, { name: "rendering" });
          signedInMidSend = true;
          rendered = await api.renderPrompt(prompt.provider, prompt.name, renderArgs);
          if (!ownsOperation(projectId, opId)) {
            abandoned = true;
            return;
          }
        }
        if (rendered.kind !== "rendered") {
          outcome = {
            message: `Fork not sent: MCP provider "${prompt.provider}" needs sign-in.`,
            tone: "error",
          };
          return;
        }
        finalText = combinePromptMessage(rendered.text, promptAppendedOf(snapshot));
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        outcome = {
          message: signedInMidSend
            ? `Signed in, but the fork then failed: ${message}`
            : `Fork failed: ${message}`,
          tone: "error",
        };
        return;
      }

      // ---- Pre-registration divergence: abort. Nothing durable exists yet. ----
      if (!composeUnchangedSince(snapshot)) {
        outcome = {
          message: `Fork not sent: the composer changed while "${prompt.name}" was rendering. Press Fork again when you're ready.`,
          tone: "notice",
        };
        return;
      }
      // Re-ask the busy-parent question. It was answered at keypress, and the
      // sign-in detour makes that answer minutes old — a branch taken mid-turn
      // permanently inherits a placeholder instead of the parent's real answer.
      // The backend re-checks too, so a miss here refuses rather than corrupts;
      // this is what turns that refusal into a sentence.
      if (!unmounted && forkBlock !== null) {
        outcome = { message: forkBlock, tone: "error" };
        return;
      }
      // Claude refuses a promptless fork, so an empty render would fail at the
      // harness with the branch already committed. Judged on the combined
      // transport text, not the renderer's output alone.
      if (finalText.trim() === "" && attachments.length === 0) {
        outcome = {
          message: `Fork not sent: "${prompt.name}" rendered to an empty message.`,
          tone: "error",
        };
        return;
      }

      setOperationPhase(projectId, opId, { name: "registering" });
      let created: ReachableFork;
      try {
        created = await createReachableFork(source.id);
      } catch (err) {
        // The prompt was never cleared, so there is nothing to hand back.
        const message = err instanceof Error ? err.message : String(err);
        outcome = { message: `Fork failed: ${message}`, tone: "error" };
        return;
      }
      if (created.kind === "unsubscribed") {
        selectForkIfRecipientsUnchanged(snapshot.selectedIds ?? [], created.fork.id);
        outcome = { message: created.message, tone: "error" };
        return;
      }
      // Committed and reachable: the send is no longer optional.
      dispatchToRecipients(finalText, attachments, [created.fork]);
      const retired = retireConsumedCompose(snapshot);
      if (retired) {
        selectForkIfRecipientsUnchanged(snapshot.selectedIds ?? [], created.fork.id);
        sendGeneration += 1;
      } else {
        outcome = {
          message: `Sent to ${created.fork.name} — your newer draft and recipients were kept.`,
          tone: "notice",
        };
      }
    } finally {
      // An abandoned operation no longer owns the slot; publishing here would
      // stamp a stale message over whatever now does.
      if (!abandoned) finishOperation(projectId, opId, outcome);
    }
  }

  /// The appended free text a snapshot captured, or `""` when it wasn't prompt
  /// content. Read from the snapshot rather than live state so a mid-render edit
  /// cannot change what gets combined.
  function promptAppendedOf(snapshot: ComposeSnapshot): string {
    return snapshot.content.kind === "prompt" ? snapshot.content.appendedText : "";
  }

  async function handleSubmit(): Promise<void> {
    if (sendDisabled) return;
    clearStatus();
    // Snapshot the whole chip set once, up front (before any await), so a
    // mid-render chip edit can't change what gets sent — same discipline as the
    // prompt/recipient snapshots below.
    const attachments = snapshotAttachments();

    if (mode === "prompt" && selectedPrompt !== null) {
      // Render once, before any optimistic turn or journal write: a render
      // failure must leave no phantom user turn for text that was never sent.
      // Snapshot everything the send depends on *before* the await — the prompt,
      // its args, the appended text, and the recipients — so mid-render edits
      // can't change what (or who) gets sent.
      const prompt = selectedPrompt;
      const renderArgs = buildRenderArgs(prompt, promptArgs);
      const appended = appendedText;
      const targets = [...selectedAgents];

      // A prompt with ≥1 forwarded argument *or* a forwarded appended text goes
      // through the held forward-prompt path (resolved + rendered server-side once
      // the sources settle), not the immediate render below. Clears back to the
      // plain composer right away — the held entry owns the rest; restore re-enters
      // prompt mode if it fails. Attachments ride the forward (it's a prompt send)
      // and clear optimistically like the prompt/args; restore rebuilds them.
      const anyArgForwarded = prompt.arguments.some(
        (a) => (promptArgSources[a.name]?.length ?? 0) > 0,
      );
      if (anyArgForwarded || promptAppendedSources.length > 0) {
        promptMenuOpen = false;
        closeMentionMenu();
        dispatchForwardPrompt(
          prompt,
          { ...promptArgs },
          appended,
          { ...promptArgSources },
          [...promptAppendedSources],
          attachments,
          targets,
        );
        clearPromptComposer();
        sendGeneration += 1;
        return;
      }

      promptMenuOpen = false;
      closeMentionMenu();
      // **The claim is project-scoped, not component-scoped.** This render can
      // open a browser for a sign-in and wait, so the send routinely outlives the
      // bar that started it — and a replacement bar starts with `sending` false,
      // showing the same prompt with an enabled Send. Submitting again sent it
      // twice; pressing Fork instead sent it to the parent *and* to a new branch,
      // which is one action producing two sends to two agents.
      const claim = beginOperation(projectId, { kind: "prompt_send" });
      if (claim === null) return;
      // What this send owns, captured before its first await so the clear at the
      // end can tell "still mine" from "a replacement composer's".
      const snapshot: ComposeSnapshot = {
        content: currentContent(),
        selectedIds: [...selectedIds],
        attachments,
        forwards: currentForwards(),
      };
      // Targeting is frozen for the render window by the operation's `rendering`
      // phase: the post-render check below aborts the send if a captured
      // recipient left the set, so a pane gesture landing mid-render would refuse
      // it for no reason the user caused. Raw selection writes (pruning a removed
      // agent) still pass — a removed recipient SHOULD trigger that abort.
      let finalText: string;
      let signedInMidSend = false;
      let outcomeForClaim: { message: string; tone: "error" | "notice" } | undefined;
      let abandoned = false;
      try {
        try {
          let outcome = await api.renderPrompt(prompt.provider, prompt.name, renderArgs);
          if (!ownsOperation(projectId, claim)) {
            abandoned = true;
            return;
          }
          if (outcome.kind === "needs_sign_in") {
            // The provider needs a browser sign-in, and the user's intent could
            // not be clearer — they just pressed Send on its prompt. Launch the
            // sign-in and continue the send once they approve in the browser.
            // The wait is unbounded, so the composer offers a way out of it. The
            // phase change also unfreezes targeting for its duration.
            setOperationPhase(projectId, claim, {
              name: "awaiting_user",
              provider: outcome.provider,
            });
            await api.signInMcpProvider(outcome.provider);
            if (!ownsOperation(projectId, claim)) {
              abandoned = true;
              return;
            }
            setOperationPhase(projectId, claim, { name: "rendering" });
            signedInMidSend = true;
            outcome = await api.renderPrompt(prompt.provider, prompt.name, renderArgs);
            if (!ownsOperation(projectId, claim)) {
              abandoned = true;
              return;
            }
          }
          if (outcome.kind !== "rendered") {
            // A second needs-sign-in, or an outcome kind this build doesn't
            // know: stop — never loop the browser open.
            outcomeForClaim = {
              message: `Send failed: MCP provider "${prompt.provider}" needs sign-in.`,
              tone: "error",
            };
            return;
          }
          finalText = combinePromptMessage(outcome.text, appended);
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          // After a successful mid-send sign-in, a failure comes from the retry
          // (typically the server) — say the sign-in itself stuck, or the user
          // is left guessing whether their browser approval was wasted.
          outcomeForClaim = {
            message: signedInMidSend
              ? `Signed in, but the send then failed: ${message}`
              : `Send failed: ${message}`,
            tone: "error",
          };
          return;
        }
        // If the composer state changed while the message was being built,
        // avoid dispatching text into a now-different prompt/recipient context.
        const stillSelected = new Set(selectedIds);
        if (selectedPrompt !== prompt || targets.some((t) => !stillSelected.has(t.id))) {
          // **Always say something.** A send the UI accepted that then vanishes
          // with no trace is worse than one that refuses: the user has no way to
          // tell it from a bug. The prompt and its arguments are still intact
          // wherever they now are.
          outcomeForClaim = {
            message: signedInMidSend
              ? "Signed in — your prompt is ready; press Send when you are."
              : "Not sent: the prompt or its recipients changed while it was being prepared.",
            tone: "notice",
          };
          return;
        }
        dispatchToRecipients(finalText, attachments, targets);
        // Prompt selection is not sticky: a successful send returns to the plain
        // composer (recipients stay selected). Appended text is consumed, not
        // carried back. Retired under compare-and-set for the same reason the fork
        // path is: this can run after a project switch destroyed the bar, and
        // clearing unconditionally then wrote a dead instance's emptied locals
        // over a replacement composer's real content.
        if (retireConsumedCompose(snapshot)) sendGeneration += 1;
        return;
      } finally {
        // Published here so a failure or notice raised after this bar is gone
        // still reaches whichever composer the user is looking at — unless the
        // user abandoned the wait, in which case the slot is someone else's now.
        if (!abandoned) finishOperation(projectId, claim, outcomeForClaim);
      }
    }

    // A send with ≥1 forward source goes through the cross-agent forward path
    // (held until the sources settle) rather than the normal send. It still
    // carries the staged attachments — a forward is a send, so the user's files
    // ride it like any message; they clear optimistically and restore rebuilds
    // their chips if the forward fails.
    if (forwardSources.length > 0) {
      closeMentionMenu();
      dispatchForward(draft.trim(), [...forwardSources], attachments, [...selectedAgents]);
      draft = "";
      forwardSources = [];
      commitChips([]);
      sendGeneration += 1;
      persistComposeNow();
      return;
    }

    dispatchToRecipients(draft.trim(), attachments, [...selectedAgents]);
    // The optimistic user turns are now in the transcript; clear for the next
    // message (recipients stay selected — sticky). Chips clear with the text;
    // their staged files persist on disk for the send.
    draft = "";
    commitChips([]);
    sendGeneration += 1;
    persistComposeNow();
  }

  /// Persist the current compose content and forward sources immediately —
  /// `flush()` writes through and cancels the pending debounce — so a send-clear is
  /// durable even if the component unmounts in the same frame (e.g. a project
  /// switch right after sending), and a stale pre-send draft can never land after
  /// the clear.
  ///
  /// Forwards are written synchronously here for the same reason as content: their
  /// persist `$effect` is *scheduled*, so an unmount before it runs would `flush()`
  /// the pre-clear forward set and resurrect it on the next mount. In the ordinary
  /// path the effect plus `onDestroy`'s flush already cover this, and the window is
  /// too narrow to drive from a jsdom test — so treat this as symmetry with the
  /// content path, not a test-pinned guarantee.
  /// Re-read the store after a background send retired the content it consumed.
  ///
  /// A ComposeBar reads the store **once**, at mount (`untrack(getCompose)`), and
  /// from then on pushes its locals *down* into it. So a send that clears the
  /// store from a continuation — possibly one started by an instance that no
  /// longer exists — is invisible to whichever bar is mounted: it keeps showing
  /// the prompt that was already sent, and its own persist effects write that
  /// prompt straight back over the clear. This is the signal that closes that
  /// gap; the clear itself only happens when the store still matched, so there is
  /// never newer work here to lose.
  let seenConsumed = untrack(() => composerConsumedCount(projectId));
  $effect(() => {
    const count = composerConsumedCount(projectId);
    if (count === seenConsumed) return;
    seenConsumed = count;
    untrack(() => reprojectFromStore());
  });

  /// Re-read the store into this bar's locals. Covers both directions a
  /// continuation can write: retiring content it consumed, and handing back a
  /// send that never dispatched. Only plain content is projected — the paths that
  /// signal are the ones that write plain content, and resolving a prompt would
  /// need the (async) prompt cache.
  function reprojectFromStore(): void {
    const stored = getCompose(projectId);
    selectedPrompt = null;
    promptArgs = {};
    promptArgSources = {};
    promptAppendedSources = [];
    forwardSources = stored.forwards?.message ?? [];
    focusPromptFieldOnMount = false;
    appendedText = "";
    draft = stored.content.kind === "plain" ? stored.content.draft : "";
    attachmentChips = restoreChips(stored.attachments ?? []);
    mode = "plain";
  }

  function persistComposeNow(): void {
    setContent(projectId, currentContent());
    setForwards(projectId, currentForwards());
    flush();
  }
</script>

{#snippet ForkIcon()}
  <!-- git-branch: a branch line diverging from a trunk. -->
  <svg
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="2"
    stroke-linecap="round"
    stroke-linejoin="round"
    class="size-3.5 shrink-0"
    aria-hidden="true"
  >
    <line x1="6" y1="3" x2="6" y2="15" />
    <circle cx="18" cy="6" r="3" />
    <circle cx="6" cy="18" r="3" />
    <path d="M18 9a9 9 0 0 1-9 9" />
  </svg>
{/snippet}

<!-- Split-rect pane glyph, shared by the @-menu's "Send to" and "Forward from"
     pane rows so both sections mark pane entries identically. -->
{#snippet paneGlyph()}
  <svg
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="1.8"
    stroke-linecap="round"
    stroke-linejoin="round"
    class="text-accent h-4 w-4 shrink-0"
    aria-hidden="true"
    data-testid="pane-glyph"
  >
    <rect x="3" y="4" width="18" height="16" rx="2" />
    <path d="M12 4v16" />
  </svg>
{/snippet}

<div class="bg-raised px-4 pt-2 pb-4" bind:this={composeEl}>
  {#if activeWorkflowRun}
    <!-- A workflow occupies this project: the live progress view *replaces* the
         compose box (not merely disables it), so queueing a message mid-run is
         structurally impossible. A `running` run shows progress with a Stop; a
         `failed`/`interrupted` run is held with a Dismiss until abandoned. -->
    <div
      class="border-border bg-raised rounded-xl border p-3 shadow-[0_10px_32px_rgba(0,0,0,0.08)]"
      data-testid="workflow-run-live"
      data-run-status={activeWorkflowRun.status}
    >
      <div class="mb-2 flex items-center justify-between gap-2">
        <span class="text-fg min-w-0 truncate text-sm font-semibold"
          >{activeWorkflowRun.workflow}</span
        >
        {#if activeWorkflowRun.status === "running"}
          <button
            type="button"
            data-testid="workflow-run-stop"
            onclick={() => void stopWorkflowRun()}
            aria-label="Stop workflow"
            class="text-muted hover:bg-status-failed-soft/70 hover:text-status-failed focus-visible:ring-focus inline-flex h-7 shrink-0 items-center gap-1 rounded-full px-2 text-xs transition-colors focus-visible:ring-1 focus-visible:outline-none"
          >
            <StopIcon class="size-4" />
            Stop
          </button>
        {:else}
          <button
            type="button"
            data-testid="workflow-run-dismiss"
            onclick={() => void dismissWorkflowRun()}
            class="text-muted hover:bg-panel hover:text-fg focus-visible:ring-focus inline-flex h-7 shrink-0 items-center rounded-full px-2.5 text-xs transition-colors focus-visible:ring-1 focus-visible:outline-none"
          >
            Dismiss
          </button>
        {/if}
      </div>
      {#if activeWorkflowRun.steps.length > 0}
        <WorkflowSteps
          steps={activeWorkflowRun.steps}
          mode="live"
          current={activeWorkflowRun.step}
          status={activeWorkflowRun.status}
          reason={activeWorkflowRun.reason}
        />
      {:else}
        <!-- Steps absent (legacy run file, or a brief pre-refresh window): fall back
             to a count line so the view is never empty. -->
        <p class="text-muted text-sm" data-testid="workflow-run-fallback">
          Step {activeWorkflowRun.step + 1} of {activeWorkflowRun.total}{#if activeWorkflowRun.status !== "running"}
            · {activeWorkflowRun.status}{/if}
        </p>
        {#if activeWorkflowRun.reason}
          <p class="text-status-failed mt-1 text-xs">{activeWorkflowRun.reason}</p>
        {/if}
      {/if}
      {#if workflowRunError}
        <p class="text-status-failed mt-2 text-xs" data-testid="workflow-run-error">
          {workflowRunError}
        </p>
      {/if}
    </div>
  {:else}
    <div
      class={cn(
        "border-border bg-raised relative rounded-xl border p-2.5 shadow-[0_10px_32px_rgba(0,0,0,0.08)] transition-colors",
        dragOver ? "border-accent" : composeFocused ? "border-focus" : "",
      )}
      data-testid="compose-box"
      data-drag-over={dragOver}
      onfocusin={() => (composeFocused = true)}
      onfocusout={(e) => {
        // Keep the ring while focus moves *between* children of the box (field →
        // button → chip); only clear when it genuinely leaves the container.
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) composeFocused = false;
      }}
    >
      {#if promptMenuOpen}
        <!-- Full compose-box width, floating just above the box (anchored to its
           top edge, opening upward so a long list is never cut off). -->
        <PromptMenu
          {prompts}
          loading={!promptsLoaded}
          onpick={pickPrompt}
          oninsert={promptMenuAllowsLiteralInsert ? setEmptyDraftFromPromptSearch : undefined}
          oncopy={copyPrompt}
          onsync={() => void syncPromptMenu()}
          syncing={promptMenuSyncing}
          onconfigure={onConfigurePrompts ? configurePrompts : undefined}
          onopenfolder={openPromptsFolder}
          onclose={() => (promptMenuOpen = false)}
        />
      {/if}
      {#if workflowMenuOpen}
        <WorkflowMenu
          {workflows}
          loading={!workflowsLoaded}
          onpick={pickWorkflow}
          oncopy={copyWorkflow}
          oncopyauthoringprompt={copyWorkflowAuthoringPrompt}
          onopenfolder={openWorkflowsFolder}
          onclose={() => (workflowMenuOpen = false)}
        />
      {/if}
      {#if menuOpen && hasMenuContent}
        <!-- Full compose-box width, matching the prompt menu's placement. The
           menu opens upward from the compose box instead of following the @
           caret, which keeps file paths readable without side tooltips. -->
        <div
          class="border-border/90 bg-raised absolute inset-x-0 bottom-full z-20 mb-1 overflow-hidden rounded-lg border p-1 text-[13px] shadow-[0_10px_28px_rgba(0,0,0,0.10)]"
          data-testid="recipient-menu"
          role="listbox"
          bind:this={menuEl}
        >
          {#if showFileSection}
            <div
              class="text-muted px-2.5 py-0.5 text-[11px] font-medium tracking-wide uppercase select-none"
            >
              Files
            </div>
          {/if}
          <div class="max-h-48 overflow-y-auto" data-testid="file-options-scroll">
            {#each fileItems as item (item.key)}
              {@const i = menuItems.findIndex((candidate) => candidate.key === item.key)}
              <button
                type="button"
                class={"hover:bg-hover flex w-full cursor-pointer items-start gap-2 rounded-md px-2.5 py-1.5 text-left leading-5 outline-none select-none " +
                  (i === highlighted ? "bg-hover" : "")}
                data-testid={`file-option-${item.path}`}
                role="option"
                aria-selected={i === highlighted}
                onclick={() => pickItem(item)}
              >
                <svg
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.8"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                  class="text-muted h-4 w-4 shrink-0"
                  aria-hidden="true"
                >
                  <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" />
                  <path d="M14 3v5h5" />
                </svg>
                <span class="flex min-w-0 flex-col">
                  <span
                    class="text-fg min-w-0 truncate text-left text-xs font-medium"
                    data-testid="file-option-label">{item.label}</span
                  >
                  {#if item.parent !== null}
                    <span
                      class="text-muted truncate text-left text-[11px]"
                      data-testid="file-option-path"
                    >
                      {item.parent}
                    </span>
                  {/if}
                </span>
              </button>
            {/each}
            {#if fileStatusText !== null}
              <div
                class="text-muted flex min-h-7 items-center px-2.5 py-1 text-left leading-5 select-none"
                data-testid="file-options-status"
              >
                {fileStatusText}
              </div>
            {/if}
          </div>

          {#if attachmentItems.length > 0}
            <div
              class={cn(
                "text-muted px-2.5 py-0.5 text-[11px] font-medium tracking-wide uppercase select-none",
                fileItems.length > 0 ? "mt-1" : "",
              )}
            >
              Attachments
            </div>
            {#each attachmentItems as item (item.key)}
              {@const i = menuItems.findIndex((candidate) => candidate.key === item.key)}
              <button
                type="button"
                class={"hover:bg-hover flex w-full cursor-pointer items-center gap-2 rounded-md px-2.5 py-1 text-left leading-5 outline-none select-none " +
                  (i === highlighted ? "bg-hover" : "")}
                data-testid={`attachment-option-${item.label}`}
                role="option"
                aria-selected={i === highlighted}
                onclick={() => pickItem(item)}
              >
                <svg
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.8"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                  class="text-muted h-4 w-4 shrink-0"
                  aria-hidden="true"
                >
                  <path
                    d="M21.44 11.05 12 20.5a5.5 5.5 0 0 1-7.78-7.78l8.49-8.49a3.5 3.5 0 1 1 4.95 4.95l-8.49 8.49a1.5 1.5 0 0 1-2.12-2.12l7.78-7.78"
                  />
                </svg>
                <span class="text-fg font-mono text-xs">{item.label}</span>
              </button>
            {/each}
          {/if}

          {#if recipientItems.length > 0}
            <div
              class={cn(
                "text-muted px-2.5 py-0.5 text-[11px] font-medium tracking-wide uppercase select-none",
                fileItems.length > 0 || attachmentItems.length > 0 ? "mt-1" : "",
              )}
            >
              Send to
            </div>
          {/if}
          {#each recipientItems as item (item.key)}
            {@const i = menuItems.findIndex((candidate) => candidate.key === item.key)}
            <button
              type="button"
              class={"hover:bg-hover flex w-full cursor-pointer items-center gap-2 rounded-md px-2.5 py-1 text-left leading-5 outline-none select-none " +
                (i === highlighted ? "bg-hover" : "")}
              data-testid={`recipient-option-${item.key}`}
              role="option"
              aria-selected={i === highlighted}
              onclick={() => pickItem(item)}
            >
              {#if item.kind === "all"}
                <svg
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                  class="text-accent h-4 w-4"
                  aria-hidden="true"
                >
                  <circle cx="12" cy="12" r="9" />
                  <path d="m8.5 12 2.5 2.5 4.5-5" />
                </svg>
                <span class="text-fg">All agents</span>
                <span class="text-muted ml-auto font-mono text-[13px]">
                  {shortcut("mod", "shift", "A")}
                </span>
              {:else if item.kind === "clear"}
                <ClearIcon class="text-muted" />
                <span class="text-fg">Clear</span>
                <span class="text-muted ml-auto font-mono text-[13px]">{shortcut("esc")}</span>
              {:else if item.kind === "pane"}
                {@render paneGlyph()}
                <span class="text-fg shrink-0">{item.pane.name}</span>
                <!-- Member names in roster order (matching chip/pane-column
                   order); the menu spans the compose box, so names fit —
                   truncate is just the degenerate-case guard. -->
                <span
                  class="text-muted min-w-0 truncate text-[11px]"
                  data-testid="pane-option-members"
                >
                  {agents
                    .filter((a) => item.pane.members.includes(a.id))
                    .map((a) => a.name)
                    .join(", ")}
                </span>
                {#if item.index < 9}
                  <span class="text-muted ml-auto font-mono text-[13px]">
                    {shortcut("mod", "alt", String(item.index + 1))}
                  </span>
                {/if}
              {:else if item.kind === "agent"}
                {@const agentIndex = agents.findIndex((a) => a.id === item.agent.id)}
                <HarnessIcon harness={item.agent.harness} size="sm" class="h-4 w-4" />
                <span class="text-fg">{item.agent.name}</span>
                {#if agentIndex >= 0 && agentIndex < 9}
                  <span class="text-muted ml-auto font-mono text-[13px]">
                    {shortcut("mod", String(agentIndex + 1))}
                  </span>
                {/if}
              {/if}
            </button>
          {/each}

          {#if forwardItems.length > 0}
            <div
              class={cn(
                "text-muted px-2.5 py-0.5 text-[11px] font-medium tracking-wide uppercase select-none",
                "mt-1",
              )}
            >
              Forward from
            </div>
          {/if}
          {#each forwardItems as item (item.key)}
            {@const i = menuItems.findIndex((candidate) => candidate.key === item.key)}
            {@const spent = emptyForwardKeys.has(item.key)}
            <button
              type="button"
              class={"flex w-full items-center gap-2 rounded-md px-2.5 py-1 text-left leading-5 outline-none select-none " +
                (spent ? "cursor-not-allowed" : "hover:bg-hover cursor-pointer ") +
                (i === highlighted ? "bg-hover" : "")}
              data-testid={`forward-option-${item.key}`}
              role="option"
              disabled={spent}
              aria-selected={i === highlighted}
              onclick={() => {
                // Guarded as well as `disabled`, so the refusal doesn't rest on
                // the browser suppressing clicks on a disabled control.
                if (!spent) pickItem(item);
              }}
            >
              <!-- ↪ forward glyph, shared by both forward entry kinds. -->
              <svg
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
                stroke-linejoin="round"
                class="text-accent h-4 w-4"
                aria-hidden="true"
              >
                <polyline points="15 17 20 12 15 7" />
                <path d="M4 18v-2a4 4 0 0 1 4-4h12" />
              </svg>
              {#if item.kind === "forward-pane"}
                {@const paneIndex = paneLayout.panes.findIndex((p) => p.id === item.pane.id)}
                {@render paneGlyph()}
                <span class="text-fg shrink-0">{item.pane.name}</span>
                <span class="text-muted min-w-0 truncate text-[11px]">
                  {agents
                    .filter((a) => item.pane.members.includes(a.id))
                    .map((a) => a.name)
                    .join(", ")}
                </span>
                {#if paneIndex >= 0 && paneIndex < 9}
                  <span class="text-muted ml-auto shrink-0 pl-2 font-mono text-[11px]"
                    >{shortcut("mod", "ctrl", String(paneIndex + 1))}</span
                  >
                {/if}
              {:else}
                <HarnessIcon harness={item.agent.harness} size="sm" class="h-4 w-4" />
                <span class={spent ? "text-muted/50" : "text-fg"}>{item.agent.name}</span>
                {#if spent}
                  <span class="text-muted ml-auto text-[11px] italic">no output</span>
                {:else if agentReadiness(item.agent.id) === "pending"}
                  <span class="text-muted ml-auto text-[11px] italic">still generating</span>
                {/if}
              {/if}
            </button>
          {/each}
        </div>
      {/if}
      {#snippet forwardSourceChips()}
        {#if forwardSources.length > 0}
          <!-- Plain-mode only: prompt mode forwards per-field, and workflow mode
             routes via its agent inputs, so the message-level forward set doesn't
             apply and is hidden in both (its state is preserved for restore when
             the prompt/workflow is removed). -->
          <div
            class="mb-1.5 flex flex-wrap items-center gap-1.5"
            data-testid="forward-source-chips"
          >
            <span class="text-muted text-xs">Forwarding from</span>
            {#each forwardSources as source (forwardSourceKey(source))}
              <ForwardSourceChip
                {source}
                readiness={sourceReadiness(source)}
                disabled={composerBusy}
                onRemove={() => removeForwardSource(forwardSourceKey(source))}
                currentProjectId={projectId}
              />
            {/each}
            {#if forwardSources.length > 1}
              <!-- Each chip carries its own ✕; the bulk clear (same ⊘ glyph as
                   "Clear recipients") only earns its place once there are several to
                   drop at once. -->
              <Tooltip label="Clear forward sources">
                {#snippet trigger(props)}
                  <button
                    {...props}
                    type="button"
                    class={cn(ICON_BUTTON_CLASS, "ml-0.5 shrink-0 disabled:opacity-50")}
                    data-testid="forward-sources-clear"
                    aria-label="Clear forward sources"
                    disabled={composerBusy}
                    onclick={() => {
                      if (!composerBusy) forwardSources = [];
                    }}
                  >
                    <ClearIcon />
                  </button>
                {/snippet}
              </Tooltip>
            {/if}
          </div>
        {/if}
      {/snippet}
      {#snippet recipientChips()}
        {#if agents.length > 1}
          <div class="flex flex-wrap items-center gap-1.5 text-xs" data-testid="recipient-field">
            <span class="text-muted">To</span>
            {#each agents as agent, i (agent.id)}
              {@const selected = selectedIds.includes(agent.id)}
              <!-- Targeted ∧ hidden — the cue exists for one hazard: sending to
                 an agent whose replies you've hidden. A hidden-but-unselected
                 chip carries no hazard, so it gets no warning. -->
              {@const chipHidden = selected && isAgentHidden(projectId, rosterIds, agent.id)}
              <Tooltip
                label={chipHidden
                  ? `${agent.name} is hidden in its pane — replies won't be visible`
                  : selected
                    ? `Drop ${agent.name}`
                    : `Add ${agent.name}`}
                shortcut={i < 9 ? shortcut("mod", String(i + 1)) : undefined}
                delayDuration={chipHidden ? 300 : 1000}
                reopen="fresh-hover"
              >
                {#snippet trigger(props)}
                  <button
                    {...props}
                    type="button"
                    class={cn(
                      "focus-visible:ring-focus inline-flex h-6 items-center gap-1 rounded-full border px-2 text-xs transition-colors focus-visible:ring-1 focus-visible:outline-none",
                      selected
                        ? "bg-accent-soft text-fg border-transparent"
                        : "border-panel bg-panel text-muted hover:bg-raised hover:text-fg",
                      composerBusy ? "cursor-not-allowed opacity-60" : "",
                    )}
                    data-testid={`recipient-chip-${agent.id}`}
                    data-selected={selected}
                    data-hidden-recipient={chipHidden || undefined}
                    aria-pressed={selected}
                    disabled={composerBusy}
                    onclick={() => toggleRecipient(agent.id)}
                  >
                    {#if i < 9}
                      <!-- Leading position number makes the ⌘1–9 toggle shortcut
                         discoverable at a glance (it maps to chip position, not a
                         fixed agent). -->
                      <span
                        class="text-muted/80 font-mono text-[10px] tabular-nums"
                        aria-hidden="true"
                      >
                        {i + 1}
                      </span>
                    {/if}
                    <HarnessIcon harness={agent.harness} size="sm" class="h-3.5 w-3.5" />
                    {agent.name}
                    {#if chipHidden}
                      <!-- Targeted-but-hidden cue: without it a user sends to a
                         hidden agent and never sees the reply appear. -->
                      <svg
                        viewBox="0 0 24 24"
                        fill="none"
                        stroke="currentColor"
                        stroke-width="2"
                        stroke-linecap="round"
                        stroke-linejoin="round"
                        class="text-warning h-3 w-3 shrink-0"
                        data-testid={`recipient-hidden-cue-${agent.id}`}
                        aria-hidden="true"
                      >
                        <path
                          d="M10.7 5.1a9.6 9.6 0 0 1 1.3-.1c7 0 10 7 10 7a13.2 13.2 0 0 1-1.7 2.5"
                        />
                        <path d="M6.6 6.6A13.5 13.5 0 0 0 2 12s3 7 10 7a9.7 9.7 0 0 0 5.4-1.6" />
                        <path d="m2 2 20 20" />
                      </svg>
                    {/if}
                  </button>
                {/snippet}
              </Tooltip>
            {/each}
            {#if selectedIds.length > 0}
              <Tooltip label="Clear recipients" shortcut={shortcut("esc")}>
                {#snippet trigger(props)}
                  <button
                    {...props}
                    type="button"
                    class={cn(ICON_BUTTON_CLASS, "ml-0.5")}
                    data-testid="recipient-clear"
                    aria-label="Clear recipients"
                    disabled={composerBusy}
                    onclick={() => {
                      if (!composerBusy) setSelectedIds([]);
                    }}
                  >
                    <ClearIcon />
                  </button>
                {/snippet}
              </Tooltip>
            {/if}
          </div>
        {/if}
      {/snippet}

      {#snippet attachmentChipRow()}
        {#if attachmentChips.length > 0}
          <div class="mb-1.5 flex flex-wrap gap-1.5" data-testid="attachment-chips">
            {#each attachmentChips as chip (chip.id)}
              <span
                class="border-border bg-panel text-fg inline-flex max-w-[14rem] items-center gap-1.5 rounded-full border py-px pr-1 pl-2 text-xs"
                data-testid={`attachment-chip-${chip.label}`}
                data-kind={chip.kind}
              >
                <span
                  class="text-muted shrink-0 font-mono text-[10px] whitespace-nowrap"
                  aria-hidden="true">{chip.label}</span
                >
                <Tooltip
                  label={chip.original_name}
                  delayDuration={SUPPLEMENTAL_TOOLTIP_DELAY}
                  focusable={false}
                >
                  {#snippet trigger(props)}
                    <span {...props} class="truncate">{chip.original_name}</span>
                  {/snippet}
                </Tooltip>
                <button
                  type="button"
                  class="text-muted hover:text-fg hover:bg-control-hover flex h-4 w-4 shrink-0 items-center justify-center rounded-full transition-colors disabled:cursor-not-allowed disabled:opacity-50"
                  data-testid={`attachment-chip-remove-${chip.label}`}
                  aria-label={`Remove ${chip.original_name}`}
                  disabled={composerBusy}
                  onclick={() => removeAttachmentChip(chip.id)}
                >
                  <svg
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="2"
                    stroke-linecap="round"
                    class="h-3 w-3"
                    aria-hidden="true"
                  >
                    <path d="m6 6 12 12M18 6 6 18" />
                  </svg>
                </button>
              </span>
            {/each}
          </div>
        {/if}
      {/snippet}

      {#if mode !== "plain"}
        {@render attachmentChipRow()}
      {/if}

      {#if restoring && promptRestoreIssue !== null}
        <div
          class="flex h-16 items-center gap-3 px-1 text-sm"
          data-testid="compose-prompt-restore-failed"
        >
          <span class="text-muted">{promptRestoreMessage()}</span>
          <Button
            size="sm"
            variant="secondary"
            onclick={retryPromptRestore}
            data-testid="prompt-restore-retry"
          >
            Check again
          </Button>
          <Button
            size="sm"
            variant="ghost"
            onclick={discardPromptRestore}
            data-testid="prompt-restore-discard"
          >
            Start over
          </Button>
        </div>
      {:else if restoring && workflowRestoreFailed}
        <!-- The workflow list failed to load, so we can't tell whether the saved
           workflow still exists. The snapshot is held (not discarded) until the
           user retries or explicitly starts over — a transient error must not
           destroy a half-filled invocation. -->
        <div
          class="flex h-16 items-center gap-3 px-1 text-sm"
          data-testid="compose-workflow-restore-failed"
        >
          <span class="text-muted">Couldn't load your saved workflow.</span>
          <Button
            size="sm"
            variant="secondary"
            onclick={retryWorkflowRestore}
            data-testid="workflow-restore-retry"
          >
            Retry
          </Button>
          <Button
            size="sm"
            variant="ghost"
            onclick={discardWorkflowRestore}
            data-testid="workflow-restore-discard"
          >
            Start over
          </Button>
        </div>
      {:else if restoring}
        <!-- A saved prompt- or workflow-mode draft is still resolving against its
           source (a possibly cold prompt cache; the local workflow list). Show a
           neutral placeholder rather than the plain textarea so the box doesn't
           flash empty and look like the draft was lost. -->
        <div
          class="text-muted flex h-16 items-center gap-2 px-1 text-sm"
          data-testid="compose-restoring"
        >
          <Spinner class="h-4 w-4" />
          Restoring {pendingWorkflowRestore !== null ? "workflow" : "prompt"}…
        </div>
      {:else if mode === "workflow" && workflowForm === null}
        <div
          class="flex min-h-16 items-center gap-3 px-1 text-sm"
          data-testid={workflowFormError === null
            ? "compose-workflow-loading"
            : "compose-workflow-load-failed"}
        >
          {#if workflowFormError === null}
            <Spinner class="h-4 w-4 shrink-0" />
            <span class="text-muted">Loading {selectedWorkflow?.name ?? "workflow"}…</span>
          {:else}
            <div class="min-w-0 flex-1">
              <p class="text-fg">Couldn't load {selectedWorkflow?.name ?? "the workflow"}.</p>
              <p class="text-status-failed mt-0.5 truncate text-xs">{workflowFormError}</p>
            </div>
            <Button
              size="sm"
              variant="secondary"
              data-testid="workflow-form-retry"
              onclick={retryWorkflowForm}
            >
              Retry
            </Button>
            <Button
              size="sm"
              variant="ghost"
              data-testid="workflow-form-start-over"
              onclick={removeWorkflow}
            >
              Start over
            </Button>
          {/if}
        </div>
      {:else if mode === "workflow" && workflowForm !== null}
        <!-- Workflow mode: the invocation form spans the compose area. The compose
           bar's To field + message forwards are hidden (the workflow routes via
           its agent inputs); the run launches in the background. -->
        <WorkflowComposer
          {projectId}
          {crossProjectBase}
          descriptor={workflowForm}
          {agents}
          loading={workflowFormLoading}
          {agentReadiness}
          panes={paneLayout.panes}
          bind:inputs={workflowInputs}
          bind:forwardSources={workflowForwardSources}
          onremove={removeWorkflow}
          onretry={retryWorkflowForm}
          onsignin={(provider) => void signInWorkflowProvider(provider)}
          signingInProvider={workflowSigningInProvider}
          onconfigure={onConfigurePrompts ? configurePrompts : undefined}
        >
          {#snippet invoke()}
            <Button
              variant="primary"
              size="sm"
              data-testid="workflow-invoke-button"
              disabled={!workflowRunnable || invokingWorkflow}
              onclick={() => void invokeWorkflowAction()}
            >
              {invokingWorkflow ? "Starting…" : "Run workflow"}
            </Button>
          {/snippet}
        </WorkflowComposer>
      {:else if mode === "prompt" && selectedPrompt !== null}
        <!-- Prompt mode stacks full-width: the prompt name titles the area, the To
           row sits just under it (handed in as a snippet), then the argument /
           appended boxes; the send button rides the composer's footer row. -->
        <PromptComposer
          {projectId}
          {crossProjectBase}
          prompt={selectedPrompt}
          bind:args={promptArgs}
          bind:appendedText
          bind:argSources={promptArgSources}
          bind:appendedSources={promptAppendedSources}
          {agents}
          panes={paneLayout.panes}
          {agentReadiness}
          focusFirstField={focusPromptFieldOnMount}
          onremove={removePrompt}
          recipients={recipientChips}
          busy={composerBusy}
          send={sendButton}
        />
      {:else if mode === "plain"}
        <div class="relative flex items-stretch gap-2">
          <div class="min-w-0 flex-1">
            {@render forwardSourceChips()}
            <!-- Plain mode owns the To row + the message-level entry points. In
                 prompt mode the To row is handed to the composer; workflow mode
                 routes through its own agent inputs. -->
            <div class="mb-1.5 min-w-0">{@render recipientChips()}</div>
            {@render attachmentChipRow()}
            <Textarea
              autosize
              data-testid="compose-textarea"
              data-shortcut-scope="composer"
              placeholder="Type a message…  (⌘+Enter to send, @ to add a recipient or forward source, / for a prompt)"
              rows={3}
              bind:ref={textareaEl}
              bind:value={draft}
              oninput={onInput}
              onkeydown={handleKey}
              class="max-h-48 min-h-16 border-0 bg-transparent p-1 shadow-none focus-visible:ring-0"
            />
          </div>
          <div
            class="-mt-0.5 flex shrink-0 flex-col items-end gap-0.5"
            data-testid="compose-action-rail"
          >
            <ForwardSourcePicker
              {agents}
              panes={paneLayout.panes}
              onPickAgent={(agent) => addForwardSource(forwardSourceForAgent(agent))}
              onPickPane={(pane) => addPaneForwardSources(pane)}
              crossProject={{
                ...crossProjectBase,
                onPickForeign: (agent, project) =>
                  addForwardSource(forwardSourceForAgent(agent, project)),
              }}
              {agentReadiness}
              disabled={composerBusy}
              showPaneShortcuts
              triggerTestid="compose-forward-button"
              triggerLabel="Forward an agent's output"
              tooltipLabel="Forward an agent's output"
              tooltipDisableHoverableContent
              triggerClass={cn(
                COMPOSER_ACTION_BUTTON_CLASS,
                composerBusy ? "cursor-not-allowed opacity-60" : "",
              )}
            />
            <Tooltip label="Insert a prompt" shortcut={shortcut("/")} disableHoverableContent>
              {#snippet trigger(props)}
                <button
                  {...props}
                  type="button"
                  class={cn(
                    COMPOSER_ACTION_BUTTON_CLASS,
                    composerBusy ? "cursor-not-allowed opacity-60" : "",
                  )}
                  data-testid="compose-prompt-button"
                  aria-label="Insert a prompt"
                  disabled={composerBusy}
                  onclick={() => {
                    if (composerBusy) return;
                    if (promptMenuOpen) {
                      promptMenuOpen = false;
                    } else {
                      openPromptMenu();
                    }
                  }}
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
                    <path d="M6 3h9l3 3v15H6z" />
                    <path d="M15 3v4h4" />
                    <path d="M9 11h6M9 15h6" />
                  </svg>
                </button>
              {/snippet}
            </Tooltip>
            <Tooltip label="Run a workflow" disableHoverableContent>
              {#snippet trigger(props)}
                <button
                  {...props}
                  type="button"
                  class={cn(
                    COMPOSER_ACTION_BUTTON_CLASS,
                    composerBusy ? "cursor-not-allowed opacity-60" : "",
                  )}
                  data-testid="compose-workflow-button"
                  aria-label="Run a workflow"
                  disabled={composerBusy}
                  onclick={() => {
                    if (composerBusy) return;
                    if (workflowMenuOpen) {
                      workflowMenuOpen = false;
                    } else {
                      openWorkflowMenu();
                    }
                  }}
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
                    <circle cx="6" cy="6" r="2" />
                    <circle cx="18" cy="6" r="2" />
                    <circle cx="18" cy="18" r="2" />
                    <path d="M8 6h5a5 5 0 0 1 5 5v5" />
                  </svg>
                </button>
              {/snippet}
            </Tooltip>
            <div class="mt-auto">{@render sendButton()}</div>
          </div>
        </div>
      {/if}
    </div>
    {#if abandonableOperation !== undefined}
      <p class="text-muted mt-2 flex items-center gap-2 text-xs" data-testid="compose-signing-in">
        <span>Waiting for browser sign-in to {abandonableOperation.provider}…</span>
        <!-- **"Stop waiting", not "Cancel".** The sign-in keeps running and may
               still succeed — its final step is deliberately un-timed so it can
               never report a failure it didn't have. What this stops is the
               *composer* waiting, so one stuck sign-in can't hold the project
               until the app restarts. -->
        <Button
          size="sm"
          variant="ghost"
          data-testid="compose-abandon-wait"
          onclick={() => {
            const op = abandonableOperation;
            if (op === undefined) return;
            abandonAwaitingUserOperation(projectId, op.id, {
              message:
                "Stopped waiting for the sign-in. It may still finish in the background; your message is still here.",
              tone: "notice",
            });
          }}
        >
          Stop waiting
        </Button>
      </p>
    {/if}
    {#if sendNotice}
      <!-- Announced for the same reason as the error above: a fork refused
           after the composer moved on is explained here, and a keyboard user
           invoking the shortcut has no other signal. -->
      <p
        class="text-muted mt-2 text-xs"
        data-testid="compose-send-notice"
        role="status"
        aria-live="polite"
      >
        {sendNotice}
      </p>
    {/if}
    {#if sendError}
      <!-- Announced, not just shown. Hiding the fork control when unavailable
           rests on the shortcut still explaining itself, and an explanation a
           screen reader never speaks is no explanation. `polite` rather than
           `alert`: this same element also carries background send failures from
           other agents, which should not interrupt mid-utterance. -->
      <p
        class="text-status-failed mt-2 text-xs"
        data-testid="compose-send-error"
        role="status"
        aria-live="polite"
      >
        {sendError}
      </p>
    {/if}
  {/if}
</div>

<!-- Send, optionally split with Fork.
     Fork is a *variant of sending*, not a compose mode — the branch comes into
     existence as the turn this send dispatches — so it belongs on the send
     control rather than beside the prompt/workflow selectors, which choose what
     kind of message this is. As the second half it also costs no vertical space
     in a bar that has little, and no horizontal space in the row the recipient
     chips grow into.
     In flight, the split control returns to the standard circular Stop button;
     there is no Fork action available while a turn is running. -->
{#snippet sendButton()}
  {@const forkVisible = forkAvailable && !showStop}
  <!-- The pill (shape + base fill) is the container; the halves are transparent
       and round themselves, so each one's hover paints a circle inside its own
       side rather than flooding it corner to corner. This only works because the
       hover is its own named colour — a translucent tint over an identically
       coloured parent composes to nothing, which is what made the first version
       look like it had no hover at all. -->
  <div
    class={cn(
      "flex h-7 shrink-0 items-center justify-center rounded-full",
      showStop
        ? "bg-active text-muted"
        : sendDisabled
          ? "bg-active text-muted/50"
          : "bg-primary text-primary-fg",
    )}
    data-testid="compose-send-group"
  >
    <Tooltip
      label={showStop ? (liveSends.size > 1 ? "Cancel all sends" : "Cancel send") : "Send"}
      shortcut={shortcut("mod", "enter")}
      disableHoverableContent
    >
      {#snippet trigger(props)}
        <button
          {...props}
          type="button"
          data-testid="compose-send"
          onclick={handlePrimaryAction}
          disabled={primaryDisabled}
          aria-label={showStop ? (liveSends.size > 1 ? "Cancel all sends" : "Cancel send") : "Send"}
          class={cn(
            "flex h-7 w-7 shrink-0 items-center justify-center rounded-full",
            showStop
              ? "hover:bg-status-failed-soft/70 hover:text-status-failed"
              : sendDisabled
                ? "cursor-not-allowed"
                : "hover:bg-primary-hover",
          )}
        >
          {#if showStop}
            <StopIcon class="size-5" />
          {:else if composerBusy}
            <svg
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="2.25"
              class="h-3.5 w-3.5 animate-spin"
              aria-hidden="true"
            >
              <path d="M21 12a9 9 0 1 1-6.2-8.6" stroke-linecap="round" />
            </svg>
          {:else}
            <svg
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="2.25"
              stroke-linecap="round"
              stroke-linejoin="round"
              class="h-3.5 w-3.5"
              aria-hidden="true"
            >
              <path d="M12 19V5M5 12l7-7 7 7" />
            </svg>
          {/if}
        </button>
      {/snippet}
    </Tooltip>
    {#if forkVisible}
      <!-- Divider: the two halves do different things and one of them creates an
           agent, so the seam stays visible while either side is hovered. Its own
           strip, so neither half's hover fill paints over it. -->
      <span class="flex h-7 w-px shrink-0 items-center" aria-hidden="true">
        <span class={cn("h-4 w-px", sendDisabled ? "bg-muted/30" : "bg-primary-fg/25")}></span>
      </span>
      <Tooltip
        label={forkCandidate === null
          ? "Fork this conversation"
          : `Fork ${forkCandidate.name} — send this message to a new agent that inherits the conversation so far.`}
        shortcut={shortcut("mod", "shift", "enter")}
        disableHoverableContent
      >
        {#snippet trigger(props)}
          <button
            {...props}
            type="button"
            data-testid="compose-fork-send"
            onclick={handleForkSend}
            disabled={sendDisabled}
            aria-label={forkCandidate === null
              ? "Fork this conversation"
              : `Fork ${forkCandidate.name}`}
            class={cn(
              "flex h-7 w-7 shrink-0 items-center justify-center rounded-full",
              sendDisabled ? "cursor-not-allowed" : "hover:bg-primary-hover",
            )}
          >
            {@render ForkIcon()}
          </button>
        {/snippet}
      </Tooltip>
    {/if}
  </div>
{/snippet}
