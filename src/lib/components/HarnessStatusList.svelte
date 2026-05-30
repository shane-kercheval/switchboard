<script lang="ts">
  import * as api from "$lib/api";
  import type { HarnessKind } from "$lib/types";
  import { harnessAvailability, refreshHarnessAvailability } from "$lib/harnessAvailability.svelte";
  import { HARNESS_SETUP_URL, HARNESS_LABEL, HARNESS_LOGIN_HINT } from "$lib/harnessDisplay";
  import HarnessIcon from "./ui/HarnessIcon.svelte";

  /// Per-harness install + auth status, shared by the no-project welcome
  /// surface and the Settings page. This is the proactive counterpart to
  /// reactive-auth — but only ever where the user opted to look (the welcome
  /// state, or the Settings page they navigated to), never as an interruptive
  /// mid-work banner. Auth marks are presence heuristics, not validity checks —
  /// the authoritative test is a successful send; a not-signed-in row never
  /// blocks anything, and an API-key user may show "not signed in" yet send
  /// fine. Version is shown without any "update available" detection (the CLIs
  /// self-update; a remote latest-version comparison is maintenance burden we
  /// don't take on). Claude's auth heuristic is macOS-only (Keychain presence).

  const HARNESSES: HarnessKind[] = ["claude_code", "codex", "gemini", "antigravity"];

  // Install/version come from the shared `harnessAvailability` store (read in
  // the template). Only auth is local: it's deliberately not in the store
  // (v1 keeps auth reactive) and is a best-effort display hint here.
  let authed = $state<Record<HarnessKind, boolean | null>>({
    claude_code: null,
    codex: null,
    gemini: null,
    antigravity: null,
  });

  const AUTH_PROBE: Record<HarnessKind, () => Promise<void>> = {
    claude_code: api.checkClaudeAuth,
    codex: api.checkCodexAuth,
    gemini: api.checkGeminiAuth,
    antigravity: api.checkAntigravityAuth,
  };

  async function probeAuth(harness: HarnessKind): Promise<void> {
    try {
      await AUTH_PROBE[harness]();
      authed[harness] = true;
    } catch {
      // A rejected probe means "not authenticated" (or the probe couldn't
      // run) — a hint, never a hard error. The send path is authoritative.
      authed[harness] = false;
    }
  }

  function refresh(): void {
    void refreshHarnessAvailability();
    for (const harness of HARNESSES) void probeAuth(harness);
  }

  // Probe on mount and whenever the window regains visibility — installing a
  // CLI or logging in via the terminal and returning updates the list with no
  // manual reload. The listener cleans up when this component unmounts.
  $effect(() => {
    refresh();
    const onVisibility = (): void => {
      if (document.visibilityState === "visible") refresh();
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => document.removeEventListener("visibilitychange", onVisibility);
  });

  function openSetup(harness: HarnessKind): void {
    void api.openExternalUrl(HARNESS_SETUP_URL[harness]);
  }
</script>

<ul
  data-testid="harness-status"
  class="border-border divide-border/60 flex flex-col divide-y rounded-lg border"
>
  {#each HARNESSES as harness (harness)}
    {@const install = harnessAvailability.status(harness)}
    {@const installing = install === null}
    {@const installed = install?.installed === true}
    <li
      data-testid={`harness-row-${harness}`}
      class="grid grid-cols-[1.5rem_5.5rem_minmax(0,1fr)_minmax(0,1.2fr)] items-center gap-x-3 px-3 py-2.5"
    >
      <HarnessIcon {harness} size="md" />
      <span class="text-fg text-sm font-medium">{HARNESS_LABEL[harness]}</span>

      <!-- Install column -->
      <span class="text-xs" data-testid={`harness-install-${harness}`}>
        {#if installing}
          <span class="text-muted">Checking…</span>
        {:else if installed}
          <span class="text-fg">Installed</span>
          {#if install?.version != null}
            <span class="text-muted">v{install.version}</span>
          {/if}
        {:else}
          <span class="inline-flex items-center gap-2">
            <span class="text-warning">Not installed</span>
            <button
              type="button"
              data-testid={`harness-setup-${harness}`}
              class="text-fg border-border hover:bg-panel inline-flex items-center gap-1 rounded-md border px-2 py-0.5 font-medium"
              onclick={() => openSetup(harness)}
            >
              Setup guide
              <svg
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
                stroke-linejoin="round"
                class="h-3 w-3"
                aria-hidden="true"
              >
                <path d="M7 17 17 7M9 7h8v8" />
              </svg>
            </button>
          </span>
        {/if}
      </span>

      <!-- Auth column (absent when not installed — auth is moot) -->
      <span class="text-xs" data-testid={`harness-auth-${harness}`}>
        {#if installing || !installed}
          <!-- nothing: still checking, or install is the blocking step -->
        {:else if authed[harness] === null}
          <span class="text-muted">Checking…</span>
        {:else if authed[harness]}
          <span class="text-fg">Authenticated</span>
        {:else}
          <span class="text-warning">{HARNESS_LOGIN_HINT[harness]}</span>
        {/if}
      </span>
    </li>
  {/each}
</ul>
