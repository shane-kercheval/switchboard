<script lang="ts">
  import type { HarnessAvailability, HarnessKind } from "$lib/types";
  import type { AgentFormSubmit } from "./CreateAgentForm.types";
  import { harnessUnavailableReason, isHarnessSelectable } from "$lib/harnessAvailability";
  import Button from "$lib/components/ui/Button.svelte";
  import Input from "$lib/components/ui/Input.svelte";

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
    /// Optional per-harness availability. When provided, gates the radio
    /// buttons (and the submit button if the currently-selected harness
    /// is unavailable). Tooltip copy matches the banner copy in
    /// `App.svelte` verbatim so the user sees the same gap stated in
    /// both places. Defaults to "both harnesses available" so tests
    /// that don't care about gating render unchanged.
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
    claudeAvailability = { harness: "claude_code", binary: "available", auth: "unsupported" },
    codexAvailability = { harness: "codex", binary: "available", auth: "available" },
    geminiAvailability = { harness: "gemini", binary: "available", auth: "available" },
    antigravityAvailability = { harness: "antigravity", binary: "available", auth: "available" },
  }: Props = $props();
  let name = $state<string>("assistant");
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

  const canSubmit = $derived(!busy && name.trim() !== "" && sessionIdValid && selectedSelectable);

  function handleSubmit(): void {
    const trimmedName = name.trim();
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
    if (selected) return "bg-primary text-primary-fg";
    if (!selectable) return "text-muted cursor-not-allowed opacity-60";
    return "text-muted hover:bg-raised";
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
  <div
    class="border-border bg-panel/70 flex gap-1 rounded-md border p-0.5"
    role="tablist"
    data-testid="mode-toggle"
  >
    <button
      type="button"
      class="h-8 flex-1 rounded px-2 text-xs font-medium transition-colors {mode === 'create'
        ? 'bg-primary text-primary-fg'
        : 'text-muted hover:bg-raised'}"
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
      class="h-8 flex-1 rounded px-2 text-xs font-medium transition-colors {mode === 'attach'
        ? 'bg-primary text-primary-fg'
        : 'text-muted hover:bg-raised'}"
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
    <legend class="text-muted text-xs">Harness</legend>
    <!-- Native radios (real arrow-key + screen-reader semantics, grouped/labeled
         by the fieldset+legend) styled as a segmented control: the input is
         visually hidden and the label is the pill; `has-[:focus-visible]` lights
         the pill when the radio is keyboard-focused. -->
    <div
      class="border-border bg-panel/70 grid grid-cols-4 gap-1 rounded-md border p-0.5"
      data-testid="harness-picker"
    >
      <label
        class="has-[:focus-visible]:ring-accent flex h-8 items-center justify-center rounded px-2 text-xs font-medium transition-colors has-[:focus-visible]:ring-2 {harnessOptionClass(
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
          onchange={() => (harness = "claude_code")}
          data-testid="harness-claude"
        />
        Claude Code
      </label>
      <label
        class="has-[:focus-visible]:ring-accent flex h-8 items-center justify-center rounded px-2 text-xs font-medium transition-colors has-[:focus-visible]:ring-2 {harnessOptionClass(
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
          onchange={() => (harness = "codex")}
          data-testid="harness-codex"
        />
        Codex
      </label>
      <label
        class="has-[:focus-visible]:ring-accent flex h-8 items-center justify-center rounded px-2 text-xs font-medium transition-colors has-[:focus-visible]:ring-2 {harnessOptionClass(
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
          onchange={() => (harness = "gemini")}
          data-testid="harness-gemini"
        />
        Gemini
      </label>
      <label
        class="has-[:focus-visible]:ring-accent flex h-8 items-center justify-center rounded px-2 text-xs font-medium transition-colors has-[:focus-visible]:ring-2 {harnessOptionClass(
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
          onchange={() => (harness = "antigravity")}
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
    <Input bind:value={name} disabled={busy} data-testid="agent-name" class="h-8 px-2" />
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
