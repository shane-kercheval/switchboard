<script lang="ts">
  /// Per-agent kebab menu in the sidebar: Stop agent, Open session file, and
  /// Resume in terminal… (a click-open panel showing a copy-ready command).
  /// Session-file actions resolve through the backend (`agent_session_info`),
  /// which reads adapter-owned sidecars for Codex/Antigravity — so they're
  /// disabled until the agent has a resolvable session.
  import type { AgentRecord } from "$lib/types";
  import {
    agentSessionInfo,
    openSessionFile as apiOpenSessionFile,
    type AgentSessionInfo,
  } from "$lib/api";
  import { stopAgent } from "$lib/state/index.svelte";
  import { copyText } from "$lib/native";
  import DropdownMenu from "$lib/components/ui/DropdownMenu.svelte";
  import DropdownMenuItem from "$lib/components/ui/DropdownMenuItem.svelte";
  import Dialog from "$lib/components/ui/Dialog.svelte";
  import { cn } from "$lib/utils";

  /// `active` = the agent is currently driving work (in-flight or queued). Gates
  /// "Stop agent" and switches the resume panel to its stronger collision warning.
  let { agent, active }: { agent: AgentRecord; active: boolean } = $props();

  let menuOpen = $state(false);
  let resumeOpen = $state(false);
  let info = $state<AgentSessionInfo | null>(null);
  let loadError = $state<string | null>(null);
  let copied = $state(false);

  // Resolve session actions each time the menu opens (a session can become
  // available after the first dispatch). A *failure* (e.g. a corrupt sidecar,
  // which the command surfaces loudly) is kept distinct from a clean
  // "no session yet" so the menu can say so rather than silently greying out.
  $effect(() => {
    if (!menuOpen) return;
    loadError = null;
    void agentSessionInfo(agent.id)
      .then((next) => {
        info = next;
        loadError = null;
      })
      .catch((err: unknown) => {
        info = null;
        loadError = err instanceof Error ? err.message : String(err);
      });
  });

  function openSessionFile(): void {
    if (!info?.session_file) return;
    // Don't swallow failures — a silently-rejected open looks like "nothing
    // happened." The path is opened backend-side (see api.openSessionFile).
    void apiOpenSessionFile(agent.id).catch((err: unknown) => {
      console.error("[switchboard] open session file failed", err);
    });
  }

  let copiedTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => () => clearTimeout(copiedTimer));
  function copyResumeCommand(): void {
    if (info?.resume_command === null || info?.resume_command === undefined) return;
    void copyText(info.resume_command);
    copied = true;
    clearTimeout(copiedTimer);
    copiedTimer = setTimeout(() => (copied = false), 1000);
  }
</script>

<DropdownMenu
  bind:open={menuOpen}
  triggerLabel={`Actions for ${agent.name}`}
  triggerTestid="agent-actions-trigger"
  triggerClass="text-muted hover:text-fg hover:bg-border flex h-6 w-6 items-center justify-center rounded-md transition-colors"
  contentTestid="agent-actions-menu"
>
  {#snippet trigger()}
    <svg viewBox="0 0 24 24" fill="currentColor" class="h-4 w-4" aria-hidden="true">
      <circle cx="12" cy="5" r="1.6" />
      <circle cx="12" cy="12" r="1.6" />
      <circle cx="12" cy="19" r="1.6" />
    </svg>
  {/snippet}
  <DropdownMenuItem
    onSelect={() => stopAgent(agent.id)}
    disabled={!active}
    data-testid="agent-action-stop"
  >
    Stop agent
  </DropdownMenuItem>
  <DropdownMenuItem
    onSelect={() => {
      copied = false;
      resumeOpen = true;
    }}
    disabled={!info?.resume_command}
    data-testid="agent-action-resume"
  >
    Resume in terminal…
  </DropdownMenuItem>
  <DropdownMenuItem
    onSelect={openSessionFile}
    disabled={!info?.session_file}
    data-testid="agent-action-open-session"
  >
    Open session file
  </DropdownMenuItem>
  {#if loadError !== null}
    <div class="text-status-failed px-2.5 py-1.5 text-xs" data-testid="agent-actions-error">
      Couldn't read session state: {loadError}
    </div>
  {/if}
</DropdownMenu>

<Dialog bind:open={resumeOpen} title="Resume in terminal" contentClass="max-w-lg">
  <div class="space-y-3" data-testid="resume-panel">
    <p class="text-muted text-xs">
      Run this in your terminal to resume this session interactively.
    </p>
    <div class="flex items-stretch gap-2">
      <code
        class="bg-panel text-fg min-w-0 flex-1 overflow-x-auto rounded-md px-2.5 py-2 font-mono text-xs whitespace-pre"
        data-testid="resume-command">{info?.resume_command ?? ""}</code
      >
      <button
        type="button"
        class={cn(
          "border-border bg-panel hover:bg-raised flex w-8 shrink-0 items-center justify-center rounded-md border transition-colors",
          copied ? "text-accent" : "text-muted hover:text-fg",
        )}
        data-testid="resume-copy"
        aria-label={copied ? "Copied" : "Copy command"}
        onclick={copyResumeCommand}
      >
        {#if copied}
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2.5"
            stroke-linecap="round"
            stroke-linejoin="round"
            class="h-4 w-4"
            aria-hidden="true"
          >
            <path d="M20 6 9 17l-5-5" />
          </svg>
        {:else}
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
            <rect x="8" y="8" width="12" height="12" rx="2" />
            <path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2" />
          </svg>
        {/if}
      </button>
    </div>
    {#if active}
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
