<script lang="ts">
  /// Browser-test host: a SidebarPanel wired to ResizeHandle and the layout
  /// store exactly the way ProjectsSidebar wires them, inside a full-width row
  /// — so viewport-relative CSS clamps and drag-start clamping are measurable
  /// against real geometry without mounting the whole projects sidebar.
  import SidebarPanel from "$lib/components/ui/SidebarPanel.svelte";
  import ResizeHandle from "$lib/components/ui/ResizeHandle.svelte";
  import {
    layout,
    PROJECTS_SIDEBAR_DEFAULT_WIDTH,
    SIDEBAR_MIN_WIDTH,
    sidebarMaxWidth,
  } from "$lib/layout.svelte";

  let draftWidth = $state<number | null>(null);
</script>

<div style="display: flex; width: 100vw; height: 300px;">
  <SidebarPanel width={draftWidth ?? layout.projectsSidebarWidth} testid="host-sidebar">
    <ResizeHandle
      value={() => draftWidth ?? layout.projectsSidebarWidth}
      min={SIDEBAR_MIN_WIDTH}
      max={sidebarMaxWidth}
      label="Resize sidebar"
      testid="host-sidebar-resizer"
      class="absolute inset-y-0 right-0 z-10 w-1"
      onDraft={(px) => (draftWidth = px)}
      onCommit={(px) => {
        layout.projectsSidebarWidth = px;
        draftWidth = null;
      }}
      onReset={() => {
        layout.projectsSidebarWidth = PROJECTS_SIDEBAR_DEFAULT_WIDTH;
        draftWidth = null;
      }}
    />
    <div>sidebar content</div>
  </SidebarPanel>
  <div style="flex: 1;">main content</div>
</div>
