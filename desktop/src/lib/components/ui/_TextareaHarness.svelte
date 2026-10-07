<!-- Test harness: binds the autosize Textarea to local $state the way real
     consumers do (e.g. the compose bar's draft), so tests can drive
     programmatic value changes — the send-clear path — from outside. -->
<script lang="ts">
  import { untrack } from "svelte";
  import Textarea from "./Textarea.svelte";

  // `initial` seeds the state once, deliberately — `untrack` marks the
  // init-only read (the codebase's pattern for mount-time snapshots).
  let { initial = "" }: { initial?: string } = $props();
  let value = $state(untrack(() => initial));

  export function setValue(next: string): void {
    value = next;
  }
</script>

<Textarea autosize bind:value data-testid="ta" />
