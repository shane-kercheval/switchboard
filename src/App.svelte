<script lang="ts">
  import { onMount } from "svelte";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import * as api from "$lib/api";
  import Banner from "$lib/components/Banner.svelte";
  import ComposeBar from "$lib/components/ComposeBar.svelte";
  import AddAgentModal from "$lib/components/AddAgentModal.svelte";
  import CreateAgentForm from "$lib/components/CreateAgentForm.svelte";
  import type { AgentFormSubmit } from "$lib/components/CreateAgentForm.types";
  import DirectorySelector from "$lib/components/DirectorySelector.svelte";
  import Sidebar from "$lib/components/Sidebar.svelte";
  import UnifiedTranscript from "$lib/components/UnifiedTranscript.svelte";
  import WelcomeScreen from "$lib/components/WelcomeScreen.svelte";
  import { registerAgent } from "$lib/state/index.svelte";
  import type {
    AgentRecord,
    DirectoryInfo,
    HarnessAvailability,
    HarnessBanner,
    ProjectSummary,
  } from "$lib/types";
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

  /// Per-harness availability, populated by the startup probes. Drives
  /// both the banner stack and the create-agent form's radio gating.
  /// Initial state: optimistic ("available") until probes return — the
  /// brief flicker is preferred over flashing all banners on first paint.
  /// Claude auth is `"unsupported"` always (keychain-based on macOS; no
  /// reliable file signal — deferred to v2 per the M2.5 plan).
  let claudeAvailability = $state<HarnessAvailability>({
    harness: "claude_code",
    binary: "available",
    auth: "unsupported",
  });
  let codexAvailability = $state<HarnessAvailability>({
    harness: "codex",
    binary: "available",
    auth: "available",
  });

  /// Banner stack ordering: binary-missing first, then auth-missing.
  /// Suppression rule: if a harness's binary is missing, its auth banner
  /// is hidden (auth is irrelevant if the CLI isn't installed). Max two
  /// banners visible (one per harness).
  const banners = $derived.by((): HarnessBanner[] => {
    const result: HarnessBanner[] = [];
    for (const a of [claudeAvailability, codexAvailability]) {
      if (a.binary === "missing") {
        result.push({ kind: "binary_missing", harness: a.harness });
      } else if (a.auth === "missing") {
        result.push({ kind: "auth_missing", harness: a.harness });
      }
    }
    return result;
  });

  function bannerCopy(b: HarnessBanner): string {
    if (b.kind === "binary_missing") {
      return b.harness === "claude_code"
        ? "Claude Code not found on PATH. Install from https://claude.com/code"
        : "Codex not found on PATH. Install from https://github.com/openai/codex";
    }
    // auth_missing — Codex only (Claude auth detection unsupported).
    return "Codex not authenticated — run `codex login` and reload Switchboard. (API-key-only auth is not supported.)";
  }

  function bannerTestid(b: HarnessBanner): string {
    return `banner-${b.kind}-${b.harness}`;
  }

  // Startup probes. Each harness's binary + auth (where applicable) runs
  // independently; failures populate the availability state and the
  // `banners` $derived recomputes. UI flow proceeds regardless — sending
  // to a missing-binary agent fails at dispatch with a typed error.
  onMount(async () => {
    const claudeBinary = api.checkClaudeBinary().then(
      () => "available" as const,
      () => "missing" as const,
    );
    const codexBinary = api.checkCodexBinary().then(
      () => "available" as const,
      () => "missing" as const,
    );
    const codexAuth = api.checkCodexAuth().then(
      () => "available" as const,
      () => "missing" as const,
    );
    const [cb, xb, xa] = await Promise.all([claudeBinary, codexBinary, codexAuth]);
    claudeAvailability = { harness: "claude_code", binary: cb, auth: "unsupported" };
    codexAvailability = { harness: "codex", binary: xb, auth: xa };
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

  /// Shared core: invoke the right Tauri command for the submission shape,
  /// then call `registerAgent` to wire listeners. Throws on either failure
  /// so the caller can surface the error in its phase-specific UI.
  async function createOrAttachAndRegister(submission: AgentFormSubmit): Promise<AgentRecord> {
    const agent =
      submission.mode === "create"
        ? await api.createAgent(submission.name, submission.harness)
        : await api.attachAgent(submission.name, submission.harness, submission.existingSessionId);
    await registerAgent(agent);
    return agent;
  }

  async function handleCreateFirstAgent(submission: AgentFormSubmit): Promise<void> {
    if (phase.kind !== "no-agent") return;
    inlineError = null;
    busy = true;
    try {
      const agent = await createOrAttachAndRegister(submission);
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

  /// Loaded-phase add-agent handler. Appends to `phase.agents` immutably
  /// (matches the rest of App.svelte's reassignment pattern and sidesteps
  /// the question of whether `$state` deep-tracks array mutations through
  /// a discriminated-union narrowing — it does, but reassign is clearer).
  let addAgentOpen = $state<boolean>(false);
  let addAgentError = $state<string | null>(null);
  let addAgentBusy = $state<boolean>(false);

  async function handleAddAgentFromLoaded(submission: AgentFormSubmit): Promise<void> {
    if (phase.kind !== "loaded") return;
    addAgentError = null;
    addAgentBusy = true;
    try {
      const agent = await createOrAttachAndRegister(submission);
      phase = {
        ...phase,
        agents: [...phase.agents, agent],
      };
      addAgentOpen = false;
    } catch (err) {
      addAgentError = err instanceof Error ? err.message : String(err);
    } finally {
      addAgentBusy = false;
    }
  }

  function handleAddAgentCancel(): void {
    addAgentOpen = false;
    addAgentError = null;
  }

  function openAddAgent(): void {
    addAgentError = null;
    addAgentOpen = true;
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
  {#each banners as banner (bannerTestid(banner))}
    <Banner message={bannerCopy(banner)} testid={bannerTestid(banner)} />
  {/each}
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
      <CreateAgentForm
        {busy}
        error={inlineError}
        onSubmit={handleCreateFirstAgent}
        {claudeAvailability}
        {codexAvailability}
      />
    {:else if phase.kind === "loaded"}
      <div class="flex flex-1 overflow-hidden" data-testid="loaded-layout">
        <Sidebar agents={phase.agents} onAddAgent={openAddAgent} />
        <div class="flex flex-1 flex-col overflow-hidden">
          <UnifiedTranscript agents={phase.agents} />
          <ComposeBar agents={phase.agents} />
        </div>
      </div>
      <AddAgentModal
        bind:open={addAgentOpen}
        busy={addAgentBusy}
        error={addAgentError}
        {claudeAvailability}
        {codexAvailability}
        onSubmit={handleAddAgentFromLoaded}
        onCancel={handleAddAgentCancel}
      />
    {/if}
  </div>
</main>
