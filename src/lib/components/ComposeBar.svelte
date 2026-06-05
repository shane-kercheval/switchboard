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
  import * as api from "$lib/api";
  import type { AgentId, AgentRecord, ProjectId, Prompt } from "$lib/types";
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
  import { onDestroy, onMount, untrack } from "svelte";
  import { listen } from "@tauri-apps/api/event";

  let { projectId, agents }: { projectId: ProjectId; agents: AgentRecord[] } = $props();

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
    if (valid.length !== selectedIds.length) selectedIds = valid;
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
  let selectedIds = $state<AgentId[]>(untrack(() => initialSelection(saved.selectedIds, agents)));

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
  function isEditableShortcutTarget(target: EventTarget | null): boolean {
    if (!(target instanceof HTMLElement)) return false;
    return (
      target.isContentEditable ||
      target.tagName === "INPUT" ||
      target.tagName === "TEXTAREA" ||
      target.tagName === "SELECT"
    );
  }

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
          selectedIds = [];
          e.preventDefault();
        }
        return;
      }
      if (!mod || e.altKey) return;
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
          selectedIds = agents.map((a) => a.id);
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
    | { kind: "agent"; key: string; agent: AgentRecord };
  type MenuItem = FileMenuItem | RecipientMenuItem;
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
    return [...fileItems, ...recipientItems];
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
      : draft.trim() === "" || !allRecipientsHydrated,
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
  const showStop = $derived(mode === "plain" && liveSends.size > 0 && draft.trim() === "");
  const primaryDisabled = $derived(showStop ? false : sendDisabled);

  function toggleRecipient(id: AgentId): void {
    if (sending) return;
    selectedIds = selectedIds.includes(id)
      ? selectedIds.filter((x) => x !== id)
      : [...selectedIds, id];
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

  function stripAtToken(): void {
    draft = draft.replace(AT_TOKEN, "$1");
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
    draft = draft.replace(AT_TOKEN, (_match, prefix: string) => {
      return `${prefix}${markdownCodeSpan(path)} `;
    });
  }

  function pickItem(item: MenuItem): void {
    if (item.kind === "file") {
      insertFileMention(item.path);
    } else if (item.kind === "all") {
      selectedIds = agents.map((a) => a.id);
      stripAtToken();
    } else if (item.kind === "clear") {
      selectedIds = [];
      stripAtToken();
    } else {
      selectedIds = [item.agent.id];
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
    fileMatches = [];
    fileSearchState = "idle";
    clearFileSearchTimer();
    fileSearchToken += 1;
  }

  onDestroy(() => {
    clearFileSearchTimer();
    fileSearchToken += 1;
  });

  async function refreshFileMatches(query: string, token: number): Promise<void> {
    try {
      const matches = await api.searchProjectFiles(projectId, query, FILE_MATCH_LIMIT);
      if (token !== fileSearchToken || !menuOpen || menuQuery !== query) return;
      fileMatches = matches;
      fileSearchState = "ready";
      highlighted = preferredHighlightIndex([...fileMenuItemsFor(matches), ...recipientItems]);
    } catch {
      if (token !== fileSearchToken || !menuOpen || menuQuery !== query) return;
      fileMatches = [];
      fileSearchState = "error";
      highlighted = preferredHighlightIndex(recipientItems);
    }
  }

  function activeAtToken(text: string): { query: string } | null {
    const match = AT_TOKEN.exec(text);
    if (!match) return null;
    return {
      query: match[2] ?? "",
    };
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
    const token = activeAtToken(draft);
    const hasAtToken = token !== null;
    const hasRecipientOptions = agents.length > 1;
    const shouldSearchFiles = hasAtToken && (token.query.length > 0 || agents.length === 1);
    const shouldOpenMenu = hasAtToken && (hasRecipientOptions || shouldSearchFiles);
    if (token !== null && shouldOpenMenu) {
      promptMenuOpen = false;
      menuQuery = token.query;
      menuOpen = true;
      if (shouldSearchFiles) {
        const q = token.query.toLowerCase();
        const retainedFileMatches = fileMatches.filter((path) => path.toLowerCase().includes(q));
        fileMatches = retainedFileMatches;
        highlighted = preferredHighlightIndex([
          ...fileMenuItemsFor(retainedFileMatches),
          ...recipientItems,
        ]);
        scheduleFileMatchRefresh(token.query);
      } else {
        fileMatches = [];
        highlighted = preferredHighlightIndex(recipientItems);
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
  function dispatchToRecipients(text: string, targets: AgentRecord[]): void {
    const sendId = crypto.randomUUID();
    // Bump this project's local last-activity so it sorts/reads as active right
    // away, before any turn event round-trips. Once per send action.
    recordProjectsActivityLocally([projectId], currentIsoTimestamp());
    for (const agent of targets) {
      const userTurnId = crypto.randomUUID();
      dispatchUserTurn(agent.id, userTurnId, text, sendId);
      // Per-recipient, fire-and-forget: an idle recipient starts immediately, a
      // busy one queues. A single recipient's IPC failure fails only its bubble.
      void (async () => {
        try {
          const messageId = await api.sendMessage(agent.id, text, sendId);
          recordSendAccepted(agent.id, userTurnId, messageId);
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          sendError = message;
          failSendStart(agent.id, userTurnId, { message, kind: "adapter_failure" });
        }
      })();
    }
  }

  async function handleSubmit(): Promise<void> {
    if (sendDisabled) return;
    sendError = null;

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
      let finalText: string;
      try {
        const rendered = await api.renderPrompt(prompt.provider, prompt.name, renderArgs);
        finalText = combinePromptMessage(rendered.text, appended);
      } catch (err) {
        sendError = err instanceof Error ? err.message : String(err);
        sending = false;
        return;
      }
      sending = false;
      // If the composer state changed outside the locked UI while rendering,
      // avoid dispatching text into a now-different prompt/recipient context.
      const stillSelected = new Set(selectedIds);
      if (selectedPrompt !== prompt || targets.some((t) => !stillSelected.has(t.id))) return;
      dispatchToRecipients(finalText, targets);
      // Prompt selection is not sticky: a successful send returns to the plain
      // composer (recipients stay selected). Appended text is consumed, not
      // carried back.
      selectedPrompt = null;
      promptArgs = {};
      focusPromptFieldOnMount = false;
      appendedText = "";
      draft = "";
      mode = "plain";
      persistContentNow();
      return;
    }

    dispatchToRecipients(draft.trim(), [...selectedAgents]);
    // The optimistic user turns are now in the transcript; clear for the next
    // message (recipients stay selected — sticky).
    draft = "";
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
    class="border-border bg-raised relative rounded-xl border p-2.5 shadow-[0_10px_32px_rgba(0,0,0,0.08)]"
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

        {#if recipientItems.length > 0}
          <div
            class={cn(
              "text-muted px-2.5 py-0.5 text-[11px] font-medium tracking-wide uppercase select-none",
              fileItems.length > 0 ? "mt-1" : "",
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
            <Tooltip
              label={selected ? `Drop ${agent.name}` : `Add ${agent.name}`}
              shortcut={i < 9 ? shortcut("mod", String(i + 1)) : undefined}
              delayDuration={1000}
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
                    if (!sending) selectedIds = [];
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
      Send failed: {sendError}
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
