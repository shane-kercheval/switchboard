<script lang="ts">
  import type { HarnessAvailability, HarnessKind } from "$lib/types";
  import type { AgentFormSubmit } from "./CreateAgentForm.types";
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
  };

  let {
    busy = false,
    error = null,
    onSubmit,
    onCancel,
    claudeAvailability = { harness: "claude_code", binary: "available", auth: "unsupported" },
    codexAvailability = { harness: "codex", binary: "available", auth: "available" },
  }: Props = $props();
  let name = $state<string>("assistant");
  let harness = $state<HarnessKind>("claude_code");
  let mode = $state<"create" | "attach">("create");
  let existingSessionId = $state<string>("");

  /// A harness is unavailable if its binary is missing OR (Codex-only)
  /// its auth is missing. Returns null when available, otherwise the
  /// tooltip / inline-error copy.
  function unavailabilityReason(a: HarnessAvailability): string | null {
    if (a.binary === "missing") {
      return a.harness === "claude_code"
        ? "Claude Code not found on PATH. Install from https://claude.com/code"
        : "Codex not found on PATH. Install from https://github.com/openai/codex";
    }
    if (a.auth === "missing") {
      return "Codex not authenticated — run `codex login` and reload Switchboard. (API-key-only auth is not supported.)";
    }
    return null;
  }

  const claudeUnavailable = $derived(unavailabilityReason(claudeAvailability));
  const codexUnavailable = $derived(unavailabilityReason(codexAvailability));
  const selectedUnavailable = $derived(
    harness === "claude_code" ? claudeUnavailable : codexUnavailable,
  );

  /// UUID shape check (any version — Codex and Claude use v4 / v7
  /// respectively; the backend re-validates). This is a UX gate so the user
  /// sees the inline hint before pressing the submit button.
  const UUID_PATTERN =
    /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;

  const sessionIdValid = $derived(mode !== "attach" || UUID_PATTERN.test(existingSessionId.trim()));

  const canSubmit = $derived(
    !busy && name.trim() !== "" && sessionIdValid && selectedUnavailable === null,
  );

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
    class="flex gap-2 rounded-md border border-neutral-200 bg-white p-1"
    role="tablist"
    data-testid="mode-toggle"
  >
    <button
      type="button"
      class="flex-1 rounded px-2 py-1 text-xs font-medium {mode === 'create'
        ? 'bg-neutral-900 text-white'
        : 'text-neutral-700 hover:bg-neutral-100'}"
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
      class="flex-1 rounded px-2 py-1 text-xs font-medium {mode === 'attach'
        ? 'bg-neutral-900 text-white'
        : 'text-neutral-700 hover:bg-neutral-100'}"
      role="tab"
      aria-selected={mode === "attach"}
      data-testid="mode-attach"
      onclick={() => selectMode("attach")}
      disabled={busy}
    >
      Attach existing
    </button>
  </div>

  <fieldset class="space-y-1" disabled={busy}>
    <legend class="text-xs text-neutral-600">Harness</legend>
    <div class="flex gap-3 text-sm" data-testid="harness-picker">
      <label
        class="flex items-center gap-1.5 {claudeUnavailable
          ? 'cursor-not-allowed text-neutral-400'
          : ''}"
        title={claudeUnavailable ?? ""}
      >
        <input
          type="radio"
          name="harness"
          value="claude_code"
          checked={harness === "claude_code"}
          disabled={claudeUnavailable !== null}
          onchange={() => (harness = "claude_code")}
          data-testid="harness-claude"
        />
        Claude Code
      </label>
      <label
        class="flex items-center gap-1.5 {codexUnavailable
          ? 'cursor-not-allowed text-neutral-400'
          : ''}"
        title={codexUnavailable ?? ""}
      >
        <input
          type="radio"
          name="harness"
          value="codex"
          checked={harness === "codex"}
          disabled={codexUnavailable !== null}
          onchange={() => (harness = "codex")}
          data-testid="harness-codex"
        />
        Codex
      </label>
    </div>
    {#if selectedUnavailable}
      <p class="text-xs text-red-700" data-testid="harness-unavailable">
        {selectedUnavailable}
      </p>
    {/if}
  </fieldset>

  <label class="block space-y-1">
    <span class="text-xs text-neutral-600">Agent name</span>
    <Input bind:value={name} disabled={busy} data-testid="agent-name" />
  </label>

  {#if mode === "attach"}
    <label class="block space-y-1">
      <span class="text-xs text-neutral-600"> Existing session UUID </span>
      <Input
        bind:value={existingSessionId}
        disabled={busy}
        placeholder="00000000-0000-0000-0000-000000000000"
        data-testid="attach-session-id"
      />
      {#if existingSessionId.trim() !== "" && !sessionIdValid}
        <span class="block text-xs text-red-700" data-testid="attach-session-id-error">
          Must be a UUID (8-4-4-4-12 hex characters).
        </span>
      {/if}
    </label>
  {/if}

  {#if error}
    <p class="text-xs text-red-700" data-testid="error">{error}</p>
  {/if}
  <div class="flex justify-end gap-2">
    {#if onCancel}
      <Button
        variant="secondary"
        data-testid="cancel-create-agent"
        disabled={busy}
        onclick={onCancel}
      >
        Cancel
      </Button>
    {/if}
    <Button data-testid="confirm-create-agent" disabled={!canSubmit} onclick={handleSubmit}>
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
  <div class="space-y-4" data-testid="create-agent-form-embedded">
    {@render formBody()}
  </div>
{:else}
  <!-- Standalone: page-fill centered card + heading. -->
  <div class="flex h-full flex-col items-center justify-center gap-6 p-8">
    <div class="w-full max-w-md space-y-4 rounded-md border border-neutral-200 bg-neutral-50 p-6">
      <div class="space-y-1">
        <h2 class="text-lg font-semibold text-neutral-900">Create an agent</h2>
        <p class="text-sm text-neutral-600">Agents live inside the active project.</p>
      </div>
      {@render formBody()}
    </div>
  </div>
{/if}
