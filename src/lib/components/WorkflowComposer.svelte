<script lang="ts">
  import type {
    AgentRecord,
    DerivedArgInfo,
    WorkflowFormDescriptor,
    WorkflowInputValue,
  } from "$lib/types";
  import type { TranscriptPane } from "$lib/state/transcriptPanes.svelte";
  import {
    forwardSourceKey,
    forwardSourceForAgent,
    forwardSourceAgentsForPane,
    type ForwardReadiness,
    type ForwardSource,
  } from "$lib/state/heldForwards.svelte";
  import { cn } from "$lib/utils";
  import { ICON_BUTTON_ON_RAISED_CLASS } from "$lib/components/ui/iconButton";
  import Textarea from "$lib/components/ui/Textarea.svelte";
  import ClearIcon from "$lib/components/ui/ClearIcon.svelte";
  import HarnessIcon from "$lib/components/ui/HarnessIcon.svelte";
  import WorkflowSteps from "$lib/components/WorkflowSteps.svelte";
  import ForwardSourceChip from "$lib/components/ui/ForwardSourceChip.svelte";
  import ForwardSourcePicker from "$lib/components/ui/ForwardSourcePicker.svelte";

  /// The workflow invocation form. A workflow parameterizes its *recipients* (its
  /// declared `agent`/`[agent]` inputs are named slots bound to real agents here),
  /// which is why the compose bar hides its own To field in workflow mode: the
  /// workflow owns routing. Fields are two kinds, rendered uniformly: declared
  /// inputs (agent chips / text fields) and **auto-derived prompt arguments** (the
  /// fillable args of the workflow's hardcoded prompts, surfaced as text fields).
  /// The prompt itself is hardcoded — there is no prompt picker.
  let {
    descriptor,
    agents,
    panes = [],
    loading = false,
    syncSettled = false,
    agentReadiness,
    inputs = $bindable(),
    forwardSources = $bindable({}),
    onremove,
    invoke,
  }: {
    descriptor: WorkflowFormDescriptor;
    agents: AgentRecord[];
    /// The project's panes, offered as a quick way to fill an `[agent]` input with
    /// a whole pane's members at once (a single `agent` slot can't hold a pane, so
    /// pane chips show only on list inputs).
    panes?: TranscriptPane[];
    /// True while the descriptor is being (re)fetched — a pending state distinct
    /// from an incompatibility: show a "resolving" affordance, not an error.
    loading?: boolean;
    /// Whether a prompt sync has settled since this workflow was picked. Gates the
    /// `unresolved` → "not found" escalation (before a sync settles, an unresolved
    /// MCP prompt is genuinely pending).
    syncSettled?: boolean;
    /// Classifies what each forward-source agent would contribute, so the user
    /// sees before picking that a source will be skipped — parity with the prompt
    /// composer's per-argument pickers.
    agentReadiness?: (id: string) => ForwardReadiness;
    /// Bound input values, keyed by name. Scalar inputs and derived args hold a
    /// string; list inputs hold a string[].
    inputs: Record<string, WorkflowInputValue>;
    /// Per-field forward sources — the agents/panes whose latest completed output
    /// the backend composes into each fillable single-text field (declared `text`
    /// inputs and derived args) at invoke, after that field's typed text. Bound so
    /// the parent reads them at invoke time. Live-UI-only, like the compose bar's.
    forwardSources?: Record<string, ForwardSource[]>;
    onremove: () => void;
    /// The invoke button(s), rendered in the footer by the parent (so the parent
    /// owns the actual invoke call + busy state).
    invoke?: import("svelte").Snippet;
  } = $props();

  // Each fillable single-text field owns its own source list; the picker/chips
  // below are handed that field's add/remove closures, so there is no shared key
  // namespace to collide with field names.
  function withSource(list: ForwardSource[], source: ForwardSource): ForwardSource[] {
    return list.some((s) => forwardSourceKey(s) === forwardSourceKey(source))
      ? list
      : [...list, source];
  }

  function addArgSource(name: string, source: ForwardSource): void {
    forwardSources[name] = withSource(forwardSources[name] ?? [], source);
  }

  function removeArgSource(name: string, key: string): void {
    forwardSources[name] = (forwardSources[name] ?? []).filter((s) => forwardSourceKey(s) !== key);
  }

  function clearArgSources(name: string): void {
    forwardSources[name] = [];
  }

  function hasSources(name: string): boolean {
    return (forwardSources[name]?.length ?? 0) > 0;
  }

  // Element refs for the fillable single-text fields (declared `text` inputs +
  // derived args), keyed by field name, so the ⌘⌃N pane chord can target the
  // field currently being typed in — mirrors the prompt composer's per-field
  // forwarding. (Names are disjoint across inputs and derived args: a `text`
  // input that shadows a same-named prompt arg removes it from `derived_args`.)
  let fieldRefs = $state<Record<string, HTMLTextAreaElement | undefined>>({});

  /// The add-source closure for whichever fillable field currently holds focus,
  /// or `null` when focus is elsewhere.
  function focusedFieldAdd(): ((source: ForwardSource) => void) | null {
    const active = document.activeElement;
    if (active === null) return null;
    for (const [name, ref] of Object.entries(fieldRefs)) {
      if (ref === active) return (source) => addArgSource(name, source);
    }
    return null;
  }

  // ⌘⌃1..9 → forward pane N into the focused field. The compose bar's own ⌘⌃
  // chord no-ops while a workflow is being composed (its handler returns when
  // `mode !== "plain"`), so there's no double-fire; the index matches the pane's
  // position in `panes` — the order the picker shows the chord for.
  $effect(() => {
    function onKeydown(e: KeyboardEvent): void {
      if (!e.metaKey || !e.ctrlKey || e.altKey || e.shiftKey) return;
      if (e.key < "1" || e.key > "9") return;
      const pane = panes[Number(e.key) - 1];
      if (pane === undefined || pane.members.length === 0) return;
      const add = focusedFieldAdd();
      if (add === null) return;
      e.preventDefault();
      for (const source of forwardSourceAgentsForPane(pane, agents)) add(source);
    }
    window.addEventListener("keydown", onKeydown);
    return () => window.removeEventListener("keydown", onKeydown);
  });

  function asString(name: string): string {
    const v = inputs[name];
    return typeof v === "string" ? v : "";
  }
  function asList(name: string): string[] {
    const v = inputs[name];
    return Array.isArray(v) ? v : [];
  }

  function setAgent(name: string, agentName: string): void {
    inputs[name] = agentName;
  }
  function toggleAgent(name: string, agentName: string): void {
    const list = asList(name);
    inputs[name] = list.includes(agentName)
      ? list.filter((n) => n !== agentName)
      : [...list, agentName];
  }

  // Panes are only meaningful as a shortcut once the user has actually split
  // (≥2 non-empty panes): with a single pane "this pane" == "every agent", which
  // the agent chips already cover. Mirrors the forward picker's `multiPane` rule.
  const multiPane = $derived(panes.filter((p) => p.members.length > 0).length > 1);
  // A single `agent` slot only offers single-member panes, so its pane row (and
  // the divider that sets it off from the agent chips) appears only when one exists.
  const hasSingleMemberPane = $derived(panes.some((p) => p.members.length === 1));

  function paneMemberNames(pane: TranscriptPane): string[] {
    return pane.members
      .map((id) => agents.find((a) => a.id === id)?.name)
      .filter((n): n is string => n !== undefined);
  }
  function paneSelected(name: string, pane: TranscriptPane): boolean {
    const list = asList(name);
    const members = paneMemberNames(pane);
    return members.length > 0 && members.every((m) => list.includes(m));
  }
  // Toggle a whole pane into/out of an `[agent]` input: add its missing members,
  // or — when every member is already selected — drop them all.
  function togglePane(name: string, pane: TranscriptPane): void {
    const members = paneMemberNames(pane);
    const list = asList(name);
    inputs[name] = paneSelected(name, pane)
      ? list.filter((n) => !members.includes(n))
      : [...list, ...members.filter((m) => !list.includes(m))];
  }

  // A single `agent` slot only offers single-member panes (a pane that maps to
  // exactly one agent), so selecting one binds that lone member. The chip reads
  // as selected when the bound agent is that member.
  function paneHoldsSelectedAgent(name: string, pane: TranscriptPane): boolean {
    const v = asString(name);
    return v !== "" && paneMemberNames(pane).includes(v);
  }
  // Clicking the chip for the already-bound member clears the slot, mirroring the
  // multi-member pane's toggle-off (an `agent` slot holds one agent, so "clear"
  // means the empty string — the unset value isMissingInput checks for).
  function selectPaneMember(name: string, pane: TranscriptPane): void {
    const [member] = paneMemberNames(pane);
    if (member === undefined) return;
    inputs[name] = asString(name) === member ? "" : member;
  }
  // Recipient-chip styling reused from the compose bar's To field (icon + low
  // height), minus the position number, which has no meaning here.
  function chipClass(selected: boolean): string {
    return cn(
      "focus-visible:ring-accent inline-flex h-6 items-center gap-1 rounded-full border px-2 text-xs transition-colors focus-visible:ring-2 focus-visible:outline-none",
      selected
        ? "bg-accent-soft text-fg border-transparent"
        : "border-panel bg-panel text-muted hover:bg-raised hover:text-fg",
    );
  }

  // Pane chips invert the agent chips: where an agent reads as a filled gray
  // pill, a pane is borderless and unfilled at rest (the composer card shows
  // through) and fills gray on hover — a second, type-level signal alongside the
  // green pane icon. The transparent border preserves the chip's box size so it
  // aligns with the bordered agent chips beside it.
  function paneChipClass(selected: boolean): string {
    return cn(
      "focus-visible:ring-accent inline-flex h-6 items-center gap-1 rounded-full border px-2 text-xs transition-colors focus-visible:ring-2 focus-visible:outline-none",
      selected
        ? "bg-accent-soft text-fg border-transparent"
        : "border-transparent bg-transparent text-muted hover:bg-panel hover:text-fg",
    );
  }

  // A required input/arg is unfilled when empty (string) or empty list. A single
  // `text` field also counts as filled when it carries ≥1 forward source — the
  // forwarded output fills it (the backend invalidates only if every source also
  // turns out empty, which can't be known until the sources settle).
  function isMissingInput(name: string, ty: string, optional: boolean): boolean {
    if (optional) return false;
    if (ty === "agent_list" || ty === "text_list") return asList(name).length === 0;
    return asString(name).trim() === "" && !hasSources(name);
  }
  function isMissingDerived(arg: DerivedArgInfo): boolean {
    return arg.required && asString(arg.name).trim() === "" && !hasSources(arg.name);
  }

  // A light "which prompt this feeds" hint for a derived field.
  function feedsLabel(arg: DerivedArgInfo): string {
    return arg.prompts.length > 0 ? `feeds ${arg.prompts.join(", ")}` : "";
  }

  const compat = $derived(descriptor.compatibility);
  const incompatible = $derived(compat.state === "incompatible" ? compat : null);
  // An `unresolved` prompt is pending (cold MCP cache) **only until a sync has
  // settled** for this pick; after that, still-unresolved means the MCP prompt is
  // genuinely gone, so it's a blocking "not found" error, not a perpetual spinner.
  // (A missing local/builtin prompt never reaches here — the backend classifies it
  // `incompatible` immediately.)
  const unresolvedMissing = $derived(
    compat.state === "unresolved" && syncSettled ? compat.prompts : null,
  );
  const pending = $derived(loading || (compat.state === "unresolved" && !syncSettled));

  const missing = $derived([
    ...descriptor.inputs.filter((i) => isMissingInput(i.name, i.ty, i.optional)).map((i) => i.name),
    ...descriptor.derived_args.filter(isMissingDerived).map((a) => a.name),
  ]);
</script>

<div class="flex flex-col gap-3" data-testid="workflow-composer">
  <div class="flex flex-col gap-1">
    <div class="flex items-center justify-between gap-2">
      <div class="flex min-w-0 items-baseline gap-1.5">
        <span class="text-fg truncate text-sm font-semibold" data-testid="workflow-composer-name">
          {descriptor.name}
        </span>
        {#if descriptor.is_builtin}
          <span
            class="border-border/80 text-muted shrink-0 rounded border px-1 text-[10px] uppercase"
          >
            Built-in
          </span>
        {/if}
      </div>
      <button
        type="button"
        class="text-muted hover:bg-panel hover:text-status-failed inline-flex h-7 w-7 shrink-0 items-center justify-center rounded-full transition-colors"
        data-testid="workflow-composer-remove"
        aria-label="Remove workflow"
        onclick={onremove}
      >
        <svg
          width="16"
          height="16"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M18 6 6 18M6 6l12 12" />
        </svg>
      </button>
    </div>

    {#if descriptor.description}
      <p class="text-muted text-xs leading-relaxed">{descriptor.description}</p>
    {/if}
  </div>

  {#if descriptor.invocable && descriptor.steps.length > 0}
    <!-- Preview of the workflow's steps: what it does and which agents it will
         invoke. Slot recipients resolve live against `inputs` as the user binds
         agents below (the same shared step component the live run view uses). -->
    <div
      class="border-border/70 bg-surface/40 rounded-md border px-2.5 py-2"
      data-testid="workflow-steps-preview"
    >
      <WorkflowSteps steps={descriptor.steps} mode="preview" {inputs} />
    </div>
  {/if}

  {#if !descriptor.invocable}
    <p class="text-status-failed text-xs" data-testid="workflow-not-supported">
      This workflow uses a step type not supported in this version.
    </p>
  {:else if pending}
    <p
      class="text-muted flex items-center gap-1.5 text-xs"
      data-testid="workflow-resolving"
      aria-live="polite"
    >
      <svg
        class="h-3.5 w-3.5 animate-spin"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        aria-hidden="true"
      >
        <path d="M21 12a9 9 0 1 1-6.219-8.56" stroke-linecap="round" />
      </svg>
      Resolving prompts…
    </p>
  {:else if incompatible}
    <div
      class="text-status-failed flex flex-col gap-0.5 text-xs"
      data-testid="workflow-incompatible"
    >
      <span class="font-medium">This workflow can't run — its prompt has changed:</span>
      {#each incompatible.issues as issue (issue.prompt + issue.argument + issue.reason)}
        <span>• {issue.reason}</span>
      {/each}
    </div>
  {:else if unresolvedMissing}
    <div
      class="text-status-failed flex flex-col gap-0.5 text-xs"
      data-testid="workflow-prompt-missing"
    >
      <span class="font-medium">This workflow can't run — a prompt it uses wasn't found:</span>
      {#each unresolvedMissing as id (id)}
        <span>• prompt <code>{id}</code> is not available</span>
      {/each}
    </div>
  {/if}

  {#snippet paneChip(name: string, pane: TranscriptPane, selected: boolean, onpick: () => void)}
    <button
      type="button"
      aria-pressed={selected}
      class={paneChipClass(selected)}
      data-testid={`workflow-pane-${name}-${pane.id}`}
      title={paneMemberNames(pane).join(", ")}
      onclick={onpick}
    >
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="1.8"
        stroke-linecap="round"
        stroke-linejoin="round"
        class="text-accent h-3.5 w-3.5 shrink-0"
        aria-hidden="true"
      >
        <rect x="3" y="4" width="18" height="16" rx="2" />
        <path d="M12 4v16" />
      </svg>
      {pane.name}
    </button>
  {/snippet}

  <!-- Sets the pane chips (group selectors) apart from the agent chips (leaves)
       they govern, so the parent→child relationship reads at a glance. -->
  {#snippet groupDivider()}
    <span class="bg-border/70 mx-0.5 w-px self-stretch" aria-hidden="true"></span>
  {/snippet}

  {#snippet forwardPicker(name: string)}
    <!-- ↪ sits beside the textarea (top-aligned, fixed square) so it reads as an
         action on that field. The field's own add closure is passed in. -->
    {#if agents.length > 0}
      <ForwardSourcePicker
        {agents}
        {panes}
        {agentReadiness}
        onPickAgent={(agent) => addArgSource(name, forwardSourceForAgent(agent))}
        onPickPane={(pane) => {
          for (const source of forwardSourceAgentsForPane(pane, agents)) addArgSource(name, source);
        }}
        showPaneShortcuts
        triggerTestid={`workflow-forward-picker-${name}`}
        triggerLabel={`Forward an agent's output into ${name}`}
        tooltipLabel="Forward an agent's output"
        triggerClass={cn(ICON_BUTTON_ON_RAISED_CLASS, "shrink-0 self-center")}
      />
    {/if}
  {/snippet}

  {#snippet forwardChips(name: string)}
    {@const sources = forwardSources[name] ?? []}
    {#if sources.length > 0}
      <div
        class="flex flex-wrap items-center gap-1.5"
        data-testid={`workflow-forward-sources-${name}`}
      >
        {#each sources as source (forwardSourceKey(source))}
          <ForwardSourceChip
            {source}
            readiness={agentReadiness?.(source.id) ?? "ready"}
            onRemove={() => removeArgSource(name, forwardSourceKey(source))}
          />
        {/each}
        {#if sources.length > 1}
          <!-- Each chip carries its own ✕; the bulk clear (same ⊘ glyph as
               "Clear recipients") only earns its place once there are several to
               drop at once. -->
          <button
            type="button"
            class={cn(ICON_BUTTON_ON_RAISED_CLASS, "ml-0.5 shrink-0")}
            data-testid={`workflow-forward-sources-${name}-clear`}
            aria-label="Clear forward sources"
            title="Clear forward sources"
            onclick={() => clearArgSources(name)}
          >
            <ClearIcon />
          </button>
        {/if}
      </div>
    {/if}
  {/snippet}

  {#if descriptor.invocable && !pending && !incompatible && !unresolvedMissing}
    <div class="flex flex-col gap-3">
      {#each descriptor.inputs as input (input.name)}
        <div class="flex flex-col gap-1" data-testid={`workflow-field-${input.name}`}>
          <label class="text-muted flex items-baseline gap-1.5 text-xs" for={`wf-${input.name}`}>
            <span class="text-fg font-medium">{input.name}</span>
            {#if input.optional}<span class="text-muted">(optional)</span>{/if}
          </label>
          {#if input.description}
            <span class="text-muted text-[11px] leading-4">{input.description}</span>
          {/if}

          {#if input.ty === "agent"}
            <div class="flex flex-wrap gap-1.5">
              {#if multiPane}
                {#each panes as pane (pane.id)}
                  <!-- A single-agent slot can only hold one agent, so only
                       single-member panes map cleanly; multi-member panes are
                       offered on `[agent]` inputs, not here. -->
                  {#if pane.members.length === 1}
                    {@render paneChip(
                      input.name,
                      pane,
                      paneHoldsSelectedAgent(input.name, pane),
                      () => selectPaneMember(input.name, pane),
                    )}
                  {/if}
                {/each}
                {#if hasSingleMemberPane}{@render groupDivider()}{/if}
              {/if}
              <span class="contents" role="radiogroup" aria-label={input.name}>
                {#each agents as agent (agent.id)}
                  <button
                    type="button"
                    role="radio"
                    aria-checked={asString(input.name) === agent.name}
                    class={chipClass(asString(input.name) === agent.name)}
                    data-testid={`workflow-agent-${input.name}-${agent.name}`}
                    onclick={() => setAgent(input.name, agent.name)}
                  >
                    <HarnessIcon harness={agent.harness} size="sm" class="h-3.5 w-3.5" />
                    {agent.name}
                  </button>
                {/each}
              </span>
            </div>
          {:else if input.ty === "agent_list"}
            <div class="flex flex-wrap gap-1.5">
              {#if multiPane}
                {#each panes as pane (pane.id)}
                  {#if pane.members.length > 0}
                    {@render paneChip(input.name, pane, paneSelected(input.name, pane), () =>
                      togglePane(input.name, pane),
                    )}
                  {/if}
                {/each}
                {@render groupDivider()}
              {/if}
              {#each agents as agent (agent.id)}
                <button
                  type="button"
                  aria-pressed={asList(input.name).includes(agent.name)}
                  class={chipClass(asList(input.name).includes(agent.name))}
                  data-testid={`workflow-agent-${input.name}-${agent.name}`}
                  onclick={() => toggleAgent(input.name, agent.name)}
                >
                  <HarnessIcon harness={agent.harness} size="sm" class="h-3.5 w-3.5" />
                  {agent.name}
                </button>
              {/each}
            </div>
          {:else if input.ty === "text_list"}
            <Textarea
              id={`wf-${input.name}`}
              data-testid={`workflow-text-${input.name}`}
              placeholder="One item per line"
              value={asList(input.name).join("\n")}
              oninput={(e: Event) => {
                const text = (e.currentTarget as HTMLTextAreaElement).value;
                inputs[input.name] = text.split("\n").filter((l) => l.trim() !== "");
              }}
            />
          {:else}
            <div class="flex items-start gap-1.5">
              <Textarea
                id={`wf-${input.name}`}
                data-testid={`workflow-text-${input.name}`}
                class="flex-1"
                bind:ref={fieldRefs[input.name]}
                value={asString(input.name)}
                oninput={(e: Event) => {
                  inputs[input.name] = (e.currentTarget as HTMLTextAreaElement).value;
                }}
              />
              {@render forwardPicker(input.name)}
            </div>
            {@render forwardChips(input.name)}
          {/if}
        </div>
      {/each}

      {#each descriptor.derived_args as arg (arg.name)}
        <div class="flex flex-col gap-1" data-testid={`workflow-arg-${arg.name}`}>
          <label class="text-muted flex items-baseline gap-1.5 text-xs" for={`wf-arg-${arg.name}`}>
            <span class="text-fg font-medium">{arg.name}</span>
            {#if !arg.required}<span class="text-muted">(optional)</span>{/if}
            {#if feedsLabel(arg)}
              <span class="text-muted text-[10px]" data-testid={`workflow-arg-feeds-${arg.name}`}>
                {feedsLabel(arg)}
              </span>
            {/if}
          </label>
          {#if arg.description}
            <span class="text-muted text-[11px] leading-4">{arg.description}</span>
          {/if}
          <div class="flex items-start gap-1.5">
            <Textarea
              id={`wf-arg-${arg.name}`}
              data-testid={`workflow-arg-input-${arg.name}`}
              class="flex-1"
              bind:ref={fieldRefs[arg.name]}
              value={asString(arg.name)}
              oninput={(e: Event) => {
                inputs[arg.name] = (e.currentTarget as HTMLTextAreaElement).value;
              }}
            />
            {@render forwardPicker(arg.name)}
          </div>
          {@render forwardChips(arg.name)}
        </div>
      {/each}
    </div>
  {/if}

  <div class="flex items-center justify-end gap-2">
    {#if missing.length > 0 && descriptor.invocable && !pending && !incompatible && !unresolvedMissing}
      <span class="text-muted text-xs" data-testid="workflow-missing">
        Fill required fields to run
      </span>
    {/if}
    {@render invoke?.()}
  </div>
</div>
