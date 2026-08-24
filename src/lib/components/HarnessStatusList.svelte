<script lang="ts">
  import * as api from "$lib/api";
  import type { HarnessKind, RecheckOutcome } from "$lib/types";
  import {
    harnessAvailability,
    isPathDegraded,
    recheckHarnessAvailability,
    refreshHarnessAvailability,
  } from "$lib/harnessAvailability.svelte";
  import Button from "./ui/Button.svelte";
  import {
    ALL_HARNESSES,
    HARNESS_ORDER_GEMINI_LAST,
    HARNESS_SETUP_URL,
    HARNESS_LABEL,
    HARNESS_LOGIN_HINT,
  } from "$lib/harnessDisplay";
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

  async function probeAuth(harness: HarnessKind, generation: number): Promise<void> {
    try {
      await AUTH_PROBE[harness]();
      if (generation === authGeneration) authed[harness] = true;
    } catch {
      // A rejected probe means "not authenticated" (or the probe couldn't
      // run) — a hint, never a hard error. The send path is authoritative.
      if (generation === authGeneration) authed[harness] = false;
    }
  }

  function refresh(): void {
    void refreshHarnessAvailability();
    const generation = authGeneration;
    for (const harness of ALL_HARNESSES) void probeAuth(harness, generation);
  }

  let rechecking = $state(false);

  /// Detection is PATH-based, and that PATH is read from the user's login shell
  /// once per app launch. If the read failed — most likely when the app was
  /// relaunched automatically at login, competing with everything else macOS
  /// starts — every harness reads as "not installed" and an ordinary refresh
  /// keeps reporting the same thing. This forces a re-read, which is the only
  /// in-app recovery short of quitting the app.
  ///
  /// Auth is probed after install resolves, and gated on the same generation:
  /// an older visibility-triggered probe must not overwrite this one's result.
  let authGeneration = 0;

  /// Outcome of the last recheck (the Refresh button): `null` before one has
  /// run. Distinguishes "the re-read worked" from "the re-read failed" from
  /// "the recheck itself couldn't run" — only the middle one is about the
  /// user's shell, and only the failure states render a note.
  let recheckOutcome = $state<RecheckOutcome | null>(null);

  async function recheck(): Promise<void> {
    if (rechecking) return;
    rechecking = true;
    authGeneration += 1;
    const generation = authGeneration;
    for (const harness of ALL_HARNESSES) authed[harness] = null;
    try {
      recheckOutcome = await recheckHarnessAvailability();
    } catch (err) {
      console.warn("[switchboard] recheck failed:", err);
      recheckOutcome = { kind: "failed" };
    } finally {
      // Released as soon as the *PATH re-read* is done, which is what the button
      // promises. Auth is a separate best-effort hint with its own "Checking…"
      // in its own column, so gating on it left the button disabled after every
      // row had already settled — the button reporting work it isn't doing.
      rechecking = false;
    }
    // Deliberately un-awaited: fired after the install answer lands (auth is
    // moot for a CLI that isn't there) but not part of what the button waits on.
    for (const harness of ALL_HARNESSES) void probeAuth(harness, generation);
  }

  /// Detection ran against a best-guess PATH because the shell read failed.
  /// Surfaced rather than swallowed: a "Not installed" derived from a PATH we
  /// couldn't read is exactly the state users report as inexplicable.
  const pathDegraded = $derived(isPathDegraded());

  /// Whether the degraded PATH actually cost us anything. If every CLI was found
  /// anyway, the fallback did its job and there is nothing for the user to act
  /// on — warning them then is crying wolf, and it trains them to ignore the
  /// message on the day it means something.
  const missingAnything = $derived(
    ALL_HARNESSES.some((h) => harnessAvailability.availability(h).binary === "missing"),
  );

  /// A non-final recheck outcome (the re-read failed, or was still running) is
  /// cleared when the backend announces a completed PATH read — that event is
  /// precisely the recovery the note tells the user to wait or retry for, and
  /// without this a single transient failure would pin an alarming "restart the
  /// app" message above a healthy list for the rest of the session. A
  /// *successful* outcome stays: a later completed read only reaffirms it.
  function clearSupersededRecheckOutcome(): void {
    if (recheckOutcome === null) return;
    if (recheckOutcome.kind === "failed" || recheckOutcome.source === "capturing") {
      recheckOutcome = null;
    }
  }

  // Probe on mount and whenever the window regains visibility — installing a
  // CLI or logging in via the terminal and returning updates the list with no
  // manual reload. Both listeners clean up when this component unmounts; the
  // event subscription resolves asynchronously, so an unmount racing the
  // registration unsubscribes as soon as the handle arrives. A failed
  // subscription is best-effort: a stale outcome note then lingers until the
  // next recheck, which is the behavior this listener exists to improve on,
  // not a new failure mode.
  $effect(() => {
    refresh();
    const onVisibility = (): void => {
      if (document.visibilityState === "visible") refresh();
    };
    document.addEventListener("visibilitychange", onVisibility);
    let unmounted = false;
    let unlistenPathResolved: (() => void) | null = null;
    api
      .listenHarnessPathResolved(clearSupersededRecheckOutcome)
      .then((unlisten) => {
        if (unmounted) unlisten();
        else unlistenPathResolved = unlisten;
      })
      .catch((err: unknown) => {
        console.warn("[switchboard] recheck-outcome listener failed to attach:", err);
      });
    return () => {
      unmounted = true;
      unlistenPathResolved?.();
      document.removeEventListener("visibilitychange", onVisibility);
    };
  });

  function openSetup(harness: HarnessKind): void {
    void api.openExternalUrl(HARNESS_SETUP_URL[harness]);
  }
</script>

<div class="flex flex-col gap-2">
  <ul
    data-testid="harness-status"
    class="harness-status-container border-border divide-border/60 flex flex-col divide-y rounded-lg border"
  >
    {#each HARNESS_ORDER_GEMINI_LAST as harness (harness)}
      {@const install = harnessAvailability.status(harness)}
      <!-- Derived, not read straight off `install.installed`, so this list obeys
           the same provisional-result rule as gating: a negative answer taken
           while the PATH is still being read shows as "Checking…", not as a
           confident "Not installed". -->
      {@const binary = harnessAvailability.availability(harness).binary}
      {@const probeFailed = binary === "probe_failed"}
      {@const installing = binary === "checking"}
      {@const installed = binary === "available"}
      <li
        data-testid={`harness-row-${harness}`}
        class="harness-status-row grid items-center gap-x-3 px-3 py-2.5"
      >
        <HarnessIcon {harness} size="md" />
        <span class="text-fg text-sm font-medium" data-testid={`harness-label-${harness}`}
          >{HARNESS_LABEL[harness]}</span
        >

        <!-- Install column -->
        <span
          class="harness-install-cell flex min-w-0 items-baseline gap-1 text-xs"
          data-testid={`harness-install-${harness}`}
        >
          {#if installing}
            <span class="text-muted whitespace-nowrap">Checking…</span>
          {:else if probeFailed}
            <!-- The check itself failed, so we don't know. Offering a Setup guide
                 here would tell the user to install what they may already have. -->
            <span class="text-warning whitespace-nowrap">Couldn't check</span>
          {:else if installed}
            <span class="text-fg shrink-0">Installed</span>
            {#if install?.version != null}
              <span class="text-muted min-w-0 truncate" data-testid={`harness-version-${harness}`}
                >v{install.version}</span
              >
            {/if}
          {:else}
            <span class="text-warning whitespace-nowrap">Not installed</span>
          {/if}
        </span>

        <!-- Auth/action column. Setup belongs here rather than beside the
             install status so every row keeps the same column alignment. -->
        <span class="harness-auth-cell flex min-w-0 items-center text-xs">
          <span data-testid={`harness-auth-${harness}`}>
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
          {#if !installing && !probeFailed && !installed}
            <span class="shrink-0">
              <button
                type="button"
                data-testid={`harness-setup-${harness}`}
                class="text-fg border-border hover:bg-panel inline-flex items-center gap-1 rounded-md border px-2 py-0.5 font-medium whitespace-nowrap"
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

        {#if harness === "gemini"}
          <!-- Full-row availability note (spans every grid column).
             Individual-tier Gemini access moved to Antigravity on 2026-06-18;
             see docs/harness-update-review.md for the tier terminology. -->
          <p
            class="harness-note text-muted pt-1 text-xs leading-5"
            data-testid="harness-note-gemini"
          >
            Gemini is no longer available on individual Google accounts — replaced by Antigravity.
            It still works if you have an organization plan (Gemini Code Assist Standard or
            Enterprise).
          </p>
        {/if}
      </li>
    {/each}
  </ul>

  <!-- Detection reads the PATH Switchboard captured from your login shell when
       it started. That capture can fail on a slow launch, and the result sticks
       for the whole session — so an explicit re-capture (the Refresh button) is
       the only in-app way back from a wrongly-empty list. The note speaks only
       when something is wrong (re-read failed, degraded PATH with a missing
       CLI, read still running): a healthy list explains itself, and copy about
       login shells and PATHs is noise to a user who isn't hitting the problem. -->
  <div class="flex items-center justify-between gap-3">
    <p class="text-muted text-xs leading-5" data-testid="harness-path-note">
      {#if recheckOutcome?.kind === "failed"}
        Refresh couldn't run — that's a problem reaching Switchboard's own backend, not with your
        shell. Try again, and restart the app if it persists.
      {:else if pathDegraded && missingAnything}
        Couldn't read your shell's PATH, so this list only covers the usual install locations — a
        CLI installed elsewhere would show as not installed. Refresh to try reading it again.
      {:else if recheckOutcome?.source === "capturing"}
        Still reading your terminal's PATH — this list will update when it finishes.
      {/if}
    </p>
    <Button
      variant="secondary"
      size="sm"
      data-testid="harness-recheck"
      disabled={rechecking}
      onclick={() => void recheck()}
    >
      {rechecking ? "Refreshing…" : "Refresh"}
    </Button>
  </div>
</div>

<style>
  .harness-status-container {
    container-type: inline-size;
  }

  .harness-status-row {
    grid-template-columns: 1.5rem minmax(0, 1fr);
    row-gap: 0.25rem;
  }

  .harness-install-cell,
  .harness-auth-cell {
    grid-column: 2;
  }

  .harness-note {
    grid-column: 1 / -1;
  }

  @container (min-width: 24rem) {
    .harness-status-row {
      grid-template-columns: 1.5rem 5.5rem minmax(0, 1fr) minmax(0, 1.2fr);
      row-gap: 0;
    }

    .harness-install-cell,
    .harness-auth-cell {
      grid-column: auto;
    }
  }
</style>
