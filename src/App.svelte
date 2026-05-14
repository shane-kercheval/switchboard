<script lang="ts">
  import { onMount } from "svelte";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import * as api from "$lib/api";
  import AgentPane from "$lib/components/AgentPane.svelte";
  import Banner from "$lib/components/Banner.svelte";
  import CreateAgentForm from "$lib/components/CreateAgentForm.svelte";
  import DirectorySelector from "$lib/components/DirectorySelector.svelte";
  import WelcomeScreen from "$lib/components/WelcomeScreen.svelte";
  import type { AgentRecord, DirectoryInfo, ProjectSummary } from "$lib/types";

  // App phase: drives which screen renders.
  type Phase =
    | { kind: "welcome" }
    | { kind: "directory-selector"; info: DirectoryInfo }
    | { kind: "no-agent"; directory: DirectoryInfo; project: ProjectSummary }
    | {
        kind: "active";
        directory: DirectoryInfo;
        project: ProjectSummary;
        agent: AgentRecord;
      };

  let phase = $state<Phase>({ kind: "welcome" });
  let busy = $state<boolean>(false);
  let inlineError = $state<string | null>(null);
  let banner = $state<string | null>(null);

  // Startup binary probe. If it fails, show a non-blocking banner with the
  // install link copy. UI flow proceeds either way — sending will fail until
  // the user installs `claude` and reloads.
  onMount(async () => {
    try {
      await api.checkClaudeBinary();
    } catch {
      banner = "Claude Code not found on PATH. Install from https://claude.com/code";
    }
  });

  function basename(path: string): string {
    const trimmed = path.endsWith("/") ? path.slice(0, -1) : path;
    const i = trimmed.lastIndexOf("/");
    return i >= 0 ? trimmed.slice(i + 1) : trimmed;
  }

  async function handlePickDirectory(): Promise<void> {
    inlineError = null;
    const result = await openDialog({ directory: true, multiple: false });
    if (result === null || typeof result !== "string") return;
    busy = true;
    try {
      const info = await api.pickDirectory(result);
      phase = { kind: "directory-selector", info };
    } catch (err) {
      inlineError = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  async function handleInitAndCreate(projectName: string): Promise<void> {
    inlineError = null;
    busy = true;
    try {
      const dir = await api.initDirectory(currentDirectoryPath()!);
      const project = await api.createProject(projectName);
      await api.setActiveProject(project.id);
      const agents = await api.listAgents(project.id);
      if (agents.length === 0) {
        phase = { kind: "no-agent", directory: dir, project };
      } else {
        phase = { kind: "active", directory: dir, project, agent: pickNewest(agents) };
      }
    } catch (err) {
      inlineError = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  async function handleCreateProject(projectName: string): Promise<void> {
    inlineError = null;
    busy = true;
    try {
      const dir = await api.initDirectory(currentDirectoryPath()!);
      const project = await api.createProject(projectName);
      await api.setActiveProject(project.id);
      phase = { kind: "no-agent", directory: dir, project };
    } catch (err) {
      inlineError = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  async function handleSelectProject(project: ProjectSummary): Promise<void> {
    inlineError = null;
    busy = true;
    try {
      const dir = await api.initDirectory(currentDirectoryPath()!);
      await api.openProject(project.id);
      await api.setActiveProject(project.id);
      const agents = await api.listAgents(project.id);
      if (agents.length === 0) {
        phase = { kind: "no-agent", directory: dir, project };
      } else {
        phase = { kind: "active", directory: dir, project, agent: pickNewest(agents) };
      }
    } catch (err) {
      inlineError = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  async function handleCreateAgent(name: string): Promise<void> {
    if (phase.kind !== "no-agent") return;
    inlineError = null;
    busy = true;
    try {
      const agent = await api.createAgent(name);
      phase = {
        kind: "active",
        directory: phase.directory,
        project: phase.project,
        agent,
      };
    } catch (err) {
      inlineError = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  function handleCancel(): void {
    phase = { kind: "welcome" };
    inlineError = null;
  }

  function currentDirectoryPath(): string | undefined {
    if (phase.kind === "directory-selector") return phase.info.path;
    if (phase.kind === "no-agent" || phase.kind === "active") return phase.directory.path;
    return undefined;
  }

  // Most-recently-created agent wins (created_at desc, then id desc as a
  // deterministic tiebreak). Callers must check `agents.length > 0` before
  // invoking this. M4 introduces an agent switcher; until then, only one
  // agent is displayed at a time. In-flight turns on agents that are no
  // longer displayed continue running on their per-agent channel but are
  // effectively orphaned in the UI for M1.5 — known limitation.
  function pickNewest(agents: AgentRecord[]): AgentRecord {
    const sorted = [...agents].sort((a, b) => {
      if (a.created_at !== b.created_at) return b.created_at.localeCompare(a.created_at);
      return b.id.localeCompare(a.id);
    });
    const first = sorted[0];
    if (!first) throw new Error("pickNewest called with empty agents array");
    return first;
  }

  const breadcrumb = $derived.by(() => {
    if (phase.kind === "active" || phase.kind === "no-agent") {
      return `${phase.project.name} — ${basename(phase.directory.path)}`;
    }
    return null;
  });
</script>

<main class="flex h-full flex-col bg-white text-neutral-900">
  {#if banner}
    <Banner message={banner} />
  {/if}
  {#if breadcrumb}
    <div
      class="border-b border-neutral-200 px-4 py-2 text-xs text-neutral-600"
      data-testid="breadcrumb"
    >
      {breadcrumb}
    </div>
  {/if}

  <div class="flex flex-1 flex-col overflow-hidden">
    {#if phase.kind === "welcome"}
      <WelcomeScreen onPickDirectory={handlePickDirectory} />
      {#if inlineError}
        <p class="px-8 pb-4 text-center text-xs text-red-700" data-testid="error">
          {inlineError}
        </p>
      {/if}
    {:else if phase.kind === "directory-selector"}
      <DirectorySelector
        info={phase.info}
        {busy}
        error={inlineError}
        onInitAndCreate={handleInitAndCreate}
        onCreateProject={handleCreateProject}
        onSelectProject={handleSelectProject}
        onCancel={handleCancel}
      />
    {:else if phase.kind === "no-agent"}
      <CreateAgentForm {busy} error={inlineError} onSubmit={handleCreateAgent} />
    {:else if phase.kind === "active"}
      <AgentPane agent={phase.agent} />
    {/if}
  </div>
</main>
