<script lang="ts">
  import type { AgentRecord, HarnessAvailability, HarnessKind } from "$lib/types";
  import type { AgentFormSubmit } from "./CreateAgentForm.types";
  import { harnessUnavailableReason, isHarnessSelectable } from "$lib/harnessAvailability";
  import { HARNESS_DEFAULT_AGENT_NAME } from "$lib/harnessDisplay";
  import { normalizeAgentName, validateAgentName } from "$lib/agentName";
  import Button from "$lib/components/ui/Button.svelte";
  import Input from "$lib/components/ui/Input.svelte";
  import { cn } from "$lib/utils";
  import {
    SEGMENTED_CONTAINER_CLASS,
    SEGMENTED_ITEM_CLASS,
    SEGMENTED_ITEM_ACTIVE_CLASS,
    SEGMENTED_ITEM_INACTIVE_CLASS,
  } from "$lib/components/ui/segmentedControl";

  type Props = {
    busy?: boolean;
    error?: string | null;
    onSubmit: (submission: AgentFormSubmit) => void;
    /// Optional cancel callback. When provided, renders a Cancel button
    /// alongside Submit AND switches to "embedded" layout (no page-fill
    /// outer wrapper, no standalone heading) — the modal supplies its own
    /// title and centering via `Dialog`. Absence of this prop means
    /// "standalone phase" (the no-agent first-time flow), which keeps the
    /// existing page-fill card layout.
    onCancel?: () => void;
    /// Existing agents in the active project, for the live name-uniqueness
    /// check. Defaults to empty (the no-agent first-time flow has none, and
    /// tests that don't exercise uniqueness render unchanged).
    roster?: AgentRecord[];
    /// Optional per-harness availability. When provided, gates the radio
    /// buttons (and the submit button if the currently-selected harness
    /// is unavailable) on binary-presence only. Tooltip copy matches the
    /// banner copy in `App.svelte` verbatim so the user sees the same
    /// gap stated in both places. Defaults to "all harnesses available"
    /// so tests that don't care about gating render unchanged.
    claudeAvailability?: HarnessAvailability;
    codexAvailability?: HarnessAvailability;
    geminiAvailability?: HarnessAvailability;
    antigravityAvailability?: HarnessAvailability;
  };

  let {
    busy = false,
    error = null,
    onSubmit,
    onCancel,
    roster = [],
    claudeAvailability = { harness: "claude_code", binary: "available" },
    codexAvailability = { harness: "codex", binary: "available" },
    geminiAvailability = { harness: "gemini", binary: "available" },
    antigravityAvailability = { harness: "antigravity", binary: "available" },
  }: Props = $props();
  let name = $state<string>(HARNESS_DEFAULT_AGENT_NAME["claude_code"]);
  let harness = $state<HarnessKind>("claude_code");
  let mode = $state<"create" | "attach">("create");
  let existingSessionId = $state<string>("");

  /// Gate state per harness. Two predicates from `harnessAvailability`:
  /// - `isHarnessSelectable` — false for `"checking"` / `"missing"`;
  ///   gates radio `disabled` and the parent's submit button.
  /// - `harnessUnavailableReason` — message text for *real* missing
  ///   states; returns null for `"checking"` so the inline gating
  ///   message doesn't show during the brief probe window.
  ///
  /// The two predicates intentionally disagree on `"checking"`: the
  /// user is blocked, but no scary "Checking…" copy renders.
  const claudeSelectable = $derived(isHarnessSelectable(claudeAvailability));
  const codexSelectable = $derived(isHarnessSelectable(codexAvailability));
  const geminiSelectable = $derived(isHarnessSelectable(geminiAvailability));
  const antigravitySelectable = $derived(isHarnessSelectable(antigravityAvailability));
  const claudeReason = $derived(harnessUnavailableReason(claudeAvailability));
  const codexReason = $derived(harnessUnavailableReason(codexAvailability));
  const geminiReason = $derived(harnessUnavailableReason(geminiAvailability));
  const antigravityReason = $derived(harnessUnavailableReason(antigravityAvailability));
  const selectedSelectable = $derived(
    harness === "claude_code"
      ? claudeSelectable
      : harness === "codex"
        ? codexSelectable
        : harness === "gemini"
          ? geminiSelectable
          : antigravitySelectable,
  );
  const selectedReason = $derived(
    harness === "claude_code"
      ? claudeReason
      : harness === "codex"
        ? codexReason
        : harness === "gemini"
          ? geminiReason
          : antigravityReason,
  );

  /// UUID shape check (any version — Codex and Claude use v4 / v7
  /// respectively; the backend re-validates). This is a UX gate so the user
  /// sees the inline hint before pressing the submit button.
  const UUID_PATTERN =
    /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;

  const sessionIdValid = $derived(mode !== "attach" || UUID_PATTERN.test(existingSessionId.trim()));

  /// Live name validation against the format rules and the roster (shared with
  /// the backend's authoritative check via `$lib/agentName`). `nameError` is
  /// the visible message: suppressed for the `empty` reason so an empty field
  /// disables submit without nagging the user mid-edit — mirrors how the
  /// attach session-id hint only appears once the user has typed.
  const nameValidation = $derived(validateAgentName(name, roster));
  const nameError = $derived(
    nameValidation.ok || nameValidation.reason === "empty" ? null : nameValidation.message,
  );

  const canSubmit = $derived(!busy && nameValidation.ok && sessionIdValid && selectedSelectable);

  function handleSubmit(): void {
    const trimmedName = normalizeAgentName(name);
    if (mode === "create") {
      onSubmit({ mode: "create", name: trimmedName, harness });
    } else {
      onSubmit({
        mode: "attach",
        name: trimmedName,
        harness,
        existingSessionId: existingSessionId.trim(),
      });
    }
  }

  /// Drop the cross-mode field when toggling so a stale, invalid session-id
  /// (and its red validation hint) doesn't linger after the user switches
  /// to create and back. Done in the explicit click handlers rather than a
  /// `$effect` so the reset stays adjacent to the trigger and there's no
  /// hidden reactive dependency.
  function selectHarness(kind: HarnessKind): void {
    if (name === HARNESS_DEFAULT_AGENT_NAME[harness]) {
      name = HARNESS_DEFAULT_AGENT_NAME[kind];
    }
    harness = kind;
  }

  function selectMode(next: "create" | "attach"): void {
    if (next === mode) return;
    mode = next;
    if (next === "create") {
      existingSessionId = "";
    }
  }

  // Visual classes for a harness option (a native radio styled as a segmented
  // pill). Selection is driven off `harness`, gating off `selectable`.
  function harnessOptionClass(kind: HarnessKind, selectable: boolean): string {
    const selected = harness === kind;
    if (selected) return SEGMENTED_ITEM_ACTIVE_CLASS;
    if (!selectable) return "text-muted cursor-not-allowed opacity-60";
    return SEGMENTED_ITEM_INACTIVE_CLASS;
  }
</script>

<!--
  Layout shape switches on whether `onCancel` was provided:
  - **Standalone** (no `onCancel`): page-fill centered card + heading.
    Used by the no-agent first-time flow.
  - **Embedded** (`onCancel` provided): bare form fields. The modal
    wrapper supplies its own title and centering via `Dialog`.

  The form-field block (mode toggle → harness → name → optional UUID →
  error → action row) is identical across both layouts.
-->
{#snippet formBody()}
  <div class={cn(SEGMENTED_CONTAINER_CLASS, "flex")} role="tablist" data-testid="mode-toggle">
    <button
      type="button"
      class={cn(
        SEGMENTED_ITEM_CLASS,
        "flex-1",
        mode === "create" ? SEGMENTED_ITEM_ACTIVE_CLASS : SEGMENTED_ITEM_INACTIVE_CLASS,
      )}
      role="tab"
      aria-selected={mode === "create"}
      data-testid="mode-create"
      onclick={() => selectMode("create")}
      disabled={busy}
    >
      Create new
    </button>
    <button
      type="button"
      class={cn(
        SEGMENTED_ITEM_CLASS,
        "flex-1",
        mode === "attach" ? SEGMENTED_ITEM_ACTIVE_CLASS : SEGMENTED_ITEM_INACTIVE_CLASS,
      )}
      role="tab"
      aria-selected={mode === "attach"}
      data-testid="mode-attach"
      onclick={() => selectMode("attach")}
      disabled={busy}
    >
      Attach existing
    </button>
  </div>

  <fieldset class="space-y-1.5" disabled={busy}>
    <legend class="text-muted text-xs">Tool</legend>
    <!-- Native radios (real arrow-key + screen-reader semantics, grouped/labeled
         by the fieldset+legend) styled as a segmented control: the input is
         visually hidden and the label is the pill; `has-[:focus-visible]` lights
         the pill when the radio is keyboard-focused. -->
    <div class={cn(SEGMENTED_CONTAINER_CLASS, "grid grid-cols-4")} data-testid="harness-picker">
      <label
        class="{SEGMENTED_ITEM_CLASS} has-[:focus-visible]:ring-accent flex items-center justify-center has-[:focus-visible]:ring-2 {harnessOptionClass(
          'claude_code',
          claudeSelectable,
        )}"
        title={claudeReason ?? ""}
      >
        <input
          type="radio"
          name="harness"
          value="claude_code"
          class="sr-only"
          checked={harness === "claude_code"}
          disabled={busy || !claudeSelectable}
          onchange={() => selectHarness("claude_code")}
          data-testid="harness-claude"
        />
        Claude Code
      </label>
      <label
        class="{SEGMENTED_ITEM_CLASS} has-[:focus-visible]:ring-accent flex items-center justify-center has-[:focus-visible]:ring-2 {harnessOptionClass(
          'codex',
          codexSelectable,
        )}"
        title={codexReason ?? ""}
      >
        <input
          type="radio"
          name="harness"
          value="codex"
          class="sr-only"
          checked={harness === "codex"}
          disabled={busy || !codexSelectable}
          onchange={() => selectHarness("codex")}
          data-testid="harness-codex"
        />
        Codex
      </label>
      <label
        class="{SEGMENTED_ITEM_CLASS} has-[:focus-visible]:ring-accent flex items-center justify-center has-[:focus-visible]:ring-2 {harnessOptionClass(
          'gemini',
          geminiSelectable,
        )}"
        title={geminiReason ?? ""}
      >
        <input
          type="radio"
          name="harness"
          value="gemini"
          class="sr-only"
          checked={harness === "gemini"}
          disabled={busy || !geminiSelectable}
          onchange={() => selectHarness("gemini")}
          data-testid="harness-gemini"
        />
        Gemini
      </label>
      <label
        class="{SEGMENTED_ITEM_CLASS} has-[:focus-visible]:ring-accent flex items-center justify-center has-[:focus-visible]:ring-2 {harnessOptionClass(
          'antigravity',
          antigravitySelectable,
        )}"
        title={antigravityReason ?? ""}
      >
        <input
          type="radio"
          name="harness"
          value="antigravity"
          class="sr-only"
          checked={harness === "antigravity"}
          disabled={busy || !antigravitySelectable}
          onchange={() => selectHarness("antigravity")}
          data-testid="harness-antigravity"
        />
        Antigravity
      </label>
    </div>
    {#if selectedReason}
      <p class="text-status-failed text-xs" data-testid="harness-unavailable">
        {selectedReason}
      </p>
    {/if}
  </fieldset>

  <label class="block space-y-1">
    <span class="text-muted text-xs">Agent name</span>
    <Input
      bind:value={name}
      disabled={busy}
      data-testid="agent-name"
      class={cn("h-8 px-2", nameError && "border-status-failed")}
      aria-invalid={!nameValidation.ok}
      aria-describedby={nameError ? "agent-name-error" : undefined}
      title={nameError ?? undefined}
    />
    {#if nameError}
      <span
        id="agent-name-error"
        class="text-status-failed block text-xs"
        data-testid="agent-name-error"
      >
        {nameError}
      </span>
    {/if}
  </label>

  {#if mode === "attach"}
    <label class="block space-y-1">
      <span class="text-muted text-xs"> Existing session UUID </span>
      <Input
        bind:value={existingSessionId}
        disabled={busy}
        placeholder="00000000-0000-0000-0000-000000000000"
        data-testid="attach-session-id"
        class="h-8 px-2"
      />
      {#if existingSessionId.trim() !== "" && !sessionIdValid}
        <span class="text-status-failed block text-xs" data-testid="attach-session-id-error">
          Must be a UUID (8-4-4-4-12 hex characters).
        </span>
      {/if}
    </label>
  {/if}

  {#if error}
    <p class="text-status-failed text-xs" data-testid="error">{error}</p>
  {/if}
  <div class="flex justify-end gap-2">
    {#if onCancel}
      <Button
        variant="secondary"
        size="sm"
        data-testid="cancel-create-agent"
        disabled={busy}
        onclick={onCancel}
      >
        Cancel
      </Button>
    {/if}
    <Button
      size="sm"
      data-testid="confirm-create-agent"
      disabled={!canSubmit}
      onclick={handleSubmit}
    >
      {#if busy}
        {mode === "create" ? "Creating…" : "Attaching…"}
      {:else}
        {mode === "create" ? "Create agent" : "Attach agent"}
      {/if}
    </Button>
  </div>
{/snippet}

{#if onCancel}
  <!-- Embedded: bare form, modal supplies its own title/centering. -->
  <div class="space-y-3.5" data-testid="create-agent-form-embedded">
    {@render formBody()}
  </div>
{:else}
  <!-- Standalone: page-fill centered card + heading. -->
  <div class="flex h-full flex-col items-center justify-center gap-6 p-8">
    <div class="border-border bg-panel w-full max-w-md space-y-4 rounded-md border p-5">
      <div class="space-y-1">
        <h2 class="text-fg text-lg font-semibold">Create an agent</h2>
        <p class="text-muted text-sm">Agents live inside the active project.</p>
      </div>
      {@render formBody()}
    </div>
  </div>
{/if}
