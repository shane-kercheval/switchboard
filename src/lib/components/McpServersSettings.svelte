<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import Button from "$lib/components/ui/Button.svelte";
  import Input from "$lib/components/ui/Input.svelte";
  import {
    addMcpProvider,
    listMcpProviders,
    removeMcpProvider,
    syncPrompts,
    testMcpConnection,
  } from "$lib/api";
  import type { McpProviderInfo, ProviderStatus } from "$lib/types";
  import { cn } from "$lib/utils";

  let providers = $state<McpProviderInfo[]>([]);
  let loadError = $state<string | null>(null);

  // Add form.
  let name = $state("");
  let url = $state("");
  let bearer = $state("");
  let adding = $state(false);
  let addError = $state<string | null>(null);

  // Test-connection probe (independent of saving).
  let testing = $state(false);
  let testResult = $state<{ ok: boolean; message: string } | null>(null);

  let syncing = $state(false);

  // Mirror the backend `is_valid_provider_name` rule + uniqueness, so the user
  // gets the error inline rather than from a rejected command.
  const RESERVED = "local";
  const trimmedName = $derived(name.trim());
  const nameError = $derived.by<string | null>(() => {
    if (trimmedName === "") return null; // don't nag an empty field
    if (trimmedName === RESERVED) return "`local` is reserved.";
    if (trimmedName.includes(":")) return "Name can't contain ':'.";
    if (providers.some((p) => p.name === trimmedName))
      return "A server with this name already exists.";
    return null;
  });
  const urlValid = $derived(/^https?:\/\//i.test(url.trim()));
  const canSubmit = $derived(!adding && trimmedName !== "" && nameError === null && urlValid);

  function bearerOrNull(): string | null {
    return bearer.trim() === "" ? null : bearer.trim();
  }

  async function refresh(): Promise<void> {
    try {
      providers = await listMcpProviders();
      loadError = null;
    } catch (e) {
      loadError = e instanceof Error ? e.message : String(e);
    }
  }

  // A background cache rebuild runs after add/remove; the command returns before
  // it finishes, so the new row first reads `Unknown`. Re-refresh when the
  // backend signals the rebuild is done to pick up the real status.
  onMount(() => {
    refresh();
    let unlisten: (() => void) | undefined;
    void listen("prompts:synced", () => {
      void refresh();
    }).then((u) => {
      unlisten = u;
    });
    return () => unlisten?.();
  });

  function resetForm(): void {
    name = "";
    url = "";
    bearer = "";
    testResult = null;
    addError = null;
  }

  async function handleAdd(): Promise<void> {
    if (!canSubmit) return;
    adding = true;
    addError = null;
    try {
      await addMcpProvider(trimmedName, url.trim(), bearerOrNull());
      resetForm();
      await refresh();
    } catch (e) {
      addError = e instanceof Error ? e.message : String(e);
    } finally {
      adding = false;
    }
  }

  async function handleTest(): Promise<void> {
    if (!urlValid) return;
    testing = true;
    testResult = null;
    try {
      const count = await testMcpConnection(url.trim(), bearerOrNull());
      testResult = { ok: true, message: `Connected — ${count} prompt${count === 1 ? "" : "s"}.` };
    } catch (e) {
      testResult = { ok: false, message: e instanceof Error ? e.message : String(e) };
    } finally {
      testing = false;
    }
  }

  async function handleRemove(providerName: string): Promise<void> {
    try {
      await removeMcpProvider(providerName);
      await refresh();
    } catch (e) {
      loadError = e instanceof Error ? e.message : String(e);
    }
  }

  async function handleSync(): Promise<void> {
    syncing = true;
    try {
      await syncPrompts();
      await refresh();
    } catch (e) {
      loadError = e instanceof Error ? e.message : String(e);
    } finally {
      syncing = false;
    }
  }

  function statusLabel(status: ProviderStatus): string {
    switch (status.state) {
      case "ok":
        return `${status.prompt_count} prompt${status.prompt_count === 1 ? "" : "s"}`;
      case "errored":
        return "Error";
      case "store_unavailable":
        return "Store unavailable";
      case "unknown":
        return "Not synced";
    }
  }

  function statusColor(status: ProviderStatus): string {
    switch (status.state) {
      case "ok":
        return "text-status-idle";
      case "errored":
        return "text-status-failed";
      case "store_unavailable":
        return "text-warning";
      case "unknown":
        return "text-muted";
    }
  }

  function statusTitle(status: ProviderStatus): string | undefined {
    return status.state === "errored" ? status.message : undefined;
  }
</script>

<div class="space-y-4" data-testid="mcp-servers">
  {#if loadError}
    <p class="text-status-failed text-xs" data-testid="mcp-load-error">{loadError}</p>
  {/if}

  {#if providers.length > 0}
    <ul class="border-border divide-border divide-y border-y">
      {#each providers as provider (provider.name)}
        <li
          class="flex items-center justify-between gap-3 py-2"
          data-testid={`mcp-row-${provider.name}`}
        >
          <div class="min-w-0">
            <div class="text-fg flex items-center gap-2 text-sm">
              <span class="font-medium">{provider.name}</span>
              <span
                class={cn("text-xs", statusColor(provider.status))}
                title={statusTitle(provider.status)}
                data-testid={`mcp-status-${provider.name}`}
              >
                {statusLabel(provider.status)}
              </span>
              {#if !provider.has_token}
                <span class="text-muted text-xs">· no token</span>
              {/if}
            </div>
            <div class="text-muted truncate text-xs">{provider.url}</div>
          </div>
          <Button
            variant="danger"
            size="sm"
            data-testid={`mcp-remove-${provider.name}`}
            onclick={() => handleRemove(provider.name)}
          >
            Remove
          </Button>
        </li>
      {/each}
    </ul>
  {:else}
    <p class="text-muted text-sm" data-testid="mcp-empty">No MCP servers configured.</p>
  {/if}

  <div>
    <Button
      variant="secondary"
      size="sm"
      data-testid="mcp-sync"
      disabled={syncing}
      onclick={handleSync}
    >
      {syncing ? "Syncing…" : "Sync prompts"}
    </Button>
  </div>

  <div class="border-border space-y-2 rounded-md border p-3" data-testid="mcp-add-form">
    <h3 class="text-fg text-sm font-medium">Add MCP server</h3>
    <label class="block space-y-1">
      <span class="text-muted text-xs">Name</span>
      <Input
        bind:value={name}
        placeholder="my-team"
        data-testid="mcp-name"
        class={cn("h-8 px-2", nameError && "border-status-failed")}
      />
      {#if nameError}
        <span class="text-status-failed block text-xs" data-testid="mcp-name-error"
          >{nameError}</span
        >
      {/if}
    </label>
    <label class="block space-y-1">
      <span class="text-muted text-xs">URL</span>
      <Input
        bind:value={url}
        placeholder="https://mcp.example.com/mcp"
        data-testid="mcp-url"
        class="h-8 px-2"
      />
    </label>
    <label class="block space-y-1">
      <span class="text-muted text-xs">Bearer token (optional)</span>
      <Input
        bind:value={bearer}
        type="password"
        placeholder="stored in your OS keychain"
        data-testid="mcp-bearer"
        class="h-8 px-2"
      />
    </label>
    {#if testResult}
      <p
        class={cn("text-xs", testResult.ok ? "text-muted" : "text-status-failed")}
        data-testid="mcp-test-result"
      >
        {testResult.message}
      </p>
    {/if}
    {#if addError}
      <p class="text-status-failed text-xs" data-testid="mcp-add-error">{addError}</p>
    {/if}
    <div class="flex justify-end gap-2">
      <Button
        variant="secondary"
        size="sm"
        data-testid="mcp-test"
        disabled={!urlValid || testing}
        onclick={handleTest}
      >
        {testing ? "Testing…" : "Test connection"}
      </Button>
      <Button size="sm" data-testid="mcp-add" disabled={!canSubmit} onclick={handleAdd}>
        {adding ? "Adding…" : "Add server"}
      </Button>
    </div>
  </div>
</div>
