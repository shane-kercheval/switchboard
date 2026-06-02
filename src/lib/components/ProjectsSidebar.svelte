<script lang="ts">
  import {
    activateProject,
    backgroundCompletedProjectIds,
    liveProjectSends,
    projects,
    selection,
    workspace,
  } from "$lib/state/workspace.svelte";
  import { cancelSend } from "$lib/state/index.svelte";
  import { basename, cn, relativeTime } from "$lib/utils";
  import SidebarPanel from "$lib/components/ui/SidebarPanel.svelte";
  import SidebarSection from "$lib/components/ui/SidebarSection.svelte";
  import Badge from "$lib/components/ui/Badge.svelte";
  import SettingsButton from "$lib/components/ui/SettingsButton.svelte";
  import SidebarToggleButton from "$lib/components/ui/SidebarToggleButton.svelte";
  import PlusIcon from "$lib/components/ui/PlusIcon.svelte";
  import StopIcon from "$lib/components/ui/StopIcon.svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import { ICON_BUTTON_CLASS } from "$lib/components/ui/iconButton";
  import { windowDragRegion } from "$lib/windowDrag";

  let {
    onAddProject,
    onOpenSettings,
    onProjectSelect,
    onToggleSidebar,
    settingsOpen = false,
  }: {
    onAddProject: () => void;
    onOpenSettings: () => void;
    onProjectSelect: () => void;
    onToggleSidebar: () => void;
    settingsOpen?: boolean;
  } = $props();

  function cancelAllForProject(projectId: string): void {
    for (const [sendId, agentIds] of liveProjectSends(projectId)) {
      cancelSend(sendId, agentIds);
    }
  }
</script>

<SidebarPanel side="left" width="w-72" testid="projects-sidebar">
  <div
    class="flex h-11 shrink-0 items-center justify-end px-3"
    data-tauri-drag-region
    use:windowDragRegion
  >
    <SettingsButton pressed={settingsOpen} testid="settings-button" onclick={onOpenSettings} />
    <SidebarToggleButton
      side="left"
      expanded={true}
      label="Hide projects sidebar"
      testid="projects-sidebar-toggle"
      onclick={onToggleSidebar}
    />
  </div>

  {#if !workspace.persistable}
    <div
      class="border-warning/30 bg-warning-soft text-warning border-b px-3 py-2 text-xs"
      data-testid="not-persistable-banner"
    >
      Couldn't read your saved workspace — your project list won't be saved this session.
    </div>
  {/if}

  <SidebarSection title="Projects">
    {#snippet action()}
      <button
        type="button"
        class={ICON_BUTTON_CLASS}
        aria-label="Add a project"
        data-testid="add-project"
        onclick={onAddProject}
      >
        <PlusIcon />
      </button>
    {/snippet}

    {#if projects.list.length === 0}
      <p class="text-muted px-3 py-3 text-xs">No projects yet.</p>
    {/if}
    <div class="flex flex-col gap-0.5 px-2 pb-2">
      {#each projects.list as project (project.id)}
        {@const liveSends = liveProjectSends(project.id)}
        {@const busy = liveSends.size > 0}
        {@const completed = !busy && project.id in backgroundCompletedProjectIds}
        <div
          class={cn(
            "hover:bg-raised/70 flex w-full items-center rounded-md transition-colors",
            project.id === selection.activeProjectId && "bg-raised hover:bg-raised",
          )}
          data-testid="project-row"
          data-project-id={project.id}
          data-active={project.id === selection.activeProjectId}
        >
          <button
            type="button"
            class="flex min-w-0 flex-1 flex-col items-start gap-0.5 px-2.5 py-2 text-left"
            onclick={() => {
              onProjectSelect();
              void activateProject(project.id);
            }}
          >
            <div class="flex w-full items-center gap-2">
              <span class="text-fg truncate text-[13px] font-semibold">
                {project.name}
              </span>
              {#if !project.available}
                <Badge class="ml-auto shrink-0" testid="project-unavailable">unavailable</Badge>
              {/if}
            </div>
            <div class="text-muted flex w-full items-center gap-1 text-xs leading-4">
              <span class="truncate" title={project.directory}>{basename(project.directory)}</span>
              <span>·</span>
              <span class="shrink-0">{relativeTime(project.last_activity)}</span>
            </div>
          </button>
          {#if busy}
            <div class="flex shrink-0 items-center pr-1.5">
              <button
                type="button"
                class="group text-muted hover:bg-status-failed-soft/70 hover:text-status-failed focus-visible:ring-accent focus-visible:bg-status-failed-soft/70 focus-visible:text-status-failed inline-flex h-6 w-6 items-center justify-center rounded-full transition-colors focus-visible:ring-2 focus-visible:outline-none"
                aria-label="Cancel all running agents"
                data-testid="project-cancel"
                onclick={() => cancelAllForProject(project.id)}
              >
                <Spinner class="h-5 w-5 group-hover:hidden group-focus-visible:hidden" />
                <StopIcon class="hidden h-5 w-5 group-hover:block group-focus-visible:block" />
              </button>
            </div>
          {:else if completed}
            <div class="flex shrink-0 items-center pr-1.5" data-testid="project-completed">
              <svg
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="1.5"
                stroke-linecap="round"
                stroke-linejoin="round"
                class="text-accent h-5 w-5"
                aria-hidden="true"
              >
                <circle cx="12" cy="12" r="9" />
                <path d="m8.5 12 2.5 2.5 4.5-5" />
              </svg>
            </div>
          {/if}
        </div>
      {/each}
    </div>
  </SidebarSection>
</SidebarPanel>
