<script lang="ts">
  import type { Snippet } from "svelte";
  import { tick } from "svelte";
  import * as api from "$lib/api";
  import type { Prompt, AgentRecord, AgentId, ProjectId } from "$lib/types";
  import type { TranscriptPane } from "$lib/state/transcriptPanes.svelte";
  import {
    forwardSourceKey,
    isForeignSource,
    forwardSourceForAgent,
    forwardSourceAgentsForPane,
    orderForwardSources,
    type ForwardReadiness,
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
  import ClearIcon from "$lib/components/ui/ClearIcon.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { cn } from "$lib/utils";
  import { COMPOSER_ACTION_BUTTON_CLASS, ICON_BUTTON_CLASS } from "$lib/components/ui/iconButton";

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
    agentReadiness,
    onremove,
    send,
    recipients,
    focusFirstField = false,
    busy = false,
    projectId,
    crossProjectBase,
  }: {
    /// The project composing this send — qualifies foreign forward-source labels.
    projectId: ProjectId;
    /// Cross-project forward sourcing, passed straight to each per-field picker
    /// so this surface offers the same Projects section as the compose bar.
    /// Shared cross-project sourcing. This component adds the field-scoped
    /// commit step itself — the picker's config requires one.
    crossProjectBase?: import("$lib/components/ui/ForwardSourcePicker.svelte").CrossProjectBase;
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
    /// Classifies what each agent would contribute, so the per-field picker and
    /// chips can warn before send that an empty source would block it.
    agentReadiness?: (id: AgentId) => ForwardReadiness;
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

  /// What a source will contribute at dispatch. Absent the classifier (the
  /// component stands alone in tests), assume it resolves normally.
  function sourceReadiness(source: ForwardSource): ForwardReadiness {
    // A foreign source's transcript isn't loaded in this project, so any
    // classification would be a guess — and the guess `forwardReadiness` makes
    // for a missing transcript is `"empty"`, which renders a false
    // "blocks your send" warning. Say nothing instead.
    if (isForeignSource(source, projectId)) return "unknown";
    return agentReadiness?.(source.id) ?? "ready";
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
    | { kind: "signing_in"; provider: string }
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
    const placeholders = orderForwardSources(sources, agents, projectId)
      .map((s) => `«forwarding from ${s.name}…»`)
      .join("\n\n");
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
    let signedInMidPreview = false;
    try {
      let outcome = await api.renderPrompt(prompt.provider, prompt.name, previewArgs());
      if (outcome.kind === "needs_sign_in") {
        // Previewing is an explicit use of this provider, and rerunning a
        // preview after sign-in dispatches nothing — so launch the browser
        // sign-in and rerun automatically. One attempt only; a second
        // needs-sign-in falls through to the error arm below.
        preview = { kind: "signing_in", provider: outcome.provider };
        await api.signInMcpProvider(outcome.provider);
        signedInMidPreview = true;
        preview = { kind: "loading" };
        outcome = await api.renderPrompt(prompt.provider, prompt.name, previewArgs());
      }
      if (outcome.kind !== "rendered") {
        preview = {
          kind: "error",
          message: `MCP provider "${prompt.provider}" needs sign-in.`,
        };
        return;
      }
      preview = { kind: "ready", text: combinePromptMessage(outcome.text, previewAppended()) };
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      // A failure after a successful mid-preview sign-in comes from the
      // retry, not the sign-in — say so, or the user is left guessing
      // whether their browser approval was wasted.
      preview = {
        kind: "error",
        message: signedInMidPreview
          ? `Signed in, but the preview then failed: ${message}`
          : message,
      };
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
          class={cn(COMPOSER_ACTION_BUTTON_CLASS, "hover:text-status-failed shrink-0")}
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

    <div class="min-h-0 space-y-3 overflow-y-auto py-1 pl-1" data-testid="prompt-fields-scroll">
      {#snippet fieldPicker(onAdd: (source: ForwardSource) => void, label: string, testid: string)}
        <!-- ↪ sits beside the input (top-aligned, fixed square) so it reads as an
             action on that field, not a floating label-row control. The field's
             own add closure is passed in — no shared key namespace. -->
        <ForwardSourcePicker
          {agents}
          {panes}
          onPickAgent={(agent) => onAdd(forwardSourceForAgent(agent))}
          crossProject={crossProjectBase && {
            ...crossProjectBase,
            onPickForeign: (
              agent: AgentRecord,
              project: { id: ProjectId; name: string },
              rosterIndex: number,
            ) => onAdd(forwardSourceForAgent(agent, { project, rosterIndex })),
          }}
          onPickPane={(pane) => {
            for (const source of forwardSourceAgentsForPane(pane, agents)) onAdd(source);
          }}
          {agentReadiness}
          disabled={busy}
          showPaneShortcuts
          triggerTestid={testid}
          triggerLabel={label}
          tooltipLabel="Forward an agent's output"
          tooltipDisableHoverableContent
          triggerClass={cn(COMPOSER_ACTION_BUTTON_CLASS, "shrink-0 self-center")}
        />
      {/snippet}

      {#snippet fieldChips(
        sources: ForwardSource[],
        onRemove: (key: string) => void,
        onClear: () => void,
        testid: string,
      )}
        {@const orderedSources = orderForwardSources(sources, agents, projectId)}
        {#if orderedSources.length > 0}
          <div class="flex flex-wrap items-center gap-1.5" data-testid={testid}>
            {#each orderedSources as source (forwardSourceKey(source))}
              <ForwardSourceChip
                {source}
                readiness={sourceReadiness(source)}
                disabled={busy}
                onRemove={() => onRemove(forwardSourceKey(source))}
                currentProjectId={projectId}
              />
            {/each}
            {#if orderedSources.length > 1}
              <!-- Each chip carries its own ✕; the bulk clear (same ⊘ glyph as
                   "Clear recipients") only earns its place once there are several
                   to drop at once. -->
              <Tooltip label="Clear forward sources">
                {#snippet trigger(props)}
                  <button
                    {...props}
                    type="button"
                    class={cn(ICON_BUTTON_CLASS, "ml-0.5 shrink-0 disabled:opacity-50")}
                    data-testid={`${testid}-clear`}
                    aria-label="Clear forward sources"
                    disabled={busy}
                    onclick={() => {
                      if (!busy) onClear();
                    }}
                  >
                    <ClearIcon />
                  </button>
                {/snippet}
              </Tooltip>
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
      class="absolute inset-0 z-10 flex items-center justify-center rounded-lg backdrop-blur-sm"
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
  {:else if preview.kind === "signing_in"}
    <div
      class="text-muted flex items-center gap-2 text-sm"
      data-testid="prompt-preview-signing-in"
      role="status"
    >
      <Spinner class="h-4 w-4" />
      Waiting for browser sign-in to {preview.provider}…
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
