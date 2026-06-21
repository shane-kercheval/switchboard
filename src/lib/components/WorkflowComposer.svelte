<script lang="ts">
  import type { AgentRecord, Prompt, WorkflowInputValue, WorkflowListing } from "$lib/types";
  import type { TranscriptPane } from "$lib/state/transcriptPanes.svelte";
  import { promptDisplayName } from "$lib/prompt";
  import { cn } from "$lib/utils";
  import Textarea from "$lib/components/ui/Textarea.svelte";
  import HarnessIcon from "$lib/components/ui/HarnessIcon.svelte";
  import PromptMenu from "$lib/components/PromptMenu.svelte";

  /// The workflow invocation form — one field per declared input, modeled on
  /// `PromptComposer`. A workflow parameterizes its *recipients* (its declared
  /// `agent`/`[agent]` inputs are named slots bound to real agents here), which
  /// is why the compose bar hides its own To field in workflow mode: the workflow
  /// owns routing. Picking is per-type: agent chips for `agent`/`[agent]`, the
  /// prompt menu for `prompt_id`, text fields for `text`/`[text]`.
  let {
    workflow,
    agents,
    prompts,
    panes = [],
    inputs = $bindable(),
    onremove,
    invoke,
  }: {
    workflow: WorkflowListing;
    agents: AgentRecord[];
    prompts: Prompt[];
    /// The project's panes, offered as a quick way to fill an `[agent]` input with
    /// a whole pane's members at once (a single `agent` slot can't hold a pane, so
    /// pane chips show only on list inputs).
    panes?: TranscriptPane[];
    /// Bound input values, keyed by input name. Scalar inputs hold a string; list
    /// inputs hold a string[]. Pre-seeded by the parent (recommended prompts).
    inputs: Record<string, WorkflowInputValue>;
    onremove: () => void;
    /// The invoke button(s), rendered in the footer by the parent (so the parent
    /// owns the actual invoke call + busy state).
    invoke?: import("svelte").Snippet;
  } = $props();

  // Which prompt_id field's prompt menu is open (by input name), if any.
  let promptMenuFor = $state<string | null>(null);

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
  function selectPaneMember(name: string, pane: TranscriptPane): void {
    const [member] = paneMemberNames(pane);
    if (member !== undefined) inputs[name] = member;
  }
  // Recipient-chip styling reused from the compose bar's To field (icon + low
  // height), minus the position number, which has no meaning here.
  function chipClass(selected: boolean): string {
    return cn(
      "focus-visible:ring-accent inline-flex items-center gap-1 rounded-full border py-px pr-2 pl-1.5 text-sm transition-colors focus-visible:ring-2 focus-visible:outline-none",
      selected
        ? "bg-accent-soft text-fg border-transparent"
        : "border-panel bg-panel text-muted hover:bg-raised hover:text-fg",
    );
  }

  function pickPrompt(name: string, prompt: Prompt): void {
    inputs[name] = `${prompt.provider}:${prompt.name}`;
    promptMenuFor = null;
  }
  function promptLabel(name: string): string {
    const id = asString(name);
    if (id === "") return "Select a prompt…";
    const found = prompts.find((p) => `${p.provider}:${p.name}` === id);
    return found ? promptDisplayName(found) : id;
  }

  // A required input is unfilled when its value is empty (string) or empty list.
  function isMissing(inputName: string, ty: string, optional: boolean): boolean {
    if (optional) return false;
    if (ty === "agent_list" || ty === "text_list") return asList(inputName).length === 0;
    return asString(inputName).trim() === "";
  }

  const missing = $derived(
    workflow.inputs.filter((i) => isMissing(i.name, i.ty, i.optional)).map((i) => i.name),
  );
  // Expose to the parent (it gates the invoke button on `missing` + invocable).
  export function missingRequired(): string[] {
    return workflow.invocable ? missing : workflow.inputs.map((i) => i.name);
  }
</script>

<div class="flex flex-col gap-3" data-testid="workflow-composer">
  <div class="flex flex-col gap-1">
    <div class="flex items-center justify-between gap-2">
      <div class="flex min-w-0 items-baseline gap-1.5">
        <span class="text-fg truncate text-sm font-semibold" data-testid="workflow-composer-name">
          {workflow.name}
        </span>
        {#if workflow.is_builtin}
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

    {#if workflow.description}
      <p class="text-muted text-xs leading-relaxed">{workflow.description}</p>
    {/if}
  </div>

  {#if !workflow.invocable}
    <p class="text-status-failed text-xs" data-testid="workflow-not-supported">
      This workflow uses a step type not supported in this version.
    </p>
  {/if}

  {#snippet paneChip(name: string, pane: TranscriptPane, selected: boolean, onpick: () => void)}
    <button
      type="button"
      aria-pressed={selected}
      class={chipClass(selected)}
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
        class="h-3.5 w-3.5 shrink-0"
        aria-hidden="true"
      >
        <rect x="3" y="4" width="18" height="16" rx="2" />
        <path d="M12 4v16" />
      </svg>
      {pane.name}
    </button>
  {/snippet}

  <div class="flex flex-col gap-3">
    {#each workflow.inputs as input (input.name)}
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
        {:else if input.ty === "prompt_id"}
          <div class="relative">
            <button
              type="button"
              class="border-border bg-panel text-fg hover:bg-panel/80 w-full rounded-md border px-2.5 py-1.5 text-left text-sm"
              data-testid={`workflow-prompt-${input.name}`}
              onclick={() => (promptMenuFor = promptMenuFor === input.name ? null : input.name)}
            >
              {promptLabel(input.name)}
            </button>
            {#if promptMenuFor === input.name}
              <PromptMenu
                {prompts}
                onpick={(p) => pickPrompt(input.name, p)}
                onclose={() => (promptMenuFor = null)}
              />
            {/if}
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
          <Textarea
            id={`wf-${input.name}`}
            data-testid={`workflow-text-${input.name}`}
            value={asString(input.name)}
            oninput={(e: Event) => {
              inputs[input.name] = (e.currentTarget as HTMLTextAreaElement).value;
            }}
          />
        {/if}
      </div>
    {/each}
  </div>

  <div class="flex items-center justify-end gap-2">
    {#if missing.length > 0 && workflow.invocable}
      <span class="text-muted text-xs" data-testid="workflow-missing">
        Fill required inputs to run
      </span>
    {/if}
    {@render invoke?.()}
  </div>
</div>
