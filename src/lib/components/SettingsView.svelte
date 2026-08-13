<script lang="ts">
  import { onMount } from "svelte";
  import { ChevronRight, FolderOpen } from "@lucide/svelte";
  import { theme, type ThemeMode } from "$lib/theme.svelte";
  import { agentCopy } from "$lib/agentCopy.svelte";
  import type { AgentCopyMode } from "$lib/agentCopyMode";
  import { cn } from "$lib/utils";
  import { ICON_BUTTON_CLASS, ICON_SIZE } from "$lib/components/ui/iconButton";
  import {
    SEGMENTED_CONTAINER_CLASS,
    SEGMENTED_ITEM_CLASS,
    SEGMENTED_ITEM_ACTIVE_CLASS,
    SEGMENTED_ITEM_INACTIVE_CLASS,
  } from "$lib/components/ui/segmentedControl";
  import HarnessStatusList from "$lib/components/HarnessStatusList.svelte";
  import AgentProfileEditor from "$lib/components/AgentProfileEditor.svelte";
  import Input from "$lib/components/ui/Input.svelte";
  import CopyButton from "$lib/components/ui/CopyButton.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import {
    loadPreferences,
    preferenceLoadState,
    preferences,
    saveStatus,
    updatePreferences,
  } from "$lib/preferences.svelte";
  import McpServersSettings from "$lib/components/McpServersSettings.svelte";
  import {
    localPromptsDir,
    openLocalPromptsDir,
    workflowsDir as workflowsDirApi,
    openWorkflowsDir,
    notificationAvailability,
  } from "$lib/api";
  import type { AgentProfile, HarnessKind, NotificationAvailability } from "$lib/types";
  import { ALL_HARNESSES, HARNESS_LABEL } from "$lib/harnessDisplay";
  import { workflowAuthoringPrompt } from "$lib/workflowAuthoring";

  let { onClose }: { onClose: () => void } = $props();
  // What macOS will actually do, as opposed to what the toggle below says. Only
  // the suppressed case is surfaced; `unavailable` (a dev build that isn't an
  // installed app) earns its place by *not* rendering that warning, since
  // "macOS is blocking notifications" would be wrong there.
  let notifyAvailability = $state<NotificationAvailability | null>(null);
  let promptsDir = $state<string | null>(null);
  let promptsDirError = $state<string | null>(null);
  let workflowsDir = $state<string | null>(null);
  let workflowsDirError = $state<string | null>(null);
  let workflowPromptOpen = $state(false);
  const workflowPrompt = $derived(workflowAuthoringPrompt(workflowsDir));

  const themeOptions: { mode: ThemeMode; label: string }[] = [
    { mode: "system", label: "System" },
    { mode: "light", label: "Light" },
    { mode: "dark", label: "Dark" },
  ];

  const copyOptions: { mode: AgentCopyMode; label: string }[] = [
    { mode: "last_answer_block", label: "Final Response" },
    { mode: "full_answer", label: "Entire Response" },
  ];

  const terminalOptions = [
    { label: "Terminal", value: "Terminal" },
    { label: "iTerm", value: "iTerm" },
  ] as const;

  // `note` adds a parenthetical clarifier rendered after the action — used where
  // the key isn't literal (the compose-bar number keys map to a chip's position,
  // not a fixed digit).
  const shortcuts: { action: string; keys: string[]; note?: string }[] = [
    { action: "Open command palette", keys: ["⌘", "⇧", "P"] },
    { action: "Focus message box", keys: ["⌘", "K"] },
    { action: "Add project", keys: ["⌘", "N"], note: "Projects view" },
    { action: "Add repository", keys: ["⌘", "N"], note: "Git view" },
    { action: "Add agent", keys: ["⌘", "⇧", "N"], note: "Projects view" },
    { action: "Refresh all repositories", keys: ["⌘", "R"], note: "Git view" },
    { action: "Jump to next unread project", keys: ["⌘", "G"] },
    { action: "Toggle Projects / Git view", keys: ["⌘", "⇧", "G"] },
    { action: "Show current project in Git view", keys: ["⌘", "⇧", "F"] },
    { action: "Open selection in editor", keys: ["⌘", "⇧", "E"] },
    { action: "Expand or restore Git details panel", keys: ["⌘", "⇧", "D"] },
    { action: "Toggle projects sidebar", keys: ["⌘", "B"] },
    { action: "Toggle agents sidebar", keys: ["⌘", "⇧", "B"] },
    { action: "Toggle Agents / Pins sidebar", keys: ["⌘", "⌥", "P"] },
    { action: "Toggle both sidebars", keys: ["⌘", "⌥", "B"] },
    { action: "Toggle settings", keys: ["⌘", ","] },
    {
      action: "Toggle a compose recipient",
      keys: ["⌘", "1–9"],
      note: "by chip position",
    },
    { action: "Select all compose recipients", keys: ["⌘", "⇧", "A"] },
    {
      action: "Send to pane",
      keys: ["⌘", "⌥", "1–9"],
      note: "by pane position, when split; shows the pane if hidden",
    },
    {
      action: "Cycle to previous / next pane",
      keys: ["⌘", "⇧", "[ / ]"],
      note: "by position, when split; shows the pane if hidden",
    },
    {
      action: "Send to clicked pane",
      keys: ["⌘", "Click"],
      note: "anywhere in the pane; hold ⌘ to preview",
    },
    {
      action: "Solo an agent in its pane",
      keys: ["⌥", "Click"],
      note: "on the eye toggle, agents sidebar",
    },
  ];

  /// Whether the last failed preference save involved any of `keys` — so the
  /// warning renders beside the control the user actually touched instead of in
  /// whichever section happens to own the renderer.
  function saveFailedFor(...keys: string[]): boolean {
    return saveStatus.error !== null && keys.some((k) => saveStatus.keys.includes(k));
  }

  const sectionClass = "border-border space-y-3 border-t pt-5";
  const sectionHeadingClass = "text-fg text-base font-semibold";

  onMount(() => {
    void loadPreferences();
    void localPromptsDir()
      .then((path) => {
        promptsDir = path;
        promptsDirError = null;
      })
      .catch((e: unknown) => {
        promptsDir = null;
        promptsDirError = e instanceof Error ? e.message : String(e);
      });
    void workflowsDirApi()
      .then((path) => {
        workflowsDir = path;
        workflowsDirError = null;
      })
      .catch((e: unknown) => {
        workflowsDir = null;
        workflowsDirError = e instanceof Error ? e.message : String(e);
      });
    void notificationAvailability()
      .then((a) => {
        notifyAvailability = a;
      })
      .catch((e: unknown) => {
        // A failed probe is not a failed feature: leave the hint unrendered
        // rather than claiming something is wrong that we couldn't check.
        notifyAvailability = null;
        console.error("[switchboard] notification availability check failed", e);
      });
  });

  function openPromptsDir(): void {
    void openLocalPromptsDir().catch((e: unknown) => {
      console.error("[switchboard] open local prompts folder failed", e);
    });
  }

  function openWorkflowsFolder(): void {
    void openWorkflowsDir().catch((e: unknown) => {
      console.error("[switchboard] open workflows folder failed", e);
    });
  }

  async function updateAgentDefault(
    harness: HarnessKind,
    slot: "primary" | "secondary",
    profile: AgentProfile | null,
  ): Promise<void> {
    await loadPreferences();
    const agentDefaults = structuredClone($state.snapshot(preferences.agent_defaults));
    if (slot === "primary" && profile !== null) {
      agentDefaults[harness].primary = profile;
    } else if (slot === "secondary") {
      agentDefaults[harness].secondary = profile;
    }
    await updatePreferences({ agent_defaults: agentDefaults });
  }
</script>

<div class="flex flex-1 overflow-y-auto px-8 pb-7" data-testid="settings-view">
  <div class="w-full max-w-2xl">
    <div class="flex h-16 items-center justify-between gap-3">
      <h1 class="text-fg text-2xl font-semibold">Settings</h1>
      <button
        type="button"
        class={ICON_BUTTON_CLASS}
        aria-label="Close settings"
        data-testid="settings-close"
        onclick={onClose}
      >
        <svg
          width={ICON_SIZE}
          height={ICON_SIZE}
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M18 6 6 18M6 6l12 12" />
        </svg>
      </button>
    </div>

    <section class={sectionClass}>
      <div>
        <h2 class={sectionHeadingClass}>Theme</h2>
      </div>

      <div
        class={cn(SEGMENTED_CONTAINER_CLASS, "inline-grid w-56 grid-cols-3")}
        role="radiogroup"
        aria-label="Theme"
      >
        {#each themeOptions as option (option.mode)}
          <button
            type="button"
            role="radio"
            class={cn(
              SEGMENTED_ITEM_CLASS,
              "flex items-center justify-center",
              theme.mode === option.mode
                ? SEGMENTED_ITEM_ACTIVE_CLASS
                : SEGMENTED_ITEM_INACTIVE_CLASS,
            )}
            aria-checked={theme.mode === option.mode}
            onclick={() => theme.set(option.mode)}
          >
            {option.label}
          </button>
        {/each}
      </div>
    </section>

    <section class={cn(sectionClass, "mt-7")} data-testid="agent-defaults-settings">
      <div>
        <h2 class={sectionHeadingClass}>Agent Defaults</h2>
        <p class="text-muted mt-1 text-sm leading-relaxed">
          Choose the initial model settings for new agents and projects. An optional secondary
          configuration makes it easy to switch an agent between two setups.
        </p>
      </div>

      {#if preferenceLoadState.ready}
        <div class="space-y-2">
          {#each ALL_HARNESSES as harness (harness)}
            <details
              class="border-border bg-raised rounded-md border"
              open={harness === "claude_code"}
            >
              <summary
                class="text-fg hover:bg-hover cursor-pointer list-none rounded-md px-3 py-2.5 text-sm font-medium"
              >
                {HARNESS_LABEL[harness]}
              </summary>
              <div class="border-border border-t p-3">
                <AgentProfileEditor
                  {harness}
                  bind:primary={
                    () => preferences.agent_defaults[harness].primary,
                    (value) => updateAgentDefault(harness, "primary", value)
                  }
                  bind:secondary={
                    () => preferences.agent_defaults[harness].secondary,
                    (value) => updateAgentDefault(harness, "secondary", value)
                  }
                  testidPrefix={`settings-profile-${harness}`}
                />
              </div>
            </details>
          {/each}
        </div>
      {:else}
        <div
          class="border-border bg-raised text-muted rounded-md border px-3 py-3 text-sm"
          data-testid="agent-defaults-loading"
        >
          Loading agent defaults…
        </div>
      {/if}

      {#if saveFailedFor("agent_defaults")}
        <p class="text-status-failed text-xs" data-testid="agent-defaults-save-error">
          Couldn't save agent defaults. Your changes still apply for this session.
        </p>
      {/if}
    </section>

    <section class={cn(sectionClass, "mt-7")}>
      <div>
        <h2 class={sectionHeadingClass}>Agent Message Copy Behavior</h2>
        <p class="text-muted mt-1 text-sm leading-relaxed">
          Agent responses can include progress notes before tool calls, then a final response after
          the work is done. Choose what the copy button captures.
        </p>
      </div>

      <div
        class={cn(SEGMENTED_CONTAINER_CLASS, "inline-grid w-72 grid-cols-2")}
        role="radiogroup"
        aria-label="Agent message copy"
      >
        {#each copyOptions as option (option.mode)}
          <button
            type="button"
            role="radio"
            class={cn(
              SEGMENTED_ITEM_CLASS,
              "flex items-center justify-center",
              agentCopy.mode === option.mode
                ? SEGMENTED_ITEM_ACTIVE_CLASS
                : SEGMENTED_ITEM_INACTIVE_CLASS,
            )}
            aria-checked={agentCopy.mode === option.mode}
            onclick={() => agentCopy.set(option.mode)}
          >
            {option.label}
          </button>
        {/each}
      </div>
    </section>

    <section class={cn(sectionClass, "mt-7")} data-testid="external-app-prefs">
      <div>
        <h2 class={sectionHeadingClass}>External Apps</h2>
        <p class="text-muted mt-1 text-sm leading-relaxed">
          Used when opening projects and worktrees, and when resuming an agent interactively.
        </p>
      </div>

      <div class="space-y-1.5">
        <label for="external-editor-command" class="text-muted block text-xs">Editor command</label>
        <Input
          id="external-editor-command"
          data-testid="external-editor-command"
          placeholder="code"
          value={preferences.editor_command ?? ""}
          disabled={!preferenceLoadState.ready}
          onchange={(e: Event) => {
            const v = (e.currentTarget as HTMLInputElement).value.trim();
            void updatePreferences({ editor_command: v === "" ? null : v });
          }}
        />
      </div>

      <div class="space-y-1.5">
        <span id="external-terminal-app-label" class="text-muted block text-xs">Terminal app</span>
        <div
          class={cn(SEGMENTED_CONTAINER_CLASS, "inline-grid w-56 grid-cols-2")}
          role="radiogroup"
          aria-labelledby="external-terminal-app-label"
          aria-disabled={!preferenceLoadState.ready}
          data-testid="external-terminal-app"
          data-value={preferences.terminal_app}
        >
          {#each terminalOptions as option (option.value)}
            <button
              type="button"
              role="radio"
              class={cn(
                SEGMENTED_ITEM_CLASS,
                "flex items-center justify-center",
                preferences.terminal_app === option.value
                  ? SEGMENTED_ITEM_ACTIVE_CLASS
                  : SEGMENTED_ITEM_INACTIVE_CLASS,
              )}
              aria-checked={preferences.terminal_app === option.value}
              disabled={!preferenceLoadState.ready}
              data-testid={`external-terminal-app-option-${option.value.toLowerCase()}`}
              onclick={() => void updatePreferences({ terminal_app: option.value })}
            >
              {option.label}
            </button>
          {/each}
        </div>
      </div>

      {#if saveFailedFor("editor_command", "terminal_app")}
        <p
          class="text-status-failed text-xs leading-relaxed"
          data-testid="external-apps-save-error"
        >
          Couldn't save your external app preferences ({saveStatus.error}). The change applies for
          now but may not survive a restart.
        </p>
      {/if}
    </section>

    <section class={cn(sectionClass, "mt-7")}>
      <h2 class={sectionHeadingClass}>Shortcuts</h2>

      <div>
        {#each shortcuts as shortcut (shortcut.action)}
          <div class="flex min-h-11 items-center justify-between gap-4 py-2">
            <div class="text-fg text-sm" data-testid="shortcut-action">
              {shortcut.action}{#if shortcut.note}<span class="text-muted ml-1 text-xs"
                  >({shortcut.note})</span
                >{/if}
            </div>
            <div class="flex items-center gap-1">
              {#each shortcut.keys as key, index (`${shortcut.action}-${index}`)}
                <kbd
                  class="border-border bg-raised text-muted flex h-6 min-w-6 items-center justify-center rounded border px-1.5 font-mono text-[11px] font-medium"
                >
                  {key}
                </kbd>
              {/each}
            </div>
          </div>
        {/each}
      </div>
    </section>

    <section class={cn(sectionClass, "mt-7")}>
      <div>
        <h2 class={sectionHeadingClass}>Supported CLIs</h2>
        <p class="text-muted mt-1 text-sm leading-relaxed">
          The AI coding CLIs Switchboard runs. Install and sign in to the ones you want to use.
        </p>
      </div>
      <HarnessStatusList />
    </section>

    <section class={cn(sectionClass, "mt-7")} data-testid="notification-prefs">
      <div>
        <h2 class={sectionHeadingClass}>Notifications</h2>
        <p class="text-muted mt-1 text-sm leading-relaxed">
          Switchboard can post a macOS notification when your agents finish — when every recipient
          of a send has responded, and when a workflow run reaches its end.
        </p>
        <p class="text-muted mt-2 text-sm leading-relaxed">
          While you're working in Switchboard, the project on screen never notifies. Other projects
          stay quiet too, unless you turn on the setting below.
        </p>
        <p class="text-muted mt-2 text-sm leading-relaxed">
          Whether a notification appears as a banner, plays a sound, or both is set in System
          Settings → Notifications → Applications → Switchboard.
        </p>
      </div>

      <div class="flex items-start justify-between gap-4">
        <div class="min-w-0">
          <div class="text-fg text-sm">Notify me when agents finish</div>
          <p class="text-muted mt-0.5 text-xs leading-relaxed">
            Turn this off to stop all notifications, including the sound.
          </p>
        </div>
        <button
          type="button"
          role="switch"
          disabled={!preferenceLoadState.ready}
          aria-checked={preferences.notify_on_completion}
          aria-label="Notify me when agents finish"
          data-testid="notify-toggle"
          class={cn(
            "relative mt-0.5 inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors outline-none",
            preferenceLoadState.ready ? "cursor-pointer" : "cursor-not-allowed opacity-50",
            preferences.notify_on_completion ? "bg-accent" : "bg-active",
          )}
          onclick={() =>
            void updatePreferences({ notify_on_completion: !preferences.notify_on_completion })}
        >
          <span
            class={cn(
              "bg-raised inline-block h-4 w-4 transform rounded-full transition-transform",
              preferences.notify_on_completion ? "translate-x-4" : "translate-x-0.5",
            )}
          ></span>
        </button>
      </div>

      <div class="flex items-start justify-between gap-4">
        <div class="min-w-0">
          <div class={cn("text-sm", preferences.notify_on_completion ? "text-fg" : "text-muted")}>
            Also notify me about other projects while I'm using Switchboard
          </div>
          <p class="text-muted mt-0.5 text-xs leading-relaxed">
            The projects sidebar shows a checkmark when a project finishes in the background. Turn
            this on to get a notification too.
          </p>
        </div>
        <button
          type="button"
          role="switch"
          disabled={!preferenceLoadState.ready || !preferences.notify_on_completion}
          aria-checked={preferences.notify_while_focused}
          aria-label="Also notify me about other projects while I'm using Switchboard"
          data-testid="notify-while-focused-toggle"
          class={cn(
            "relative mt-0.5 inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors outline-none",
            preferenceLoadState.ready && preferences.notify_on_completion
              ? "cursor-pointer"
              : "cursor-not-allowed opacity-50",
            preferences.notify_while_focused ? "bg-accent" : "bg-active",
          )}
          onclick={() =>
            void updatePreferences({ notify_while_focused: !preferences.notify_while_focused })}
        >
          <span
            class={cn(
              "bg-raised inline-block h-4 w-4 transform rounded-full transition-transform",
              preferences.notify_while_focused ? "translate-x-4" : "translate-x-0.5",
            )}
          ></span>
        </button>
      </div>

      {#if saveFailedFor("notify_on_completion", "notify_while_focused")}
        <p class="text-status-failed text-xs leading-relaxed" data-testid="notify-save-error">
          Couldn't save your preferences ({saveStatus.error}). The change applies for now but may
          not survive a restart.
        </p>
      {/if}

      {#if notifyAvailability === "suppressed"}
        <p class="text-status-failed text-xs leading-relaxed" data-testid="notify-suppressed">
          macOS is blocking notifications for Switchboard, so this setting has no effect. Turn them
          back on in System Settings → Notifications → Applications → Switchboard.
        </p>
      {/if}
    </section>

    <section class={cn(sectionClass, "mt-7")}>
      <div>
        <h2 class={sectionHeadingClass}>Built-in content</h2>
        <p class="text-muted mt-1 text-sm leading-relaxed">
          Switchboard ships read-only example prompts and workflows that live in the app, not your
          folders. To customize one, use “Copy to my prompts” / “Copy to my workflows” in its
          picker, then edit your copy.
        </p>
      </div>

      <div class="flex items-start justify-between gap-4">
        <div class="min-w-0">
          <div class="text-fg text-sm">Show built-in prompts &amp; workflows</div>
          <p class="text-muted mt-0.5 text-xs leading-relaxed">
            Turn this off to see only your own content in the prompt and workflow pickers.
          </p>
        </div>
        <button
          type="button"
          role="switch"
          disabled={!preferenceLoadState.ready}
          aria-checked={preferences.show_builtins}
          aria-label="Show built-in prompts and workflows"
          data-testid="show-builtins-toggle"
          class={cn(
            "relative mt-0.5 inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors outline-none",
            preferenceLoadState.ready ? "cursor-pointer" : "cursor-not-allowed opacity-50",
            preferences.show_builtins ? "bg-accent" : "bg-active",
          )}
          onclick={() => void updatePreferences({ show_builtins: !preferences.show_builtins })}
        >
          <span
            class={cn(
              "bg-raised inline-block h-4 w-4 transform rounded-full transition-transform",
              preferences.show_builtins ? "translate-x-4" : "translate-x-0.5",
            )}
          ></span>
        </button>
      </div>
    </section>

    <section class={cn(sectionClass, "mt-7")}>
      <div>
        <h2 class={sectionHeadingClass}>Workflows</h2>
        <p class="text-muted mt-1 text-sm leading-relaxed">
          Your workflows are user-global — the same set is available in every project. Drop YAML
          files in this folder (or use “Copy to my workflows” on a built-in), then invoke them from
          the compose bar's <span class="font-medium">Workflow</span> button.
        </p>
      </div>
      <div
        class="border-border bg-panel flex min-h-12 items-center gap-2 rounded-md border px-2.5 py-2"
      >
        <div class="min-w-0 flex-1">
          <div class="text-muted text-xs leading-4">Workflows folder</div>
          {#if workflowsDir}
            <div
              class="text-fg truncate font-mono text-[11px] leading-4"
              title={workflowsDir}
              data-testid="workflows-dir"
            >
              {workflowsDir}
            </div>
          {:else if workflowsDirError}
            <div class="text-status-failed truncate text-xs leading-4" title={workflowsDirError}>
              {workflowsDirError}
            </div>
          {:else}
            <div class="text-muted truncate text-xs leading-4">Loading…</div>
          {/if}
        </div>
        <Tooltip label="Open workflows folder in Finder" side="top">
          {#snippet trigger(props)}
            <button
              {...props}
              type="button"
              class={cn(ICON_BUTTON_CLASS, "shrink-0")}
              aria-label="Open workflows folder in Finder"
              data-testid="workflows-open"
              disabled={workflowsDir === null}
              onclick={openWorkflowsFolder}
            >
              <FolderOpen size={14} strokeWidth={1.8} aria-hidden="true" />
            </button>
          {/snippet}
        </Tooltip>
      </div>

      <div
        class="border-border bg-panel overflow-hidden rounded-md border"
        data-testid="workflow-authoring"
      >
        <div class="flex items-start gap-2 p-2.5">
          <button
            type="button"
            class="focus-visible:ring-focus flex min-w-0 flex-1 items-start gap-2 rounded-sm text-left outline-none focus-visible:ring-1"
            aria-expanded={workflowPromptOpen}
            aria-controls="workflow-authoring-prompt"
            data-testid="workflow-authoring-toggle"
            onclick={() => (workflowPromptOpen = !workflowPromptOpen)}
          >
            <ChevronRight
              size={15}
              strokeWidth={1.8}
              aria-hidden="true"
              class={cn(
                "text-muted mt-0.5 shrink-0 transition-transform",
                workflowPromptOpen && "rotate-90",
              )}
            />
            <span class="min-w-0">
              <span class="text-fg block text-sm font-medium">Create with an AI agent</span>
              <span class="text-muted mt-0.5 block text-xs leading-relaxed">
                Copy this prompt into a coding agent to create a custom workflow.
              </span>
            </span>
          </button>
          <Tooltip label="Copy Prompt" side="top">
            {#snippet trigger(props)}
              <span {...props} class="shrink-0">
                <CopyButton
                  text={workflowPrompt}
                  label="Copy workflow authoring prompt"
                  testid="workflow-authoring-copy"
                />
              </span>
            {/snippet}
          </Tooltip>
        </div>

        {#if workflowPromptOpen}
          <pre
            id="workflow-authoring-prompt"
            data-testid="workflow-authoring-prompt"
            class="border-border bg-surface text-fg max-h-80 overflow-auto border-t px-3 py-2.5 font-mono text-[11px] leading-relaxed break-words whitespace-pre-wrap">{workflowPrompt}</pre>
        {/if}
      </div>
    </section>

    <section class={cn(sectionClass, "mt-7")}>
      <div>
        <h2 class={sectionHeadingClass}>Prompt servers (MCP)</h2>
        <p class="text-muted mt-1 text-sm leading-relaxed">
          Add MCP servers that expose prompts (e.g. Tiddly). Their prompts become available to every
          agent via the compose bar. Bearer tokens are stored in your OS keychain, never in config.
        </p>
      </div>

      <div
        class="border-border bg-panel flex min-h-12 items-center gap-2 rounded-md border px-2.5 py-2"
      >
        <div class="min-w-0 flex-1">
          <div class="text-muted text-xs leading-4">Local prompts folder</div>
          {#if promptsDir}
            <div
              class="text-fg truncate font-mono text-[11px] leading-4"
              title={promptsDir}
              data-testid="local-prompts-dir"
            >
              {promptsDir}
            </div>
          {:else if promptsDirError}
            <div class="text-status-failed truncate text-xs leading-4" title={promptsDirError}>
              {promptsDirError}
            </div>
          {:else}
            <div class="text-muted truncate text-xs leading-4">Loading…</div>
          {/if}
        </div>
        <Tooltip label="Open local prompts folder in Finder" side="top">
          {#snippet trigger(props)}
            <button
              {...props}
              type="button"
              class={cn(ICON_BUTTON_CLASS, "shrink-0")}
              aria-label="Open local prompts folder in Finder"
              data-testid="local-prompts-open"
              disabled={promptsDir === null}
              onclick={openPromptsDir}
            >
              <FolderOpen size={14} strokeWidth={1.8} aria-hidden="true" />
            </button>
          {/snippet}
        </Tooltip>
      </div>
      <McpServersSettings />
    </section>

    <div class="h-10" aria-hidden="true"></div>
  </div>
</div>
