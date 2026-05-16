<script lang="ts">
  import { onMount } from "svelte";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import * as api from "$lib/api";
  import Banner from "$lib/components/Banner.svelte";
  import ComposeBar from "$lib/components/ComposeBar.svelte";
  import CreateAgentForm from "$lib/components/CreateAgentForm.svelte";
  import type { AgentFormSubmit } from "$lib/components/CreateAgentForm.types";
  import DirectorySelector from "$lib/components/DirectorySelector.svelte";
  import Sidebar from "$lib/components/Sidebar.svelte";
  import UnifiedTranscript from "$lib/components/UnifiedTranscript.svelte";
  import WelcomeScreen from "$lib/components/WelcomeScreen.svelte";
  import { registerAgent } from "$lib/state/index.svelte";
  import type { AgentRecord, DirectoryInfo, ProjectSummary } from "$lib/types";
  import { basename } from "$lib/utils";

  // App phase: drives which screen renders.
  //
  // The "loaded" phase replaces M1.5's singleton-active-agent "active"
  // phase per the M2.5 plan — no implicit focused agent. `agents` is the
  // list registered with the state module on project load; the new
  // Sidebar / UnifiedTranscript / ComposeBar components read transcripts
  // and runtimes for these agents from the state module directly.
  type Phase =
    | { kind: "welcome" }
    | { kind: "directory-selector"; info: DirectoryInfo }
    | { kind: "no-agent"; directory: DirectoryInfo; project: ProjectSummary }
    | {
        kind: "loaded";
        directory: DirectoryInfo;
        project: ProjectSummary;
        agents: AgentRecord[];
      };

  let phase = $state<Phase>({ kind: "welcome" });
  let busy = $state<boolean>(false);
  let inlineError = $state<string | null>(null);
  let banner = $state<string | null>(null);

  // Startup binary probe. If it fails, show a non-blocking banner with the
  // install link copy. UI flow proceeds either way — sending will fail until
  // the user installs `claude` and reloads.
  //
  // (Pass D will extend this to per-harness banners — Claude + Codex
  // independently, plus subscription-auth detection.)
  onMount(async () => {
    try {
      await api.checkClaudeBinary();
    } catch {
      banner = "Claude Code not found on PATH. Install from https://claude.com/code";
    }
  });

  /// Register every agent in the loaded list with the state module before
  /// the UI renders the 3-pane layout. registerAgent is idempotent under
  /// concurrent calls (per the pendingRegistrations guard), so
  /// Promise.all is safe — overlapping calls for the same agent_id share
  /// one in-flight registration.
  async function registerAll(agents: AgentRecord[]): Promise<void> {
    await Promise.all(agents.map((a) => registerAgent(a)));
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
        await registerAll(agents);
        phase = { kind: "loaded", directory: dir, project, agents };
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
        await registerAll(agents);
        phase = { kind: "loaded", directory: dir, project, agents };
      }
    } catch (err) {
      inlineError = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  async function handleCreateAgent(submission: AgentFormSubmit): Promise<void> {
    if (phase.kind !== "no-agent") return;
    inlineError = null;
    busy = true;
    try {
      const agent =
        submission.mode === "create"
          ? await api.createAgent(submission.name, submission.harness)
          : await api.attachAgent(
              submission.name,
              submission.harness,
              submission.existingSessionId,
            );
      await registerAgent(agent);
      phase = {
        kind: "loaded",
        directory: phase.directory,
        project: phase.project,
        agents: [agent],
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
    if (phase.kind === "no-agent" || phase.kind === "loaded") return phase.directory.path;
    return undefined;
  }

  const breadcrumb = $derived.by(() => {
    if (phase.kind === "loaded" || phase.kind === "no-agent") {
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
    {:else if phase.kind === "loaded"}
      <div class="flex flex-1 overflow-hidden" data-testid="loaded-layout">
        <Sidebar agents={phase.agents} />
        <div class="flex flex-1 flex-col overflow-hidden">
          <UnifiedTranscript agents={phase.agents} />
          <ComposeBar agents={phase.agents} />
        </div>
      </div>
    {/if}
  </div>
</main>
