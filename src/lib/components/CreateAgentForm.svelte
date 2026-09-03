<script lang="ts">
  import { onMount } from "svelte";
  import type { AgentRecord, AgentSelection, HarnessAvailability, HarnessKind } from "$lib/types";
  import type { AgentFormSubmit } from "./CreateAgentForm.types";
  import { harnessUnavailableReason, isHarnessSelectable } from "$lib/harnessAvailability";
  import { HARNESS_LABEL, ALL_HARNESSES } from "$lib/harnessDisplay";
  import {
    defaultAgentName,
    defaultAgentNameForSelection,
    selectionIsValid,
  } from "$lib/agentSelection";
  import { loadPreferences, preferenceLoadState, preferences } from "$lib/preferences.svelte";
  import { normalizeAgentName, validateAgentName } from "$lib/agentName";
  import Button from "$lib/components/ui/Button.svelte";
  import Input from "$lib/components/ui/Input.svelte";
  import AgentSelectionEditor from "$lib/components/AgentSelectionEditor.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
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
  let name = $state<string>("");
  /// Set once the user edits the name field, which freezes it: until then the
  /// name tracks the model/effort and harness selection (see the `$effect` below);
  /// after, the user's value is theirs to keep.
  let nameTouched = $state<boolean>(false);
  let harness = $state<HarnessKind>("claude_code");
  let mode = $state<"create" | "attach">("create");
  let existingSessionId = $state<string>("");

  /// Model + effort selection — **create mode only**. Attach brings in an
  /// existing on-disk session and pins nothing: the pickers aren't shown there
  /// and no model/effort flag is submitted, so the harness runs the resumed
  /// session as-is. The user manages model/effort from the agent's actions menu
  /// afterward (the canonical place for an existing agent).
  function defaultsFor(kind: HarnessKind): AgentSelection {
    const defaults = preferences.agent_defaults[kind];
    return {
      model: defaults.default_model,
      effort: defaults.default_effort,
      model_choices: [...defaults.model_choices],
      effort_choices: [...defaults.effort_choices],
    };
  }
  let agentSelection = $state<AgentSelection>({
    model: null,
    effort: null,
    model_choices: [],
    effort_choices: [],
  });
  let defaultsInitialized = $state<boolean>(false);

  function initializeDefaults(): void {
    if (defaultsInitialized) return;
    agentSelection = defaultsFor(harness);
    name = defaultAgentNameForSelection(harness, agentSelection);
    defaultsInitialized = true;
  }

  onMount(() => {
    void loadPreferences().then(initializeDefaults);
  });

  $effect(() => {
    if (preferenceLoadState.ready) initializeDefaults();
  });

  /// Keep the name in lock-step with the current selection while the user
  /// hasn't taken it over. Multiple choices use the stable harness name because
  /// either axis may change after creation.
  /// Attach keeps the bare harness name because it inherits an existing session
  /// and pins nothing. Writing `name` here is safe — the effect never reads it,
  /// so there's no reactive loop.
  $effect(() => {
    const auto =
      mode === "attach"
        ? defaultAgentName(harness, undefined, undefined)
        : defaultAgentNameForSelection(harness, agentSelection);
    if (!nameTouched) name = auto;
  });

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
  /// Claude/Antigravity identify a session by a UUID (`parse_uuid`),
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

  const canSubmit = $derived(
    defaultsInitialized &&
      !busy &&
      nameValidation.ok &&
      sessionIdValid &&
      selectedSelectable &&
      (mode === "attach" || selectionIsValid(agentSelection, harness)),
  );
  const formDisabled = $derived(busy || !defaultsInitialized);

  function handleSubmit(): void {
    if (!canSubmit) return;
    const trimmedName = normalizeAgentName(name);
    if (mode === "attach") {
      // Attach pins nothing — model/effort are managed from the agent's actions
      // menu after the existing session is brought in.
      onSubmit({
        mode: "attach",
        name: trimmedName,
        harness,
        existingSessionId: existingSessionId.trim(),
      });
      return;
    }
    // The unset sentinel (an unsupported-capability axis) collapses to
    // `undefined` → the backend sends no flag. Backend normalization is the real
    // trust boundary; this is the UX layer.
    onSubmit({
      mode: "create",
      name: trimmedName,
      harness,
      selection: $state.snapshot(agentSelection),
    });
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
    harness = kind;
    // Reset the pickers to the new harness's default so a stale, out-of-list
    // value (e.g. a Codex model carried over to Antigravity) can't be submitted.
    const defaults = defaultsFor(kind);
    agentSelection = defaults;
  }

  function selectMode(next: "create" | "attach"): void {
    if (next === mode) return;
    mode = next;
    if (next === "create") {
      existingSessionId = "";
    }
    // Create-mode model/effort selections are intentionally preserved across a
    // mode toggle (draft preservation) — an incidental switch to attach and back
    // shouldn't discard the user's picks. Harness change is the only reset point
    // (see `selectHarness`), since that's what can leave a stale out-of-list value.
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
      disabled={formDisabled}
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
      disabled={formDisabled}
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

  <fieldset class="space-y-1.5" disabled={formDisabled}>
    <legend class="text-muted text-xs">Tool</legend>
    <!-- Native radios (real arrow-key + screen-reader semantics, grouped/labeled
         by the fieldset+legend) styled as a segmented control: the input is
         visually hidden and the label is the pill; `has-[:focus-visible]` lights
         the pill when the radio is keyboard-focused. -->
    <!-- One pill per harness, looped over `ALL_HARNESSES` (no
         hardcoded set or fixed column count) so a new harness picks up the
         picker automatically. Columns are inline-styled because Tailwind
         can't generate a dynamic `grid-cols-N`. -->
    <div
      class={cn(SEGMENTED_CONTAINER_CLASS, "grid")}
      style="grid-template-columns: repeat({ALL_HARNESSES.length}, minmax(0, 1fr));"
      data-testid="harness-picker"
    >
      {#each ALL_HARNESSES as kind (kind)}
        {@const unavailableReason = reason(kind)}
        {#snippet option(props: Record<string, unknown> = {})}
          <label
            {...props}
            class="{SEGMENTED_ITEM_CLASS} has-[:focus-visible]:ring-focus flex items-center justify-center has-[:focus-visible]:ring-1 {harnessOptionClass(
              kind,
              selectable(kind),
            )}"
          >
            <input
              type="radio"
              name="harness"
              value={kind}
              class="sr-only"
              checked={harness === kind}
              disabled={formDisabled || !selectable(kind)}
              onchange={() => selectHarness(kind)}
              data-testid={`harness-${kind}`}
            />
            {HARNESS_LABEL[kind]}
          </label>
        {/snippet}
        {#if unavailableReason}
          <Tooltip label={unavailableReason} side="bottom">
            {#snippet trigger(props)}{@render option(props)}{/snippet}
          </Tooltip>
        {:else}
          {@render option()}
        {/if}
      {/each}
    </div>
    {#if selectedReason}
      <p class="text-status-failed text-xs" data-testid="harness-unavailable">
        {selectedReason}
      </p>
    {/if}
  </fieldset>

  <!-- Model/effort are a create-only concern. Attach brings in an existing
       session and pins nothing; the user sets model/effort from the agent's
       actions menu afterward. -->
  {#if mode === "create"}
    {#if defaultsInitialized}
      <AgentSelectionEditor
        {harness}
        selection={agentSelection}
        context="start"
        onChange={(next) => (agentSelection = next)}
        disabled={formDisabled}
        testidPrefix="create-selection"
      />
    {:else}
      <p class="text-muted text-xs" data-testid="agent-defaults-loading">Loading defaults…</p>
    {/if}
  {/if}

  <label class="block space-y-1">
    <span class="text-muted text-xs">Agent name</span>
    <Input
      bind:value={name}
      disabled={formDisabled}
      data-testid="agent-name"
      class={cn("h-8 px-2", nameError && "border-status-failed")}
      aria-invalid={!nameValidation.ok}
      aria-describedby={nameError ? "agent-name-error" : undefined}
      oninput={() => (nameTouched = true)}
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
        disabled={formDisabled}
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
        disabled={formDisabled}
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
    <div class="border-border bg-raised w-full max-w-lg space-y-4 rounded-md border p-5">
      <div class="space-y-1">
        <h2 class="text-fg text-lg font-semibold">Create an agent</h2>
        <p class="text-muted text-sm">Agents live inside the active project.</p>
      </div>
      {@render formBody()}
    </div>
  </div>
{/if}
