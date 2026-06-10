<!-- Hosts PromptComposer with $state-owned args/appendedText, mirroring the
     real parent (ComposeBar). Binding the component's `bind:` props to a plain
     object would be a non-reactive binding — a Svelte warning and not how the
     component is used. -->
<script lang="ts">
  import { untrack } from "svelte";
  import PromptComposer from "$lib/components/PromptComposer.svelte";
  import type { Prompt } from "$lib/types";

  let {
    prompt,
    args: initialArgs,
    appendedText: initialAppended = "",
  }: { prompt: Prompt; args: Record<string, string>; appendedText?: string } = $props();

  let args = $state(untrack(() => ({ ...initialArgs })));
  let appendedText = $state(untrack(() => initialAppended));
</script>

<PromptComposer {prompt} bind:args bind:appendedText onremove={() => undefined} />
