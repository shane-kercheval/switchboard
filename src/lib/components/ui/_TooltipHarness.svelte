<script lang="ts">
  /// Test-only fixture: composes Tooltip so `Tooltip.test.ts` can exercise
  /// both content modes (label-only and children-slot). Not a test file
  /// itself — the leading `_` and `.svelte` extension keep it out of
  /// Vitest's `*.test` glob. Mirrors `_DropdownMenuHarness.svelte`.
  import Tooltip from "./Tooltip.svelte";

  /// `mode` picks which variant the harness renders. Defaults to label
  /// for the common case.
  let {
    mode = "label" as
      | "label"
      | "children"
      | "fresh-hover"
      | "two"
      | "bound"
      | "bound-fresh"
      | "focus-override"
      | "non-hoverable"
      | "dynamic",
  }: {
    mode?:
      | "label"
      | "children"
      | "fresh-hover"
      | "two"
      | "bound"
      | "bound-fresh"
      | "focus-override"
      | "non-hoverable"
      | "dynamic";
  } = $props();
  let toggleCount = $state(0);
  let showFirst = $state(true);
  let boundOpen = $state(false);
  let dynamicReopen = $state<"default" | "fresh-hover">("fresh-hover");
</script>

{#if mode === "label"}
  <Tooltip label="hello label" shortcut="⌘K">
    {#snippet trigger(props)}
      <button {...props} type="button" data-testid="tt-trigger">trigger</button>
    {/snippet}
  </Tooltip>
{:else if mode === "children"}
  <Tooltip>
    {#snippet trigger(props)}
      <button {...props} type="button" data-testid="tt-trigger">trigger</button>
    {/snippet}
    <ul data-testid="tt-rich-content">
      <li>row one</li>
      <li>row two</li>
    </ul>
  </Tooltip>
{:else if mode === "fresh-hover"}
  <Tooltip label={`toggle ${toggleCount}`} reopen="fresh-hover">
    {#snippet trigger(props)}
      <button {...props} type="button" data-testid="tt-trigger" onclick={() => (toggleCount += 1)}
        >trigger</button
      >
    {/snippet}
  </Tooltip>
{:else if mode === "two"}
  {#if showFirst}
    <Tooltip label="first tooltip" reopen="fresh-hover">
      {#snippet trigger(props)}
        <button {...props} type="button" data-testid="tt-first">first</button>
      {/snippet}
    </Tooltip>
  {/if}
  <Tooltip label="second tooltip" reopen="fresh-hover">
    {#snippet trigger(props)}
      <button {...props} type="button" data-testid="tt-second">second</button>
    {/snippet}
  </Tooltip>
  <button type="button" data-testid="tt-remove-first" onclick={() => (showFirst = false)}>
    remove first
  </button>
{:else if mode === "bound" || mode === "bound-fresh"}
  <button type="button" data-testid="tt-set-open" onclick={() => (boundOpen = true)}>
    open tooltip
  </button>
  <Tooltip
    label="bound tooltip"
    bind:open={boundOpen}
    reopen={mode === "bound-fresh" ? "fresh-hover" : "default"}
  >
    {#snippet trigger(props)}
      <button {...props} type="button" data-testid="tt-trigger">trigger</button>
    {/snippet}
  </Tooltip>
  <output data-testid="tt-open-state">{boundOpen ? "open" : "closed"}</output>
{:else if mode === "focus-override"}
  <Tooltip label="focus override" reopen="fresh-hover" ignoreNonKeyboardFocus={false}>
    {#snippet trigger(props)}
      <button {...props} type="button" data-testid="tt-trigger">trigger</button>
    {/snippet}
  </Tooltip>
{:else if mode === "non-hoverable"}
  <Tooltip label="non-hoverable tooltip" disableHoverableContent>
    {#snippet trigger(props)}
      <button {...props} type="button" data-testid="tt-trigger">trigger</button>
    {/snippet}
  </Tooltip>
{:else}
  <button type="button" data-testid="tt-set-default" onclick={() => (dynamicReopen = "default")}
    >default</button
  >
  <button type="button" data-testid="tt-set-fresh" onclick={() => (dynamicReopen = "fresh-hover")}
    >fresh hover</button
  >
  <Tooltip label="dynamic tooltip" reopen={dynamicReopen}>
    {#snippet trigger(props)}
      <button {...props} type="button" data-testid="tt-trigger">trigger</button>
    {/snippet}
  </Tooltip>
{/if}
