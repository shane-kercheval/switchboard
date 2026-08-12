<!-- Hosts PromptComposer with $state-owned args/appendedText, mirroring the
     real parent (ComposeBar). Binding the component's `bind:` props to a plain
     object would be a non-reactive binding — a Svelte warning and not how the
     component is used. -->
<script lang="ts">
  import { untrack } from "svelte";
  import PromptComposer from "$lib/components/PromptComposer.svelte";
  import type { AgentRecord, Prompt } from "$lib/types";

  let {
    prompt,
    args: initialArgs,
    appendedText: initialAppended = "",
    agents = [],
    width,
  }: {
    prompt: Prompt;
    args: Record<string, string>;
    appendedText?: string;
    agents?: AgentRecord[];
    width?: number;
  } = $props();

  let args = $state(untrack(() => ({ ...initialArgs })));
  let appendedText = $state(untrack(() => initialAppended));
</script>

<div style:width={width === undefined ? undefined : `${width}px`}>
  <PromptComposer {prompt} bind:args bind:appendedText {agents} onremove={() => undefined}>
    {#snippet send()}
      <!-- Width is immaterial to alignment; the right edge mirrors the real
           Send/Fork snippet without pulling the whole ComposeBar into this host. -->
      <div class="h-7 w-[3.5rem]" data-testid="prompt-send-probe"></div>
    {/snippet}
  </PromptComposer>
</div>
