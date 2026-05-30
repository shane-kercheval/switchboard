<script lang="ts">
  import {
    cancelSend,
    dispatchUserTurn,
    failSendStart,
    recordSendAccepted,
    runtimes,
    transcripts,
    ui,
  } from "$lib/state/index.svelte";
  import { buildLiveSendsMap } from "$lib/state/liveSends";
  import * as api from "$lib/api";
  import type { AgentId, AgentRecord } from "$lib/types";
  import Textarea from "$lib/components/ui/Textarea.svelte";
  import StopIcon from "$lib/components/ui/StopIcon.svelte";
  import HarnessIcon from "$lib/components/ui/HarnessIcon.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { cn } from "$lib/utils";
  import { shortcut } from "$lib/platform";

  let { agents }: { agents: AgentRecord[] } = $props();

  let prompt = $state<string>("");
  let sendError = $state<string | null>(null);
  let composeEl = $state<HTMLDivElement | undefined>(undefined);

  /// Recipient set — every agent is shown as a toggle chip (click to add/drop);
  /// `@name` is the keyboard path to the same toggle. Sticky across sends
  /// (defaults to the last recipient on first render).
  let selectedIds = $state<AgentId[]>([]);
  let initialized = false;
  $effect(() => {
    if (!initialized && agents.length > 0) {
      const last = ui.lastRecipientId;
      selectedIds = last !== null && agents.some((a) => a.id === last) ? [last] : [agents[0]!.id];
      initialized = true;
    }
  });
  // Drop any selected ids whose agent disappeared (project switch / removal).
  $effect(() => {
    const valid = selectedIds.filter((id) => agents.some((a) => a.id === id));
    if (valid.length !== selectedIds.length) selectedIds = valid;
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
      const mod = e.metaKey || e.ctrlKey;
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

  /// `@`-quick-add: a trailing `@token` opens a typeahead of *unselected*
  /// agents; Enter / click adds the highlighted one and strips the token. This
  /// is the keyboard route to selecting recipients without touching the mouse.
  let menuOpen = $state(false);
  let menuEl = $state<HTMLDivElement | undefined>(undefined);
  let menuQuery = $state("");
  let highlighted = $state(0);
  const AT_TOKEN = /(^|\s)@([\w-]*)$/;

  const candidates = $derived(
    menuOpen
      ? agents.filter(
          (a) =>
            !selectedIds.includes(a.id) && a.name.toLowerCase().includes(menuQuery.toLowerCase()),
        )
      : [],
  );

  /// The menu's navigable rows: the bulk actions (matched by the query, like
  /// the agents) lead, then the unselected agents. **All** appears only when
  /// not everyone is selected and its keyword matches the query; **Clear** only
  /// when something is selected and its keyword matches. So a bare `@` shows
  /// both, `@a` keeps All, `@c`/`@cl` keeps Clear, `@bo` shows just agents.
  type MenuItem =
    | { kind: "all"; key: string }
    | { kind: "clear"; key: string }
    | { kind: "agent"; key: string; agent: AgentRecord };
  const menuItems = $derived.by<MenuItem[]>(() => {
    if (!menuOpen) return [];
    const q = menuQuery.toLowerCase();
    const actions: MenuItem[] = [];
    if (selectedIds.length < agents.length && "all".includes(q)) {
      actions.push({ kind: "all", key: "all" });
    }
    if (selectedIds.length > 0 && "clear".includes(q)) {
      actions.push({ kind: "clear", key: "clear" });
    }
    const agentRows: MenuItem[] = candidates.map((agent) => ({
      kind: "agent",
      key: agent.id,
      agent,
    }));
    return [...actions, ...agentRows];
  });

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

  function pickItem(item: MenuItem): void {
    if (item.kind === "all") {
      selectedIds = agents.map((a) => a.id);
    } else if (item.kind === "clear") {
      selectedIds = [];
    } else if (!selectedIds.includes(item.agent.id)) {
      selectedIds = [...selectedIds, item.agent.id];
    }
    // The menu only opens from a typed `@token`, so always strip it.
    prompt = prompt.replace(AT_TOKEN, "$1");
    menuOpen = false;
  }

  $effect(() => {
    if (!menuOpen) return;
    function onPointerDown(e: PointerEvent): void {
      if (menuEl?.contains(e.target as Node)) return;
      menuOpen = false;
    }
    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  });

  function onInput(): void {
    const m = AT_TOKEN.exec(prompt);
    if (m && agents.length > 1) {
      menuQuery = m[2] ?? "";
      highlighted = 0;
      menuOpen = true;
    } else {
      menuOpen = false;
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
                  "focus-visible:ring-accent inline-flex items-center gap-1 rounded-full border py-0.5 pr-2 pl-1.5 text-sm transition-colors focus-visible:ring-2 focus-visible:outline-none",
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
                class="text-muted hover:text-fg hover:bg-panel ml-0.5 flex h-[26px] w-[26px] items-center justify-center rounded-full transition-colors"
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
                  class="h-5 w-5"
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
    <div class="relative flex items-end gap-2">
      {#if menuOpen && menuItems.length > 0}
        <div
          class="border-border/90 bg-raised absolute bottom-full left-0 z-10 mb-1 max-h-64 w-64 overflow-y-auto rounded-lg border p-1 text-[13px] shadow-[0_10px_28px_rgba(0,0,0,0.10)]"
          data-testid="recipient-menu"
          role="listbox"
          bind:this={menuEl}
        >
          <div
            class="text-muted px-2.5 py-1 text-[11px] font-medium tracking-wide uppercase select-none"
          >
            Send to
          </div>
          {#each menuItems as item, i (item.key)}
            <button
              type="button"
              class={"hover:bg-panel/80 flex w-full cursor-pointer items-center gap-2.5 rounded-md px-2.5 py-1.5 text-left leading-5 outline-none select-none " +
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
              {:else}
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
        placeholder="Type a message…  (⌘+Enter to send, @ to add a recipient)"
        rows={3}
        bind:value={prompt}
        oninput={onInput}
        onkeydown={handleKey}
        class="min-h-16 border-0 bg-transparent p-1 shadow-none focus-visible:ring-0"
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
              <StopIcon />
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
