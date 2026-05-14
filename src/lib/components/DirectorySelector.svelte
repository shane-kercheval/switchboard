<script lang="ts">
  import Button from "$lib/components/ui/Button.svelte";
  import Input from "$lib/components/ui/Input.svelte";
  import type { DirectoryInfo, ProjectSummary } from "$lib/types";
  import { basename } from "$lib/utils";

  type Props = {
    info: DirectoryInfo;
    busy?: boolean;
    error?: string | null;
    onInitAndCreate: (projectName: string) => void;
    onCreateProject: (projectName: string) => void;
    onSelectProject: (project: ProjectSummary) => void;
    onCancel: () => void;
  };

  let {
    info,
    busy = false,
    error = null,
    onInitAndCreate,
    onCreateProject,
    onSelectProject,
    onCancel,
  }: Props = $props();

  // svelte-ignore state_referenced_locally
  // Initial value only — App.svelte remounts this component on a new pick.
  let projectName = $state<string>(basename(info.path));
  let creatingAnother = $state<boolean>(false);
</script>

<div class="flex h-full flex-col items-center justify-center gap-6 p-8">
  <div class="w-full max-w-lg space-y-4">
    <div class="space-y-1 text-center">
      <h2 class="text-xl font-semibold text-neutral-900">Working directory</h2>
      <p class="font-mono text-xs text-neutral-500">{info.path}</p>
    </div>

    {#if !info.has_switchboard}
      <!-- No .switchboard/ → init + create initial project -->
      <div class="space-y-3 rounded-md border border-neutral-200 bg-neutral-50 p-4">
        <p class="text-sm text-neutral-700">
          Initialize Switchboard in this directory and create a project named
          <code class="rounded bg-white px-1 font-mono text-neutral-900">{basename(info.path)}</code
          >?
        </p>
        <label class="block space-y-1">
          <span class="text-xs text-neutral-600">Project name</span>
          <Input bind:value={projectName} disabled={busy} data-testid="initial-project-name" />
        </label>
        {#if error}
          <p class="text-xs text-red-700" data-testid="error">{error}</p>
        {/if}
        <div class="flex justify-end gap-2">
          <Button variant="ghost" onclick={onCancel} disabled={busy}>Cancel</Button>
          <Button
            data-testid="confirm-init"
            disabled={busy || projectName.trim() === ""}
            onclick={() => onInitAndCreate(projectName.trim())}
          >
            {busy ? "Initializing…" : "Initialize"}
          </Button>
        </div>
      </div>
    {:else if info.projects.length === 0}
      <!-- .switchboard/ exists, no projects → just create one -->
      <div class="space-y-3 rounded-md border border-neutral-200 bg-neutral-50 p-4">
        <p class="text-sm text-neutral-700">
          This directory is initialized but has no projects yet. Create one to continue.
        </p>
        <label class="block space-y-1">
          <span class="text-xs text-neutral-600">Project name</span>
          <Input bind:value={projectName} disabled={busy} />
        </label>
        {#if error}
          <p class="text-xs text-red-700" data-testid="error">{error}</p>
        {/if}
        <div class="flex justify-end gap-2">
          <Button variant="ghost" onclick={onCancel} disabled={busy}>Cancel</Button>
          <Button
            disabled={busy || projectName.trim() === ""}
            onclick={() => onCreateProject(projectName.trim())}
          >
            {busy ? "Creating…" : "Create project"}
          </Button>
        </div>
      </div>
    {:else}
      <!-- .switchboard/ exists, projects present → pick one or create another -->
      <div class="space-y-3 rounded-md border border-neutral-200 bg-neutral-50 p-4">
        <p class="text-sm text-neutral-700">Open an existing project:</p>
        <ul class="space-y-1">
          {#each info.projects as project (project.id)}
            <li>
              <button
                data-testid="project-row"
                class="flex w-full items-center justify-between rounded-md border border-neutral-300 bg-white px-3 py-2 text-left text-sm text-neutral-900 hover:border-neutral-400 hover:bg-neutral-100 disabled:cursor-not-allowed disabled:opacity-50"
                disabled={busy}
                onclick={() => onSelectProject(project)}
              >
                <span class="font-medium">{project.name}</span>
                <span class="font-mono text-xs text-neutral-500"
                  >{new Date(project.created_at).toLocaleDateString()}</span
                >
              </button>
            </li>
          {/each}
        </ul>

        {#if !creatingAnother}
          <Button variant="secondary" onclick={() => (creatingAnother = true)} disabled={busy}>
            Create another project
          </Button>
        {:else}
          <label class="block space-y-1 pt-2">
            <span class="text-xs text-neutral-600">New project name</span>
            <Input bind:value={projectName} disabled={busy} />
          </label>
          {#if error}
            <p class="text-xs text-red-700" data-testid="error">{error}</p>
          {/if}
          <div class="flex justify-end gap-2">
            <Button variant="ghost" onclick={() => (creatingAnother = false)} disabled={busy}>
              Cancel
            </Button>
            <Button
              disabled={busy || projectName.trim() === ""}
              onclick={() => onCreateProject(projectName.trim())}
            >
              {busy ? "Creating…" : "Create"}
            </Button>
          </div>
        {/if}
        <div class="flex justify-end pt-2">
          <Button variant="ghost" onclick={onCancel} disabled={busy}>Cancel</Button>
        </div>
      </div>
    {/if}
  </div>
</div>
