<script lang="ts">
  import type { AgentRecord, Prompt, WorkflowInputValue, WorkflowListing } from "$lib/types";
  import { promptDisplayName } from "$lib/prompt";
  import { cn } from "$lib/utils";
  import Textarea from "$lib/components/ui/Textarea.svelte";
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
    inputs = $bindable(),
    onremove,
    invoke,
  }: {
    workflow: WorkflowListing;
    agents: AgentRecord[];
    prompts: Prompt[];
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

<div
  class="border-border bg-panel/40 flex flex-col gap-3 rounded-lg border p-3"
  data-testid="workflow-composer"
>
  <div class="flex items-center justify-between gap-2">
    <div class="flex items-baseline gap-1.5">
      <span class="text-fg text-sm font-semibold" data-testid="workflow-composer-name">
        {workflow.name}
      </span>
      {#if workflow.is_builtin}
        <span class="border-border/80 text-muted rounded border px-1 text-[10px] uppercase">
          Built-in
        </span>
      {/if}
    </div>
    <button
      type="button"
      class="text-muted hover:text-fg text-xs"
      data-testid="workflow-composer-remove"
      onclick={onremove}
    >
      Remove
    </button>
  </div>

  {#if workflow.description}
    <p class="text-muted text-xs leading-relaxed">{workflow.description}</p>
  {/if}

  {#if !workflow.invocable}
    <p class="text-status-failed text-xs" data-testid="workflow-not-supported">
      This workflow uses a step type not supported in this version.
    </p>
  {/if}

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
          <div class="flex flex-wrap gap-1.5" role="radiogroup" aria-label={input.name}>
            {#each agents as agent (agent.id)}
              <button
                type="button"
                role="radio"
                aria-checked={asString(input.name) === agent.name}
                class={cn(
                  "rounded-full border px-2.5 py-1 text-xs",
                  asString(input.name) === agent.name
                    ? "border-accent bg-accent/15 text-fg"
                    : "border-border text-muted hover:text-fg",
                )}
                data-testid={`workflow-agent-${input.name}-${agent.name}`}
                onclick={() => setAgent(input.name, agent.name)}
              >
                {agent.name}
              </button>
            {/each}
          </div>
        {:else if input.ty === "agent_list"}
          <div class="flex flex-wrap gap-1.5">
            {#each agents as agent (agent.id)}
              <button
                type="button"
                aria-pressed={asList(input.name).includes(agent.name)}
                class={cn(
                  "rounded-full border px-2.5 py-1 text-xs",
                  asList(input.name).includes(agent.name)
                    ? "border-accent bg-accent/15 text-fg"
                    : "border-border text-muted hover:text-fg",
                )}
                data-testid={`workflow-agent-${input.name}-${agent.name}`}
                onclick={() => toggleAgent(input.name, agent.name)}
              >
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
