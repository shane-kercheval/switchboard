<script lang="ts">
  import { SquareTerminal } from "@lucide/svelte";
  import HarnessStatusList from "./HarnessStatusList.svelte";
</script>

<!-- Rendered as one row of the welcome screen's explainer list — same icon-row
     shape as the sibling rows in WelcomeScreen. -->
<li data-testid="getting-started" class="flex gap-3">
  <SquareTerminal
    size={18}
    strokeWidth={1.8}
    aria-hidden="true"
    class="text-muted mt-0.5 shrink-0"
  />
  <div class="flex min-w-0 flex-1 flex-col gap-1">
    <p class="text-fg text-sm font-medium">Supported CLIs</p>
    <p class="text-muted text-sm leading-relaxed">
      Install and sign in from your terminal — Switchboard detects both automatically and refreshes
      when you come back to the app.
    </p>
    <div class="mt-2">
      <HarnessStatusList />
    </div>
  </div>
</li>
