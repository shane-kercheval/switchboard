<script lang="ts">
  // Test-only harness: owns reactive `$state` for the prompt-composer's bindable
  // args/argSources so component tests can exercise the per-argument forward
  // picker's mutations (pick adds a chip, remove drops it) the way `ComposeBar`
  // does in production. Passing a plain object straight to a `$bindable` prop
  // wouldn't be a `$state` proxy, so in-component index-assignment wouldn't
  // re-render.
  import { untrack } from "svelte";
  import PromptComposer from "./PromptComposer.svelte";
  import type { Prompt, AgentRecord, AgentId } from "$lib/types";
  import type { TranscriptPane } from "$lib/state/transcriptPanes.svelte";
  import type { ForwardSource } from "$lib/state/heldForwards.svelte";

  let {
    prompt,
    agents,
    panes = [],
    agentHasOutput,
    initialArgs,
    initialArgSources = {},
  }: {
    prompt: Prompt;
    agents: AgentRecord[];
    panes?: TranscriptPane[];
    agentHasOutput?: (id: AgentId) => boolean;
    initialArgs: Record<string, string>;
    initialArgSources?: Record<string, ForwardSource[]>;
  } = $props();

  // Seed once from props (the parent never updates them); `untrack` keeps the
  // one-time read out of a reactive scope so Svelte doesn't warn about it.
  let args = $state(untrack(() => ({ ...initialArgs })));
  let argSources = $state<Record<string, ForwardSource[]>>(
    untrack(() => ({ ...initialArgSources })),
  );
  let appendedText = $state("");
</script>

<PromptComposer
  {prompt}
  bind:args
  bind:appendedText
  bind:argSources
  {agents}
  {panes}
  {agentHasOutput}
  onremove={() => {}}
/>
