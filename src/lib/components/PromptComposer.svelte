<script lang="ts">
  import type { Snippet } from "svelte";
  import { tick } from "svelte";
  import * as api from "$lib/api";
  import type { Prompt, AgentRecord, AgentId } from "$lib/types";
  import type { TranscriptPane } from "$lib/state/transcriptPanes.svelte";
  import {
    forwardSourceKey,
    forwardSourceForAgent,
    forwardSourceAgentsForPane,
    type ForwardSource,
  } from "$lib/state/heldForwards.svelte";
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
    appendedSources = $bindable([]),
    agents = [],
    panes = [],
    agentHasOutput,
    onremove,
    send,
    recipients,
    focusFirstField = false,
    busy = false,
  }: {
    prompt: Prompt;
    args: Record<string, string>;
    appendedText: string;
    /// Per-argument forward sources — the agents/panes whose latest output gets
    /// composed into each argument (typed text first, then the forwarded blocks).
    /// Bound so the compose bar can read them at send time and route through the
    /// forward-prompt path. Live-UI-only, like the compose bar's own forward set.
    argSources?: Record<string, ForwardSource[]>;
    /// Forward sources for the appended-text field — the appended text is just
    /// another forwardable field (composed into the appended tail at send).
    appendedSources?: ForwardSource[];
    agents?: AgentRecord[];
    panes?: TranscriptPane[];
    /// Flags agents with no completed output yet, so the per-field picker and
    /// chips can show "no output" before send.
    agentHasOutput?: (id: AgentId) => boolean;
    onremove: () => void;
    /// The compose bar's send button, rendered in the footer row beside Preview
    /// so the two actions align. Optional so the component stands alone in tests.
    send?: Snippet;
    /// The recipient ("To") chips, handed down by the compose bar so they render
    /// directly under the prompt name (the prompt titles the whole send, above
    /// the recipients). Optional so the component stands alone in tests.
    recipients?: Snippet;
    /// Focuses the first editable prompt field when a user explicitly selects a
    /// prompt from the picker. Saved/restored prompt drafts leave focus alone.
    focusFirstField?: boolean;
    busy?: boolean;
  } = $props();

  /// Whether a source has nothing to forward yet — an agent with no completed turn.
  function sourceIsEmpty(source: ForwardSource): boolean {
    if (!agentHasOutput) return false;
    return !agentHasOutput(source.id);
  }

  // Each forwardable field (every argument, plus the appended text) owns its own
  // source list; the snippets below are handed that list plus add/remove closures,
  // so there is no shared key namespace to collide with argument names.
  function withSource(list: ForwardSource[], source: ForwardSource): ForwardSource[] {
    return list.some((s) => forwardSourceKey(s) === forwardSourceKey(source))
      ? list
      : [...list, source];
  }

  function addArgSource(name: string, source: ForwardSource): void {
    argSources[name] = withSource(argSources[name] ?? [], source);
  }

  function removeArgSource(name: string, key: string): void {
    argSources[name] = (argSources[name] ?? []).filter((s) => forwardSourceKey(s) !== key);
  }

  function addAppendedSource(source: ForwardSource): void {
    appendedSources = withSource(appendedSources, source);
  }

  function removeAppendedSource(key: string): void {
    appendedSources = appendedSources.filter((s) => forwardSourceKey(s) !== key);
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
  function withPlaceholders(typed: string, sources: ForwardSource[]): string {
    const lead = typed.trim();
    const placeholders = sources.map((s) => `«forwarding from ${s.name}…»`).join("\n\n");
    return lead === "" ? placeholders : `${lead}\n\n${placeholders}`;
  }

  function previewArgs(): Record<string, string> {
    const out = buildRenderArgs(prompt, args);
    for (const arg of prompt.arguments) {
      const sources = argSources[arg.name] ?? [];
      if (sources.length === 0) continue;
      out[arg.name] = withPlaceholders(args[arg.name] ?? "", sources);
    }
    return out;
  }

  /// The appended text as previewed: forwarded appended sources show as
  /// placeholders after any typed appended lead (real output resolves at send).
  function previewAppended(): string {
    return appendedSources.length === 0
      ? appendedText
      : withPlaceholders(appendedText, appendedSources);
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

  /// The add-source closure for whichever of this composer's fields currently
  /// holds focus (an argument textarea or the appended-text box), or `null` when
  /// focus is elsewhere. Lets the ⌘⌃N pane chord target the field being typed in.
  function focusedFieldAdd(): ((source: ForwardSource) => void) | null {
    const active = document.activeElement;
    if (active === null) return null;
    for (const arg of prompt.arguments) {
      if (argRefs[arg.name] === active) return (source) => addArgSource(arg.name, source);
    }
    if (appendedRef === active) return addAppendedSource;
    return null;
  }

  // ⌘⌃1..9 → forward pane N into the focused field, mirroring the compose bar's
  // whole-message chord but routed per-field (the compose bar's own handler
  // no-ops in prompt mode, so there's no double-fire). Index matches the pane's
  // position in `panes`, the same order the picker shows the chord for.
  $effect(() => {
    function onKeydown(e: KeyboardEvent): void {
      if (busy) return;
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

  function openPreview(): void {
    if (busy) return;
    previewOpen = true;
    void runPreview();
  }

  async function runPreview(): Promise<void> {
    preview = { kind: "loading" };
    try {
      const rendered = await api.renderPrompt(prompt.provider, prompt.name, previewArgs());
      preview = { kind: "ready", text: combinePromptMessage(rendered.text, previewAppended()) };
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
    <div class="flex flex-col gap-1">
      <div class="flex items-center justify-between gap-2">
        <div class="flex min-w-0 items-baseline gap-1.5" data-testid="prompt-selector">
          <span class="text-fg truncate text-sm font-semibold">{promptDisplayName(prompt)}</span>
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
    </div>

    {@render recipients?.()}

    <div
      class="min-h-0 [scrollbar-gutter:stable] space-y-3 overflow-y-auto py-1 pr-3 pl-1"
      data-testid="prompt-fields-scroll"
    >
      {#snippet fieldPicker(onAdd: (source: ForwardSource) => void, label: string, testid: string)}
        <!-- ↪ sits beside the input (top-aligned, fixed square) so it reads as an
             action on that field, not a floating label-row control. The field's
             own add closure is passed in — no shared key namespace. -->
        <ForwardSourcePicker
          {agents}
          {panes}
          onPickAgent={(agent) => onAdd(forwardSourceForAgent(agent))}
          onPickPane={(pane) => {
            for (const source of forwardSourceAgentsForPane(pane, agents)) onAdd(source);
          }}
          {agentHasOutput}
          disabled={busy}
          showPaneShortcuts
          triggerTestid={testid}
          triggerLabel={label}
          tooltipLabel="Forward an agent's output"
          triggerClass="text-muted hover:text-fg hover:bg-panel border-border focus-visible:ring-accent flex h-9 w-9 shrink-0 items-center justify-center self-start rounded-md border transition-colors focus-visible:ring-2 focus-visible:outline-none"
        />
      {/snippet}

      {#snippet fieldChips(
        sources: ForwardSource[],
        onRemove: (key: string) => void,
        onClear: () => void,
        testid: string,
      )}
        {#if sources.length > 0}
          <div class="flex flex-wrap items-center gap-1.5" data-testid={testid}>
            {#each sources as source (forwardSourceKey(source))}
              <ForwardSourceChip
                {source}
                empty={sourceIsEmpty(source)}
                disabled={busy}
                onRemove={() => onRemove(forwardSourceKey(source))}
              />
            {/each}
            {#if sources.length > 1}
              <!-- Each chip carries its own ✕; the bulk clear (same ⊘ glyph as
                   "Clear recipients") only earns its place once there are several
                   to drop at once. -->
              <button
                type="button"
                class="text-muted hover:text-fg hover:bg-panel ml-0.5 flex h-6 w-6 shrink-0 items-center justify-center rounded-full transition-colors disabled:opacity-50"
                data-testid={`${testid}-clear`}
                aria-label="Clear forward sources"
                title="Clear forward sources"
                disabled={busy}
                onclick={() => {
                  if (!busy) onClear();
                }}
              >
                <svg
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.75"
                  stroke-linecap="round"
                  class="h-4 w-4"
                  aria-hidden="true"
                >
                  <circle cx="12" cy="12" r="9" />
                  <path d="m5.6 5.6 12.8 12.8" />
                </svg>
              </button>
            {/if}
          </div>
        {/if}
      {/snippet}

      {#each prompt.arguments as arg (arg.name)}
        {@const isMissing = missing.includes(arg.name)}
        <div class="flex flex-col gap-1">
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
          {#if arg.description}
            <p class="text-muted text-[11px]">{arg.description}</p>
          {/if}
          <div class="flex items-start gap-1.5">
            <Textarea
              autosize
              id={`prompt-arg-${arg.name}`}
              data-testid={`prompt-arg-${arg.name}`}
              rows={2}
              bind:ref={argRefs[arg.name]}
              bind:value={args[arg.name]}
              disabled={busy}
              class={cn("max-h-40 min-h-9 flex-1 text-sm", isMissing ? "border-status-failed" : "")}
            />
            {#if agents.length > 0}
              {@render fieldPicker(
                (source) => addArgSource(arg.name, source),
                `Forward an agent's output into ${arg.name}`,
                `prompt-arg-forward-${arg.name}`,
              )}
            {/if}
          </div>
          {@render fieldChips(
            argSources[arg.name] ?? [],
            (key) => removeArgSource(arg.name, key),
            () => (argSources[arg.name] = []),
            `prompt-arg-sources-${arg.name}`,
          )}
        </div>
      {/each}

      <div class="flex flex-col gap-1">
        <label class="text-fg text-xs font-medium" for="prompt-appended">Appended text</label>
        <div class="flex items-start gap-1.5">
          <Textarea
            autosize
            id="prompt-appended"
            data-testid="prompt-appended"
            rows={2}
            placeholder="Optional text appended after the prompt…"
            bind:ref={appendedRef}
            bind:value={appendedText}
            disabled={busy}
            class="max-h-40 min-h-9 flex-1 text-sm"
          />
          {#if agents.length > 0}
            {@render fieldPicker(
              addAppendedSource,
              "Forward an agent's output into the appended text",
              "prompt-appended-forward",
            )}
          {/if}
        </div>
        {@render fieldChips(
          appendedSources,
          removeAppendedSource,
          () => (appendedSources = []),
          "prompt-appended-sources",
        )}
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
