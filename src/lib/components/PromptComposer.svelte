<script lang="ts">
  import type { Snippet } from "svelte";
  import { tick } from "svelte";
  import * as api from "$lib/api";
  import type { Prompt, AgentRecord, AgentId } from "$lib/types";
  import type { TranscriptPane } from "$lib/state/transcriptPanes.svelte";
  import type { ForwardSource } from "$lib/state/heldForwards.svelte";
  import {
    buildRenderArgs,
    combinePromptMessage,
    missingRequiredArgs,
    promptDisplayName,
  } from "$lib/prompt";
  import Textarea from "$lib/components/ui/Textarea.svelte";
  import Button from "$lib/components/ui/Button.svelte";
  import Dialog from "$lib/components/ui/Dialog.svelte";
  import Markdown from "$lib/components/ui/Markdown.svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import ForwardSourceChip from "$lib/components/ui/ForwardSourceChip.svelte";
  import ForwardSourcePicker from "$lib/components/ui/ForwardSourcePicker.svelte";
  import { cn } from "$lib/utils";

  /// Prompt mode: the chosen prompt, its argument inputs, an appended-text field,
  /// and a Preview overlay of the combined message. The parent (`ComposeBar`)
  /// owns the canonical state via `bind:` so it can persist and send; this
  /// component is the editing surface plus the preview. (Changing the prompt is
  /// done from the compose bar's prompt button — remove-and-pick — so there is no
  /// in-composer "change" affordance.)
  let {
    prompt,
    args = $bindable(),
    appendedText = $bindable(),
    argSources = $bindable({}),
    agents = [],
    panes = [],
    agentHasOutput,
    onremove,
    send,
    focusFirstField = false,
    busy = false,
  }: {
    prompt: Prompt;
    args: Record<string, string>;
    appendedText: string;
    /// Per-argument forward sources — the agents whose latest output gets composed
    /// into each argument (typed text first, then the forwarded blocks). Bound so
    /// the compose bar can read them at send time and route through the
    /// forward-prompt path. Live-UI-only, like the compose bar's own forward set.
    argSources?: Record<string, ForwardSource[]>;
    agents?: AgentRecord[];
    panes?: TranscriptPane[];
    /// Flags agents with no completed output yet, so the per-argument picker and
    /// chips can show "no output" before send.
    agentHasOutput?: (id: AgentId) => boolean;
    onremove: () => void;
    /// The compose bar's send button, rendered in the footer row beside Preview
    /// so the two actions align. Optional so the component stands alone in tests.
    send?: Snippet;
    /// Focuses the first editable prompt field when a user explicitly selects a
    /// prompt from the picker. Saved/restored prompt drafts leave focus alone.
    focusFirstField?: boolean;
    busy?: boolean;
  } = $props();

  function addArgSource(argName: string, source: ForwardSource): void {
    const list = argSources[argName] ?? [];
    if (list.some((s) => s.id === source.id)) return;
    argSources[argName] = [...list, source];
  }

  function addArgSourcesFromPane(argName: string, pane: TranscriptPane): void {
    // Expand the pane to its members at pick time; each becomes its own source —
    // the canonical composition labels agents individually, never the pane.
    for (const id of pane.members) {
      const agent = agents.find((a) => a.id === id);
      if (agent) addArgSource(argName, { id: agent.id, name: agent.name });
    }
  }

  function removeArgSource(argName: string, id: AgentId): void {
    argSources[argName] = (argSources[argName] ?? []).filter((s) => s.id !== id);
  }

  type PreviewState =
    | { kind: "idle" }
    | { kind: "loading" }
    | { kind: "ready"; text: string }
    | { kind: "error"; message: string };
  let preview = $state<PreviewState>({ kind: "idle" });
  let previewOpen = $state(false);
  let argRefs = $state<Record<string, HTMLTextAreaElement | undefined>>({});
  let appendedRef = $state<HTMLTextAreaElement | undefined>(undefined);
  let focusedPromptKey = $state<string | null>(null);

  // A required argument with ≥1 forward source is not "missing" even when typed
  // empty — the forwarded output fills it (the backend invalidates only if every
  // source also turns out empty, which can't be known until the sources settle).
  const missing = $derived(
    missingRequiredArgs(prompt, args).filter((name) => (argSources[name]?.length ?? 0) === 0),
  );
  const promptKey = $derived(`${prompt.provider}:${prompt.name}`);

  /// Build the preview args: forwarded arguments can't show real forwarded output
  /// (it's live, resolved server-side at send time), so each shows a placeholder
  /// after any typed lead text — the preview conveys structure, not final content.
  function previewArgs(): Record<string, string> {
    const out = buildRenderArgs(prompt, args);
    for (const arg of prompt.arguments) {
      const sources = argSources[arg.name] ?? [];
      if (sources.length === 0) continue;
      const typed = (args[arg.name] ?? "").trim();
      const placeholders = sources.map((s) => `«forwarding from ${s.name}…»`).join("\n\n");
      out[arg.name] = typed === "" ? placeholders : `${typed}\n\n${placeholders}`;
    }
    return out;
  }

  function firstPromptField(): HTMLTextAreaElement | undefined {
    const firstArg = prompt.arguments[0];
    return firstArg === undefined ? appendedRef : argRefs[firstArg.name];
  }

  $effect(() => {
    if (!focusFirstField || focusedPromptKey === promptKey) return;
    const targetPromptKey = promptKey;
    focusedPromptKey = targetPromptKey;
    void tick().then(() => {
      if (focusedPromptKey === targetPromptKey) firstPromptField()?.focus();
    });
  });

  function openPreview(): void {
    if (busy) return;
    previewOpen = true;
    void runPreview();
  }

  async function runPreview(): Promise<void> {
    preview = { kind: "loading" };
    try {
      const rendered = await api.renderPrompt(prompt.provider, prompt.name, previewArgs());
      preview = { kind: "ready", text: combinePromptMessage(rendered.text, appendedText) };
    } catch (e) {
      preview = { kind: "error", message: e instanceof Error ? e.message : String(e) };
    }
  }
</script>

<div
  class="relative flex max-h-[min(56dvh,34rem)] min-h-0 flex-col overflow-hidden"
  data-testid="prompt-composer"
  data-shortcut-scope="composer"
  aria-busy={busy}
>
  <div
    class={cn(
      "flex min-h-0 flex-col gap-3 transition-[filter,opacity]",
      busy ? "opacity-55 blur-[1px]" : "",
    )}
    data-testid="prompt-composer-content"
  >
    <div class="flex items-center gap-1.5">
      <div
        class="border-border bg-panel inline-flex h-7 min-w-0 items-center gap-1.5 rounded-full border px-3"
        data-testid="prompt-selector"
      >
        <span class="text-fg truncate text-sm font-medium">{promptDisplayName(prompt)}</span>
        <span class="text-muted shrink-0 font-mono text-[11px]">{prompt.provider}</span>
      </div>
      <button
        type="button"
        class="text-muted hover:bg-panel hover:text-status-failed inline-flex h-7 w-7 shrink-0 items-center justify-center rounded-full transition-colors"
        data-testid="prompt-remove"
        aria-label="Remove prompt"
        disabled={busy}
        onclick={() => {
          if (!busy) onremove();
        }}
        class:cursor-not-allowed={busy}
        class:opacity-50={busy}
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

    {#if prompt.description}
      <p class="text-muted text-xs">{prompt.description}</p>
    {/if}

    <div
      class="min-h-0 [scrollbar-gutter:stable] space-y-3 overflow-y-auto py-1 pr-3 pl-1"
      data-testid="prompt-fields-scroll"
    >
      {#each prompt.arguments as arg (arg.name)}
        {@const isMissing = missing.includes(arg.name)}
        {@const sources = argSources[arg.name] ?? []}
        <div class="flex flex-col gap-1">
          <div class="flex items-baseline justify-between gap-1.5">
            <label
              class="text-fg flex items-baseline gap-1.5 text-xs font-medium"
              for={`prompt-arg-${arg.name}`}
            >
              <span>{arg.name}</span>
              {#if arg.required}
                <span class="text-status-failed" data-testid={`prompt-arg-required-${arg.name}`}
                  >required</span
                >
              {:else}
                <span class="text-muted font-normal">optional</span>
              {/if}
            </label>
            {#if agents.length > 0}
              <ForwardSourcePicker
                {agents}
                {panes}
                onPickAgent={(agent) => addArgSource(arg.name, { id: agent.id, name: agent.name })}
                onPickPane={(pane) => addArgSourcesFromPane(arg.name, pane)}
                {agentHasOutput}
                disabled={busy}
                triggerTestid={`prompt-arg-forward-${arg.name}`}
                triggerLabel={`Forward an agent's output into ${arg.name}`}
                tooltipLabel="Forward an agent's output"
                triggerClass="text-muted hover:text-fg hover:bg-panel focus-visible:ring-accent flex h-5 w-5 shrink-0 items-center justify-center rounded-full transition-colors focus-visible:ring-2 focus-visible:outline-none"
              />
            {/if}
          </div>
          {#if arg.description}
            <p class="text-muted text-[11px]">{arg.description}</p>
          {/if}
          <Textarea
            autosize
            id={`prompt-arg-${arg.name}`}
            data-testid={`prompt-arg-${arg.name}`}
            rows={2}
            bind:ref={argRefs[arg.name]}
            bind:value={args[arg.name]}
            disabled={busy}
            class={cn("max-h-40 min-h-9 text-sm", isMissing ? "border-status-failed" : "")}
          />
          {#if sources.length > 0}
            <div
              class="flex flex-wrap items-center gap-1.5"
              data-testid={`prompt-arg-sources-${arg.name}`}
            >
              {#each sources as source (source.id)}
                <ForwardSourceChip
                  {source}
                  empty={agentHasOutput ? !agentHasOutput(source.id) : false}
                  disabled={busy}
                  onRemove={() => removeArgSource(arg.name, source.id)}
                />
              {/each}
            </div>
          {/if}
        </div>
      {/each}

      <div class="flex flex-col gap-1">
        <label class="text-fg text-xs font-medium" for="prompt-appended">Appended text</label>
        <Textarea
          autosize
          id="prompt-appended"
          data-testid="prompt-appended"
          rows={2}
          placeholder="Optional text appended after the prompt…"
          bind:ref={appendedRef}
          bind:value={appendedText}
          disabled={busy}
          class="max-h-40 min-h-9 text-sm"
        />
      </div>
    </div>

    <div class="flex items-center justify-between gap-2">
      <Button
        variant="secondary"
        size="sm"
        data-testid="prompt-preview-button"
        disabled={busy || missing.length > 0}
        onclick={openPreview}
      >
        Preview
      </Button>
      {@render send?.()}
    </div>
  </div>

  {#if busy}
    <div
      class="bg-raised/70 absolute inset-0 z-10 flex items-center justify-center rounded-lg backdrop-blur-[1px]"
      data-testid="prompt-rendering"
      role="status"
    >
      <div
        class="border-border bg-raised text-fg flex items-center gap-2 rounded-full border px-3 py-2 text-sm shadow-lg"
      >
        <Spinner class="h-4 w-4 shrink-0" />
        <span>Rendering prompt…</span>
      </div>
    </div>
  {/if}
</div>

<Dialog
  bind:open={previewOpen}
  title="Preview"
  onClose={() => (previewOpen = false)}
  contentClass="max-w-2xl"
>
  {#if preview.kind === "loading"}
    <div
      class="text-muted flex items-center gap-2 text-sm"
      data-testid="prompt-preview-loading"
      role="status"
    >
      <Spinner class="h-4 w-4" />
      Rendering preview…
    </div>
  {:else if preview.kind === "error"}
    <div class="text-status-failed text-sm" data-testid="prompt-preview-error">
      Preview failed: {preview.message}
    </div>
  {:else if preview.kind === "ready"}
    <div class="max-h-[60vh] overflow-y-auto" data-testid="prompt-preview">
      <Markdown text={preview.text} />
    </div>
  {/if}
</Dialog>
