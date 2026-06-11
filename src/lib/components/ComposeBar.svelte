<script lang="ts">
  import {
    cancelSend,
    dispatchUserTurn,
    failSendStart,
    recordSendAccepted,
    runtimes,
    transcripts,
  } from "$lib/state/index.svelte";
  import { buildLiveSendsMap } from "$lib/state/liveSends";
  import {
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
  import { isAgentHidden, layoutFor, type TranscriptPane } from "$lib/state/transcriptPanes.svelte";
  import * as api from "$lib/api";
  import type {
    AgentId,
    AgentRecord,
    Attachment,
    AttachmentKind,
    ProjectId,
    Prompt,
  } from "$lib/types";
  import { classifyKind } from "$lib/attachments";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { buildRenderArgs, combinePromptMessage, missingRequiredArgs } from "$lib/prompt";
  import Textarea from "$lib/components/ui/Textarea.svelte";
  import StopIcon from "$lib/components/ui/StopIcon.svelte";
  import HarnessIcon from "$lib/components/ui/HarnessIcon.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import PromptMenu from "$lib/components/PromptMenu.svelte";
  import PromptComposer from "$lib/components/PromptComposer.svelte";
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
  // `mode` swaps the compose area between the plain textarea and the structured
  // prompt composer; the recipients header and send button are shared.
  let mode = $state<"plain" | "prompt">("plain");
  let selectedPrompt = $state<Prompt | null>(null);
  let promptArgs = $state<Record<string, string>>({});
  let appendedText = $state<string>("");
  let promptMenuOpen = $state(false);
  let promptMenuWrapEl = $state<HTMLDivElement | undefined>(undefined);
  let prompts = $state<Prompt[]>([]);
  let focusPromptFieldOnMount = $state(false);
  // Whether the cache has been read at least once, so the picker can show a
  // "loading" row instead of momentarily claiming there are no prompts.
  let promptsLoaded = $state(false);
  let sending = $state(false);
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
  onMount(() => {
    if (pendingRestore === null) return;
    void loadPrompts().then(() => tryRestorePrompt(false));
    const unlisten = listen("prompts:synced", () => {
      void loadPrompts().then(() => tryRestorePrompt(true));
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
      if (e.key === "Enter") {
        if (mode === "prompt" && composeEl?.contains(document.activeElement)) {
          e.preventDefault();
          handlePrimaryAction();
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
  type MenuItem = FileMenuItem | AttachmentMenuItem | RecipientMenuItem;
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
  const menuItems = $derived.by<MenuItem[]>(() => {
    if (!menuOpen) return [];
    return [...fileItems, ...attachmentItems, ...recipientItems];
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
    selectedPrompt === null ? [] : missingRequiredArgs(selectedPrompt, promptArgs),
  );

  /// Send is gated on a recipient + every recipient's history being loaded, plus
  /// per-mode content: plain needs non-empty text; prompt needs a selected prompt
  /// with all required arguments filled, and is blocked while a render is in
  /// flight. **Not** gated on run_status — send-while-busy queues.
  const sendDisabled = $derived(
    mode === "prompt"
      ? selectedPrompt === null || missingRequired.length > 0 || sending || !allRecipientsHydrated
      : (draft.trim() === "" && attachmentChips.length === 0) || !allRecipientsHydrated,
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
    mode === "plain" && liveSends.size > 0 && draft.trim() === "" && attachmentChips.length === 0,
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
      // targeting freeze).
      targetRecipients(projectId, [...item.pane.members]);
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

  // Click-outside closes the prompt picker (its own Escape/pick also close it).
  $effect(() => {
    if (!promptMenuOpen) return;
    function onPointerDown(e: PointerEvent): void {
      if (promptMenuWrapEl?.contains(e.target as Node)) return;
      promptMenuOpen = false;
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
    appendedText = carried;
    focusPromptFieldOnMount = true;
    draft = "";
    mode = "prompt";
    promptMenuOpen = false;
  }

  /// Leave prompt mode, carrying Appended text back into the plain textarea.
  function removePrompt(): void {
    draft = appendedText;
    mode = "plain";
    selectedPrompt = null;
    promptArgs = {};
    focusPromptFieldOnMount = false;
    appendedText = "";
  }

  function handlePrimaryAction(): void {
    if (showStop) {
      for (const [sendId, agentIds] of liveSends) cancelSend(sendId, agentIds);
      return;
    }
    void handleSubmit();
  }

  /// Dispatch `text` to `targets` under one send_id. Shared by the plain and
  /// prompt paths — the prompt path renders first, then calls this with the
  /// finished text and the recipients captured at click time (so toggling chips
  /// mid-render can't redirect the send).
  function dispatchToRecipients(
    text: string,
    attachments: Attachment[],
    targets: AgentRecord[],
  ): void {
    const sendId = crypto.randomUUID();
    // Bump this project's local last-activity so it sorts/reads as active right
    // away, before any turn event round-trips. Once per send action.
    recordProjectsActivityLocally([projectId], currentIsoTimestamp());
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

    dispatchToRecipients(draft.trim(), attachments, [...selectedAgents]);
    // The optimistic user turns are now in the transcript; clear for the next
    // message (recipients stay selected — sticky). Chips clear with the text;
    // their staged files persist on disk for the send.
    draft = "";
    attachmentChips = [];
    composeGeneration += 1;
    persistContentNow();
  }

  /// Persist the current compose content immediately (not via the scheduled
  /// `$effect`), so a send-clear is durable even if the component unmounts in the
  /// same frame (e.g. a project switch right after sending).
  function persistContentNow(): void {
    setContent(projectId, currentContent());
  }
</script>

<div class="bg-raised px-4 pt-2 pb-4" bind:this={composeEl}>
  <div
    class={cn(
      "border-border bg-raised relative rounded-xl border p-2.5 shadow-[0_10px_32px_rgba(0,0,0,0.08)] transition-colors",
      dragOver ? "ring-accent border-accent ring-2" : "",
    )}
    data-testid="compose-box"
    data-drag-over={dragOver}
    bind:this={promptMenuWrapEl}
  >
    {#if promptMenuOpen}
      <!-- Full compose-box width, floating just above the box (anchored to its
           top edge, opening upward so a long list is never cut off). -->
      <PromptMenu
        {prompts}
        loading={!promptsLoaded}
        onpick={pickPrompt}
        onclose={() => (promptMenuOpen = false)}
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
      </div>
    {/if}
    <div class="mb-1.5 flex items-start justify-between gap-2">
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
                    "focus-visible:ring-accent inline-flex items-center gap-1 rounded-full border py-px pr-2 pl-1.5 text-sm transition-colors focus-visible:ring-2 focus-visible:outline-none",
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
      {:else}
        <div></div>
      {/if}
      <div class="shrink-0">
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
      </div>
    </div>

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
    {:else if mode === "prompt" && selectedPrompt !== null}
      <!-- Prompt mode stacks full-width: the argument/appended boxes span the
           whole compose area; the send button sits in the composer's footer row
           beside Preview (passed down as a snippet). -->
      <div class="mt-2">
        <PromptComposer
          prompt={selectedPrompt}
          bind:args={promptArgs}
          bind:appendedText
          focusFirstField={focusPromptFieldOnMount}
          onremove={removePrompt}
          busy={sending}
          send={sendButton}
        />
      </div>
    {:else}
      <div class="relative flex items-end gap-2">
        <Textarea
          autosize
          data-testid="compose-textarea"
          data-shortcut-scope="composer"
          placeholder="Type a message…  (⌘+Enter to send, @ to add a recipient, / for a prompt)"
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
