<script lang="ts">
  import {
    cancelSend,
    dispatchUserTurn,
    failSendStart,
    recordSendAccepted,
    runtimes,
    transcripts,
  } from "$lib/state/index.svelte";
  import {
    addHeldForward,
    removeHeldForward,
    setForwardCaption,
    forwardSourceKey,
    expandForwardSources,
    forwardSourceForAgent,
    forwardSourceForPane,
    type ForwardSource,
  } from "$lib/state/heldForwards.svelte";
  import { buildLiveSendsMap } from "$lib/state/liveSends";
  import {
    flush,
    getCompose,
    setContent,
    setSelection,
    type ComposeContent,
    type PromptContent,
  } from "$lib/state/composeStore";
  import { recordProjectsActivityLocally } from "$lib/state/workspace.svelte";
  import {
    selectionFor,
    setRecipients,
    setTargetingLocked,
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
    AttachmentKind,
    ProjectId,
    Prompt,
    WorkflowFormDescriptor,
    WorkflowInputValue,
    WorkflowListing,
  } from "$lib/types";
  import { classifyKind } from "$lib/attachments";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { isPermissionGranted, requestPermission } from "@tauri-apps/plugin-notification";
  import { buildRenderArgs, combinePromptMessage, missingRequiredArgs } from "$lib/prompt";
  import Textarea from "$lib/components/ui/Textarea.svelte";
  import StopIcon from "$lib/components/ui/StopIcon.svelte";
  import HarnessIcon from "$lib/components/ui/HarnessIcon.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
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
  import { shortcut } from "$lib/platform";
  import { isEditableShortcutTarget } from "$lib/keyboard";
  import { onDestroy, onMount, tick, untrack } from "svelte";
  import { listen } from "@tauri-apps/api/event";

  let {
    projectId,
    agents,
    focusOnMount = false,
  }: { projectId: ProjectId; agents: AgentRecord[]; focusOnMount?: boolean } = $props();

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
  // fields plus a local `id` for list keying / removal. Chips are **session-only**
  // (deliberately not persisted in the compose snapshot): the load-time GC
  // reclaims any staged-but-unsent file, so a restored chip would dangle at a
  // deleted path — re-drop to re-attach.
  type AttachmentChip = Attachment & { id: string };
  let attachmentChips = $state<AttachmentChip[]>([]);
  let dragOver = $state(false);
  // Per-kind label counters. Monotonic across a compose session: removing a chip
  // never renumbers the survivors, so a label always refers to the same file.
  const attachmentCounters: Record<AttachmentKind, number> = {
    image: 0,
    text: 0,
    file: 0,
    unknown: 0,
  };
  // Bumped whenever the chip set is committed (send) or abandoned (unmount).
  // A staging result captures the generation it began under and is discarded if
  // the generation has since moved on — so a slow copy can't land a chip in a
  // composer that was already cleared. Plain, not `$state`: never rendered.
  let composeGeneration = 0;

  function addAttachmentChip(staged: { path: string; original_name: string }): void {
    const kind = classifyKind(staged.original_name);
    attachmentCounters[kind] += 1;
    attachmentChips = [
      ...attachmentChips,
      {
        id: crypto.randomUUID(),
        label: `${kind}-${attachmentCounters[kind]}`,
        kind,
        path: staged.path,
        original_name: staged.original_name,
      },
    ];
  }

  function removeAttachmentChip(id: string): void {
    attachmentChips = attachmentChips.filter((chip) => chip.id !== id);
  }

  // Forward sources: agents whose latest output is forwarded into this send (the
  // §7 manual cross-agent forward). Picked from the `@`-menu's "Forward from"
  // entries; a pane entry expands to its members at pick time. Session-only,
  // cleared on send / restore — like attachment chips. A send with ≥1 forward
  // source dispatches via `forward_message` instead of the normal send path.
  let forwardSources = $state<ForwardSource[]>([]);

  function addForwardSource(source: ForwardSource): void {
    if (forwardSources.some((s) => forwardSourceKey(s) === forwardSourceKey(source))) return;
    forwardSources = [...forwardSources, source];
  }

  function removeForwardSource(key: string): void {
    forwardSources = forwardSources.filter((s) => forwardSourceKey(s) !== key);
  }

  /// True when this agent already has a completed turn the forward can carry — a
  /// forward source with no output yet is flagged on its chip up front.
  function agentHasCompletedOutput(agentId: AgentId): boolean {
    return (transcripts[agentId] ?? []).some(
      (turn) => turn.role === "agent" && turn.status === "complete",
    );
  }

  /// Whether a source has nothing to forward yet — an agent with no completed
  /// turn, or a pane whose every member is empty (or has no members).
  function sourceIsEmpty(source: ForwardSource): boolean {
    const ids = source.kind === "agent" ? [source.id] : source.members;
    return ids.length === 0 || ids.every((id) => !agentHasCompletedOutput(id));
  }

  /// Agent names that actually carried output, for the partial-empty caption —
  /// derived from the expanded agent ids (panes included) minus the backend's
  /// skipped names.
  function includedNames(sources: ForwardSource[], skipped: string[]): string[] {
    return expandForwardSources(sources)
      .map((id) => agents.find((a) => a.id === id)?.name)
      .filter((name): name is string => name !== undefined && !skipped.includes(name));
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
    const gen = composeGeneration;
    for (const path of paths) {
      try {
        const staged = await api.stageAttachment(projectId, path);
        // The drop's compose session may have been sent or torn down while the
        // copy was in flight; if so, discard the result rather than resurrecting
        // a chip into a now-cleared (or different) composer.
        if (gen !== composeGeneration) return;
        addAttachmentChip(staged);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        sendError = `Couldn't attach ${basename(path)}: ${message}`;
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
          if (!sending) void stageDroppedPaths(payload.paths);
        }
      });
      void dropSub.catch((e) => console.error("[attachments] onDragDropEvent failed", e));
    } catch {
      dropSub = undefined;
    }
    // Await the subscription promise before unlistening, so an unmount that beats
    // the promise still tears the listener down (matching the `prompts:synced`
    // cleanup below). A bare `unlisten?.()` would no-op in that race and leak a
    // global listener that keeps staging into a stale project context.
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
  let promptArgSources = $state<Record<string, ForwardSource[]>>({});
  // Forward sources for the appended-text field (the appended text is just
  // another forwardable field; the backend composes it into the appended tail).
  let promptAppendedSources = $state<ForwardSource[]>([]);
  let appendedText = $state<string>("");
  let promptMenuOpen = $state(false);
  let prompts = $state<Prompt[]>([]);
  let focusPromptFieldOnMount = $state(false);
  // Whether the cache has been read at least once, so the picker can show a
  // "loading" row instead of momentarily claiming there are no prompts.
  let promptsLoaded = $state(false);
  let sending = $state(false);

  // Workflow invocation state (live-UI-only). The menu lists the project's
  // workflows; picking an invocable one enters workflow mode with a per-input
  // form. Not persisted across reloads (a half-filled invocation is transient).
  let workflowMenuOpen = $state(false);
  let workflows = $state<WorkflowListing[]>([]);
  let workflowsLoaded = $state(false);
  let selectedWorkflow = $state<WorkflowListing | null>(null);
  // The resolved invocation form for the picked workflow (declared inputs +
  // auto-derived prompt-argument fields + compatibility). Fetched per-pick via
  // `describe_workflow_form`; re-fetched on `prompts:synced` so a cold MCP cache
  // resolves. Null until the first fetch settles.
  let workflowForm = $state<WorkflowFormDescriptor | null>(null);
  let workflowFormLoading = $state(false);
  // Monotonic token: each pick/re-fetch bumps it; a fetch ignores its reply if a
  // newer one superseded it (name alone isn't a workflow's identity — a built-in
  // and a same-named copied user workflow share a name).
  let workflowFormGen = 0;
  // Whether a prompt sync has settled since this workflow was picked. Before that,
  // an `unresolved` prompt is genuinely pending (cold MCP cache); after a settled
  // sync, a still-`unresolved` prompt is a real "not found" error, not a spinner.
  let workflowSyncSettled = $state(false);
  let workflowInputs = $state<Record<string, WorkflowInputValue>>({});
  // Per-field forward sources for the workflow's fillable single-text fields,
  // keyed by field name. Live-UI-only, reset whenever the workflow changes.
  let workflowForwardSources = $state<Record<string, ForwardSource[]>>({});
  let invokingWorkflow = $state(false);
  // A saved prompt-mode draft to restore once the cache loads; consumed when
  // restoration settles. Null when the saved draft was plain.
  let pendingRestore = $state<PromptContent | null>(
    saved.content.kind === "prompt" ? saved.content : null,
  );
  // True while a saved prompt-mode draft is still being resolved against the
  // cache. Gates the persist effect so the not-yet-restored plain placeholder
  // can't overwrite the saved snapshot before restoration settles.
  let restoring = $state(saved.content.kind === "prompt");

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

  // Only touch the cache at mount when there's a saved prompt-mode draft to
  // restore; otherwise the picker loads prompts lazily on open. (Avoids an
  // unnecessary mount-time read in the common plain-draft case.) The cache may
  // be cold (MCP prompts land only after the launch-time sync), so also re-try
  // when the backend signals a completed sync — and only then is "still absent"
  // proof the prompt is gone.
  // A single `prompts:synced` subscription drives two cache-warm re-tries: (1)
  // restoring a saved prompt-mode draft whose prompt was cold at mount, and (2)
  // re-resolving the workflow form so a workflow hardcoding an MCP prompt leaves
  // its "Resolving…" pending state once the cache warms — without a re-pick.
  onMount(() => {
    const hadDraft = pendingRestore !== null;
    if (hadDraft) {
      void loadPrompts().then(() => tryRestorePrompt(false));
    }
    const unlisten = listen("prompts:synced", () => {
      if (hadDraft) {
        void loadPrompts().then(() => tryRestorePrompt(true));
      }
      if (mode === "workflow" && selectedWorkflow !== null) {
        // A sync has now settled for this pick: a still-unresolved prompt after the
        // re-fetch is a real "not found" error, not a perpetual pending state.
        workflowSyncSettled = true;
        void loadWorkflowForm(selectedWorkflow);
      }
    });
    return () => void unlisten.then((u) => u());
  });

  onMount(() => {
    if (!focusOnMount) return;
    if (pendingRestore === null) {
      requestAnimationFrame(() => textareaEl?.focus());
    } else {
      focusPromptFieldOnMount = true;
    }
  });

  /// Resolve a saved prompt-mode draft against the loaded cache. If the prompt
  /// is present, re-enter prompt mode with the saved argument values. If it's
  /// absent, only downgrade to plain once `syncSettled` — a completed sync proves
  /// the prompt is actually gone (renamed/removed). A cold cache (`syncSettled`
  /// false) is left pending so a transient miss never destroys the draft.
  function tryRestorePrompt(syncSettled: boolean): void {
    if (pendingRestore === null) return;
    const snapshot = pendingRestore;
    const found = prompts.find((p) => p.provider === snapshot.provider && p.name === snapshot.name);
    if (found !== undefined) {
      selectedPrompt = found;
      promptArgs = Object.fromEntries(
        found.arguments.map((a) => [a.name, snapshot.args[a.name] ?? ""]),
      );
      appendedText = snapshot.appendedText;
      focusPromptFieldOnMount = focusPromptFieldOnMount || focusOnMount;
      mode = "prompt";
      pendingRestore = null;
      restoring = false;
      return;
    }
    if (syncSettled) {
      // Proven gone: fall back to plain, carrying the appended text so nothing
      // the user typed is lost.
      draft = snapshot.appendedText;
      mode = "plain";
      pendingRestore = null;
      restoring = false;
      if (focusOnMount) requestAnimationFrame(() => textareaEl?.focus());
    }
  }

  /// The current compose content as a persistable snapshot. Single definition so
  /// the persist effect and the explicit send-clear persist agree.
  function currentContent(): ComposeContent {
    return mode === "prompt" && selectedPrompt !== null
      ? {
          kind: "prompt",
          provider: selectedPrompt.provider,
          name: selectedPrompt.name,
          args: { ...promptArgs },
          appendedText,
        }
      : { kind: "plain", draft };
  }

  // Drop any selected ids whose agent disappeared (agent removed at runtime).
  $effect(() => {
    const valid = selectedIds.filter((id) => agents.some((a) => a.id === id));
    if (valid.length !== selectedIds.length) setSelectedIds(valid);
  });

  // Persist the compose content per project (machine-local; see composeStore).
  // Plain and prompt modes are distinct persisted states. Skipped while a saved
  // prompt-mode draft is still being restored, so the pre-restore plain
  // placeholder can't overwrite (and destroy) the saved snapshot. The send-clear
  // path persists explicitly (`persistContentNow`) so it survives a same-frame
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
  // Defensive: a fresh composer can never start with targeting frozen (the
  // unmount release above should make this a no-op, but a stuck lock would
  // silently disable pane targeting forever — not a failure worth risking).
  untrack(() => setTargetingLocked(projectId, false));
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
        } else if (!sending && selectedIds.length > 0) {
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
          if (!sending) addForwardSource(forwardSourceForPane(pane, agents));
        }
        return;
      }
      if (e.key === "Enter") {
        if (composeEl?.contains(document.activeElement)) {
          if (mode === "prompt") {
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
          if (sending) return;
          setSelectedIds(agents.map((a) => a.id));
        }
        return;
      }
      if (e.key < "1" || e.key > "9") return;
      const agent = agents[Number(e.key) - 1];
      if (agent === undefined) return;
      e.preventDefault();
      if (sending) return;
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
    if (paneLayout.panes.length > 1) {
      for (const pane of paneLayout.panes) {
        if (pane.members.length === 0) continue;
        if (!pane.name.toLowerCase().includes(q)) continue;
        items.push({ kind: "forward-pane", key: `forward-pane:${pane.id}`, pane });
      }
    }
    const alreadyForwarded = expandForwardSources(forwardSources);
    for (const agent of agentCandidates) {
      if (alreadyForwarded.includes(agent.id)) continue;
      items.push({ kind: "forward-agent", key: `forward-agent:${agent.id}`, agent });
    }
    return items;
  });
  const menuItems = $derived.by<MenuItem[]>(() => {
    if (!menuOpen) return [];
    return [...fileItems, ...attachmentItems, ...recipientItems, ...forwardItems];
  });
  const fileStatusText = $derived.by<string | null>(() => {
    if (fileItems.length > 0) return null;
    if (fileSearchState === "searching") return "Searching files...";
    if (fileSearchState === "ready") return "No matching files";
    if (fileSearchState === "error") return "File search unavailable";
    return null;
  });
  const showFileSection = $derived(fileItems.length > 0 || fileStatusText !== null);
  const hasMenuContent = $derived(menuItems.length > 0 || showFileSection);

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
  /// with all required arguments filled, and is blocked while a render is in
  /// flight. **Not** gated on run_status — send-while-busy queues.
  const sendDisabled = $derived(
    mode === "prompt"
      ? selectedPrompt === null || missingRequired.length > 0 || sending || !allRecipientsHydrated
      : (draft.trim() === "" && attachmentChips.length === 0 && forwardSources.length === 0) ||
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
    if (sending) return;
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
      // One pane chip (membership snapshotted at pick time, moments before send);
      // it expands to per-agent blocks only at dispatch.
      addForwardSource(forwardSourceForPane(item.pane, agents));
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
    // Abandon any in-flight staging for this (now unmounting) compose session.
    composeGeneration += 1;
    // A mid-render unmount (project switch via the parent's `{#key}`) must not
    // leave the project's pane targeting frozen.
    setTargetingLocked(projectId, false);
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
      openPromptMenu();
      return;
    }
    // Escape (menu dismiss / clear recipients) is handled by the window-level
    // listener above, so it works whether the textarea or a chip has focus.
    if (event.key === "Enter" && event.metaKey) {
      event.preventDefault();
      handlePrimaryAction();
    }
  }

  function openPromptMenu(): void {
    closeMentionMenu();
    workflowMenuOpen = false;
    void loadPrompts();
    promptMenuOpen = true;
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
      sendError = null;
    } catch (err) {
      sendError = `Couldn't copy prompt: ${err instanceof Error ? err.message : String(err)}`;
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

  async function loadWorkflows(): Promise<void> {
    try {
      const list = await api.listWorkflows();
      workflows = Array.isArray(list) ? list : [];
    } catch {
      workflows = [];
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
    selectedWorkflow = workflow;
    workflowForm = null;
    workflowInputs = {};
    workflowForwardSources = {};
    workflowSyncSettled = false;
    mode = "workflow";
    workflowMenuOpen = false;
    void loadWorkflowForm(workflow);
  }

  /// Fetch (or re-fetch) the descriptor for the picked workflow and seed any
  /// not-yet-present fields. Seeding is additive so a re-fetch (e.g. after
  /// `prompts:synced` resolves a previously-unresolved prompt) preserves what the
  /// user already typed. A monotonic generation token guards stale replies — name
  /// alone is not a workflow's identity (a built-in and a same-named copied user
  /// workflow share a name), and the token is also future-proof against further
  /// identity fields.
  async function loadWorkflowForm(workflow: WorkflowListing): Promise<void> {
    const gen = ++workflowFormGen;
    workflowFormLoading = true;
    try {
      const form = await api.describeWorkflowForm(workflow.name, workflow.is_builtin);
      if (gen !== workflowFormGen) return; // superseded by a newer pick/re-fetch
      workflowForm = form;
      const seeded: Record<string, WorkflowInputValue> = { ...workflowInputs };
      for (const input of form.inputs) {
        if (input.name in seeded) continue;
        seeded[input.name] = input.ty === "agent_list" || input.ty === "text_list" ? [] : "";
      }
      for (const arg of form.derived_args) {
        if (!(arg.name in seeded)) seeded[arg.name] = "";
      }
      workflowInputs = seeded;
    } catch (err) {
      if (gen === workflowFormGen) {
        sendError = `Couldn't load workflow: ${err instanceof Error ? err.message : String(err)}`;
      }
    } finally {
      if (gen === workflowFormGen) workflowFormLoading = false;
    }
  }

  function removeWorkflow(): void {
    mode = "plain";
    selectedWorkflow = null;
    workflowForm = null;
    workflowFormLoading = false;
    workflowInputs = {};
    workflowForwardSources = {};
    workflowSyncSettled = false;
    workflowFormGen++; // invalidate any in-flight fetch for the removed workflow
  }

  async function copyWorkflow(workflow: WorkflowListing): Promise<void> {
    try {
      await api.copyBuiltinWorkflow(workflow.name);
      await loadWorkflows();
      sendError = null;
    } catch (err) {
      sendError = `Couldn't copy workflow: ${err instanceof Error ? err.message : String(err)}`;
    }
  }

  function openWorkflowsFolder(): void {
    void api.openWorkflowsDir().catch((err: unknown) => {
      console.error("[switchboard] open workflows folder failed", err);
    });
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

  /// Request OS-notification permission once, contextually at first invoke.
  async function ensureNotificationPermission(): Promise<void> {
    try {
      if (!(await isPermissionGranted())) {
        await requestPermission();
      }
    } catch (err) {
      console.warn("[switchboard] notification permission request failed", err);
    }
  }

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
    sendError = null;
    try {
      // Contextual permission request at first invoke (not at cold startup).
      void ensureNotificationPermission();
      // Pane-expand each field's sources to agent ids; omit empty fields so the
      // map carries only fields the user actually attached a forward to.
      const forwardSources: Record<string, AgentId[]> = {};
      for (const [name, sources] of Object.entries(workflowForwardSources)) {
        if (sources.length > 0) forwardSources[name] = expandForwardSources(sources);
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
      sendError = `Couldn't run workflow: ${err instanceof Error ? err.message : String(err)}`;
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
        const outcome = await api.forwardMessage(body, expandForwardSources(sources), forwardId);
        removeHeldForward(forwardProjectId, forwardId);
        if (outcome.status === "resolved") {
          // Dispatch the composed body as a normal send under this forward's
          // send_id — the user message + responses render and group via the
          // existing machinery. The forward marker is derived from the body's
          // sentinel lines at render time (durable across reload); only the
          // partial-empty caption needs the live store (it can't be reconstructed
          // — skipped sources leave no trace in the wire body).
          dispatchToRecipients(outcome.body, attachments, targets, sendId, forwardProjectId);
          if (outcome.skipped.length > 0) {
            setForwardCaption(forwardProjectId, sendId, {
              included: includedNames(sources, outcome.skipped),
              skipped: outcome.skipped,
            });
          }
        } else {
          // invalidated (a source failed/cancelled, or all sources empty) or the
          // user cancelled the hold — nothing resolved; restore the composer.
          restoreForward(body, sources, attachments);
          if (outcome.status === "invalidated") sendError = `Forward not sent: ${outcome.reason}`;
        }
      } catch (err) {
        removeHeldForward(forwardProjectId, forwardId);
        sendError = `Forward failed: ${err instanceof Error ? err.message : String(err)}`;
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
      attachmentChips = attachments.map((a) => ({ ...a, id: crypto.randomUUID() }));
    }
    persistContentNow();
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
    // body "" — a prompt forward composes server-side (render after fill), so the
    // held row only signals the wait; there's no pre-composed body to show.
    addHeldForward(forwardProjectId, {
      forwardId,
      sendId,
      body: "",
      sources: allSources,
      recipients,
    });
    const forwardArgs: api.ForwardArg[] = prompt.arguments
      .filter((a) => (argSources[a.name]?.length ?? 0) > 0)
      .map((a) => ({
        name: a.name,
        sources: expandForwardSources(argSources[a.name] ?? []),
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
          expandForwardSources(appendedSources),
          forwardId,
        );
        removeHeldForward(forwardProjectId, forwardId);
        if (outcome.status === "resolved") {
          dispatchToRecipients(outcome.body, attachments, targets, sendId, forwardProjectId);
          if (outcome.skipped.length > 0) {
            setForwardCaption(forwardProjectId, sendId, {
              included: includedNames(allSources, outcome.skipped),
              skipped: outcome.skipped,
            });
          }
        } else {
          restoreForwardPrompt(
            prompt,
            typedArgs,
            appended,
            argSources,
            appendedSources,
            attachments,
          );
          if (outcome.status === "invalidated") sendError = `Forward not sent: ${outcome.reason}`;
        }
      } catch (err) {
        removeHeldForward(forwardProjectId, forwardId);
        sendError = `Forward failed: ${err instanceof Error ? err.message : String(err)}`;
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
    attachmentChips = attachments.map((a) => ({ ...a, id: crypto.randomUUID() }));
    mode = "prompt";
    persistContentNow();
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
          sendError = `Send failed: ${message}`;
          failSendStart(agent.id, userTurnId, { message, kind: "adapter_failure" });
        }
      })();
    }
  }

  async function handleSubmit(): Promise<void> {
    if (sendDisabled) return;
    sendError = null;
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
        selectedPrompt = null;
        promptArgs = {};
        promptArgSources = {};
        promptAppendedSources = [];
        // A completed send is a fresh start: drop any plain-mode forward set that
        // was hidden during prompt mode, so it can't silently resurface and ride a
        // later plain send. (Removing the prompt before sending still restores it.)
        forwardSources = [];
        focusPromptFieldOnMount = false;
        appendedText = "";
        draft = "";
        attachmentChips = [];
        composeGeneration += 1;
        mode = "plain";
        persistContentNow();
        return;
      }

      promptMenuOpen = false;
      closeMentionMenu();
      sending = true;
      // Freeze pane targeting for the render window: the post-render check
      // below silently aborts the send if a captured recipient left the set,
      // so a pane gesture landing mid-render would drop the send with no
      // feedback. Raw selection writes (pruning a removed agent) still pass —
      // a removed recipient SHOULD trigger that abort. `finally` (plus the
      // unmount/init releases) guarantees the lock can't outlive the render.
      setTargetingLocked(projectId, true);
      let finalText: string;
      try {
        const rendered = await api.renderPrompt(prompt.provider, prompt.name, renderArgs);
        finalText = combinePromptMessage(rendered.text, appended);
      } catch (err) {
        sendError = `Send failed: ${err instanceof Error ? err.message : String(err)}`;
        return;
      } finally {
        sending = false;
        setTargetingLocked(projectId, false);
      }
      // If the composer state changed outside the locked UI while rendering,
      // avoid dispatching text into a now-different prompt/recipient context.
      const stillSelected = new Set(selectedIds);
      if (selectedPrompt !== prompt || targets.some((t) => !stillSelected.has(t.id))) return;
      dispatchToRecipients(finalText, attachments, targets);
      // Prompt selection is not sticky: a successful send returns to the plain
      // composer (recipients stay selected). Appended text is consumed, not
      // carried back.
      selectedPrompt = null;
      promptArgs = {};
      promptArgSources = {};
      promptAppendedSources = [];
      // A completed send is a fresh start — see the prompt-forward branch above.
      forwardSources = [];
      focusPromptFieldOnMount = false;
      appendedText = "";
      draft = "";
      // Chips clear optimistically with the text (the optimistic user turn already
      // renders them); the staged files persist on disk for the send.
      attachmentChips = [];
      composeGeneration += 1;
      mode = "plain";
      persistContentNow();
      return;
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
      attachmentChips = [];
      composeGeneration += 1;
      persistContentNow();
      return;
    }

    dispatchToRecipients(draft.trim(), attachments, [...selectedAgents]);
    // The optimistic user turns are now in the transcript; clear for the next
    // message (recipients stay selected — sticky). Chips clear with the text;
    // their staged files persist on disk for the send.
    draft = "";
    attachmentChips = [];
    composeGeneration += 1;
    persistContentNow();
  }

  /// Persist the current compose content immediately — `flush()` writes through
  /// and cancels the pending debounce — so a send-clear is durable even if the
  /// component unmounts in the same frame (e.g. a project switch right after
  /// sending), and a stale pre-send draft can never land after the clear.
  function persistContentNow(): void {
    setContent(projectId, currentContent());
    flush();
  }
</script>

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
            class="text-muted hover:bg-status-failed-soft/70 hover:text-status-failed focus-visible:ring-accent inline-flex h-7 shrink-0 items-center gap-1 rounded-full px-2 text-xs transition-colors focus-visible:ring-2 focus-visible:outline-none"
          >
            <StopIcon class="size-4" />
            Stop
          </button>
        {:else}
          <button
            type="button"
            data-testid="workflow-run-dismiss"
            onclick={() => void dismissWorkflowRun()}
            class="text-muted hover:bg-panel hover:text-fg focus-visible:ring-accent inline-flex h-7 shrink-0 items-center rounded-full px-2.5 text-xs transition-colors focus-visible:ring-2 focus-visible:outline-none"
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
        dragOver ? "ring-accent border-accent ring-2" : "",
      )}
      data-testid="compose-box"
      data-drag-over={dragOver}
    >
      {#if promptMenuOpen}
        <!-- Full compose-box width, floating just above the box (anchored to its
           top edge, opening upward so a long list is never cut off). -->
        <PromptMenu
          {prompts}
          loading={!promptsLoaded}
          onpick={pickPrompt}
          oncopy={copyPrompt}
          onclose={() => (promptMenuOpen = false)}
        />
      {/if}
      {#if workflowMenuOpen}
        <WorkflowMenu
          {workflows}
          loading={!workflowsLoaded}
          onpick={pickWorkflow}
          oncopy={copyWorkflow}
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
                class={"hover:bg-panel/80 flex w-full cursor-pointer items-start gap-2 rounded-md px-2.5 py-1.5 text-left leading-5 outline-none select-none " +
                  (i === highlighted ? "bg-panel/80" : "")}
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
                class={"hover:bg-panel/80 flex w-full cursor-pointer items-center gap-2 rounded-md px-2.5 py-1 text-left leading-5 outline-none select-none " +
                  (i === highlighted ? "bg-panel/80" : "")}
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
              class={"hover:bg-panel/80 flex w-full cursor-pointer items-center gap-2 rounded-md px-2.5 py-1 text-left leading-5 outline-none select-none " +
                (i === highlighted ? "bg-panel/80" : "")}
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
                <svg
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                  stroke-linecap="round"
                  class="text-muted h-4 w-4"
                  aria-hidden="true"
                >
                  <circle cx="12" cy="12" r="9" />
                  <path d="m5.6 5.6 12.8 12.8" />
                </svg>
                <span class="text-fg">Clear</span>
                <span class="text-muted ml-auto font-mono text-[13px]">{shortcut("esc")}</span>
              {:else if item.kind === "pane"}
                <svg
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.8"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                  class="text-accent h-4 w-4"
                  aria-hidden="true"
                >
                  <rect x="3" y="4" width="18" height="16" rx="2" />
                  <path d="M12 4v16" />
                </svg>
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
            <button
              type="button"
              class={"hover:bg-panel/80 flex w-full cursor-pointer items-center gap-2 rounded-md px-2.5 py-1 text-left leading-5 outline-none select-none " +
                (i === highlighted ? "bg-panel/80" : "")}
              data-testid={`forward-option-${item.key}`}
              role="option"
              aria-selected={i === highlighted}
              onclick={() => pickItem(item)}
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
                <span class="text-fg">{item.agent.name}</span>
                {#if !agentHasCompletedOutput(item.agent.id)}
                  <span class="text-muted ml-auto text-[11px] italic">no output yet</span>
                {/if}
              {/if}
            </button>
          {/each}
        </div>
      {/if}
      {#if mode === "plain" && forwardSources.length > 0}
        <!-- Plain-mode only: prompt mode forwards per-field, and workflow mode
           routes via its agent inputs, so the message-level forward set doesn't
           apply and is hidden in both (its state is preserved for restore when
           the prompt/workflow is removed). -->
        <div class="mb-1.5 flex flex-wrap items-center gap-1.5" data-testid="forward-source-chips">
          <span class="text-muted text-xs">Forwarding from</span>
          {#each forwardSources as source (forwardSourceKey(source))}
            <ForwardSourceChip
              {source}
              empty={sourceIsEmpty(source)}
              disabled={sending}
              onRemove={() => removeForwardSource(forwardSourceKey(source))}
            />
          {/each}
        </div>
      {/if}
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
              >
                {#snippet trigger(props)}
                  <button
                    {...props}
                    type="button"
                    class={cn(
                      "focus-visible:ring-accent inline-flex h-6 items-center gap-1 rounded-full border px-2 text-xs transition-colors focus-visible:ring-2 focus-visible:outline-none",
                      selected
                        ? "bg-accent-soft text-fg border-transparent"
                        : "border-panel bg-panel text-muted hover:bg-raised hover:text-fg",
                      sending ? "cursor-not-allowed opacity-60" : "",
                    )}
                    data-testid={`recipient-chip-${agent.id}`}
                    data-selected={selected}
                    data-hidden-recipient={chipHidden || undefined}
                    aria-pressed={selected}
                    disabled={sending}
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
                    class="text-muted hover:text-fg hover:bg-panel ml-0.5 flex h-[26px] w-[26px] items-center justify-center rounded-full transition-colors"
                    data-testid="recipient-clear"
                    aria-label="Clear recipients"
                    disabled={sending}
                    onclick={() => {
                      if (!sending) setSelectedIds([]);
                    }}
                  >
                    <svg
                      viewBox="0 0 24 24"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="1.75"
                      stroke-linecap="round"
                      class="h-4 w-4"
                      aria-hidden="true"
                    >
                      <circle cx="12" cy="12" r="9" />
                      <path d="m5.6 5.6 12.8 12.8" />
                    </svg>
                  </button>
                {/snippet}
              </Tooltip>
            {/if}
          </div>
        {/if}
      {/snippet}

      {#if mode === "plain"}
        <!-- Plain mode owns the To row + the message-level entry points. In prompt
           mode the To row is handed to the composer (so the prompt name titles
           the whole thing, above the recipients); workflow mode routes via its
           own agent inputs, so neither shows. -->
        <div class="mb-1.5 flex items-start justify-between gap-2">
          <div class="min-w-0">{@render recipientChips()}</div>
          <div class="flex shrink-0 items-center gap-1">
            <ForwardSourcePicker
              {agents}
              panes={paneLayout.panes}
              onPickAgent={(agent) => addForwardSource(forwardSourceForAgent(agent))}
              onPickPane={(pane) => addForwardSource(forwardSourceForPane(pane, agents))}
              agentHasOutput={agentHasCompletedOutput}
              disabled={sending}
              showPaneShortcuts
              triggerTestid="compose-forward-button"
              triggerText="Forward"
              triggerLabel="Forward an agent's output"
              tooltipLabel="Forward an agent's output"
              triggerClass={cn(
                "text-muted hover:text-fg hover:bg-panel focus-visible:ring-accent flex h-6 items-center gap-1 rounded-full border border-transparent px-2 text-xs transition-colors focus-visible:ring-2 focus-visible:outline-none",
                sending ? "cursor-not-allowed opacity-60" : "",
              )}
            />
            <Tooltip label="Insert a prompt" shortcut={shortcut("/")}>
              {#snippet trigger(props)}
                <button
                  {...props}
                  type="button"
                  class={cn(
                    "text-muted hover:text-fg hover:bg-panel focus-visible:ring-accent flex h-6 items-center gap-1 rounded-full border border-transparent px-2 text-xs transition-colors focus-visible:ring-2 focus-visible:outline-none",
                    sending ? "cursor-not-allowed opacity-60" : "",
                  )}
                  data-testid="compose-prompt-button"
                  aria-label="Insert a prompt"
                  disabled={sending}
                  onclick={() => {
                    if (sending) return;
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
                    stroke-width="2"
                    stroke-linecap="round"
                    class="h-4 w-4"
                    aria-hidden="true"
                  >
                    <path d="M12 5v14M5 12h14" />
                  </svg>
                  Prompt
                </button>
              {/snippet}
            </Tooltip>
            <Tooltip label="Run a workflow">
              {#snippet trigger(props)}
                <button
                  {...props}
                  type="button"
                  class={cn(
                    "text-muted hover:text-fg hover:bg-panel focus-visible:ring-accent flex h-6 items-center gap-1 rounded-full border border-transparent px-2 text-xs transition-colors focus-visible:ring-2 focus-visible:outline-none",
                    sending ? "cursor-not-allowed opacity-60" : "",
                  )}
                  data-testid="compose-workflow-button"
                  aria-label="Run a workflow"
                  disabled={sending}
                  onclick={() => {
                    if (sending) return;
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
                    stroke-width="2"
                    stroke-linecap="round"
                    class="h-4 w-4"
                    aria-hidden="true"
                  >
                    <path d="M12 5v14M5 12h14" />
                  </svg>
                  Workflow
                </button>
              {/snippet}
            </Tooltip>
          </div>
        </div>
      {/if}

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
              <span class="truncate" title={chip.original_name}>{chip.original_name}</span>
              <button
                type="button"
                class="text-muted hover:text-fg hover:bg-raised flex h-4 w-4 shrink-0 items-center justify-center rounded-full transition-colors disabled:cursor-not-allowed disabled:opacity-50"
                data-testid={`attachment-chip-remove-${chip.label}`}
                aria-label={`Remove ${chip.original_name}`}
                disabled={sending}
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

      {#if restoring}
        <!-- A saved prompt-mode draft is still resolving against the (possibly
           cold) cache. Show a neutral placeholder rather than the plain textarea
           so the box doesn't flash empty and look like the draft was lost. -->
        <div
          class="text-muted flex h-16 items-center gap-2 px-1 text-sm"
          data-testid="compose-restoring"
        >
          <Spinner class="h-4 w-4" />
          Restoring prompt…
        </div>
      {:else if mode === "workflow" && workflowForm !== null}
        <!-- Workflow mode: the invocation form spans the compose area. The compose
           bar's To field + message forwards are hidden (the workflow routes via
           its agent inputs); the run launches in the background. -->
        <WorkflowComposer
          descriptor={workflowForm}
          {agents}
          loading={workflowFormLoading}
          syncSettled={workflowSyncSettled}
          agentHasOutput={agentHasCompletedOutput}
          panes={paneLayout.panes}
          bind:inputs={workflowInputs}
          bind:forwardSources={workflowForwardSources}
          onremove={removeWorkflow}
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
          prompt={selectedPrompt}
          bind:args={promptArgs}
          bind:appendedText
          bind:argSources={promptArgSources}
          bind:appendedSources={promptAppendedSources}
          {agents}
          panes={paneLayout.panes}
          agentHasOutput={agentHasCompletedOutput}
          focusFirstField={focusPromptFieldOnMount}
          onremove={removePrompt}
          recipients={recipientChips}
          busy={sending}
          send={sendButton}
        />
      {:else}
        <div class="relative flex items-end gap-2">
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
          {@render sendButton()}
        </div>
      {/if}
    </div>
    {#if sendError}
      <p class="text-status-failed mt-2 text-xs" data-testid="compose-send-error">
        {sendError}
      </p>
    {/if}
  {/if}
</div>

{#snippet sendButton()}
  <Tooltip
    label={showStop ? (liveSends.size > 1 ? "Cancel all sends" : "Cancel send") : "Send"}
    shortcut={shortcut("mod", "enter")}
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
          "flex h-7 w-7 shrink-0 items-center justify-center rounded-full transition-colors",
          showStop
            ? "bg-border text-muted hover:bg-status-failed-soft/70 hover:text-status-failed"
            : sendDisabled
              ? "bg-border text-muted/50 cursor-not-allowed"
              : "bg-primary text-primary-fg hover:bg-primary/90",
        )}
      >
        {#if showStop}
          <StopIcon class="size-5" />
        {:else if sending}
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
{/snippet}
