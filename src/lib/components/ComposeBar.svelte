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
  import { getCompose, setDraft, setSelection } from "$lib/state/composeStore";
  import * as api from "$lib/api";
  import type { AgentId, AgentRecord, ProjectId } from "$lib/types";
  import Textarea from "$lib/components/ui/Textarea.svelte";
  import StopIcon from "$lib/components/ui/StopIcon.svelte";
  import HarnessIcon from "$lib/components/ui/HarnessIcon.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { cn } from "$lib/utils";
  import { shortcut } from "$lib/platform";
  import { onDestroy, untrack } from "svelte";

  let { projectId, agents }: { projectId: ProjectId; agents: AgentRecord[] } = $props();

  // The compose bar is remounted per project (App.svelte's `{#key}`), and the
  // parent only mounts it once the roster is loaded and non-empty — so the
  // saved snapshot can be applied synchronously here, against a populated
  // roster, with no first-render-empty window to guard against. `projectId` and
  // the mount-time roster are constant for this component's life; `untrack`
  // states that the initial read is deliberate, not a missed dependency.
  const saved = untrack(() => getCompose(projectId));

  let prompt = $state<string>(saved.draft);
  let sendError = $state<string | null>(null);
  let composeEl = $state<HTMLDivElement | undefined>(undefined);
  let inputWrapEl = $state<HTMLDivElement | undefined>(undefined);
  let textareaEl = $state<HTMLTextAreaElement | undefined>(undefined);

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

  function resizeTextarea(textarea: HTMLTextAreaElement | undefined, _draft: string): void {
    if (textarea === undefined) return;
    textarea.style.height = "auto";
    const naturalHeight = textarea.scrollHeight;
    const maxHeight = Number.parseFloat(getComputedStyle(textarea).maxHeight);
    const cappedHeight = Number.isFinite(maxHeight)
      ? Math.min(naturalHeight, maxHeight)
      : naturalHeight;
    textarea.style.height = `${cappedHeight}px`;
    textarea.style.overflowY = naturalHeight > cappedHeight ? "auto" : "hidden";
  }

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

  // Drop any selected ids whose agent disappeared (agent removed at runtime).
  $effect(() => {
    const valid = selectedIds.filter((id) => agents.some((a) => a.id === id));
    if (valid.length !== selectedIds.length) selectedIds = valid;
  });

  // Persist draft + recipient selection per project (machine-local; see
  // composeStore). Synchronous write-through — no debounce — so a send-clear
  // can't be resurrected by a deferred write.
  $effect(() => {
    setDraft(projectId, prompt);
  });
  // Track both the draft and bound textarea: draft changes resize the box, and
  // the ref change performs the initial resize once the DOM node is available.
  $effect(() => {
    resizeTextarea(textareaEl, prompt);
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

  // Keyboard routes to the recipient chips, working even while typing (the
  // modifier chord inserts no text). Window-level so they fire regardless of
  // focus. Mod+Shift+A selects every agent; Mod+1..9 toggles the Nth agent
  // (same order as the sidebar).
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
        // First dismiss the @ menu, otherwise clear the recipient set. The draft
        // text is untouched either way.
        if (menuOpen) {
          menuOpen = false;
          e.preventDefault();
        } else if (selectedIds.length > 0) {
          selectedIds = [];
          e.preventDefault();
        }
        return;
      }
      if (!mod || e.altKey) return;
      if (e.shiftKey) {
        if (e.key.toLowerCase() === "a") {
          e.preventDefault();
          selectedIds = agents.map((a) => a.id);
        }
        return;
      }
      if (e.key < "1" || e.key > "9") return;
      const agent = agents[Number(e.key) - 1];
      if (agent === undefined) return;
      e.preventDefault();
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
  let menuAnchor = $state<{ left: number; top: number } | null>(null);
  let highlighted = $state(0);
  const AT_TOKEN = /(^|\s)@([^\s]*)$/;
  const FILE_MATCH_LIMIT = 12;
  const FILE_SEARCH_DEBOUNCE_MS = 180;
  const MENU_WIDTH = 256;

  const agentCandidates = $derived(
    menuOpen ? agents.filter((a) => a.name.toLowerCase().includes(menuQuery.toLowerCase())) : [],
  );

  /// The menu's navigable rows: file matches render first in their own section,
  /// then recipient actions and matching agents. **All** appears only when not
  /// everyone is selected and its keyword matches the query; **Clear** only when
  /// something is selected and its keyword matches. Even though files render
  /// first, keyboard selection prefers a matched agent when one exists.
  type FileMenuItem = { kind: "file"; key: string; path: string };
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

  /// Send is gated only on a recipient + non-empty prompt + every recipient's
  /// history being loaded (`complete`/`failed`). **Not** on run_status — a busy
  /// recipient's message queues (send-while-busy is un-gated).
  const sendDisabled = $derived(
    selectedAgents.length === 0 ||
      prompt.trim() === "" ||
      !selectedAgents.every((a) => {
        const rt = runtimes[a.id];
        return (
          rt !== undefined &&
          (rt.hydration_status === "complete" || rt.hydration_status === "failed")
        );
      }),
  );

  // Every live send across this project's agents, mapped to the agents it's
  // live for. The composer stop cancels *all* of it, not just the most recent
  // send, so one click halts everything the project's agents are running and
  // have queued. (IPC failures prune `pending_sends` via failSendStart, so
  // failed recipients drop out without extra bookkeeping.)
  const liveSends = $derived(buildLiveSendsMap(agents, runtimes, transcripts));
  // A non-empty draft means the primary action is "send/queue this prompt".
  const showStop = $derived(liveSends.size > 0 && prompt.trim() === "");
  const primaryDisabled = $derived(showStop ? false : sendDisabled);

  function toggleRecipient(id: AgentId): void {
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
    }));
  }

  function stripAtToken(): void {
    prompt = prompt.replace(AT_TOKEN, "$1");
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
    prompt = prompt.replace(AT_TOKEN, (_match, prefix: string) => {
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
    menuAnchor = null;
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

  function activeAtToken(text: string): { query: string; atIndex: number } | null {
    const match = AT_TOKEN.exec(text);
    if (!match) return null;
    return {
      query: match[2] ?? "",
      atIndex: match.index + (match[1]?.length ?? 0),
    };
  }

  function updateMenuAnchor(atIndex: number): void {
    if (textareaEl === undefined || inputWrapEl === undefined) {
      menuAnchor = null;
      return;
    }

    const style = getComputedStyle(textareaEl);
    const mirror = document.createElement("div");
    mirror.style.position = "absolute";
    mirror.style.visibility = "hidden";
    mirror.style.pointerEvents = "none";
    mirror.style.whiteSpace = "pre-wrap";
    mirror.style.overflowWrap = "break-word";
    mirror.style.boxSizing = style.boxSizing;
    mirror.style.width = `${textareaEl.clientWidth}px`;
    mirror.style.font = style.font;
    mirror.style.letterSpacing = style.letterSpacing;
    mirror.style.lineHeight = style.lineHeight;
    mirror.style.padding = style.padding;
    mirror.style.border = style.border;

    const marker = document.createElement("span");
    marker.textContent = "@";
    mirror.append(document.createTextNode(prompt.slice(0, atIndex)), marker);
    // eslint-disable-next-line svelte/no-dom-manipulating -- textarea caret measurement needs an off-tree mirror that Svelte does not own
    inputWrapEl.append(mirror);

    const left = textareaEl.offsetLeft + marker.offsetLeft - textareaEl.scrollLeft;
    const top = textareaEl.offsetTop + marker.offsetTop - textareaEl.scrollTop;
    mirror.remove();

    menuAnchor = {
      left: Math.max(0, Math.min(left, Math.max(0, inputWrapEl.clientWidth - MENU_WIDTH))),
      top: Math.max(0, top),
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

  $effect(() => {
    if (!menuOpen) return;
    if (menuItems.length === 0) return;
    if (highlighted >= menuItems.length) highlighted = menuItems.length - 1;
  });

  function onInput(): void {
    const token = activeAtToken(prompt);
    const hasAtToken = token !== null;
    const hasRecipientOptions = agents.length > 1;
    const shouldSearchFiles = hasAtToken && (token.query.length > 0 || agents.length === 1);
    const shouldOpenMenu = hasAtToken && (hasRecipientOptions || shouldSearchFiles);
    if (token !== null && shouldOpenMenu) {
      menuQuery = token.query;
      menuOpen = true;
      updateMenuAnchor(token.atIndex);
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
    // Escape (menu dismiss / clear recipients) is handled by the window-level
    // listener above, so it works whether the textarea or a chip has focus.
    if (event.key === "Enter" && event.metaKey) {
      event.preventDefault();
      handlePrimaryAction();
    }
  }

  function handlePrimaryAction(): void {
    if (showStop) {
      for (const [sendId, agentIds] of liveSends) cancelSend(sendId, agentIds);
      return;
    }
    handleSubmit();
  }

  function handleSubmit(): void {
    if (sendDisabled) return;
    const submittedText = prompt.trim();
    sendError = null;

    // One send_id for the whole action; each recipient is an independent turn
    // sharing it (the backend groups, and cancel-send is scoped to it).
    const sendId = crypto.randomUUID();
    const targets = [...selectedAgents];
    for (const agent of targets) {
      const userTurnId = crypto.randomUUID();
      dispatchUserTurn(agent.id, userTurnId, submittedText, sendId);
      // Per-recipient, fire-and-forget: an idle recipient starts immediately, a
      // busy one queues. A single recipient's IPC failure fails only its bubble.
      void (async () => {
        try {
          const messageId = await api.sendMessage(agent.id, submittedText, sendId);
          recordSendAccepted(agent.id, userTurnId, messageId);
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          sendError = message;
          failSendStart(agent.id, userTurnId, { message, kind: "adapter_failure" });
        }
      })();
    }
    // The optimistic user turns are now in the transcript; clear for the next
    // message (recipients stay selected — sticky).
    prompt = "";
  }
</script>

<div class="bg-raised px-4 pt-2 pb-4" bind:this={composeEl}>
  <div
    class="border-border bg-raised rounded-xl border p-2.5 shadow-[0_10px_32px_rgba(0,0,0,0.08)]"
  >
    {#if agents.length > 1}
      <div class="mb-1.5 flex flex-wrap items-center gap-1.5 text-xs" data-testid="recipient-field">
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
                )}
                data-testid={`recipient-chip-${agent.id}`}
                data-selected={selected}
                aria-pressed={selected}
                onclick={() => toggleRecipient(agent.id)}
              >
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
                class="text-muted hover:text-fg hover:bg-panel ml-0.5 flex h-6 w-6 items-center justify-center rounded-full transition-colors"
                data-testid="recipient-clear"
                aria-label="Clear recipients"
                onclick={() => (selectedIds = [])}
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
    <div class="relative flex items-end gap-2" bind:this={inputWrapEl}>
      {#if menuOpen && hasMenuContent}
        <div
          class="border-border/90 bg-raised absolute z-10 w-64 overflow-hidden rounded-lg border p-1 text-[13px] shadow-[0_10px_28px_rgba(0,0,0,0.10)]"
          style={menuAnchor === null
            ? undefined
            : `left: ${menuAnchor.left}px; top: ${menuAnchor.top}px; transform: translateY(calc(-100% - 0.25rem));`}
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
                class={"hover:bg-panel/80 flex w-full cursor-pointer items-center gap-2 rounded-md px-2.5 py-1 text-left leading-5 outline-none select-none " +
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
                <span class="text-fg truncate">{item.path}</span>
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
      <Textarea
        data-testid="compose-textarea"
        data-shortcut-scope="composer"
        placeholder="Type a message…  (⌘+Enter to send, @ to add a recipient)"
        rows={3}
        bind:ref={textareaEl}
        bind:value={prompt}
        oninput={onInput}
        onkeydown={handleKey}
        class="max-h-48 min-h-16 border-0 bg-transparent p-1 shadow-none focus-visible:ring-0"
      />
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
            aria-label={showStop
              ? liveSends.size > 1
                ? "Cancel all sends"
                : "Cancel send"
              : "Send"}
            class={cn(
              "flex h-8 w-8 shrink-0 items-center justify-center rounded-full transition-colors",
              showStop
                ? "bg-border text-muted hover:bg-status-failed-soft/70 hover:text-status-failed"
                : sendDisabled
                  ? "bg-border text-muted/50 cursor-not-allowed"
                  : "bg-primary text-primary-fg hover:bg-primary/90",
            )}
          >
            {#if showStop}
              <StopIcon class="size-6" />
            {:else}
              <svg
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2.25"
                stroke-linecap="round"
                stroke-linejoin="round"
                class="h-4 w-4"
                aria-hidden="true"
              >
                <path d="M12 19V5M5 12l7-7 7 7" />
              </svg>
            {/if}
          </button>
        {/snippet}
      </Tooltip>
    </div>
  </div>
  {#if sendError}
    <p class="text-status-failed mt-2 text-xs" data-testid="compose-send-error">
      Send failed: {sendError}
    </p>
  {/if}
</div>
