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
    BinaryState,
    DirectoryInfo,
    HarnessAvailability,
    HarnessBanner,
    ProjectSummary,
  } from "$lib/types";
  import { bannerCopy, bannerTestid } from "$lib/harnessAvailability";
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

  /// Per-harness availability state. **Stored as flat per-probe fields**
  /// rather than as the `HarnessAvailability` discriminated union directly:
  /// the union is the consumer-facing boundary type (banners + form
  /// gating), but probe handlers write a single field at a time. Holding
  /// the runtime state as union values would force every per-probe write
  /// to re-construct the whole variant and read-then-spread the unchanged
  /// fields, which is friction without benefit. Instead: flat fields here
  /// → derived unions for consumers.
  ///
  /// Initial state uses `"checking"` to encode the pre-probe state in
  /// the type system rather than fail-open-by-convention. Form gating
  /// treats `"checking"` as not-selectable (silent disable) so a user
  /// fast enough to reach the create form before probes complete can't
  /// submit before we know. Banners stay hidden during checking because
  /// the suppression rule only pushes on `"missing"`.
  ///
  /// Claude auth is `"unsupported"` always (keychain-based on macOS; no
  /// reliable file signal — deferred to v2 per the M2.5 plan).
  let claudeBinary = $state<BinaryState>("checking");
  let codexBinary = $state<BinaryState>("checking");
  let codexAuth = $state<"available" | "missing" | "checking">("checking");

  const claudeAvailability = $derived<HarnessAvailability>({
    harness: "claude_code",
    binary: claudeBinary,
    auth: "unsupported",
  });
  const codexAvailability = $derived<HarnessAvailability>({
    harness: "codex",
    binary: codexBinary,
    auth: codexAuth,
  });

  /// Banner stack ordering: binary-missing first, then auth-missing.
  /// Suppression rule: if a harness's binary is missing, its auth banner
  /// is hidden (auth is irrelevant if the CLI isn't installed). Max two
  /// banners visible (one per harness).
  ///
  /// The `auth_missing` push gates on `a.harness === "codex"` because
  /// `HarnessBanner.auth_missing` is type-narrowed to Codex (v1 invariant).
  const banners = $derived.by((): HarnessBanner[] => {
    const result: HarnessBanner[] = [];
    for (const a of [claudeAvailability, codexAvailability]) {
      if (a.binary === "missing") {
        result.push({ kind: "binary_missing", harness: a.harness });
      } else if (a.auth === "missing" && a.harness === "codex") {
        result.push({ kind: "auth_missing", harness: "codex" });
      }
    }
    return result;
  });

  // Startup probes. Each probe writes its own slice as soon as it
  // resolves — no `Promise.all` barrier. A slow `check_codex_auth`
  // doesn't block the Claude binary state from leaving `"checking"`;
  // a slow `check_codex_binary` doesn't block its own auth check
  // from updating. The data model stays honest about *what we know
  // now* vs *what we're still waiting on*, same principle as the
  // `"checking"` state itself.
  //
  // UI flow proceeds regardless — sending to a missing-binary agent
  // fails at dispatch with a typed error.
  onMount(() => {
    api.checkClaudeBinary().then(
      () => (claudeBinary = "available"),
      () => (claudeBinary = "missing"),
    );
    api.checkCodexBinary().then(
      () => (codexBinary = "available"),
      () => (codexBinary = "missing"),
    );
    api.checkCodexAuth().then(
      () => (codexAuth = "available"),
      () => (codexAuth = "missing"),
    );
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
