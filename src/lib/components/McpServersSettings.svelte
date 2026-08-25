<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import Button from "$lib/components/ui/Button.svelte";
  import Input from "$lib/components/ui/Input.svelte";
  import SegmentedSelect from "$lib/components/ui/SegmentedSelect.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { SUPPLEMENTAL_TOOLTIP_DELAY } from "$lib/components/ui/tooltip";
  import {
    addMcpProvider,
    listMcpProviders,
    removeMcpProvider,
    signInMcpProvider,
    signOutMcpProvider,
    syncPrompts,
    testMcpConnection,
    testSavedMcpProvider,
  } from "$lib/api";
  import type { McpProviderInfo, ProviderStatus } from "$lib/types";
  import { cn } from "$lib/utils";

  let providers = $state<McpProviderInfo[]>([]);
  let loadError = $state<string | null>(null);

  // Add form.
  let name = $state("");
  let url = $state("");
  // Widened to string for SegmentedSelect's bindable value; only "bearer" and
  // "oauth" are ever assigned.
  let authMode = $state("bearer");
  let bearer = $state("");
  let adding = $state(false);
  let addError = $state<string | null>(null);

  // Pre-save probe (bearer-only: an OAuth server can't be probed before it's
  // saved and signed in). The result is stamped with what was actually tested
  // and rendered only while the form still matches — so editing the URL,
  // switching to OAuth, or a slow probe resolving after a switch can never
  // leave a verdict on screen for a configuration that was never tested.
  let testing = $state(false);
  let testResult = $state<{
    ok: boolean;
    message: string;
    url: string;
    bearer: string | null;
  } | null>(null);
  const testResultVisible = $derived(
    testResult !== null &&
      authMode === "bearer" &&
      testResult.url === url.trim() &&
      testResult.bearer === bearerOrNull(),
  );

  let syncing = $state(false);

  // Per-row in-flight action and outcome line. Sign-in is a browser
  // round-trip that can take minutes — the pending state must survive it, so
  // it is deliberately independent state, never derived from `providers`.
  type RowAction = "signing-in" | "signing-out" | "testing" | "removing";
  type Tone = "ok" | "accent" | "warning" | "error" | "muted";
  let rowAction = $state<Record<string, RowAction | undefined>>({});
  let rowNotice = $state<
    Record<string, { tone: Tone; text: string; transient?: boolean } | undefined>
  >({});

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
  const canSync = $derived(!syncing && providers.length > 0);

  function bearerOrNull(): string | null {
    return bearer.trim() === "" ? null : bearer.trim();
  }

  function isOauth(provider: McpProviderInfo): boolean {
    return provider.auth.type === "oauth";
  }

  function errorText(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  async function refresh(): Promise<void> {
    try {
      providers = await listMcpProviders();
      loadError = null;
    } catch (e) {
      loadError = errorText(e);
    }
  }

  // A background cache rebuild runs after add/remove/sign-in/sign-out; the
  // command returns before it finishes, so a row first reads a stale status.
  // Re-refresh when the backend signals the rebuild is done. Transient flow
  // notices ("Signed in.") are dropped then — the fresh status carries the
  // same fact — while errors and probe results persist until the user acts on
  // the row again.
  onMount(() => {
    refresh();
    let unlisten: (() => void) | undefined;
    void listen("prompts:synced", () => {
      for (const key of Object.keys(rowNotice)) {
        if (rowNotice[key]?.transient) rowNotice[key] = undefined;
      }
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
    authMode = "bearer";
    testResult = null;
    addError = null;
  }

  async function handleAdd(): Promise<void> {
    if (!canSubmit) return;
    adding = true;
    addError = null;
    try {
      const auth =
        authMode === "oauth" ? ({ type: "oauth" } as const) : ({ type: "bearer" } as const);
      await addMcpProvider(
        trimmedName,
        url.trim(),
        auth,
        authMode === "bearer" ? bearerOrNull() : null,
      );
      resetForm();
      await refresh();
    } catch (e) {
      addError = errorText(e);
    } finally {
      adding = false;
    }
  }

  async function handleTest(): Promise<void> {
    if (!urlValid) return;
    const testedUrl = url.trim();
    const testedBearer = bearerOrNull();
    testing = true;
    testResult = null;
    try {
      const count = await testMcpConnection(testedUrl, testedBearer);
      testResult = {
        ok: true,
        message: `Connected — ${count} prompt${count === 1 ? "" : "s"}.`,
        url: testedUrl,
        bearer: testedBearer,
      };
    } catch (e) {
      testResult = { ok: false, message: errorText(e), url: testedUrl, bearer: testedBearer };
    } finally {
      testing = false;
    }
  }

  async function startSignIn(providerName: string): Promise<void> {
    // Idempotence guard: the button disables while an action is pending, but
    // a double-click's second event can race the re-render (and test drivers
    // dispatch clicks regardless of `disabled`).
    if (rowAction[providerName] !== undefined) return;
    rowAction[providerName] = "signing-in";
    rowNotice[providerName] = undefined;
    let ok = false;
    try {
      await signInMcpProvider(providerName);
      ok = true;
    } catch (e) {
      rowNotice[providerName] = { tone: "error", text: errorText(e) };
    } finally {
      // Refresh even on failure: a failed re-sign-in can already have signed
      // the user out (the deliberate pre-browser token wipe), and the row
      // must not keep asserting the old world until the background sync's
      // event happens to land. The error notice set above must survive this.
      await refresh();
      if (ok) rowNotice[providerName] = { tone: "ok", text: "Signed in.", transient: true };
      rowAction[providerName] = undefined;
    }
  }

  async function handleSignOut(providerName: string): Promise<void> {
    // Idempotence guard: the button disables while an action is pending, but
    // a double-click's second event can race the re-render (and test drivers
    // dispatch clicks regardless of `disabled`).
    if (rowAction[providerName] !== undefined) return;
    rowAction[providerName] = "signing-out";
    rowNotice[providerName] = undefined;
    let ok = false;
    try {
      await signOutMcpProvider(providerName);
      ok = true;
    } catch (e) {
      rowNotice[providerName] = { tone: "error", text: errorText(e) };
    } finally {
      await refresh();
      if (ok) rowNotice[providerName] = { tone: "ok", text: "Signed out.", transient: true };
      rowAction[providerName] = undefined;
    }
  }

  async function handleRowTest(providerName: string): Promise<void> {
    // Idempotence guard: the button disables while an action is pending, but
    // a double-click's second event can race the re-render (and test drivers
    // dispatch clicks regardless of `disabled`).
    if (rowAction[providerName] !== undefined) return;
    rowAction[providerName] = "testing";
    rowNotice[providerName] = undefined;
    try {
      const status = await testSavedMcpProvider(providerName);
      // The probe reuses the status vocabulary, so its notice reuses the
      // status tone: "needs sign-in" renders accent, a store failure renders
      // warning — never folded into error red.
      rowNotice[providerName] = { tone: statusTone(status), text: probeLabel(status) };
    } catch (e) {
      rowNotice[providerName] = { tone: "error", text: errorText(e) };
    } finally {
      rowAction[providerName] = undefined;
    }
  }

  async function handleRemove(providerName: string): Promise<void> {
    // Idempotence guard: the button disables while an action is pending, but
    // a double-click's second event can race the re-render (and test drivers
    // dispatch clicks regardless of `disabled`).
    if (rowAction[providerName] !== undefined) return;
    rowAction[providerName] = "removing";
    rowNotice[providerName] = undefined;
    let ok = false;
    try {
      await removeMcpProvider(providerName);
      ok = true;
    } catch (e) {
      // Removal can be refused (a sign-in/sign-out is in progress) or fail on
      // the credential delete; the reason belongs on the row it concerns.
      rowNotice[providerName] = { tone: "error", text: errorText(e) };
    } finally {
      await refresh();
      if (ok) {
        // Row state is keyed by name and must not outlive the provider: a
        // re-added same-named server would otherwise inherit this row's
        // notice or an armed confirmation.
        delete rowAction[providerName];
        delete rowNotice[providerName];
      } else {
        rowAction[providerName] = undefined;
      }
    }
  }

  async function handleSync(): Promise<void> {
    if (!canSync) return;
    syncing = true;
    try {
      await syncPrompts();
      await refresh();
    } catch (e) {
      loadError = errorText(e);
    } finally {
      syncing = false;
    }
  }

  // One status → tone mapping shared by the status column and the row-notice
  // rendering, so the two surfaces cannot drift. The default arms keep the
  // wire-format rule (the Rust enum is #[non_exhaustive]; unknown
  // discriminants must degrade gracefully) without giving up compile-time
  // exhaustiveness for variants the TS union does know.
  const TONE_CLASS: Record<Tone, string> = {
    ok: "text-status-idle",
    accent: "text-accent",
    warning: "text-warning",
    error: "text-status-failed",
    muted: "text-muted",
  };

  function statusTone(status: ProviderStatus): Tone {
    switch (status.state) {
      case "ok":
        return "ok";
      case "errored":
        return "error";
      case "store_unavailable":
        return "warning";
      case "needs_auth":
        // Actionable ("sign in"), not a failure — its own styling, never
        // folded into error red.
        return "accent";
      case "unknown":
        return "muted";
      default: {
        status satisfies never;
        return "muted";
      }
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
      case "needs_auth":
        return "Needs sign-in";
      case "unknown":
        return "Not synced";
      default: {
        const _exhaustive: never = status;
        return (_exhaustive as { state: string }).state;
      }
    }
  }

  function statusTitle(status: ProviderStatus): string | undefined {
    return status.state === "errored" ? status.message : undefined;
  }

  /// The row-level Test outcome, in the same vocabulary as the status column.
  function probeLabel(status: ProviderStatus): string {
    switch (status.state) {
      case "ok":
        return `Connected — ${status.prompt_count} prompt${status.prompt_count === 1 ? "" : "s"}.`;
      case "errored":
        return status.message;
      case "store_unavailable":
        return "The OS keychain could not be read.";
      case "needs_auth":
        return "Needs sign-in.";
      case "unknown":
        return "Not synced.";
      default: {
        const _exhaustive: never = status;
        return (_exhaustive as { state: string }).state;
      }
    }
  }

  // Sign-in is pointless while the keychain itself can't be read — the flow
  // would fail at its own credential read. Disabled (not hidden), so the row
  // stays recognizably an OAuth row and the button self-heals when a sync
  // clears the status.
  function signInBlocked(provider: McpProviderInfo): boolean {
    return provider.status.state === "store_unavailable";
  }
</script>

<div class="space-y-4" data-testid="mcp-servers">
  {#if loadError}
    <p class="text-status-failed text-xs" data-testid="mcp-load-error">{loadError}</p>
  {/if}

  {#if providers.length > 0}
    <ul class="border-border divide-border divide-y border-y">
      {#each providers as provider (provider.name)}
        {@const action = rowAction[provider.name]}
        {@const notice = rowNotice[provider.name]}
        {@const providerStatusTitle = statusTitle(provider.status)}
        <li class="space-y-2 py-2" data-testid={`mcp-row-${provider.name}`}>
          <div class="flex items-center justify-between gap-3">
            <div class="min-w-0">
              <div class="text-fg flex items-center gap-2 text-sm">
                <span class="font-medium">{provider.name}</span>
                {#if providerStatusTitle}
                  <Tooltip label={providerStatusTitle} delayDuration={SUPPLEMENTAL_TOOLTIP_DELAY}>
                    {#snippet trigger(props)}
                      <span
                        {...props}
                        class={cn("text-xs", TONE_CLASS[statusTone(provider.status)])}
                        data-testid={`mcp-status-${provider.name}`}
                      >
                        {statusLabel(provider.status)}
                      </span>
                    {/snippet}
                  </Tooltip>
                {:else}
                  <span
                    class={cn("text-xs", TONE_CLASS[statusTone(provider.status)])}
                    data-testid={`mcp-status-${provider.name}`}
                  >
                    {statusLabel(provider.status)}
                  </span>
                {/if}
                {#if !provider.has_token}
                  <span class="text-muted text-xs"
                    >· {isOauth(provider) ? "not signed in" : "no token"}</span
                  >
                {/if}
              </div>
              <div class="text-muted truncate text-xs">{provider.url}</div>
            </div>
            <div class="flex shrink-0 items-center gap-2">
              {#if isOauth(provider)}
                <Tooltip
                  label="The OS keychain could not be read"
                  delayDuration={SUPPLEMENTAL_TOOLTIP_DELAY}
                  disabled={!signInBlocked(provider)}
                  focusable={signInBlocked(provider)}
                >
                  {#snippet trigger(props)}
                    <span {...props} class="inline-flex">
                      <Button
                        variant="secondary"
                        size="sm"
                        data-testid={`mcp-sign-in-${provider.name}`}
                        disabled={action !== undefined || signInBlocked(provider)}
                        onclick={() => void startSignIn(provider.name)}
                      >
                        {action === "signing-in" ? "Waiting for browser…" : "Sign in"}
                      </Button>
                    </span>
                  {/snippet}
                </Tooltip>
                {#if provider.has_token}
                  <Button
                    variant="secondary"
                    size="sm"
                    data-testid={`mcp-sign-out-${provider.name}`}
                    disabled={action !== undefined}
                    onclick={() => handleSignOut(provider.name)}
                  >
                    {action === "signing-out" ? "Signing out…" : "Sign out"}
                  </Button>
                  <Button
                    variant="secondary"
                    size="sm"
                    data-testid={`mcp-test-${provider.name}`}
                    disabled={action !== undefined}
                    onclick={() => handleRowTest(provider.name)}
                  >
                    {action === "testing" ? "Testing…" : "Test"}
                  </Button>
                {/if}
              {/if}
              <Button
                variant="danger"
                size="sm"
                data-testid={`mcp-remove-${provider.name}`}
                disabled={action !== undefined}
                onclick={() => handleRemove(provider.name)}
              >
                {action === "removing" ? "Removing…" : "Remove"}
              </Button>
            </div>
          </div>
          {#if notice}
            <p
              class={cn("text-xs", TONE_CLASS[notice.tone])}
              data-testid={`mcp-notice-${provider.name}`}
            >
              {notice.text}
            </p>
          {/if}
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
      disabled={!canSync}
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
    <div class="space-y-1">
      <span class="text-muted text-xs">Authentication</span>
      <SegmentedSelect
        bind:value={authMode}
        options={[
          { label: "Bearer token", value: "bearer" },
          { label: "OAuth sign-in", value: "oauth" },
        ]}
        ariaLabel="Authentication mode"
        testid="mcp-auth-mode"
      />
      <p class="text-muted text-xs" data-testid="mcp-auth-hint">
        {#if authMode === "bearer"}
          Paste a token issued by the server; it's stored in your OS keychain. A token is the path
          for headless or scripted use.
        {:else}
          OAuth with dynamic client registration: after adding the server, sign in with your browser
          from its row. Nothing to paste.
        {/if}
      </p>
    </div>
    {#if authMode === "bearer"}
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
    {/if}
    {#if testResult !== null && testResultVisible}
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
    <div class="flex items-center justify-end gap-2">
      {#if authMode === "bearer"}
        <Button
          variant="secondary"
          size="sm"
          data-testid="mcp-test"
          disabled={!urlValid || testing}
          onclick={handleTest}
        >
          {testing ? "Testing…" : "Test connection"}
        </Button>
      {:else}
        <span class="text-muted min-w-0 flex-1 text-left text-xs" data-testid="mcp-oauth-test-hint">
          A connection test needs credentials — add the server, sign in from its row, then use its
          Test action.
        </span>
      {/if}
      <Button
        size="sm"
        class="shrink-0"
        data-testid="mcp-add"
        disabled={!canSubmit}
        onclick={handleAdd}
      >
        {adding ? "Adding…" : "Add server"}
      </Button>
    </div>
  </div>
</div>
