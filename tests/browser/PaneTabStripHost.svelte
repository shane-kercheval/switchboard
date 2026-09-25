<script lang="ts">
  import PaneTabStrip from "$lib/components/PaneTabStrip.svelte";
  import type { HeaderPaneEntry } from "$lib/components/PaneTabStrip.types";

  let entries = $state<HeaderPaneEntry[]>(
    Array.from({ length: 10 }, (_, index) => ({
      pane: {
        id: `pane-${index + 1}`,
        name: `Pane ${index + 1}`,
        members: [`agent-${index + 1}`],
        hidden: [],
      },
      state: index === 1 ? "minimized" : index === 2 ? "behind_maximized" : "visible",
    })),
  );
  let selectCount = $state(0);
  let openCount = $state(0);

  function reorder(paneId: string, toIndex: number): void {
    const fromIndex = entries.findIndex((entry) => entry.pane.id === paneId);
    const next = [...entries];
    next.splice(toIndex, 0, next.splice(fromIndex, 1)[0]!);
    entries = next;
  }
</script>

<div
  class="bg-raised flex h-11 items-center gap-1 overflow-hidden border px-2"
  style="width: 420px;"
  data-testid="pane-strip-header"
>
  <div class="min-w-0 flex-1"></div>
  <PaneTabStrip
    {entries}
    projectId="test-project"
    paneIsActive={() => false}
    paneIsCompleted={() => false}
    onSelectVisible={() => (selectCount += 1)}
    onOpenHidden={() => (openCount += 1)}
    onReorder={(_, paneId, toIndex) => reorder(paneId, toIndex)}
  />
  <button class="h-7 w-7 shrink-0" data-testid="fixed-pane-control">+</button>
  <button class="h-7 w-7 shrink-0" data-testid="fixed-view-control">V</button>
</div>
<output data-testid="pane-select-count">{selectCount}</output>
<output data-testid="pane-open-count">{openCount}</output>
