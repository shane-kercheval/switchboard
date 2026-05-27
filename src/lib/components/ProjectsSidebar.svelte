<script lang="ts">
  import { untrack } from "svelte";
  import {
    activateProject,
    agentsByProject,
    projects,
    selection,
    workspace,
  } from "$lib/state/workspace.svelte";
  import { cancelSend, runtimes, transcripts } from "$lib/state/index.svelte";
  import type { AgentId } from "$lib/types";
  import { basename, cn, relativeTime } from "$lib/utils";
  import SidebarPanel from "$lib/components/ui/SidebarPanel.svelte";
  import SidebarSection from "$lib/components/ui/SidebarSection.svelte";
  import DropdownMenu from "$lib/components/ui/DropdownMenu.svelte";
  import DropdownMenuItem from "$lib/components/ui/DropdownMenuItem.svelte";
  import Badge from "$lib/components/ui/Badge.svelte";
  import SettingsButton from "$lib/components/ui/SettingsButton.svelte";
  import SidebarToggleButton from "$lib/components/ui/SidebarToggleButton.svelte";
  import PlusIcon from "$lib/components/ui/PlusIcon.svelte";
  import { ICON_BUTTON_CLASS } from "$lib/components/ui/iconButton";
  import { windowDragRegion } from "$lib/windowDrag";

  let {
    onNewProject,
    onAddExisting,
    onOpenSettings,
    onProjectSelect,
    onToggleSidebar,
    settingsOpen = false,
  }: {
    onNewProject: () => void;
    onAddExisting: () => void;
    onOpenSettings: () => void;
    onProjectSelect: () => void;
    onToggleSidebar: () => void;
    settingsOpen?: boolean;
  } = $props();

  /// All non-cancelled live sends across a project's agents: pending (not
  /// cancel_requested) + currently-streaming turns. Mirrors the `liveSends`
  /// derived in ComposeBar but scoped to one project.
  function liveProjectSends(projectId: string): Map<string, AgentId[]> {
    const roster = agentsByProject[projectId] ?? [];
    const bySend = new Map<string, AgentId[]>();
    const add = (sendId: string, agentId: AgentId): void => {
      const arr = bySend.get(sendId) ?? [];
      if (!arr.includes(agentId)) arr.push(agentId);
      bySend.set(sendId, arr);
    };
    for (const agent of roster) {
      for (const p of runtimes[agent.id]?.pending_sends ?? []) {
        if (!p.cancel_requested) add(p.send_id, agent.id);
      }
      for (const turn of transcripts[agent.id] ?? []) {
        if (turn.role === "agent" && turn.status === "streaming" && turn.send_id !== undefined) {
          add(turn.send_id, agent.id);
        }
      }
    }
    return bySend;
  }

  function cancelAllForProject(projectId: string): void {
    for (const [sendId, agentIds] of liveProjectSends(projectId)) {
      cancelSend(sendId, agentIds);
    }
  }

  /// Projects that completed (busy → idle) while the user was not viewing them.
  /// Cleared when the user clicks that project. Plain object so Svelte 5 tracks
  /// property reads and writes for fine-grained reactivity.
  let completedProjectIds = $state<Record<string, true>>({});
  let _prevBusy = new Set<string>();

  $effect(() => {
    const nowBusy = new Set(
      projects.list.filter((p) => liveProjectSends(p.id).size > 0).map((p) => p.id),
    );
    const added: string[] = [];
    for (const id of _prevBusy) {
      if (!nowBusy.has(id) && id !== selection.activeProjectId) added.push(id);
    }
    _prevBusy = nowBusy;
    // untrack so writing completedProjectIds doesn't re-subscribe this effect
    untrack(() => {
      for (const id of added) completedProjectIds[id] = true;
    });
  });
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
      <DropdownMenu
        triggerTestid="add-project"
        triggerLabel="Add a project"
        triggerClass={ICON_BUTTON_CLASS}
        contentTestid="add-project-menu"
      >
        {#snippet trigger()}
          <PlusIcon />
        {/snippet}
        <DropdownMenuItem onSelect={onNewProject} data-testid="menu-new-project">
          New project
        </DropdownMenuItem>
        <DropdownMenuItem onSelect={onAddExisting} data-testid="menu-add-existing">
          Add existing project
        </DropdownMenuItem>
      </DropdownMenu>
    {/snippet}

    {#if projects.list.length === 0}
      <p class="text-muted px-3 py-3 text-xs">No projects yet.</p>
    {/if}
    <div class="flex flex-col gap-0.5 px-2 pb-2">
      {#each projects.list as project (project.id)}
        {@const liveSends = liveProjectSends(project.id)}
        {@const busy = liveSends.size > 0}
        {@const completed = !busy && project.id in completedProjectIds}
        <div
          class={cn(
            "flex w-full items-center rounded-md transition-colors hover:bg-raised/70",
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
              delete completedProjectIds[project.id];
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
                <span
                  class="border-muted/30 border-t-muted block h-5 w-5 animate-spin rounded-full border-2 group-hover:hidden group-focus-visible:hidden"
                  aria-hidden="true"
                ></span>
                <svg
                  viewBox="0 0 24 24"
                  fill="currentColor"
                  class="hidden h-5 w-5 group-hover:block group-focus-visible:block"
                  aria-hidden="true"
                >
                  <rect x="7" y="7" width="10" height="10" rx="2" />
                </svg>
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
