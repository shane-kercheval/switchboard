<script lang="ts">
  import type { Snippet } from "svelte";
  import * as api from "$lib/api";
  import type { Prompt } from "$lib/types";
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
    onremove,
    send,
  }: {
    prompt: Prompt;
    args: Record<string, string>;
    appendedText: string;
    onremove: () => void;
    /// The compose bar's send button, rendered in the footer row beside Preview
    /// so the two actions align. Optional so the component stands alone in tests.
    send?: Snippet;
  } = $props();

  type PreviewState =
    | { kind: "idle" }
    | { kind: "loading" }
    | { kind: "ready"; text: string }
    | { kind: "error"; message: string };
  let preview = $state<PreviewState>({ kind: "idle" });
  let previewOpen = $state(false);

  const missing = $derived(missingRequiredArgs(prompt, args));

  function openPreview(): void {
    previewOpen = true;
    void runPreview();
  }

  async function runPreview(): Promise<void> {
    preview = { kind: "loading" };
    try {
      const rendered = await api.renderPrompt(
        prompt.provider,
        prompt.name,
        buildRenderArgs(prompt, args),
      );
      preview = { kind: "ready", text: combinePromptMessage(rendered.text, appendedText) };
    } catch (e) {
      preview = { kind: "error", message: e instanceof Error ? e.message : String(e) };
    }
  }
</script>

<div class="flex flex-col gap-3" data-testid="prompt-composer">
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

  {#if prompt.description}
    <p class="text-muted text-xs">{prompt.description}</p>
  {/if}

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
      <Textarea
        id={`prompt-arg-${arg.name}`}
        data-testid={`prompt-arg-${arg.name}`}
        rows={2}
        bind:value={args[arg.name]}
        class={cn("max-h-40 min-h-9 text-sm", isMissing ? "border-status-failed" : "")}
      />
    </div>
  {/each}

  <div class="flex flex-col gap-1">
    <label class="text-fg text-xs font-medium" for="prompt-appended">Appended text</label>
    <Textarea
      id="prompt-appended"
      data-testid="prompt-appended"
      rows={2}
      placeholder="Optional text appended after the prompt…"
      bind:value={appendedText}
      class="max-h-40 min-h-9 text-sm"
    />
  </div>

  <div class="flex items-center justify-between gap-2">
    <Button
      variant="secondary"
      size="sm"
      data-testid="prompt-preview-button"
      disabled={missing.length > 0}
      onclick={openPreview}
    >
      Preview
    </Button>
    {@render send?.()}
  </div>
</div>

<Dialog
  bind:open={previewOpen}
  title="Preview"
  onClose={() => (previewOpen = false)}
  contentClass="max-w-2xl"
>
  {#if preview.kind === "loading"}
    <div class="text-muted text-sm" data-testid="prompt-preview-loading">Rendering preview…</div>
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
