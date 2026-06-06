<script lang="ts">
  import type { AgentRecord, HarnessAvailability, HarnessKind } from "$lib/types";
  import type { AgentFormSubmit } from "./CreateAgentForm.types";
  import { harnessUnavailableReason, isHarnessSelectable } from "$lib/harnessAvailability";
  import {
    ALL_HARNESSES,
    HARNESS_DEFAULT_AGENT_NAME,
    HARNESS_LABEL,
    SUPPORTS_EFFORT_SELECTION,
    SUPPORTS_MODEL_SELECTION,
  } from "$lib/harnessDisplay";
  import {
    DEFAULT_EFFORT,
    DEFAULT_MODEL,
    EFFORT_OPTIONS,
    MODEL_OPTIONS,
    type SelectOption,
  } from "$lib/agentSelection";
  import { normalizeAgentName, validateAgentName } from "$lib/agentName";
  import Button from "$lib/components/ui/Button.svelte";
  import Input from "$lib/components/ui/Input.svelte";
  import Select from "$lib/components/ui/Select.svelte";
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
    /// Per-harness availability, gating the picker (and the submit button when
    /// the selected harness is unavailable) on binary presence. A missing entry
    /// defaults to "available" — so tests that don't exercise gating can pass
    /// `{}` (or omit it) and render unchanged. Keyed by `HarnessKind` so it
    /// scales with the harness set rather than four named props.
    availability?: Partial<Record<HarnessKind, HarnessAvailability>>;
  };

  let {
    busy = false,
    error = null,
    onSubmit,
    onCancel,
    roster = [],
    availability = {},
  }: Props = $props();
  let name = $state<string>(HARNESS_DEFAULT_AGENT_NAME["claude_code"]);
  let harness = $state<HarnessKind>("claude_code");
  let mode = $state<"create" | "attach">("create");
  let existingSessionId = $state<string>("");

  /// Model + effort selection. The empty string is the "keep current" sentinel
  /// (attach default) and maps to `undefined` on submit — i.e. send no
  /// flag and leave the attached session's existing model/effort untouched.
  /// **Create and attach have different correct defaults:** create preselects
  /// the harness default and submits it concretely (a new agent has no session
  /// to preserve); attach defaults to "keep current" because Claude is
  /// session-sticky — overriding would silently switch a running session's
  /// model. The user can still pick a concrete value in attach mode to switch
  /// deliberately.
  const KEEP_CURRENT = "";
  function defaultModelFor(forMode: "create" | "attach", kind: HarnessKind): string {
    if (forMode === "attach") return KEEP_CURRENT;
    return DEFAULT_MODEL[kind] ?? KEEP_CURRENT;
  }
  function defaultEffortFor(forMode: "create" | "attach", kind: HarnessKind): string {
    if (forMode === "attach") return KEEP_CURRENT;
    return DEFAULT_EFFORT[kind] ?? KEEP_CURRENT;
  }
  let model = $state<string>(defaultModelFor("create", "claude_code"));
  let effort = $state<string>(defaultEffortFor("create", "claude_code"));

  const modelSupported = $derived(SUPPORTS_MODEL_SELECTION[harness]);
  const effortSupported = $derived(SUPPORTS_EFFORT_SELECTION[harness]);
  /// Attach prepends the "no override" sentinel so leaving the picker alone
  /// sends no flag; create offers only concrete values. The label is "No
  /// override" rather than "keep current" because omitting the flag only
  /// *preserves* the session's model on the sticky harness (Claude) — Gemini
  /// reverts to its default and Codex resolves its config default. "No
  /// override" describes what Switchboard does (send nothing) for all three.
  const modelOptions = $derived<SelectOption[]>(
    mode === "attach"
      ? [{ label: "No override", value: KEEP_CURRENT }, ...MODEL_OPTIONS[harness]]
      : MODEL_OPTIONS[harness],
  );
  const effortOptions = $derived<SelectOption[]>(
    mode === "attach"
      ? [{ label: "No override", value: KEEP_CURRENT }, ...EFFORT_OPTIONS[harness]]
      : EFFORT_OPTIONS[harness],
  );

  /// Per-harness gate, looked up by kind (no per-harness branches). Missing
  /// availability defaults to "available". Two predicates from
  /// `harnessAvailability`:
  /// - `isHarnessSelectable` — false for `"checking"` / `"missing"`; gates the
  ///   radio `disabled` and the parent's submit button.
  /// - `harnessUnavailableReason` — message text for *real* missing states;
  ///   null for `"checking"` so no scary "Checking…" copy shows during the probe
  ///   window. (The two intentionally disagree on `"checking"`: blocked, but
  ///   silent.)
  function availabilityFor(kind: HarnessKind): HarnessAvailability {
    return availability[kind] ?? { harness: kind, binary: "available" };
  }
  function selectable(kind: HarnessKind): boolean {
    return isHarnessSelectable(availabilityFor(kind));
  }
  function reason(kind: HarnessKind): string | null {
    return harnessUnavailableReason(availabilityFor(kind));
  }
  const selectedSelectable = $derived(selectable(harness));
  const selectedReason = $derived(reason(harness));

  /// UUID shape check (any version — Codex and Claude use v4 / v7
  /// respectively; the backend re-validates). This is a UX gate so the user
  /// sees the inline hint before pressing the submit button.
  const UUID_PATTERN =
    /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;

  /// Attach-id validation is harness-aware, matching the backend contract:
  /// Claude/Gemini/Antigravity identify a session by a UUID (`parse_uuid`),
  /// but a Codex thread-id is an arbitrary string used verbatim — so the form
  /// must accept a non-UUID Codex id (a garbage value simply matches no
  /// session file and the backend rejects it). Non-empty is the right floor;
  /// the authoritative check is the backend's file lookup.
  const sessionIdRequiresUuid = $derived(harness !== "codex");
  const sessionIdValid = $derived(
    mode !== "attach" ||
      (sessionIdRequiresUuid
        ? UUID_PATTERN.test(existingSessionId.trim())
        : existingSessionId.trim().length > 0),
  );

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
    if (!canSubmit) return;
    const trimmedName = normalizeAgentName(name);
    // The "keep current" sentinel (and any unsupported-capability empty value)
    // collapses to `undefined` → the backend sends no flag. The M3 backend
    // normalization is the real trust boundary; this is the UX layer.
    const selectedModel = model === KEEP_CURRENT ? undefined : model;
    const selectedEffort = effort === KEEP_CURRENT ? undefined : effort;
    const selection = {
      ...(selectedModel !== undefined ? { model: selectedModel } : {}),
      ...(selectedEffort !== undefined ? { effort: selectedEffort } : {}),
    };
    if (mode === "create") {
      onSubmit({ mode: "create", name: trimmedName, harness, ...selection });
    } else {
      onSubmit({
        mode: "attach",
        name: trimmedName,
        harness,
        existingSessionId: existingSessionId.trim(),
        ...selection,
      });
    }
  }

  function submitOnEnter(event: KeyboardEvent): void {
    if (event.key !== "Enter") return;
    event.preventDefault();
    handleSubmit();
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
    // Reset the pickers to the new harness's default so a stale, out-of-list
    // value (e.g. a Codex model carried over to Gemini) can't be submitted.
    model = defaultModelFor(mode, kind);
    effort = defaultEffortFor(mode, kind);
  }

  function selectMode(next: "create" | "attach"): void {
    if (next === mode) return;
    mode = next;
    if (next === "create") {
      existingSessionId = "";
    }
    // Create and attach have different correct defaults (concrete vs. "keep
    // current"), so re-seed on the toggle.
    model = defaultModelFor(next, harness);
    effort = defaultEffortFor(next, harness);
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

  <p class="text-muted text-xs leading-relaxed">
    {#if mode === "create"}
      Starts a fresh session with no prior history. The CLI creates a new conversation in your
      project directory.
    {:else}
      Connects to an existing session by its UUID, picking up the conversation where it left off.
      Use this to bring a session the CLI already has on disk into Switchboard.
    {/if}
  </p>

  <fieldset class="space-y-1.5" disabled={busy}>
    <legend class="text-muted text-xs">Tool</legend>
    <!-- Native radios (real arrow-key + screen-reader semantics, grouped/labeled
         by the fieldset+legend) styled as a segmented control: the input is
         visually hidden and the label is the pill; `has-[:focus-visible]` lights
         the pill when the radio is keyboard-focused. -->
    <!-- One pill per harness, looped over `ALL_HARNESSES` (no hardcoded set or
         fixed column count) so a new harness picks up the picker automatically.
         Columns are inline-styled because Tailwind can't generate a dynamic
         `grid-cols-N`. -->
    <div
      class={cn(SEGMENTED_CONTAINER_CLASS, "grid")}
      style="grid-template-columns: repeat({ALL_HARNESSES.length}, minmax(0, 1fr));"
      data-testid="harness-picker"
    >
      {#each ALL_HARNESSES as kind (kind)}
        <label
          class="{SEGMENTED_ITEM_CLASS} has-[:focus-visible]:ring-accent flex items-center justify-center has-[:focus-visible]:ring-2 {harnessOptionClass(
            kind,
            selectable(kind),
          )}"
          title={reason(kind) ?? ""}
        >
          <input
            type="radio"
            name="harness"
            value={kind}
            class="sr-only"
            checked={harness === kind}
            disabled={busy || !selectable(kind)}
            onchange={() => selectHarness(kind)}
            data-testid={`harness-${kind}`}
          />
          {HARNESS_LABEL[kind]}
        </label>
      {/each}
    </div>
    {#if selectedReason}
      <p class="text-status-failed text-xs" data-testid="harness-unavailable">
        {selectedReason}
      </p>
    {/if}
  </fieldset>

  <!-- Model: a curated per-harness dropdown where the harness supports it, a
       short note where it doesn't (Antigravity's model is set inside
       Antigravity). -->
  {#if modelSupported}
    <label class="block space-y-1">
      <span class="text-muted text-xs">Model</span>
      <Select
        bind:value={model}
        options={modelOptions}
        disabled={busy}
        data-testid="model-select"
        aria-label="Model"
      />
    </label>
  {:else}
    <p class="text-muted text-xs leading-relaxed" data-testid="model-note">
      {HARNESS_LABEL[harness]}'s model is selected inside {HARNESS_LABEL[harness]} — it can't be set from
      Switchboard.
    </p>
  {/if}

  <!-- Effort: a curated dropdown for Claude/Codex, a note for Gemini (config
       only) and Antigravity (folded into the model). -->
  {#if effortSupported}
    <label class="block space-y-1">
      <span class="text-muted text-xs">Reasoning effort</span>
      <Select
        bind:value={effort}
        options={effortOptions}
        disabled={busy}
        data-testid="effort-select"
        aria-label="Reasoning effort"
      />
      {#if harness === "claude_code"}
        <span class="text-muted block text-xs">
          Higher levels may run at a lower effort on some models.
        </span>
      {/if}
    </label>
  {:else if harness === "gemini"}
    <p class="text-muted text-xs leading-relaxed" data-testid="effort-note">
      Gemini's reasoning effort is set in Gemini's own config, not from Switchboard.
    </p>
  {:else}
    <p class="text-muted text-xs leading-relaxed" data-testid="effort-note">
      Antigravity folds reasoning effort into the model you pick inside Antigravity.
    </p>
  {/if}

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
      onkeydown={submitOnEnter}
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
      <!-- Codex identifies a session by an arbitrary thread-id string; the
           other harnesses use a UUID. Label/validate accordingly. -->
      <span class="text-muted text-xs">
        {sessionIdRequiresUuid ? "Existing session UUID" : "Existing session ID"}
      </span>
      <Input
        bind:value={existingSessionId}
        disabled={busy}
        placeholder={sessionIdRequiresUuid
          ? "00000000-0000-0000-0000-000000000000"
          : "the session's thread id"}
        data-testid="attach-session-id"
        class="h-8 px-2"
        onkeydown={submitOnEnter}
      />
      {#if sessionIdRequiresUuid && existingSessionId.trim() !== "" && !sessionIdValid}
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
        class="w-28"
        data-testid="cancel-create-agent"
        disabled={busy}
        onclick={onCancel}
      >
        Cancel
      </Button>
    {/if}
    <Button
      size="sm"
      class="w-28"
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
    <div class="border-border bg-raised w-full max-w-md space-y-4 rounded-md border p-5">
      <div class="space-y-1">
        <h2 class="text-fg text-lg font-semibold">Create an agent</h2>
        <p class="text-muted text-sm">Agents live inside the active project.</p>
      </div>
      {@render formBody()}
    </div>
  </div>
{/if}
