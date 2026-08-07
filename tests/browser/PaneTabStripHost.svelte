<script lang="ts">
  import PaneTabStrip from "$lib/components/PaneTabStrip.svelte";
  import type { HeaderPaneEntry } from "$lib/components/PaneTabStrip.types";

  const entries: HeaderPaneEntry[] = Array.from({ length: 10 }, (_, index) => ({
    pane: {
      id: `pane-${index + 1}`,
      name: `Pane ${index + 1}`,
      members: [`agent-${index + 1}`],
      hidden: [],
    },
    state: "visible",
  }));
</script>

<div
  class="bg-raised flex h-11 items-center gap-1 overflow-hidden border px-2"
  style="width: 420px;"
  data-testid="pane-strip-header"
>
  <div class="min-w-0 flex-1"></div>
  <PaneTabStrip
    {entries}
    paneIsActive={() => false}
    paneIsCompleted={() => false}
    onSelectVisible={() => undefined}
    onOpenHidden={() => undefined}
  />
  <button class="h-7 w-7 shrink-0" data-testid="fixed-pane-control">+</button>
  <button class="h-7 w-7 shrink-0" data-testid="fixed-view-control">V</button>
</div>
