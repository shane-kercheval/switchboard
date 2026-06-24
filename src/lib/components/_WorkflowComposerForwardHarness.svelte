<script lang="ts">
  // Test-only harness: owns reactive `$state` for the workflow composer's bindable
  // inputs/forwardSources so component tests can exercise the per-field forward
  // picker's mutations (pick adds a chip, remove drops it) the way `ComposeBar`
  // does in production. Passing a plain object straight to a `$bindable` prop
  // wouldn't be a `$state` proxy, so in-component index-assignment wouldn't
  // re-render.
  import { untrack } from "svelte";
  import WorkflowComposer from "./WorkflowComposer.svelte";
  import type { AgentRecord, WorkflowFormDescriptor, WorkflowInputValue } from "$lib/types";
  import type { TranscriptPane } from "$lib/state/transcriptPanes.svelte";
  import type { ForwardSource } from "$lib/state/heldForwards.svelte";

  let {
    descriptor,
    agents,
    panes = [],
    loading = false,
    initialInputs = {},
    initialForwardSources = {},
    onForwardSources,
  }: {
    descriptor: WorkflowFormDescriptor;
    agents: AgentRecord[];
    panes?: TranscriptPane[];
    loading?: boolean;
    initialInputs?: Record<string, WorkflowInputValue>;
    initialForwardSources?: Record<string, ForwardSource[]>;
    /// Lets a test read the live bound value after a mutation.
    onForwardSources?: (sources: Record<string, ForwardSource[]>) => void;
  } = $props();

  // Seed once from props (the parent never updates them); `untrack` keeps the
  // one-time read out of a reactive scope so Svelte doesn't warn about it.
  let inputs = $state<Record<string, WorkflowInputValue>>(untrack(() => ({ ...initialInputs })));
  let forwardSources = $state<Record<string, ForwardSource[]>>(
    untrack(() => ({ ...initialForwardSources })),
  );

  $effect(() => {
    onForwardSources?.(forwardSources);
  });
</script>

<WorkflowComposer
  {descriptor}
  {agents}
  {panes}
  {loading}
  bind:inputs
  bind:forwardSources
  onremove={() => {}}
/>
