<script lang="ts">
  /// Read-only preview of a workflow step's prompt, opened from its chip.
  ///  - **named** (`builtin`/`local`): the raw, unrendered template body
  ///    (placeholders like `{{ context }}` intact — the authored template, not
  ///    what an agent receives).
  ///  - **named MCP**: no un-rendered source over the protocol, so it falls back
  ///    to the cached metadata (description + declared arguments) with a note.
  ///  - **inline**: the send's inline template text, shown directly (it travels on
  ///    the step, so there is nothing to fetch).
  import type { Prompt, PromptSource, StepPrompt } from "$lib/types";
  import { getPromptSource, listPrompts } from "$lib/api";
  import { isLocalProvider, parsePromptId } from "$lib/prompt";
  import Dialog from "$lib/components/ui/Dialog.svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";

  type Props = {
    /// Two-way bound visibility.
    open: boolean;
    /// The step prompt to preview; null when nothing is selected. Changing it while
    /// open re-loads (a named prompt re-fetches; inline is instant).
    prompt: StepPrompt | null;
  };

  let { open = $bindable(), prompt }: Props = $props();

  type Loaded = {
    source: PromptSource | null;
    meta: Prompt | null;
    /// The resolved provider of the loaded prompt (`local`/`builtin` or an MCP
    /// name), or null for inline. Captured at fetch time so the null-source message
    /// reflects *this* load, not the live `prompt` prop (which can change async).
    provider: string | null;
  };

  let loading = $state(false);
  let error = $state<string | null>(null);
  let loaded = $state<Loaded | null>(null);

  /// Guards against a slower earlier fetch overwriting a newer one when the user
  /// reopens the dialog for a different chip before the first resolves.
  let requestSeq = 0;

  /// The dialog title: the full `provider:name` id for a named prompt, or a plain
  /// label for inline (which has no id).
  const title = $derived(
    prompt === null ? "Prompt" : prompt.kind === "named" ? prompt.id : "Inline prompt",
  );

  /// Boxed-note styling for the status messages (server-rendered / unresolved /
  /// unavailable), so they read as a distinct callout rather than blending into
  /// the prompt's own description prose. Mirrors the composer's note box.
  const CALLOUT = "border-border/70 bg-surface/40 text-muted rounded-md border px-2.5 py-2 text-xs";

  async function load(p: StepPrompt): Promise<void> {
    const seq = ++requestSeq;
    error = null;
    loaded = null;

    // Inline text lives on the step — show it directly, no fetch, no metadata.
    if (p.kind === "inline") {
      loading = false;
      loaded = { source: { text: p.text }, meta: null, provider: null };
      return;
    }

    loading = true;
    const parsed = parsePromptId(p.id);
    if (!parsed) {
      if (seq === requestSeq) {
        loading = false;
        error = `Not a named prompt: ${p.id}`;
      }
      return;
    }
    try {
      // The template body is the primary content; metadata (description + args) is
      // a best-effort supplement, so its failure must not discard a good body —
      // degrade `listPrompts` to empty rather than rejecting the whole load.
      const [source, prompts] = await Promise.all([
        getPromptSource(parsed.provider, parsed.name),
        listPrompts().catch(() => [] as Prompt[]),
      ]);
      if (seq !== requestSeq) return;
      const meta =
        prompts.find((x) => x.provider === parsed.provider && x.name === parsed.name) ?? null;
      loaded = { source, meta, provider: parsed.provider };
    } catch (e) {
      if (seq !== requestSeq) return;
      error = e instanceof Error ? e.message : String(e);
    } finally {
      if (seq === requestSeq) loading = false;
    }
  }

  $effect(() => {
    if (open && prompt) {
      void load(prompt);
    }
  });

  function close(): void {
    open = false;
  }
</script>

<Dialog {open} onClose={close} {title} contentClass="max-w-2xl">
  <div class="flex flex-col gap-3" data-testid="prompt-preview">
    {#if loading}
      <div class="text-muted flex items-center gap-2 text-sm" data-testid="prompt-preview-loading">
        <Spinner class="h-4 w-4" /> Loading prompt…
      </div>
    {:else if error}
      <p class="text-status-failed text-sm" data-testid="prompt-preview-error">{error}</p>
    {:else if loaded}
      {@const meta = loaded.meta}
      {#if meta?.description}
        <p class="text-muted text-sm" data-testid="prompt-preview-description">
          {meta.description}
        </p>
      {/if}

      {#if meta && meta.arguments.length > 0}
        <div class="flex flex-col gap-1" data-testid="prompt-preview-arguments">
          <span class="text-fg text-xs font-medium">Arguments</span>
          <ul class="flex flex-col gap-1">
            {#each meta.arguments as arg (arg.name)}
              <li class="text-xs">
                <span class="text-fg font-mono">{arg.name}</span>
                <span class="text-muted">· {arg.required ? "required" : "optional"}</span>
                {#if arg.description}
                  <span class="text-muted"> — {arg.description}</span>
                {/if}
              </li>
            {/each}
          </ul>
        </div>
      {/if}

      {#if loaded.source}
        <pre
          class="bg-panel text-fg border-border/70 max-h-[55vh] overflow-auto rounded border p-3 text-xs break-words whitespace-pre-wrap"
          data-testid="prompt-preview-body">{loaded.source.text}</pre>
      {:else if loaded.provider !== null && isLocalProvider(loaded.provider)}
        <!-- A local/builtin prompt always has a template when it resolves, so a
             null source here means it no longer resolves (deleted or malformed). -->
        <p class={CALLOUT} role="note" data-testid="prompt-preview-unresolved">
          This prompt could not be resolved — it may have been deleted or is malformed. Try Sync to
          refresh the prompt list.
        </p>
      {:else if meta}
        <!-- A known MCP prompt: the server renders it, so there's no template to show. -->
        <p class={CALLOUT} role="note" data-testid="prompt-preview-no-source">
          This prompt is rendered by the <span class="font-mono">{loaded.provider}</span> MCP server,
          so its template can't be previewed here.
        </p>
      {:else}
        <!-- Null source and no cached metadata: nothing to show either way. -->
        <p class={CALLOUT} role="note" data-testid="prompt-preview-unavailable">
          Preview unavailable — this prompt could not be resolved.
        </p>
      {/if}
    {/if}
  </div>
</Dialog>
