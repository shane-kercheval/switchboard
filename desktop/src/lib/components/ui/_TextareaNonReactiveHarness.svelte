<!-- Test harness: deliberately binds through a computed member of a plain
     (non-$state) object received as a prop — structurally the PromptComposer
     shape (`bind:value={args[arg.name]}`), the one binding mode where typing
     changes the DOM with no reactive signal reaching Textarea's value effect.
     Svelte permits it with a runtime dev warning
     (binding_property_non_reactive). The input-path resize in Textarea is the
     only thing keeping autosize alive here; the companion test fails if that
     path is ever removed (e.g. "simplified" to effect-only). NOTE: a simpler
     script-local `bind:value={obj.value}` does NOT reproduce the failure —
     Svelte services that shape from a child-local mirror — which is why this
     harness mirrors the real consumer's shape instead. -->
<script lang="ts">
  import Textarea from "./Textarea.svelte";

  let { args = $bindable({ value: "" }) }: { args?: Record<string, string> } = $props();
  const key = "value";
</script>

<Textarea autosize bind:value={args[key]} data-testid="ta" />
