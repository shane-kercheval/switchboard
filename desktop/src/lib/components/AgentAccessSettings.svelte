<script lang="ts">
  /// Settings for agent access, the MCP server other agents connect to: the
  /// switch, the port, whether it is listening, the token, and each client's
  /// configuration. The backend owns the state; every action returns the new
  /// status, which replaces the shown one.
  import { onMount } from "svelte";
  import Button from "$lib/components/ui/Button.svelte";
  import CopyButton from "$lib/components/ui/CopyButton.svelte";
  import Input from "$lib/components/ui/Input.svelte";
  import Disclosure from "$lib/components/ui/Disclosure.svelte";
  import Switch from "$lib/components/ui/Switch.svelte";
  import {
    agentAccessStatus,
    regenerateAgentAccessToken,
    setAgentAccessEnabled,
    setAgentAccessPort,
  } from "$lib/api";
  import { CODEX_TOKEN_VARIABLE, agentAccessSnippets } from "$lib/agentAccessSnippets";
  import type { AgentAccessStatus } from "$lib/types";

  let status = $state<AgentAccessStatus | null>(null);
  let loadError = $state<string | null>(null);
  let actionError = $state<string | null>(null);
  let pending = $state(false);
  let portDraft = $state("");
  let tokenVisible = $state(false);

  /// The configuration each Copy button copies: always the real token.
  const snippets = $derived(status?.token ? agentAccessSnippets(status.url, status.token) : null);
  /// The configuration shown on screen: the token masked like its own row
  /// until Show, so a screen share of Settings never shows it by accident.
  const shownSnippets = $derived(
    status?.token
      ? agentAccessSnippets(status.url, tokenVisible ? status.token : masked(status.token))
      : null,
  );

  onMount(() => {
    void load();
  });

  async function load(): Promise<void> {
    try {
      show(await agentAccessStatus());
    } catch (err) {
      loadError = String(err);
    }
  }

  function show(next: AgentAccessStatus): void {
    status = next;
    portDraft = String(next.port);
  }

  async function run(action: () => Promise<AgentAccessStatus>): Promise<void> {
    pending = true;
    actionError = null;
    try {
      show(await action());
    } catch (err) {
      actionError = String(err);
    } finally {
      pending = false;
    }
  }

  function toggle(): void {
    if (status === null) return;
    const enabled = !status.enabled;
    void run(() => setAgentAccessEnabled(enabled));
  }

  function applyPort(): void {
    if (status === null) return;
    const port = Number(portDraft.trim());
    if (!Number.isInteger(port) || port < 1 || port > 65_535) {
      actionError = "Choose a port from 1 to 65535.";
      return;
    }
    if (port === status.port) return;
    void run(() => setAgentAccessPort(port));
  }

  function regenerate(): void {
    void run(regenerateAgentAccessToken);
  }

  function masked(token: string): string {
    return `${"•".repeat(12)}${token.slice(-4)}`;
  }
</script>

{#if loadError}
  <p class="text-status-failed text-xs" data-testid="agent-access-load-error">
    Couldn't read the Agent MCP Server setting ({loadError}).
  </p>
{:else if status === null}
  <p class="text-muted text-xs">Loading…</p>
{:else}
  <div class="flex items-start justify-between gap-4">
    <div class="min-w-0">
      <div class="text-fg text-sm">Let agents connect over MCP</div>
      <p class="text-muted mt-0.5 text-xs leading-relaxed" data-testid="agent-access-state">
        {#if status.listening}
          Listening on <span class="text-fg font-mono">{status.url}</span>. Only programs on this
          Mac that hold the token can connect.
        {:else if status.enabled}
          On, but not listening.
        {:else}
          Off. Nothing is listening.
        {/if}
      </p>
    </div>
    <Switch
      checked={status.enabled}
      disabled={pending}
      ariaLabel="Let agents connect over MCP"
      testid="agent-access-toggle"
      onclick={toggle}
    />
  </div>

  {#if status.error}
    <p class="text-status-failed text-xs leading-relaxed" data-testid="agent-access-error">
      {status.error}
    </p>
  {/if}
  {#if actionError}
    <p class="text-status-failed text-xs leading-relaxed" data-testid="agent-access-action-error">
      {actionError}
    </p>
  {/if}

  <div class="flex items-center gap-2">
    <label class="text-muted text-xs" for="agent-access-port">Port</label>
    <Input
      id="agent-access-port"
      class="h-7 w-24 font-mono text-xs"
      inputmode="numeric"
      data-testid="agent-access-port"
      disabled={pending}
      bind:value={portDraft}
      onkeydown={(event) => {
        if (event.key === "Enter") applyPort();
      }}
      onblur={applyPort}
    />
    <span class="text-muted text-xs">
      Clients connect to this port on 127.0.0.1. Change it if another program uses it.
    </span>
  </div>

  {#if status.token}
    <div
      class="border-border bg-panel flex min-h-12 items-center gap-2 rounded-md border px-2.5 py-2"
    >
      <div class="min-w-0 flex-1">
        <div class="text-muted text-xs leading-4">Token</div>
        <div
          class="text-fg truncate font-mono text-[11px] leading-4"
          data-testid="agent-access-token"
        >
          {tokenVisible ? status.token : masked(status.token)}
        </div>
      </div>
      <Button
        variant="ghost"
        size="sm"
        data-testid="agent-access-token-reveal"
        onclick={() => (tokenVisible = !tokenVisible)}
      >
        {tokenVisible ? "Hide" : "Show"}
      </Button>
      <CopyButton text={status.token} label="Copy token" testid="agent-access-token-copy" />
      <Button
        variant="secondary"
        size="sm"
        disabled={pending}
        data-testid="agent-access-regenerate"
        onclick={regenerate}
      >
        Regenerate
      </Button>
    </div>
    <p class="text-muted text-xs leading-relaxed">
      Regenerating replaces the token and disconnects every client at once. Each client keeps its
      own copy of the token in its configuration, so give each the new one.
    </p>
  {/if}

  {#if snippets}
    <div class="space-y-3" data-testid="agent-access-clients">
      <Disclosure id="agent-access-claude-code-setup" label="Connect Claude Code">
        <div class="flex items-center justify-between gap-2">
          <div class="text-fg text-sm font-medium">Claude Code</div>
          <CopyButton
            text={snippets.claudeCode}
            label="Copy Claude Code command"
            testid="agent-access-claude-code-copy"
          />
        </div>
        <pre
          class="bg-raised text-fg overflow-x-auto rounded-md px-3 py-2 font-mono text-[11px] leading-relaxed break-all whitespace-pre-wrap"
          data-testid="agent-access-claude-code">{shownSnippets?.claudeCode}</pre>
        <p class="text-muted text-xs leading-relaxed">
          Run this in the folder you'll orchestrate from, and choose a folder that isn't any
          project's folder. Claude Code's MCP configuration applies per folder, and the agents
          Switchboard launches run Claude Code in their project's folder, so registering it there
          would give them these tools too. For the same reason, don't add
          <span class="font-mono">--scope user</span>.
        </p>
        <p class="text-muted text-xs leading-relaxed" data-testid="agent-access-claude-code-readd">
          After regenerating the token or changing the port, run
          <span class="font-mono">claude mcp remove switchboard --scope local</span> in that folder first:
          adding a name that already exists fails.
        </p>
      </Disclosure>

      <Disclosure id="agent-access-codex-setup" label="Connect Codex">
        <div class="flex items-center justify-between gap-2">
          <div class="text-fg text-sm font-medium">Codex</div>
          <CopyButton
            text={snippets.codexConfig}
            label="Copy Codex configuration"
            testid="agent-access-codex-copy"
          />
        </div>
        <pre
          class="bg-raised text-fg overflow-x-auto rounded-md px-3 py-2 font-mono text-[11px] leading-relaxed whitespace-pre"
          data-testid="agent-access-codex">{shownSnippets?.codexConfig}</pre>
        <p class="text-muted text-xs leading-relaxed">
          Put this in <span class="font-mono">.codex/config.toml</span> inside a folder Codex trusts
          that isn't any project's folder, not in
          <span class="font-mono">~/.codex/config.toml</span>, which every Codex agent Switchboard
          launches reads. Then set the token in the shell you start Codex from with this line:
        </p>
        <div class="flex items-center gap-2">
          <pre
            class="bg-raised text-fg min-w-0 flex-1 overflow-x-auto rounded-md px-3 py-2 font-mono text-[11px] leading-relaxed whitespace-pre"
            data-testid="agent-access-codex-env">{shownSnippets?.codexEnvironment}</pre>
          <CopyButton
            text={snippets.codexEnvironment}
            label={`Copy the ${CODEX_TOKEN_VARIABLE} line`}
            testid="agent-access-codex-env-copy"
          />
        </div>
        <p class="text-muted text-xs leading-relaxed" data-testid="agent-access-codex-token-file">
          Keep that line in a file only you can read, not in your shell profile itself: a profile
          such as <span class="font-mono">~/.zshrc</span> can be read by other accounts on this Mac.
          Run
          <span class="font-mono">touch ~/.switchboard-token && chmod 600 ~/.switchboard-token</span
          >
          first, paste the line into that file, and add
          <span class="font-mono">source ~/.switchboard-token</span> to your profile. After regenerating
          the token, update that file and open a new shell.
        </p>
        <p class="text-muted text-xs leading-relaxed">
          Codex ends any tool call after 300 seconds. To let one wait longer, add
          <span class="font-mono">tool_timeout_sec = 900</span> to the block.
        </p>
      </Disclosure>

      <Disclosure id="agent-access-claude-desktop-setup" label="Connect Claude Desktop">
        <div class="flex items-center justify-between gap-2">
          <div class="text-fg text-sm font-medium">Claude Desktop</div>
          <CopyButton
            text={snippets.claudeDesktop}
            label="Copy Claude Desktop configuration"
            testid="agent-access-claude-desktop-copy"
          />
        </div>
        <pre
          class="bg-raised text-fg max-h-64 overflow-auto rounded-md px-3 py-2 font-mono text-[11px] leading-relaxed whitespace-pre"
          data-testid="agent-access-claude-desktop">{shownSnippets?.claudeDesktop}</pre>
        <p class="text-muted text-xs leading-relaxed">
          Add this to <span class="font-mono">claude_desktop_config.json</span> (Claude Desktop:
          Settings → Developer → Edit Config). Replace the two paths with yours: run
          <span class="font-mono">which npx</span> in a terminal, and use that path for
          <span class="font-mono">command</span> and its folder for
          <span class="font-mono">PATH</span>. Claude Desktop doesn't read your shell profile, so it
          needs both spelled out. It works in Chat and in Code with the Local environment; Code in
          the cloud can't reach your Mac.
        </p>
      </Disclosure>
    </div>
  {:else}
    <p class="text-muted text-xs leading-relaxed">
      Turn the Agent MCP Server on to create the token and see each client's configuration.
    </p>
  {/if}
{/if}
