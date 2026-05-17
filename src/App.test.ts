import { vi, describe, it, expect, beforeEach, afterEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import type { AgentRecord, DirectoryInfo, NormalizedEvent, ProjectSummary } from "$lib/types";

// App.svelte tests focus narrowly on the primary phase-transition routing
// of the M2.5 acceptance flow:
//
//   welcome → directory-selector → no-agent → loaded
//
// Plus the cancel-back-to-welcome path. Per-screen UI is already covered by
// Sidebar / UnifiedTranscript / ComposeBar / DirectorySelector tests; these
// tests verify the App-level orchestration that glues them together (which
// IPC commands fire, in what order, and which phase renders next).
//
// Edge cases (cancel error states, multi-project switcher, rebind UI
// state-reset across phases) are deliberately deferred to M4 when the
// project switcher exercises them harder.

const invokeMock = vi.fn(
  async (_cmd: string, _args?: Record<string, unknown>): Promise<unknown> => null,
);
// `agent:<id>` event-channel callbacks captured here so E2E tests can fire
// a recorded sequence into the state module and assert the round trip.
const listenCallbacks = new Map<string, (e: { payload: NormalizedEvent }) => void>();
const listenMock = vi.fn(async (event: string, handler: unknown): Promise<() => void> => {
  listenCallbacks.set(event, handler as (e: { payload: NormalizedEvent }) => void);
  // Honest unlisten: mirrors Tauri's `listen()` return shape (call it to
  // detach the handler). Intra-test mount/unmount cycles are rare today
  // but a no-op would silently keep stale callbacks alive across them.
  return () => {
    listenCallbacks.delete(event);
  };
});
const openDialogMock = vi.fn(async (_options: unknown): Promise<string | null> => null);

function fireTo(channel: string, event: NormalizedEvent): void {
  const cb = listenCallbacks.get(channel);
  if (cb === undefined) throw new Error(`no listener registered for ${channel}`);
  cb({ payload: event });
}

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: (event: string, handler: unknown) => listenMock(event, handler),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: (options: unknown) => openDialogMock(options),
}));

const PATH = "/tmp/sw-test";
const PROJECT: ProjectSummary = {
  id: "11111111-1111-7000-8000-111111111111",
  name: "alpha",
  created_at: "2026-05-13T00:00:00Z",
};
const AGENT: AgentRecord = {
  id: "22222222-2222-7000-8000-222222222222",
  project_id: PROJECT.id,
  name: "assistant",
  harness: "claude_code",
  session_id: "33333333-3333-7000-8000-333333333333",
  created_at: "2026-05-13T00:00:01Z",
};

const INFO_NO_SWITCHBOARD: DirectoryInfo = {
  path: PATH,
  has_switchboard: false,
  projects: [],
};

/**
 * Drives `invokeMock` with a per-command lookup so tests can be declarative
 * about which commands succeed (and with what return value) without
 * assuming any specific call order — App.svelte may legitimately reorder
 * sub-steps within a phase transition.
 *
 * The three startup probes (`check_claude_binary`, `check_codex_binary`,
 * `check_codex_auth`) default to success so non-banner tests don't need to
 * spell them out, but `unexpected invoke call` still throws for any IPC
 * the test didn't anticipate. Banner-failure tests use
 * `invokeMock.mockImplementation` directly to override individual probes.
 */
function setInvokeResponses(map: Record<string, unknown>): void {
  const withDefaults: Record<string, unknown> = {
    check_claude_binary: null,
    check_codex_binary: null,
    check_codex_auth: null,
    ...map,
  };
  invokeMock.mockImplementation(async (cmd: string) => {
    if (!(cmd in withDefaults)) {
      throw new Error(`unexpected invoke call: ${cmd}`);
    }
    return withDefaults[cmd];
  });
}

describe("App", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    listenMock.mockReset();
    listenCallbacks.clear();
    // Restore the capture-callback default after `.mockReset()` clears it.
    listenMock.mockImplementation(async (event: string, handler: unknown) => {
      listenCallbacks.set(event, handler as (e: { payload: NormalizedEvent }) => void);
      return () => {
        listenCallbacks.delete(event);
      };
    });
    openDialogMock.mockReset();
  });

  // The state-rune module is a singleton; tests that register agents
  // would otherwise leak state across runs. Drain via the test-only
  // reset helper.
  afterEach(async () => {
    const { _testing } = await import("$lib/state/index.svelte");
    _testing.reset();
  });

  it("mounts and renders the welcome screen when all harness probes succeed", async () => {
    setInvokeResponses({});
    const App = (await import("./App.svelte")).default;
    render(App);
    expect(screen.getByText("Switchboard")).toBeInTheDocument();
    expect(screen.getByText("Open working directory")).toBeInTheDocument();
    await waitFor(() => {
      expect(screen.queryByTestId(/^banner-/)).not.toBeInTheDocument();
    });
  });

  it("renders a Claude binary-missing banner when only the Claude probe fails", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "check_claude_binary")
        throw new Error("harness probe failed: harness binary not found");
      if (cmd === "check_codex_binary" || cmd === "check_codex_auth") return null;
      throw new Error(`unexpected invoke call: ${cmd}`);
    });
    const App = (await import("./App.svelte")).default;
    render(App);
    await waitFor(() => {
      expect(screen.getByTestId("banner-binary_missing-claude_code")).toBeInTheDocument();
    });
    // The other harness's banners must NOT show — independent per-harness state.
    expect(screen.queryByTestId("banner-binary_missing-codex")).not.toBeInTheDocument();
    expect(screen.queryByTestId("banner-auth_missing-codex")).not.toBeInTheDocument();
  });

  it("renders a Codex auth-missing banner when only the Codex auth probe fails", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "check_codex_auth") throw new Error("auth not configured");
      if (cmd === "check_claude_binary" || cmd === "check_codex_binary") return null;
      throw new Error(`unexpected invoke call: ${cmd}`);
    });
    const App = (await import("./App.svelte")).default;
    render(App);
    await waitFor(() => {
      expect(screen.getByTestId("banner-auth_missing-codex")).toBeInTheDocument();
    });
  });

  it("both binaries missing: two binary banners render simultaneously, no auth banner", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "check_claude_binary" || cmd === "check_codex_binary") throw new Error("missing");
      if (cmd === "check_codex_auth") return null;
      throw new Error(`unexpected invoke call: ${cmd}`);
    });
    const App = (await import("./App.svelte")).default;
    render(App);
    await waitFor(() => {
      expect(screen.getByTestId("banner-binary_missing-claude_code")).toBeInTheDocument();
      expect(screen.getByTestId("banner-binary_missing-codex")).toBeInTheDocument();
    });
    // Suppression rule applies per-harness independently — Codex auth banner
    // hidden because Codex binary is missing.
    expect(screen.queryByTestId("banner-auth_missing-codex")).not.toBeInTheDocument();
  });

  it("slow probe doesn't block fast probes — each updates its slice independently", async () => {
    // `check_codex_auth` never resolves; `check_claude_binary` and
    // `check_codex_binary` both fail fast. The Claude + Codex binary
    // banners must surface even while the auth probe is still in flight.
    // Pins the per-probe-resolution invariant (no Promise.all barrier).
    const authPromise = new Promise<null>(() => {
      // Intentionally never resolves. Each test runs in its own
      // singleton-state context (the `afterEach` reset drains
      // listeners); the hung promise is GC'd when the test scope
      // tears down.
    });
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "check_claude_binary" || cmd === "check_codex_binary") throw new Error("missing");
      if (cmd === "check_codex_auth") return authPromise;
      throw new Error(`unexpected invoke call: ${cmd}`);
    });
    const App = (await import("./App.svelte")).default;
    render(App);
    // Both binary banners surface without waiting on the hung auth probe.
    await waitFor(() => {
      expect(screen.getByTestId("banner-binary_missing-claude_code")).toBeInTheDocument();
      expect(screen.getByTestId("banner-binary_missing-codex")).toBeInTheDocument();
    });
    // Auth banner is correctly suppressed (Codex binary missing), but
    // also wouldn't surface independently since the probe hasn't
    // resolved yet.
    expect(screen.queryByTestId("banner-auth_missing-codex")).not.toBeInTheDocument();
  });

  it("suppresses Codex auth banner when Codex binary is also missing (auth is irrelevant)", async () => {
    // Both Codex probes fail. Only the binary banner should show — the
    // user can't act on the auth gap until they install the CLI.
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "check_codex_binary" || cmd === "check_codex_auth") throw new Error("missing");
      if (cmd === "check_claude_binary") return null;
      throw new Error(`unexpected invoke call: ${cmd}`);
    });
    const App = (await import("./App.svelte")).default;
    render(App);
    await waitFor(() => {
      expect(screen.getByTestId("banner-binary_missing-codex")).toBeInTheDocument();
    });
    expect(screen.queryByTestId("banner-auth_missing-codex")).not.toBeInTheDocument();
  });

  it("welcome → directory-selector: clicking Open working directory invokes pick_directory and renders the selector", async () => {
    setInvokeResponses({
      check_claude_binary: null,
      pick_directory: INFO_NO_SWITCHBOARD,
    });
    openDialogMock.mockResolvedValueOnce(PATH);

    const App = (await import("./App.svelte")).default;
    render(App);
    await waitFor(() => {
      expect(screen.getByText("Open working directory")).toBeInTheDocument();
    });

    await fireEvent.click(screen.getByText("Open working directory"));

    await waitFor(() => {
      expect(screen.getByText("Working directory")).toBeInTheDocument();
    });
    // Asserts on the IPC sequence — would catch a handler that rendered the
    // right phase but skipped the inspect call (or vice versa).
    expect(openDialogMock).toHaveBeenCalledOnce();
    const pickCalls = invokeMock.mock.calls.filter(([cmd]) => cmd === "pick_directory");
    expect(pickCalls).toHaveLength(1);
    expect(pickCalls[0]?.[1]).toEqual({ path: PATH });
  });

  it("directory-selector → no-agent: confirming Initialize fires init_directory + create_project + set_active_project + list_agents, then renders Create agent", async () => {
    setInvokeResponses({
      check_claude_binary: null,
      pick_directory: INFO_NO_SWITCHBOARD,
      init_directory: { ...INFO_NO_SWITCHBOARD, has_switchboard: true },
      create_project: PROJECT,
      set_active_project: null,
      list_agents: [],
    });
    openDialogMock.mockResolvedValueOnce(PATH);

    const App = (await import("./App.svelte")).default;
    render(App);
    await waitFor(() => expect(screen.getByText("Open working directory")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("Open working directory"));
    await waitFor(() => expect(screen.getByTestId("confirm-init")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("confirm-init"));

    await waitFor(() => {
      expect(screen.getByText("Create an agent")).toBeInTheDocument();
    });
    // The IPC sequence must include all four backend steps. A handler that
    // skipped any one (e.g., forgot to call set_active_project) would render
    // the right next-phase but leave AppState mis-configured.
    const cmds = invokeMock.mock.calls.map(([cmd]) => cmd);
    expect(cmds).toContain("init_directory");
    expect(cmds).toContain("create_project");
    expect(cmds).toContain("set_active_project");
    expect(cmds).toContain("list_agents");
  });

  it("no-agent → loaded: submitting Create agent invokes create_agent and renders the 3-pane layout", async () => {
    setInvokeResponses({
      check_claude_binary: null,
      pick_directory: INFO_NO_SWITCHBOARD,
      init_directory: { ...INFO_NO_SWITCHBOARD, has_switchboard: true },
      create_project: PROJECT,
      set_active_project: null,
      list_agents: [],
      create_agent: AGENT,
    });
    openDialogMock.mockResolvedValueOnce(PATH);

    const App = (await import("./App.svelte")).default;
    render(App);
    await waitFor(() => expect(screen.getByText("Open working directory")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("Open working directory"));
    await waitFor(() => expect(screen.getByTestId("confirm-init")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("confirm-init"));
    await waitFor(() => expect(screen.getByTestId("confirm-create-agent")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("confirm-create-agent"));

    // The "loaded" phase renders Sidebar + UnifiedTranscript + ComposeBar.
    await waitFor(() => {
      expect(screen.getByTestId("loaded-layout")).toBeInTheDocument();
      expect(screen.getByTestId("sidebar")).toBeInTheDocument();
      expect(screen.getByTestId("unified-transcript")).toBeInTheDocument();
      expect(screen.getByTestId("compose-textarea")).toBeInTheDocument();
    });
    // Sidebar surfaces the agent's name.
    expect(screen.getByTestId("agent-name")).toHaveTextContent("assistant");

    const createAgentCalls = invokeMock.mock.calls.filter(([cmd]) => cmd === "create_agent");
    expect(createAgentCalls).toHaveLength(1);
    expect(createAgentCalls[0]?.[1]).toEqual({ name: "assistant", harness: "claude_code" });
  });

  it("directory-selector → welcome: clicking Cancel returns to the welcome screen", async () => {
    setInvokeResponses({
      check_claude_binary: null,
      pick_directory: INFO_NO_SWITCHBOARD,
    });
    openDialogMock.mockResolvedValueOnce(PATH);

    const App = (await import("./App.svelte")).default;
    render(App);
    await waitFor(() => expect(screen.getByText("Open working directory")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("Open working directory"));
    await waitFor(() => expect(screen.getByText("Working directory")).toBeInTheDocument());

    // Cancel from the directory-selector phase. M1.5 user flow — not deferred
    // to M4 — so it gets a dedicated assertion.
    const cancels = screen.getAllByRole("button", { name: /cancel/i });
    await fireEvent.click(cancels[cancels.length - 1]!);

    await waitFor(() => {
      expect(screen.getByText("Open working directory")).toBeInTheDocument();
      expect(screen.queryByText("Working directory")).not.toBeInTheDocument();
    });
  });

  // M2.5 plan's "dynamic agent add" acceptance test. The first agent's
  // session is already loaded; the user opens the sidebar "+" entry point,
  // submits, and the new agent appears in the sidebar (and is registered
  // in the state module so its events would flow on dispatch). The
  // load-bearing property: the new agent's listener is wired before the
  // phase transition completes, so an immediate dispatch wouldn't lose
  // the first event.
  it("loaded → add agent via sidebar modal: appends to phase.agents and registers a listener", async () => {
    const SECOND_AGENT: AgentRecord = {
      id: "44444444-4444-7000-8000-444444444444",
      project_id: PROJECT.id,
      name: "second",
      harness: "codex",
      session_id: null,
      created_at: "2026-05-13T00:00:02Z",
    };
    setInvokeResponses({
      check_claude_binary: null,
      pick_directory: INFO_NO_SWITCHBOARD,
      init_directory: { ...INFO_NO_SWITCHBOARD, has_switchboard: true },
      create_project: PROJECT,
      set_active_project: null,
      list_agents: [],
      create_agent: AGENT,
    });
    openDialogMock.mockResolvedValueOnce(PATH);

    // Walk through to the loaded phase with one agent already present.
    const App = (await import("./App.svelte")).default;
    render(App);
    await waitFor(() => expect(screen.getByText("Open working directory")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("Open working directory"));
    await waitFor(() => expect(screen.getByTestId("confirm-init")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("confirm-init"));
    await waitFor(() => expect(screen.getByTestId("confirm-create-agent")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    await waitFor(() => expect(screen.getByTestId("loaded-layout")).toBeInTheDocument());

    // listen() count at the loaded boundary — registerAgent wires one
    // listener per agent. Captures the baseline so we can assert the
    // second-agent registration adds exactly one more.
    const listenCallsBeforeAdd = listenMock.mock.calls.length;

    // Now swap create_agent to return the second agent for the next call.
    // (setInvokeResponses replaces the dispatch table; preserve everything
    // else.)
    setInvokeResponses({
      check_claude_binary: null,
      pick_directory: INFO_NO_SWITCHBOARD,
      init_directory: { ...INFO_NO_SWITCHBOARD, has_switchboard: true },
      create_project: PROJECT,
      set_active_project: null,
      list_agents: [],
      create_agent: SECOND_AGENT,
    });

    // Sidebar's "+" opens the modal. The modal renders a CreateAgentForm
    // (embedded variant — same data-testid="confirm-create-agent").
    await fireEvent.click(screen.getByTestId("sidebar-add-agent"));
    await waitFor(() => expect(screen.getByTestId("dialog-content")).toBeInTheDocument());
    // Scope queries to the modal — Sidebar also uses `data-testid="agent-name"`
    // for each agent row, so a global getByTestId("agent-name") would match
    // multiple after the modal opens.
    const modal = screen.getByTestId("dialog-content");

    // Default form values (mode=create, harness=claude_code, name=assistant)
    // would collide with the existing agent's name. Change the name to
    // something unique before submitting.
    const nameInput = within(modal).getByTestId("agent-name") as HTMLInputElement;
    await fireEvent.input(nameInput, { target: { value: "second" } });
    await fireEvent.click(within(modal).getByTestId("harness-codex"));
    await fireEvent.click(within(modal).getByTestId("confirm-create-agent"));

    // The new agent should land in the sidebar (immutable phase update).
    // Both agents render side-by-side; assert the second is now present.
    await waitFor(() => {
      const names = screen.getAllByTestId("agent-name");
      expect(names.some((n) => n.textContent === "second")).toBe(true);
    });

    // Modal closes after successful submission.
    expect(screen.queryByTestId("dialog-content")).not.toBeInTheDocument();

    // create_agent IPC fired with the form's values (proves the modal's
    // submission was correctly threaded through createOrAttachAndRegister).
    const createAgentCalls = invokeMock.mock.calls.filter(([cmd]) => cmd === "create_agent");
    expect(createAgentCalls).toHaveLength(2); // first agent + this one
    expect(createAgentCalls[1]?.[1]).toEqual({ name: "second", harness: "codex" });

    // Exactly one additional listener was registered — the load-bearing
    // property for "immediately dispatch and no events miss." A regression
    // that updated phase.agents without calling registerAgent would still
    // render the sidebar but silently drop events.
    expect(listenMock.mock.calls.length).toBe(listenCallsBeforeAdd + 1);
    expect(listenMock.mock.calls.at(-1)?.[0]).toBe(`agent:${SECOND_AGENT.id}`);
  });

  // Companion to the "dynamic agent add" test above, exercising the
  // **attach existing session** code path through the same modal. The
  // load-bearing assertion is the IPC shape: a regression that mis-wired
  // the form's `attach` mode to call `create_agent` (or to ship
  // existing_session_id under the wrong field) would still render the
  // sidebar agent but silently never reuse the existing harness session.
  it("loaded → attach agent via sidebar modal: invokes attach_agent with existing_session_id and registers a listener", async () => {
    const EXISTING_SESSION_ID = "55555555-5555-7000-8000-555555555555";
    const ATTACHED_AGENT: AgentRecord = {
      id: "66666666-6666-7000-8000-666666666666",
      project_id: PROJECT.id,
      name: "attached-claude",
      harness: "claude_code",
      session_id: EXISTING_SESSION_ID,
      created_at: "2026-05-13T00:00:03Z",
    };
    setInvokeResponses({
      pick_directory: INFO_NO_SWITCHBOARD,
      init_directory: { ...INFO_NO_SWITCHBOARD, has_switchboard: true },
      create_project: PROJECT,
      set_active_project: null,
      list_agents: [],
      create_agent: AGENT,
    });
    openDialogMock.mockResolvedValueOnce(PATH);

    const App = (await import("./App.svelte")).default;
    render(App);
    await waitFor(() => expect(screen.getByText("Open working directory")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("Open working directory"));
    await waitFor(() => expect(screen.getByTestId("confirm-init")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("confirm-init"));
    await waitFor(() => expect(screen.getByTestId("confirm-create-agent")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    await waitFor(() => expect(screen.getByTestId("loaded-layout")).toBeInTheDocument());

    const listenCallsBeforeAttach = listenMock.mock.calls.length;

    setInvokeResponses({
      pick_directory: INFO_NO_SWITCHBOARD,
      init_directory: { ...INFO_NO_SWITCHBOARD, has_switchboard: true },
      create_project: PROJECT,
      set_active_project: null,
      list_agents: [],
      attach_agent: ATTACHED_AGENT,
    });

    await fireEvent.click(screen.getByTestId("sidebar-add-agent"));
    await waitFor(() => expect(screen.getByTestId("dialog-content")).toBeInTheDocument());
    const modal = screen.getByTestId("dialog-content");

    // Switch the modal from "create" mode (default) to "attach".
    await fireEvent.click(within(modal).getByTestId("mode-attach"));

    const nameInput = within(modal).getByTestId("agent-name") as HTMLInputElement;
    await fireEvent.input(nameInput, { target: { value: "attached-claude" } });
    const sessionInput = within(modal).getByTestId("attach-session-id") as HTMLInputElement;
    await fireEvent.input(sessionInput, { target: { value: EXISTING_SESSION_ID } });
    await fireEvent.click(within(modal).getByTestId("confirm-create-agent"));

    // Sidebar shows the attached agent.
    await waitFor(() => {
      const names = screen.getAllByTestId("agent-name");
      expect(names.some((n) => n.textContent === "attached-claude")).toBe(true);
    });
    // Modal closes after successful submission.
    expect(screen.queryByTestId("dialog-content")).not.toBeInTheDocument();

    // attach_agent IPC fired with the attach-shaped payload. A regression
    // that fell back to create_agent (wrong command) or dropped the
    // existing_session_id field would fail this check.
    const attachCalls = invokeMock.mock.calls.filter(([cmd]) => cmd === "attach_agent");
    expect(attachCalls).toHaveLength(1);
    expect(attachCalls[0]?.[1]).toEqual({
      name: "attached-claude",
      harness: "claude_code",
      // camelCase at the IPC boundary — Tauri converts to Rust snake_case
      // on the way in. See `src/lib/api.ts::attachAgent`.
      existingSessionId: EXISTING_SESSION_ID,
    });
    // No create_agent call leaked from the attach path.
    const createAgentCallsForAttach = invokeMock.mock.calls.filter(
      ([cmd, args]) =>
        cmd === "create_agent" && (args as Record<string, unknown>)?.["name"] === "attached-claude",
    );
    expect(createAgentCallsForAttach).toHaveLength(0);

    // Listener for the new agent registered. Same load-bearing property
    // as the dynamic-add test: a regression that updated phase.agents
    // without wiring the channel would still render the sidebar but
    // silently drop dispatch events.
    expect(listenMock.mock.calls.length).toBe(listenCallsBeforeAttach + 1);
    expect(listenMock.mock.calls.at(-1)?.[0]).toBe(`agent:${ATTACHED_AGENT.id}`);
  });

  // The pair of E2E tests below exercise the full round trip:
  //   compose-bar Send → invoke("send_message") → captured listen callback
  //   → reducer → UI render → run_status returns to idle.
  // Per-layer unit tests cover each piece in isolation; these pin the
  // wiring through App.svelte against IPC-field renames or listener-
  // registration regressions. One test per harness because the
  // post-terminal event ordering differs (Codex emits RateLimitEvent +
  // SessionMeta between TurnEnd and AgentIdle).
  it("E2E (Claude): send → turn_start → content_chunk → turn_end → agent_idle renders the response and unblocks Send", async () => {
    setInvokeResponses({
      pick_directory: INFO_NO_SWITCHBOARD,
      init_directory: { ...INFO_NO_SWITCHBOARD, has_switchboard: true },
      create_project: PROJECT,
      set_active_project: null,
      list_agents: [],
      create_agent: AGENT,
      send_message: "77777777-7777-7000-8000-777777777777", // turn_id
    });
    openDialogMock.mockResolvedValueOnce(PATH);

    const App = (await import("./App.svelte")).default;
    render(App);
    await waitFor(() => expect(screen.getByText("Open working directory")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("Open working directory"));
    await waitFor(() => expect(screen.getByTestId("confirm-init")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("confirm-init"));
    await waitFor(() => expect(screen.getByTestId("confirm-create-agent")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    await waitFor(() => expect(screen.getByTestId("loaded-layout")).toBeInTheDocument());

    // Type the prompt and Send.
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "hi" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    // IPC for send_message fired with the right args.
    await waitFor(() => {
      const sendCalls = invokeMock.mock.calls.filter(([cmd]) => cmd === "send_message");
      expect(sendCalls).toHaveLength(1);
      expect(sendCalls[0]?.[1]).toEqual({ agentId: AGENT.id, prompt: "hi" });
    });

    // Fire the recorded Claude event sequence through the captured listener.
    const turnId = "88888888-8888-7000-8000-888888888888";
    const channel = `agent:${AGENT.id}`;
    fireTo(channel, {
      type: "turn_start",
      turn_id: turnId,
      started_at: "2026-05-16T00:00:00Z",
    });
    fireTo(channel, {
      type: "content_chunk",
      turn_id: turnId,
      kind: "text",
      text: "hello back",
    });
    fireTo(channel, {
      type: "turn_end",
      turn_id: turnId,
      outcome: { status: "completed" },
      ended_at: "2026-05-16T00:00:01Z",
      usage: null,
    });
    fireTo(channel, { type: "agent_idle", agent_id: AGENT.id });

    // Transcript shows the streamed text.
    await waitFor(() => {
      expect(screen.getByTestId("unified-transcript")).toHaveTextContent("hello back");
    });
    // Send is re-enabled (run_status flipped back to idle on agent_idle).
    // Send-disabled state also depends on the textarea having content; the
    // compose bar clears the textarea on submit, so a fresh prompt must
    // be typed to observe the re-enabled state.
    await fireEvent.input(textarea, { target: { value: "again" } });
    await waitFor(() => {
      expect(screen.getByTestId("compose-send")).not.toBeDisabled();
    });
  });

  it("E2E (Codex): post-terminal sequence (turn_end → rate_limit_event → session_meta → agent_idle) lands metadata and unblocks Send", async () => {
    const CODEX_AGENT: AgentRecord = {
      id: "99999999-9999-7000-8000-999999999999",
      project_id: PROJECT.id,
      name: "codex-bot",
      harness: "codex",
      session_id: null,
      created_at: "2026-05-13T00:00:04Z",
    };
    setInvokeResponses({
      pick_directory: INFO_NO_SWITCHBOARD,
      init_directory: { ...INFO_NO_SWITCHBOARD, has_switchboard: true },
      create_project: PROJECT,
      set_active_project: null,
      list_agents: [],
      create_agent: CODEX_AGENT,
      send_message: "aaaaaaaa-aaaa-7000-8000-aaaaaaaaaaaa", // turn_id
    });
    openDialogMock.mockResolvedValueOnce(PATH);

    const App = (await import("./App.svelte")).default;
    render(App);
    await waitFor(() => expect(screen.getByText("Open working directory")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("Open working directory"));
    await waitFor(() => expect(screen.getByTestId("confirm-init")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("confirm-init"));
    await waitFor(() => expect(screen.getByTestId("confirm-create-agent")).toBeInTheDocument());
    // The create-agent form defaults to Claude; switch to Codex.
    await fireEvent.click(screen.getByTestId("harness-codex"));
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    await waitFor(() => expect(screen.getByTestId("loaded-layout")).toBeInTheDocument());

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "ack?" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => {
      const sendCalls = invokeMock.mock.calls.filter(([cmd]) => cmd === "send_message");
      expect(sendCalls).toHaveLength(1);
      expect(sendCalls[0]?.[1]).toEqual({ agentId: CODEX_AGENT.id, prompt: "ack?" });
    });

    const turnId = "bbbbbbbb-bbbb-7000-8000-bbbbbbbbbbbb";
    const channel = `agent:${CODEX_AGENT.id}`;
    fireTo(channel, {
      type: "turn_start",
      turn_id: turnId,
      started_at: "2026-05-16T00:00:00Z",
    });
    fireTo(channel, {
      type: "content_chunk",
      turn_id: turnId,
      kind: "text",
      text: "ack",
    });

    // Transcript shows the response text mid-stream (before terminal).
    await waitFor(() => {
      expect(screen.getByTestId("unified-transcript")).toHaveTextContent("ack");
    });

    // Codex post-terminal enrichment: turn_end is followed by
    // rate_limit_event + session_meta + agent_idle, in that order. The
    // load-bearing frontend invariant is that **run_status stays
    // "processing" through the entire window** until `agent_idle` lands —
    // so a fresh prompt typed mid-window must keep Send disabled. This
    // test pins that invariant by re-populating the textarea after
    // turn_end (the compose-bar clears it on submit) and asserting Send
    // stays disabled after each post-terminal event except the final
    // `agent_idle`. A regression that flipped run_status to idle on
    // turn_end (the obvious-but-wrong simplification) would fail at the
    // first post-turn_end assertion below.
    fireTo(channel, {
      type: "turn_end",
      turn_id: turnId,
      outcome: { status: "completed" },
      ended_at: "2026-05-16T00:00:01Z",
      usage: {
        input_tokens: 100,
        output_tokens: 5,
        context_window: 200000,
      },
    });
    // Re-populate the textarea so Send-disabled is *only* a function of
    // run_status, not of empty input.
    await fireEvent.input(textarea, { target: { value: "next" } });
    await waitFor(() => {
      expect(screen.getByTestId("compose-send")).toBeDisabled();
    });

    fireTo(channel, {
      type: "rate_limit_event",
      agent_id: CODEX_AGENT.id,
      info: { primary: { used_percent: 12.5 } },
    });
    // Still in post-terminal window — Send must remain disabled.
    expect(screen.getByTestId("compose-send")).toBeDisabled();

    fireTo(channel, {
      type: "session_meta",
      agent_id: CODEX_AGENT.id,
      model: "gpt-test",
      harness_version: "0.130.0",
      tools: [],
      mcp_servers: [{ name: "fs", status: "connected" }],
      skills: [],
      raw: null,
    });
    // session_meta is the last post-terminal event before agent_idle.
    // Send must STILL be disabled — if a regression flipped run_status
    // on session_meta (or earlier), this is where it surfaces.
    expect(screen.getByTestId("compose-send")).toBeDisabled();

    fireTo(channel, { type: "agent_idle", agent_id: CODEX_AGENT.id });
    // Only now — after agent_idle — does Send re-enable.
    await waitFor(() => {
      expect(screen.getByTestId("compose-send")).not.toBeDisabled();
    });
  });
});
