import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { tick } from "svelte";
import type { AgentRecord, NormalizedEvent, Prompt } from "$lib/types";
// Static import so the component-tree transform happens at module collection,
// not inside the first test's timeout (cold CI transforms have no vite cache).
// `vi.mock` is hoisted above imports, so the mocks below still apply.
import ComposeBar from "./ComposeBar.svelte";
import { workflowRuns, _testing as workflowsTesting } from "$lib/state/workflows.svelte";
import type { WorkflowRunInfo } from "$lib/types";

const invokeMock = vi.fn(
  async (_cmd: string, _args?: Record<string, unknown>): Promise<unknown> => null,
);

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

const listeners = new Map<string, (e: { payload: NormalizedEvent }) => void>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (e: { payload: NormalizedEvent }) => void) => {
    listeners.set(name, cb);
    return vi.fn();
  }),
}));

type DragDropPayload =
  | { type: "enter"; paths: string[]; position: { x: number; y: number } }
  | { type: "over"; position: { x: number; y: number } }
  | { type: "drop"; paths: string[]; position: { x: number; y: number } }
  | { type: "leave" };

// Capture the compose bar's drag-drop subscription so tests can drive OS file
// drops (the webview event Tauri raises instead of an HTML5 `drop`). The
// subscription promise is deferred — `resolveDropSub()` resolves it with the
// tracked `dropUnlisten`, letting a test exercise the unmount-beats-promise race.
let dragDropCb: ((e: { payload: DragDropPayload }) => void) | undefined;
const dropUnlisten = vi.fn();
let resolveDropSub: (() => void) | undefined;
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: (cb: (e: { payload: DragDropPayload }) => void) => {
      dragDropCb = cb;
      return new Promise<() => void>((resolve) => {
        resolveDropSub = () => resolve(dropUnlisten);
      });
    },
  }),
}));

function fireDrop(paths: string[]): void {
  if (dragDropCb === undefined) throw new Error("no drag-drop subscription");
  // Position is carried by the event but unused — a drop anywhere in the window
  // attaches (the compose bar is the only drop target).
  dragDropCb({ payload: { type: "drop", paths, position: { x: 0, y: 0 } } });
}

async function loadState() {
  return await import("$lib/state/index.svelte");
}

const PROJECT_ID = "00000000-0000-7000-8000-0000000000ff";

const AGENT_A: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000aaa",
  project_id: PROJECT_ID,
  name: "alice",
  harness: "claude_code",
  session_locator: { uuid: "00000000-0000-7000-8000-000000000001" },
  created_at: "2026-05-16T00:00:00Z",
};
const AGENT_B: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000bbb",
  project_id: "00000000-0000-7000-8000-0000000000ff",
  name: "bob",
  harness: "codex",
  session_locator: null,
  created_at: "2026-05-16T00:00:01Z",
};

async function loadComposeStore() {
  return await import("$lib/state/composeStore");
}

async function loadWorkspace() {
  return await import("$lib/state/workspace.svelte");
}

function fireTo(channel: string, event: NormalizedEvent): void {
  const cb = listeners.get(channel);
  if (cb === undefined) throw new Error(`no listener for ${channel}`);
  cb({ payload: event });
}

const chip = (id: string) => screen.getByTestId(`recipient-chip-${id}`);

beforeEach(async () => {
  // Pane/selection state is module-global; reset it BEFORE each test, not in
  // afterEach: vitest runs afterEach hooks LIFO, so a teardown reset would
  // fire while the previous test's ComposeBar is still mounted — clearing the
  // selection store triggers its live persistence effect, which writes a
  // spurious deselect-all into composeStore after composeStore's own reset.
  (await import("$lib/state/transcriptPanes.svelte"))._testing.reset();
  (await import("$lib/state/recipientSelection.svelte"))._testing.reset();
  listeners.clear();
  dragDropCb = undefined;
  resolveDropSub = undefined;
  dropUnlisten.mockClear();
  invokeMock.mockReset();
  invokeMock.mockImplementation(
    async (cmd: string, args?: Record<string, unknown>): Promise<unknown> => {
      if (cmd === "search_project_files") return [];
      // Echo a staged attachment back for a dropped source path: the basename
      // becomes `original_name`, and a staged path is returned.
      if (cmd === "stage_attachment") {
        const source = String((args as { sourcePath?: unknown })?.sourcePath ?? "drop");
        const name = source.split("/").pop() ?? source;
        return {
          path: `/proj/.switchboard/projects/p/attachments/uuid__${name}`,
          original_name: name,
        };
      }
      return null;
    },
  );
});

afterEach(async () => {
  const { _testing } = await loadState();
  _testing.reset();
  (await loadComposeStore())._testing.reset();
  (await loadWorkspace())._testing.reset();
});

describe("ComposeBar", () => {
  it("hides the recipient field for a single agent but still sends to it", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockResolvedValueOnce("msg-1");

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    expect(screen.queryByTestId("recipient-field")).toBeNull();
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(true);

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "hi" } });
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(false);
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => {
      const calls = invokeMock.mock.calls.filter(([c]) => c === "send_message");
      expect(calls).toHaveLength(1);
      expect(calls[0]?.[1]).toMatchObject({ agentId: AGENT_A.id, prompt: "hi" });
      expect(typeof (calls[0]?.[1] as { sendId?: unknown }).sendId).toBe("string");
    });
  });

  it("shows a toggle chip per agent; the first is selected by default", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "false");
  });

  it("grows the message box with content up to its max height", async () => {
    const scrollHeight = vi.spyOn(HTMLTextAreaElement.prototype, "scrollHeight", "get");
    const getComputedStyleSpy = vi.spyOn(window, "getComputedStyle");
    try {
      const state = await loadState();
      await state.registerAgent(AGENT_A);

      getComputedStyleSpy.mockReturnValue({ maxHeight: "192px" } as CSSStyleDeclaration);
      scrollHeight.mockImplementation(function (this: HTMLTextAreaElement): number {
        if (this.value.includes("six")) return this.style.height === "auto" ? 240 : 192;
        if (this.value === "short again") return 72;
        return 96;
      });
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
      await tick();
      const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
      expect(textarea.style.height).toBe("96px");
      expect(textarea.style.overflowY).toBe("hidden");

      await fireEvent.input(textarea, { target: { value: "one\ntwo\nthree\nfour\nfive\nsix" } });
      await tick();
      expect(textarea.style.height).toBe("192px");
      expect(textarea.style.overflowY).toBe("auto");

      await fireEvent.input(textarea, { target: { value: "short again" } });
      await tick();
      expect(textarea.style.height).toBe("72px");
      expect(textarea.style.overflowY).toBe("hidden");
    } finally {
      scrollHeight.mockRestore();
      getComputedStyleSpy.mockRestore();
    }
  });

  it("toggles a recipient on and off by clicking its chip", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    await fireEvent.click(chip(AGENT_B.id));
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "true");
    // Toggle alice off; bob stays on.
    await fireEvent.click(chip(AGENT_A.id));
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "false");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "true");
  });

  it("@-quick-add: typing @bob opens the menu, selects via keyboard, strips the token", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "ping @bo" } });
    // bob is offered (alice is already selected); Enter picks bob as the sole recipient.
    await screen.findByTestId(`recipient-option-${AGENT_B.id}`);
    await fireEvent.keyDown(textarea, { key: "Enter" });

    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "false");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "true");
    // The "@bo" token is stripped; the text typed before it (with its space) stays.
    expect(textarea.value).toBe("ping ");
  });

  it("@ menu opens for an @ token in the middle of a message and splices the mention at the caret", async () => {
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "search_project_files") return ["docs/bob.md"];
      return null;
    });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    // "@bo" sits in the middle, with text after it; caret right after "@bo".
    textarea.value = "ping @bo world";
    textarea.setSelectionRange(8, 8);
    await fireEvent.input(textarea);

    // The menu opens for the mid-message token (the bug: it only opened at the
    // end of the text before this fix).
    await screen.findByTestId(`recipient-option-${AGENT_B.id}`);

    // Picking a file mention splices at the caret and preserves the trailing text.
    await fireEvent.click(await screen.findByTestId("file-option-docs/bob.md"));
    await waitFor(() => expect(textarea.value).toBe("ping `docs/bob.md` world"));
  });

  it("picks the menu's token even when the caret moved off it (arrow keys) while open", async () => {
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "search_project_files") return ["docs/bob.md"];
      return null;
    });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    textarea.value = "ping @bo world";
    textarea.setSelectionRange(8, 8);
    await fireEvent.input(textarea);
    await screen.findByTestId("file-option-docs/bob.md");

    // Simulate ArrowLeft moving the caret into the middle of the token while the
    // menu stays open. The pick must still splice the captured token, not the
    // moved caret — otherwise the draft is corrupted (the regression this guards).
    textarea.setSelectionRange(7, 7);
    await fireEvent.keyDown(textarea, { key: "ArrowLeft" });
    await fireEvent.click(screen.getByTestId("file-option-docs/bob.md"));
    await waitFor(() => expect(textarea.value).toBe("ping `docs/bob.md` world"));
  });

  it("stripping a recipient @token mid-message collapses the redundant space", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    textarea.value = "ping @bob world";
    textarea.setSelectionRange(9, 9); // caret right after "@bob"
    await fireEvent.input(textarea);

    await fireEvent.click(await screen.findByTestId(`recipient-option-${AGENT_B.id}`));
    await waitFor(() => expect(textarea.value).toBe("ping world"));
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "true");
  });

  it("adds a trailing space when a mid-message mention is not already followed by one", async () => {
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "search_project_files") return ["docs/bob.md"];
      return null;
    });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    // Caret after "@bo", immediately followed by "hi" (no space) → mention gets one.
    textarea.value = "ping @bohi";
    textarea.setSelectionRange(8, 8);
    await fireEvent.input(textarea);

    await fireEvent.click(await screen.findByTestId("file-option-docs/bob.md"));
    await waitFor(() => expect(textarea.value).toBe("ping `docs/bob.md` hi"));
  });

  it("a non-collapsed selection does not open the @ menu", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    textarea.value = "ping @bo world";
    textarea.setSelectionRange(5, 8); // selection spanning "@bo", not a typing caret
    await fireEvent.input(textarea);
    await tick();
    expect(screen.queryByTestId("recipient-menu")).toBeNull();
  });

  it("@ menu includes already-selected agents because picking one makes it the sole recipient", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    await fireEvent.click(chip(AGENT_B.id)); // alice + bob selected
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "true");

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "send @ali" } });
    await fireEvent.click(await screen.findByTestId(`recipient-option-${AGENT_A.id}`));

    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "false");
    expect(textarea.value).toBe("send ");
  });

  it("@ menu shows matching files above recipients but Enter prefers a matched recipient", async () => {
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "search_project_files") return ["docs/bob.md", "src/box.ts"];
      return null;
    });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "ping @bo" } });

    const file = await screen.findByTestId("file-option-docs/bob.md");
    const bob = await screen.findByTestId(`recipient-option-${AGENT_B.id}`);
    const menu = screen.getByTestId("recipient-menu");
    const menuText = menu.textContent ?? "";
    expect(menu).toHaveClass("inset-x-0", "bottom-full");
    expect(menuText.indexOf("Files")).toBeLessThan(menuText.indexOf("Send to"));
    expect(file).toHaveAttribute("aria-selected", "false");
    const fileLabel = file.querySelector('[data-testid="file-option-label"]');
    const filePath = file.querySelector('[data-testid="file-option-path"]');
    expect(fileLabel).not.toHaveAttribute("dir");
    expect(fileLabel).not.toHaveAttribute("title");
    expect(fileLabel).toHaveTextContent("bob.md");
    expect(fileLabel).toHaveClass("min-w-0", "truncate", "text-left", "text-xs", "font-medium");
    expect(filePath).toHaveTextContent("docs");
    expect(filePath).toHaveClass("truncate", "text-left", "text-[11px]");
    expect(bob).not.toHaveClass("text-xs");
    await fireEvent.pointerEnter(file);
    expect(screen.queryByTestId("tooltip-content")).toBeNull();
    expect(bob).toHaveAttribute("aria-selected", "true");

    await fireEvent.keyDown(textarea, { key: "Enter" });
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "false");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "true");
    expect(textarea.value).toBe("ping ");
  });

  it("@ menu closes the prompt menu before opening", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    await fireEvent.click(screen.getByTestId("compose-prompt-button"));
    expect(await screen.findByTestId("prompt-menu")).toBeInTheDocument();

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "ping @bo" } });

    expect(await screen.findByTestId("recipient-menu")).toBeInTheDocument();
    expect(screen.queryByTestId("prompt-menu")).toBeNull();
  });

  it("opening the prompt menu (via /) closes an open workflow menu", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    await fireEvent.click(screen.getByTestId("compose-workflow-button"));
    expect(await screen.findByTestId("workflow-menu")).toBeInTheDocument();

    // The `/` keyboard path opens the prompt menu; it must close the workflow
    // menu (mirroring the workflow button, which closes the prompt menu) so the
    // two popovers can't render stacked.
    await fireEvent.keyDown(screen.getByTestId("compose-textarea"), { key: "/" });
    expect(await screen.findByTestId("prompt-menu")).toBeInTheDocument();
    expect(screen.queryByTestId("workflow-menu")).toBeNull();
  });

  it("dismisses the prompt menu on a click outside it, but not inside", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    await fireEvent.click(screen.getByTestId("compose-prompt-button"));
    expect(await screen.findByTestId("prompt-menu")).toBeInTheDocument();

    // A pointer down inside the menu leaves it open (picking happens there).
    await fireEvent.pointerDown(screen.getByTestId("prompt-menu-search"));
    expect(screen.queryByTestId("prompt-menu")).toBeInTheDocument();

    // A pointer down on the textarea — inside the compose box but outside the
    // menu — dismisses it. The old hit region was the whole box, so an in-box
    // click like this left the menu stuck open.
    await fireEvent.pointerDown(screen.getByTestId("compose-textarea"));
    expect(screen.queryByTestId("prompt-menu")).toBeNull();
  });

  it("@ menu inserts a selected README file mention without changing recipients", async () => {
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "search_project_files") return ["README.md"];
      return null;
    });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "read @readme" } });
    await fireEvent.click(await screen.findByTestId("file-option-README.md"));

    expect(textarea.value).toBe("read `README.md` ");
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "false");
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("search_project_files", {
        projectId: PROJECT_ID,
        query: "readme",
        limit: 12,
      });
    });
  });

  it("single-agent projects show files on a bare @", async () => {
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "search_project_files") return ["README.md"];
      return null;
    });
    const state = await loadState();
    await state.registerAgent(AGENT_A);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "@" } });
    expect(screen.getByTestId("file-options-status")).toHaveTextContent("Searching files...");
    await fireEvent.click(await screen.findByTestId("file-option-README.md"));

    expect(screen.queryByTestId("recipient-field")).toBeNull();
    expect(textarea.value).toBe("`README.md` ");
  });

  it("@ file search shows an empty state when there are no matches", async () => {
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "search_project_files") return [];
      return null;
    });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "open @zz" } });
    expect(screen.getByTestId("file-options-status")).toHaveTextContent("Searching files...");
    await waitFor(() => {
      expect(screen.getByTestId("file-options-status")).toHaveTextContent("No matching files");
    });
  });

  it("@ file insertion handles replacement markers and backticks in paths", async () => {
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "search_project_files") return ["hello/$&.txt", "weird`name.ts"];
      return null;
    });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "open @hello" } });
    await fireEvent.click(await screen.findByTestId("file-option-hello/$&.txt"));
    expect(textarea.value).toBe("open `hello/$&.txt` ");

    await fireEvent.input(textarea, { target: { value: "open @weird" } });
    await fireEvent.click(await screen.findByTestId("file-option-weird`name.ts"));
    expect(textarea.value).toBe("open ``weird`name.ts`` ");
  });

  it("keeps recipient options visible when file search fails", async () => {
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "search_project_files") throw new Error("project unavailable");
      return null;
    });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "ping @bo" } });

    expect(await screen.findByTestId(`recipient-option-${AGENT_B.id}`)).toBeInTheDocument();
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("search_project_files", {
        projectId: PROJECT_ID,
        query: "bo",
        limit: 12,
      });
    });
    expect(screen.getByTestId("recipient-menu")).toBeInTheDocument();
    expect(screen.getByTestId("file-options-status")).toHaveTextContent("File search unavailable");
    expect(screen.queryByTestId("file-option-stale.ts")).toBeNull();
  });

  it("debounces file search without delaying recipient filtering", async () => {
    vi.useFakeTimers();
    try {
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      await state.registerAgent(AGENT_B);

      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

      const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
      await fireEvent.input(textarea, { target: { value: "ping @b" } });
      expect(screen.getByTestId(`recipient-option-${AGENT_B.id}`)).toBeInTheDocument();
      await fireEvent.input(textarea, { target: { value: "ping @bo" } });

      expect(invokeMock.mock.calls.some(([cmd]) => cmd === "search_project_files")).toBe(false);
      await vi.advanceTimersByTimeAsync(179);
      expect(invokeMock.mock.calls.some(([cmd]) => cmd === "search_project_files")).toBe(false);
      await vi.advanceTimersByTimeAsync(1);

      await waitFor(() => {
        expect(invokeMock).toHaveBeenCalledWith("search_project_files", {
          projectId: PROJECT_ID,
          query: "bo",
          limit: 12,
        });
      });
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "search_project_files")).toHaveLength(
        1,
      );
    } finally {
      vi.useRealTimers();
    }
  });

  it("cancels pending file search when unmounted", async () => {
    vi.useFakeTimers();
    try {
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      await state.registerAgent(AGENT_B);

      const { unmount } = render(ComposeBar, {
        props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] },
      });

      const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
      await fireEvent.input(textarea, { target: { value: "ping @bo" } });
      unmount();
      await vi.advanceTimersByTimeAsync(180);

      expect(invokeMock.mock.calls.some(([cmd]) => cmd === "search_project_files")).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps still-matching file rows visible while the next search is pending", async () => {
    vi.useFakeTimers();
    try {
      invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
        if (cmd !== "search_project_files") return null;
        const query = args?.query;
        if (query === "r") return ["README.md"];
        if (query === "re") return ["README.md", "docs/release-notes.md"];
        return [];
      });
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      await state.registerAgent(AGENT_B);

      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

      const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
      await fireEvent.input(textarea, { target: { value: "open @r" } });
      await vi.advanceTimersByTimeAsync(180);
      expect(await screen.findByTestId("file-option-README.md")).toBeInTheDocument();

      await fireEvent.input(textarea, { target: { value: "open @re" } });
      expect(screen.getByTestId("file-option-README.md")).toBeInTheDocument();
      expect(screen.queryByTestId("file-option-docs/release-notes.md")).toBeNull();

      await vi.advanceTimersByTimeAsync(180);
      expect(await screen.findByTestId("file-option-docs/release-notes.md")).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps the matching agent highlighted when retained file rows stay visible", async () => {
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "search_project_files") return ["docs/bob.md"];
      return null;
    });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "ping @b" } });

    const file = await screen.findByTestId("file-option-docs/bob.md");
    const bob = await screen.findByTestId(`recipient-option-${AGENT_B.id}`);
    expect(file).toHaveAttribute("aria-selected", "false");
    expect(bob).toHaveAttribute("aria-selected", "true");

    await fireEvent.input(textarea, { target: { value: "ping @bo" } });
    expect(file).toHaveAttribute("aria-selected", "false");
    expect(bob).toHaveAttribute("aria-selected", "true");
  });

  it("a bare @ offers All / Clear actions that bulk-select and deselect", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "@" } });
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "search_project_files")).toBe(false);
    expect(screen.queryByText("Files")).toBeNull();

    // All → every agent selected.
    await fireEvent.click(await screen.findByTestId("recipient-option-all"));
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "true");
    expect(textarea.value).toBe(""); // the @ token is stripped

    // Clear → none selected.
    await fireEvent.input(textarea, { target: { value: "@" } });
    await fireEvent.click(await screen.findByTestId("recipient-option-clear"));
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "false");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "false");
  });

  it("hides All when everyone is selected and Clear when no one is", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;

    // Select everyone (alice is default; add bob) → All has nothing to do.
    await fireEvent.keyDown(document.body, { key: "2", metaKey: true });
    await fireEvent.input(textarea, { target: { value: "@" } });
    expect(await screen.findByTestId("recipient-option-clear")).toBeInTheDocument();
    expect(screen.queryByTestId("recipient-option-all")).toBeNull();

    // Clear everyone → Clear has nothing to do.
    await fireEvent.click(screen.getByTestId("recipient-option-clear"));
    await fireEvent.input(textarea, { target: { value: "@" } });
    expect(await screen.findByTestId("recipient-option-all")).toBeInTheDocument();
    expect(screen.queryByTestId("recipient-option-clear")).toBeNull();
  });

  it("Mod+N toggles the Nth agent (sidebar order)", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    // alice (index 0) selected by default; bob not.
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "false");

    // Mod+2 toggles the second agent on; Mod+1 toggles the first off.
    await fireEvent.keyDown(document.body, { key: "2", metaKey: true });
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "true");
    await fireEvent.keyDown(document.body, { key: "1", metaKey: true });
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "false");
  });

  it("Mod+N does not toggle recipients while a dialog (e.g. the command palette) is open", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "false");

    const dialog = document.createElement("div");
    dialog.setAttribute("role", "dialog");
    document.body.appendChild(dialog);

    // The chord would normally select bob; with a dialog open it's suppressed.
    await fireEvent.keyDown(document.body, { key: "2", metaKey: true });
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "false");

    dialog.remove();
  });

  it("Mod+Shift+A selects every agent", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    await fireEvent.keyDown(document.body, { key: "a", metaKey: true, shiftKey: true });
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "true");
  });

  it("Mod+K focuses the message box from outside the composer", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    const outside = document.createElement("button");
    document.body.appendChild(outside);
    outside.focus();
    expect(document.activeElement).toBe(outside);

    await fireEvent.keyDown(outside, { key: "k", metaKey: true });

    expect(screen.getByTestId("compose-textarea")).toHaveFocus();
    outside.remove();
  });

  it("Mod+K does not steal focus from another editable field", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    const input = document.createElement("input");
    document.body.appendChild(input);
    input.focus();
    expect(document.activeElement).toBe(input);

    await fireEvent.keyDown(input, { key: "k", metaKey: true });

    expect(input).toHaveFocus();
    input.remove();
  });

  it("Mod+K does not focus the message box behind an alert dialog", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    const alertDialog = document.createElement("div");
    alertDialog.setAttribute("role", "alertdialog");
    const dialogButton = document.createElement("button");
    alertDialog.appendChild(dialogButton);
    document.body.appendChild(alertDialog);
    dialogButton.focus();
    expect(dialogButton).toHaveFocus();

    await fireEvent.keyDown(dialogButton, { key: "k", metaKey: true });

    expect(dialogButton).toHaveFocus();
    expect(screen.getByTestId("compose-textarea")).not.toHaveFocus();
    alertDialog.remove();
  });

  it("fans one message out to all selected recipients sharing one send_id", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    invokeMock.mockResolvedValue("msg-x");

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    await fireEvent.click(chip(AGENT_B.id)); // select both

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "status?" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => {
      const calls = invokeMock.mock.calls.filter(([c]) => c === "send_message");
      expect(calls).toHaveLength(2);
    });
    const calls = invokeMock.mock.calls.filter(([c]) => c === "send_message");
    const agentIds = calls.map((c) => (c[1] as { agentId: string }).agentId).sort();
    expect(agentIds).toEqual([AGENT_A.id, AGENT_B.id].sort());
    const sendIds = new Set(calls.map((c) => (c[1] as { sendId: string }).sendId));
    expect(sendIds.size).toBe(1);
    expect((state.transcripts[AGENT_A.id] ?? []).length).toBe(1);
    expect((state.transcripts[AGENT_B.id] ?? []).length).toBe(1);
  });

  it("stamps project activity when a message is sent", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-05-25T12:00:00Z"));
    try {
      const state = await loadState();
      const ws = await loadWorkspace();
      await state.registerAgent(AGENT_A);
      ws.projects.list = [
        {
          id: PROJECT_ID,
          name: "project",
          created_at: "2026-05-16T00:00:00Z",
          directory: "/work/project",
          available: true,
          last_activity: "2026-05-16T00:00:00Z",
          archived: false,
        },
      ];
      invokeMock.mockResolvedValue("msg-1");

      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

      const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
      await fireEvent.input(textarea, { target: { value: "status?" } });
      await fireEvent.click(screen.getByTestId("compose-send"));

      expect(ws.projectActivityOverrides[PROJECT_ID]).toBe("2026-05-25T12:00:00.000Z");
      expect(ws.projects.list[0]).toMatchObject({
        id: PROJECT_ID,
        last_activity: "2026-05-25T12:00:00.000Z",
      });
    } finally {
      vi.useRealTimers();
    }
  });

  it("turns the empty-draft send button into cancel for the latest live send", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    invokeMock.mockResolvedValue("msg-x");

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    await fireEvent.click(chip(AGENT_B.id));
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "status?" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => {
      expect(screen.getByTestId("compose-send")).toHaveAttribute("aria-label", "Cancel send");
    });
    await fireEvent.click(screen.getByTestId("compose-send"));

    const cancelCall = invokeMock.mock.calls.find(([cmd]) => cmd === "cancel_send");
    expect(cancelCall?.[1]).toMatchObject({
      recipients: expect.arrayContaining([AGENT_A.id, AGENT_B.id]),
    });
  });

  it("the empty-draft stop cancels ALL live sends, not just the latest", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    invokeMock.mockResolvedValue("msg-x");

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;

    // Send #1 to alice (default selected).
    await fireEvent.input(textarea, { target: { value: "to alice" } });
    await fireEvent.click(screen.getByTestId("compose-send"));
    // Send #2 to bob only (toggle alice off, bob on).
    await fireEvent.click(chip(AGENT_A.id));
    await fireEvent.click(chip(AGENT_B.id));
    await fireEvent.input(textarea, { target: { value: "to bob" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    // Two distinct sends are now live → the stop affordance covers all of them.
    await waitFor(() => {
      expect(screen.getByTestId("compose-send")).toHaveAttribute("aria-label", "Cancel all sends");
    });
    await fireEvent.click(screen.getByTestId("compose-send"));

    const cancelCalls = invokeMock.mock.calls.filter(([cmd]) => cmd === "cancel_send");
    const cancelledSendIds = new Set(cancelCalls.map((c) => (c[1] as { sendId: string }).sendId));
    expect(cancelledSendIds.size).toBe(2); // both sends cancelled, not just the last
    const cancelledRecipients = cancelCalls.flatMap(
      (c) => (c[1] as { recipients: string[] }).recipients,
    );
    expect(cancelledRecipients).toEqual(expect.arrayContaining([AGENT_A.id, AGENT_B.id]));
  });

  it("uses Mod+Enter to cancel when the empty-draft send button is in stop mode", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockResolvedValue("msg-1");

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "first" } });
    await fireEvent.click(screen.getByTestId("compose-send"));
    await waitFor(() => {
      expect(screen.getByTestId("compose-send")).toHaveAttribute("aria-label", "Cancel send");
    });
    await fireEvent.keyDown(textarea, { key: "Enter", metaKey: true });

    const sendCalls = invokeMock.mock.calls.filter(([cmd]) => cmd === "send_message");
    const cancelCall = invokeMock.mock.calls.find(([cmd]) => cmd === "cancel_send");
    expect(sendCalls).toHaveLength(1);
    expect(cancelCall?.[1]).toMatchObject({ recipients: [AGENT_A.id] });
  });

  it("send-while-busy is un-gated: Send stays enabled while a recipient is processing", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockResolvedValue("msg-1");

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "first" } });
    await fireEvent.click(screen.getByTestId("compose-send"));
    fireTo(`agent:${AGENT_A.id}`, {
      type: "turn_start",
      turn_id: "turn-1",
      message_id: "msg-1",
      send_id: "msg-1",
      started_at: "2026-05-16T00:00:00Z",
    });
    await waitFor(() => expect(state.runtimes[AGENT_A.id]?.run_status).toBe("processing"));

    await fireEvent.input(textarea, { target: { value: "second" } });
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(false);
    expect(screen.getByTestId("compose-send")).toHaveAttribute("aria-label", "Send");
  });

  it("a per-recipient IPC failure fails only that recipient and surfaces the error", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    // Dispatch order is selection order: alice (default) then bob.
    invokeMock.mockResolvedValueOnce("msg-a").mockRejectedValueOnce(new Error("bob exploded"));

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await fireEvent.click(chip(AGENT_B.id));

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "go" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => {
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent("bob exploded");
    });
    expect(state.runtimes[AGENT_B.id]?.run_status).toBe("idle");
    expect(state.runtimes[AGENT_A.id]?.run_status).toBe("starting");
    // alice is still pending → just her user turn; bob's failure surfaces as a
    // failed agent turn beneath his user turn.
    expect((state.transcripts[AGENT_A.id] ?? []).length).toBe(1);
    const bobTurns = state.transcripts[AGENT_B.id] ?? [];
    expect(bobTurns.length).toBe(2);
    const bobFailed = bobTurns[1];
    expect(bobFailed?.role === "agent" ? bobFailed.status : null).toBe("failed");
  });

  it("clears the prompt on submit but keeps the recipients selected (sticky)", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockResolvedValue("msg-1");

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "hi" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => expect(textarea.value).toBe(""));
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");
  });

  it("Clear and Escape (with composer focus) both deselect all recipients", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");

    await fireEvent.click(screen.getByTestId("recipient-clear"));
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "false");

    // Re-select, then clear via Escape while the composer holds focus.
    await fireEvent.click(chip(AGENT_A.id));
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    textarea.focus();
    await fireEvent.keyDown(window, { key: "Escape" });
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "false");
  });

  it("Escape is a no-op when focus is outside the composer", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");

    // Focus an element outside the compose surface; Escape must not clear the
    // recipients (Escape is overloaded across the app and only owns the
    // composer's selection while the composer has focus).
    const outside = document.createElement("button");
    document.body.appendChild(outside);
    outside.focus();
    await fireEvent.keyDown(window, { key: "Escape" });
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");
    outside.remove();
  });
});

describe("ComposeBar persistence", () => {
  it("retains draft and recipient selection across a project-switch remount", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    const first = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] },
    });
    await fireEvent.input(screen.getByTestId("compose-textarea"), {
      target: { value: "half-written" },
    });
    await fireEvent.click(chip(AGENT_B.id)); // alice (default) + bob
    first.unmount();

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    expect((screen.getByTestId("compose-textarea") as HTMLTextAreaElement).value).toBe(
      "half-written",
    );
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "true");
  });

  it("restores draft and selection persisted by a previous session (restart)", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    const store = await loadComposeStore();
    store.setContent(PROJECT_ID, { kind: "plain", draft: "from last time" });
    store.setSelection(PROJECT_ID, [AGENT_B.id]);
    store.flush();
    store._testing.reloadFromStorage(); // drop in-memory copy; re-read localStorage

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    expect((screen.getByTestId("compose-textarea") as HTMLTextAreaElement).value).toBe(
      "from last time",
    );
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "true");
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "false");
  });

  it("clears the persisted draft on send so it can't reappear next time", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockResolvedValue("msg-1");
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "send me" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => expect(textarea.value).toBe(""));
    const store = await loadComposeStore();
    const content = store.getCompose(PROJECT_ID).content;
    expect(content).toEqual({ kind: "plain", draft: "" });
  });

  it("persists a deliberate deselect-all and restores it as empty (not the default)", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    const first = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] },
    });
    await fireEvent.click(screen.getByTestId("recipient-clear"));
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "false");
    const store = await loadComposeStore();
    expect(store.getCompose(PROJECT_ID).selectedIds).toEqual([]);
    first.unmount();

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "false");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "false");
  });

  it("drops a saved recipient whose agent no longer exists on restore", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    const store = await loadComposeStore();
    store.setSelection(PROJECT_ID, ["00000000-0000-7000-8000-00000000dead", AGENT_A.id]);
    store.flush();
    store._testing.reloadFromStorage();

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "false");
    // The ghost id is pruned from the persisted set too (init re-persists).
    expect(store.getCompose(PROJECT_ID).selectedIds).toEqual([AGENT_A.id]);
  });

  it("keeps drafts isolated per project", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const OTHER_PROJECT = "00000000-0000-7000-8000-0000000000ee";

    const first = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await fireEvent.input(screen.getByTestId("compose-textarea"), {
      target: { value: "project one's draft" },
    });
    first.unmount();

    render(ComposeBar, { props: { projectId: OTHER_PROJECT, agents: [AGENT_A] } });
    expect((screen.getByTestId("compose-textarea") as HTMLTextAreaElement).value).toBe("");
  });

  it("a draft typed right before a project switch survives via the destroy flush", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const store = await loadComposeStore();

    const first = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await fireEvent.input(screen.getByTestId("compose-textarea"), {
      target: { value: "mid-debounce draft" },
    });
    // Unmount (the project-switch path): the deferred draft write must land in
    // onDestroy — reloadFromStorage then drops memory so only disk survives.
    first.unmount();
    store._testing.reloadFromStorage();

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    expect((screen.getByTestId("compose-textarea") as HTMLTextAreaElement).value).toBe(
      "mid-debounce draft",
    );
  });

  it("a fast project switch keeps each project's draft in its own slot", async () => {
    // The deferral's worst case: type in project 1, switch to project 2, and
    // restart before any timer fires. The forced disk round-trip in the middle
    // is what makes this a real test — without it, the in-memory store
    // satisfies the assertions even with the flush points deleted.
    const OTHER_PROJECT = "00000000-0000-7000-8000-0000000000ee";
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const store = await loadComposeStore();

    const first = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await fireEvent.input(screen.getByTestId("compose-textarea"), {
      target: { value: "project one draft" },
    });
    first.unmount();

    const second = render(ComposeBar, {
      props: { projectId: OTHER_PROJECT, agents: [AGENT_A] },
    });
    second.unmount();
    store._testing.reloadFromStorage();

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    expect((screen.getByTestId("compose-textarea") as HTMLTextAreaElement).value).toBe(
      "project one draft",
    );
    // Project 1's draft never leaked into project 2's slot.
    expect(store.getCompose(OTHER_PROJECT).content).toEqual({ kind: "plain", draft: "" });
  });

  it("recovers a single-agent project from a saved selection whose agent is gone", async () => {
    // Saved "send to bob" against a project that now has only alice: bob is
    // filtered out, and a single-agent project shows no chips — so without the
    // single-agent guard the composer would be unsendable with no recovery UI.
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockResolvedValue("msg-1");
    const store = await loadComposeStore();
    store.setSelection(PROJECT_ID, [AGENT_B.id]);
    store.flush();
    store._testing.reloadFromStorage();

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    expect(screen.queryByTestId("recipient-field")).toBeNull(); // no chips for one agent
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "hi" } });
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(false);
    await fireEvent.click(screen.getByTestId("compose-send"));
    await waitFor(() => {
      const calls = invokeMock.mock.calls.filter(([c]) => c === "send_message");
      expect(calls).toHaveLength(1);
      expect(calls[0]?.[1]).toMatchObject({ agentId: AGENT_A.id });
    });
  });

  it("falls back to the default when a saved multi-agent selection is all stale", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    const store = await loadComposeStore();
    store.setSelection(PROJECT_ID, [
      "00000000-0000-7000-8000-00000000dea1",
      "00000000-0000-7000-8000-00000000dea2",
    ]);
    store.flush();
    store._testing.reloadFromStorage();

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    // All saved ids are gone → default to the first agent rather than empty.
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "false");
  });

  it("a transient empty roster does not clobber the saved selection", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    const store = await loadComposeStore();

    const { rerender } = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] },
    });
    await fireEvent.click(chip(AGENT_B.id)); // persist alice + bob
    expect(store.getCompose(PROJECT_ID).selectedIds).toEqual([AGENT_A.id, AGENT_B.id]);

    await rerender({ projectId: PROJECT_ID, agents: [] });
    // The roster-gated write must skip the empty roster, leaving the save intact.
    expect(store.getCompose(PROJECT_ID).selectedIds).toEqual([AGENT_A.id, AGENT_B.id]);
  });
});

const REVIEW: Prompt = {
  provider: "local",
  name: "review",
  title: null,
  description: "Review a diff",
  arguments: [{ name: "focus", description: "What to focus on", required: true }],
  tags: [],
};
const SUMMARY: Prompt = {
  provider: "tiddly",
  name: "summary",
  title: "Summary",
  description: null,
  arguments: [],
  tags: [],
};

/// Route invoke per command for the prompt-mode flow. `render` lets a test
/// substitute a rejection or a deferred gate for `render_prompt`.
function mockPromptBackend(
  opts: {
    prompts?: Prompt[];
    render?: () => Promise<{ text: string }>;
  } = {},
): void {
  invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
    if (cmd === "search_project_files") return [];
    if (cmd === "list_prompts") return opts.prompts ?? [];
    if (cmd === "render_prompt") return opts.render ? await opts.render() : { text: "RENDERED" };
    if (cmd === "send_message") return "msg-id";
    return null;
  });
}

async function enterPromptMode(testId: string): Promise<void> {
  await fireEvent.click(screen.getByTestId("compose-prompt-button"));
  await waitFor(() => expect(screen.getByTestId(testId)).toBeInTheDocument());
  await fireEvent.click(screen.getByTestId(testId));
  await waitFor(() => expect(screen.getByTestId("prompt-composer")).toBeInTheDocument());
}

describe("ComposeBar prompt mode", () => {
  it("opens the prompt picker from the cache without a render (network) call", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    mockPromptBackend({ prompts: [REVIEW, SUMMARY] });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    await fireEvent.click(screen.getByTestId("compose-prompt-button"));
    await waitFor(() =>
      expect(screen.getByTestId("prompt-option-local:review")).toBeInTheDocument(),
    );
    expect(invokeMock.mock.calls.some(([c]) => c === "list_prompts")).toBe(true);
    expect(invokeMock.mock.calls.some(([c]) => c === "render_prompt")).toBe(false);
  });

  it("pre-fills appended text from the textarea when entering prompt mode", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    mockPromptBackend({ prompts: [SUMMARY] });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    await fireEvent.input(screen.getByTestId("compose-textarea"), {
      target: { value: "carried text" },
    });
    await enterPromptMode("prompt-option-tiddly:summary");
    expect((screen.getByTestId("prompt-appended") as HTMLTextAreaElement).value).toBe(
      "carried text",
    );
  });

  it("blocks send until required arguments are filled", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    mockPromptBackend({ prompts: [REVIEW] });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    await enterPromptMode("prompt-option-local:review");
    const focusArg = screen.getByTestId("prompt-arg-focus");
    expect(focusArg).toHaveFocus();
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(true);
    await fireEvent.input(focusArg, { target: { value: "tests" } });
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(false);
    await fireEvent.keyDown(focusArg, { key: "Enter", metaKey: true });

    await waitFor(() => {
      const sends = invokeMock.mock.calls.filter(([c]) => c === "send_message");
      expect(sends).toHaveLength(1);
    });
  });

  it("returns to the plain composer carrying appended text back on remove", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    mockPromptBackend({ prompts: [SUMMARY] });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    await fireEvent.input(screen.getByTestId("compose-textarea"), { target: { value: "keep me" } });
    await enterPromptMode("prompt-option-tiddly:summary");
    await fireEvent.click(screen.getByTestId("prompt-remove"));

    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    expect((screen.getByTestId("compose-textarea") as HTMLTextAreaElement).value).toBe("keep me");
    expect(screen.queryByTestId("prompt-composer")).toBeNull();
  });

  it("renders once and fans the combined message out to all recipients", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    mockPromptBackend({ prompts: [SUMMARY] });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    await fireEvent.click(chip(AGENT_B.id)); // select both
    await enterPromptMode("prompt-option-tiddly:summary");
    await fireEvent.input(screen.getByTestId("prompt-appended"), { target: { value: "tail" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => {
      const sends = invokeMock.mock.calls.filter(([c]) => c === "send_message");
      expect(sends).toHaveLength(2);
    });
    const renders = invokeMock.mock.calls.filter(([c]) => c === "render_prompt");
    expect(renders).toHaveLength(1); // rendered ONCE, not per recipient
    const sends = invokeMock.mock.calls.filter(([c]) => c === "send_message");
    for (const call of sends) {
      expect((call[1] as { prompt: string }).prompt).toBe("RENDERED\n\ntail");
    }
    const sendIds = new Set(sends.map((c) => (c[1] as { sendId: string }).sendId));
    expect(sendIds.size).toBe(1);
    expect((state.transcripts[AGENT_A.id] ?? [])[0]).toMatchObject({ text: "RENDERED\n\ntail" });
  });

  it("a render failure at send surfaces an error, keeps state, and writes no turn", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    mockPromptBackend({
      prompts: [SUMMARY],
      render: () => Promise.reject(new Error("render boom")),
    });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    await enterPromptMode("prompt-option-tiddly:summary");
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() =>
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent("render boom"),
    );
    // Composer state preserved; no optimistic turn, no send.
    expect(screen.getByTestId("prompt-composer")).toBeInTheDocument();
    expect((state.transcripts[AGENT_A.id] ?? []).length).toBe(0);
    expect(invokeMock.mock.calls.some(([c]) => c === "send_message")).toBe(false);
  });

  it("shows a pending, disabled send while the render is in flight, then dispatches", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    let release!: (v: { text: string }) => void;
    const gate = new Promise<{ text: string }>((res) => {
      release = res;
    });
    mockPromptBackend({ prompts: [SUMMARY], render: () => gate });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    await fireEvent.click(chip(AGENT_B.id));
    await enterPromptMode("prompt-option-tiddly:summary");
    await fireEvent.click(screen.getByTestId("compose-send"));

    // Render is awaiting: controls whose values were snapshotted are locked and
    // no dispatch happens until the MCP render returns.
    await waitFor(() =>
      expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(true),
    );
    expect(screen.getByTestId("prompt-rendering")).toHaveTextContent("Rendering prompt");
    expect(screen.getByTestId("prompt-rendering").querySelector(".animate-spin")).not.toBeNull();
    expect((screen.getByTestId("prompt-appended") as HTMLTextAreaElement).disabled).toBe(true);
    expect((screen.getByTestId("prompt-preview-button") as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByTestId("prompt-remove") as HTMLButtonElement).disabled).toBe(true);
    expect((chip(AGENT_A.id) as HTMLButtonElement).disabled).toBe(true);
    expect((chip(AGENT_B.id) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByTestId("recipient-clear") as HTMLButtonElement).disabled).toBe(true);
    await fireEvent.keyDown(window, { key: "2", metaKey: true });
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "true");
    expect(invokeMock.mock.calls.some(([c]) => c === "send_message")).toBe(false);

    release({ text: "DONE" });
    await waitFor(() => {
      const sends = invokeMock.mock.calls.filter(([c]) => c === "send_message");
      expect(sends).toHaveLength(2);
    });
    // Successful send returns to the plain composer.
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    expect((state.transcripts[AGENT_A.id] ?? [])[0]).toMatchObject({ text: "DONE" });
    expect((state.transcripts[AGENT_B.id] ?? [])[0]).toMatchObject({ text: "DONE" });
  });

  it("restores a persisted prompt-mode draft (prompt, args, appended text)", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const store = await loadComposeStore();
    store.setContent(PROJECT_ID, {
      kind: "prompt",
      provider: "local",
      name: "review",
      args: { focus: "saved focus" },
      appendedText: "saved tail",
    });
    store.flush();
    store._testing.reloadFromStorage();
    mockPromptBackend({ prompts: [REVIEW] });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    await waitFor(() => expect(screen.getByTestId("prompt-composer")).toBeInTheDocument());
    expect((screen.getByTestId("prompt-arg-focus") as HTMLTextAreaElement).value).toBe(
      "saved focus",
    );
    expect((screen.getByTestId("prompt-appended") as HTMLTextAreaElement).value).toBe("saved tail");
  });

  it("keeps a saved prompt draft pending on a cold cache, then restores it after sync", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const store = await loadComposeStore();
    store.setContent(PROJECT_ID, {
      kind: "prompt",
      provider: "local",
      name: "review",
      args: { focus: "saved focus" },
      appendedText: "tail",
    });
    store.flush();
    store._testing.reloadFromStorage();

    let promptList: Prompt[] = []; // cache cold at mount (MCP not synced yet)
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "search_project_files") return [];
      if (cmd === "list_prompts") return promptList;
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    // Cold: shows the restoring placeholder (not plain), and must NOT clobber the
    // saved snapshot in storage.
    await waitFor(() => expect(screen.getByTestId("compose-restoring")).toBeInTheDocument());
    expect(screen.queryByTestId("prompt-composer")).toBeNull();
    expect(store.getCompose(PROJECT_ID).content).toMatchObject({ kind: "prompt", name: "review" });

    // Sync completes with the prompt present → restore with args intact.
    promptList = [REVIEW];
    listeners.get("prompts:synced")?.({ payload: null as unknown as NormalizedEvent });

    await waitFor(() => expect(screen.getByTestId("prompt-composer")).toBeInTheDocument());
    expect((screen.getByTestId("prompt-arg-focus") as HTMLTextAreaElement).value).toBe(
      "saved focus",
    );
    expect((screen.getByTestId("prompt-appended") as HTMLTextAreaElement).value).toBe("tail");
  });

  it("downgrades a saved prompt draft to plain (carrying appended text) once a sync proves it gone", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const store = await loadComposeStore();
    store.setContent(PROJECT_ID, {
      kind: "prompt",
      provider: "tiddly",
      name: "ghost",
      args: { focus: "x" },
      appendedText: "leftover text",
    });
    store.flush();
    store._testing.reloadFromStorage();
    mockPromptBackend({ prompts: [] }); // prompt never appears, even after sync

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    await waitFor(() => expect(screen.getByTestId("compose-restoring")).toBeInTheDocument());
    listeners.get("prompts:synced")?.({ payload: null as unknown as NormalizedEvent });

    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    expect((screen.getByTestId("compose-textarea") as HTMLTextAreaElement).value).toBe(
      "leftover text",
    );
  });

  it("keeps prompt removal locked while the send render is in flight", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    let release!: (v: { text: string }) => void;
    const gate = new Promise<{ text: string }>((res) => {
      release = res;
    });
    mockPromptBackend({ prompts: [SUMMARY], render: () => gate });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    await enterPromptMode("prompt-option-tiddly:summary");
    await fireEvent.click(screen.getByTestId("compose-send"));
    expect((screen.getByTestId("prompt-remove") as HTMLButtonElement).disabled).toBe(true);
    await fireEvent.click(screen.getByTestId("prompt-remove"));
    release({ text: "DONE" });
    await waitFor(() =>
      expect(invokeMock.mock.calls.some(([c]) => c === "send_message")).toBe(true),
    );

    expect((state.transcripts[AGENT_A.id] ?? [])[0]).toMatchObject({ text: "DONE" });
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
  });

  it("keeps recipients locked while the send render is in flight", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    let release!: (v: { text: string }) => void;
    const gate = new Promise<{ text: string }>((res) => {
      release = res;
    });
    mockPromptBackend({ prompts: [SUMMARY], render: () => gate });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    await fireEvent.click(chip(AGENT_B.id)); // select both A + B
    await enterPromptMode("prompt-option-tiddly:summary");
    await fireEvent.click(screen.getByTestId("compose-send"));
    expect((chip(AGENT_A.id) as HTMLButtonElement).disabled).toBe(true);
    await fireEvent.click(chip(AGENT_A.id));
    release({ text: "DONE" });
    await waitFor(() => {
      const sends = invokeMock.mock.calls.filter(([c]) => c === "send_message");
      expect(sends).toHaveLength(2);
    });

    expect((state.transcripts[AGENT_A.id] ?? [])[0]).toMatchObject({ text: "DONE" });
    expect((state.transcripts[AGENT_B.id] ?? [])[0]).toMatchObject({ text: "DONE" });
  });

  it("stamps project activity on a prompt send", async () => {
    const state = await loadState();
    const ws = await loadWorkspace();
    await state.registerAgent(AGENT_A);
    ws.projects.list = [
      {
        id: PROJECT_ID,
        name: "project",
        created_at: "2026-05-16T00:00:00Z",
        directory: "/work/project",
        available: true,
        last_activity: "2026-05-16T00:00:00Z",
        archived: false,
      },
    ];
    mockPromptBackend({ prompts: [SUMMARY] });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    await enterPromptMode("prompt-option-tiddly:summary");
    await fireEvent.click(screen.getByTestId("compose-send"));
    await waitFor(() =>
      expect(invokeMock.mock.calls.some(([c]) => c === "send_message")).toBe(true),
    );
    // The shared dispatch path stamps activity once for the prompt send too.
    expect(ws.projectActivityOverrides[PROJECT_ID]).toBeDefined();
  });
});

describe("ComposeBar — attachments", () => {
  it("stages dropped files and renders a labeled chip per file (by extension)", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    fireDrop(["/a/diagram.png", "/a/notes.txt", "/a/data.bin", "/a/shot.jpg"]);

    await waitFor(() => {
      expect(screen.getByTestId("attachment-chip-image-1")).toBeInTheDocument();
      expect(screen.getByTestId("attachment-chip-text-1")).toBeInTheDocument();
      expect(screen.getByTestId("attachment-chip-file-1")).toBeInTheDocument();
      expect(screen.getByTestId("attachment-chip-image-2")).toBeInTheDocument();
    });
    // The staged command was called once per dropped path.
    expect(invokeMock.mock.calls.filter(([c]) => c === "stage_attachment")).toHaveLength(4);
  });

  it("removing a chip does not renumber the survivors", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    fireDrop(["/a/one.png"]);
    fireDrop(["/a/two.png"]);
    await screen.findByTestId("attachment-chip-image-2");

    await fireEvent.click(screen.getByTestId("attachment-chip-remove-image-1"));

    // image-2 stays image-2 (no renumber); image-1 is gone.
    expect(screen.queryByTestId("attachment-chip-image-1")).toBeNull();
    expect(screen.getByTestId("attachment-chip-image-2")).toBeInTheDocument();
  });

  it("inserts a chip's reference token from the @ menu", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    fireDrop(["/a/diagram.png"]);
    await screen.findByTestId("attachment-chip-image-1");

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "look at @image" } });
    await fireEvent.click(await screen.findByTestId("attachment-option-image-1"));

    expect(textarea.value).toContain("`image-1`");
  });

  it("sends the attachment list with the clean text and clears chips on success", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    fireDrop(["/a/diagram.png"]);
    await screen.findByTestId("attachment-chip-image-1");
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "compare this" } });
    invokeMock.mockResolvedValueOnce("msg-1"); // the send_message receipt
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => {
      const calls = invokeMock.mock.calls.filter(([c]) => c === "send_message");
      expect(calls).toHaveLength(1);
      const args = calls[0]?.[1] as { prompt?: string; attachments?: unknown[] };
      expect(args.prompt).toBe("compare this");
      expect(args.attachments).toHaveLength(1);
      expect(args.attachments?.[0]).toMatchObject({ label: "image-1", kind: "image" });
    });
    // Chips clear with the text on a send.
    await waitFor(() => expect(screen.queryByTestId("attachment-chips")).toBeNull());
  });

  it("can send attachments with empty text", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    fireDrop(["/a/diagram.png"]);
    await screen.findByTestId("attachment-chip-image-1");
    // No text typed — the send button is enabled purely by the attachment.
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(false);
  });
});

describe("ComposeBar — attachment lifecycle", () => {
  it("discards a staging result that resolves after the message was sent", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    // Gate stage_attachment so the drop's copy is still in flight at send time.
    let releaseStage: (() => void) | undefined;
    const staged = new Promise<void>((r) => (releaseStage = r));
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "stage_attachment") {
        await staged;
        return { path: "/p/attachments/uuid__late.png", original_name: "late.png" };
      }
      if (cmd === "send_message") return "msg-1";
      return null;
    });

    fireDrop(["/a/late.png"]);
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "go" } });
    await fireEvent.click(screen.getByTestId("compose-send"));
    await waitFor(() =>
      expect(invokeMock.mock.calls.some(([c]) => c === "send_message")).toBe(true),
    );

    // The staging finishes only now — after the send cleared the composer. Its
    // chip must NOT resurrect into the next compose session.
    releaseStage?.();
    await tick();
    await tick();
    expect(screen.queryByTestId("attachment-chip-image-1")).toBeNull();
  });

  it("unregisters the drag-drop listener even when it resolves after unmount", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const { unmount } = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    // Unmount before the subscription promise resolves (the leak-prone race).
    unmount();
    resolveDropSub?.();
    await Promise.resolve();
    await Promise.resolve();

    expect(dropUnlisten).toHaveBeenCalledTimes(1);
  });

  it("shows an error and adds no chip when staging rejects", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "stage_attachment") throw new Error("disk full");
      return null;
    });

    fireDrop(["/a/diagram.png"]);

    const err = await screen.findByTestId("compose-send-error");
    // A staging failure reads as an attach error, not a misleading "Send failed".
    expect(err.textContent).toContain("Couldn't attach");
    expect(err.textContent).toContain("diagram.png");
    expect(screen.queryByTestId("attachment-chip-image-1")).toBeNull();
  });
});

describe("ComposeBar pane targeting", () => {
  const ROSTER = [AGENT_A.id, AGENT_B.id];

  async function importPanes() {
    return await import("$lib/state/transcriptPanes.svelte");
  }
  async function importSelection() {
    return await import("$lib/state/recipientSelection.svelte");
  }

  async function renderTwoAgents(): Promise<HTMLTextAreaElement> {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    return screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
  }

  it("offers no pane entries in the @ menu with the single default pane", async () => {
    const textarea = await renderTwoAgents();
    await fireEvent.input(textarea, { target: { value: "@pane" } });
    await screen.findByTestId("recipient-menu");
    expect(screen.queryByTestId(/^recipient-option-pane:/)).not.toBeInTheDocument();
  });

  it("@panename targets the pane with replace semantics once split", async () => {
    const panes = await importPanes();
    const selection = await importSelection();
    const paneId = panes.moveAgentToNewPane(PROJECT_ID, ROSTER, AGENT_B.id);
    panes.renamePane(PROJECT_ID, ROSTER, paneId, "reviewers");

    const textarea = await renderTwoAgents();
    // Start from a different selection so replace (not add) is observable.
    selection.setRecipients(PROJECT_ID, [AGENT_A.id]);

    await fireEvent.input(textarea, { target: { value: "@review" } });
    const option = await screen.findByTestId(`recipient-option-pane:${paneId}`);
    // The entry spells out the pane's member names, not a count.
    expect(within(option).getByTestId("pane-option-members")).toHaveTextContent("bob");
    await fireEvent.click(option);

    expect(selection.selectionFor(PROJECT_ID)).toEqual([AGENT_B.id]);
    // The @-token is consumed like an agent pick.
    expect(textarea.value).toBe("");
  });

  it("@panename reveals a minimized pane when targeting it", async () => {
    const panes = await importPanes();
    const selection = await importSelection();
    const paneId = panes.moveAgentToNewPane(PROJECT_ID, ROSTER, AGENT_B.id);
    panes.renamePane(PROJECT_ID, ROSTER, paneId, "reviewers");
    panes.minimizePane(PROJECT_ID, ROSTER, paneId);

    const textarea = await renderTwoAgents();
    await fireEvent.input(textarea, { target: { value: "@review" } });
    const option = await screen.findByTestId(`recipient-option-pane:${paneId}`);
    await fireEvent.click(option);

    // Targeting both retargets and reveals — a send must never stream into a
    // pane the user cannot see.
    expect(selection.selectionFor(PROJECT_ID)).toEqual([AGENT_B.id]);
    expect(panes.layoutFor(PROJECT_ID, ROSTER).minimized).toEqual([]);
  });

  it("@panename is fully inert while targeting is locked — no retarget, no reveal", async () => {
    const panes = await importPanes();
    const selection = await importSelection();
    const paneId = panes.moveAgentToNewPane(PROJECT_ID, ROSTER, AGENT_B.id);
    panes.renamePane(PROJECT_ID, ROSTER, paneId, "reviewers");
    panes.minimizePane(PROJECT_ID, ROSTER, paneId);

    const textarea = await renderTwoAgents();
    selection.setRecipients(PROJECT_ID, [AGENT_A.id]);
    selection.setTargetingLocked(PROJECT_ID, true);

    await fireEvent.input(textarea, { target: { value: "@review" } });
    const option = await screen.findByTestId(`recipient-option-pane:${paneId}`);
    await fireEvent.click(option);

    // The gesture is atomic under the prompt-render lock: the refused target
    // write must not leave a revealed pane implying it became the target.
    expect(selection.selectionFor(PROJECT_ID)).toEqual([AGENT_A.id]);
    expect(panes.layoutFor(PROJECT_ID, ROSTER).minimized).toEqual([paneId]);
    // The @-token is still consumed — refusal is silent, like every other
    // lock-refused gesture.
    expect(textarea.value).toBe("");

    selection.setTargetingLocked(PROJECT_ID, false);
  });

  it("pane entries list ahead of agent entries", async () => {
    const panes = await importPanes();
    panes.moveAgentToNewPane(PROJECT_ID, ROSTER, AGENT_B.id);
    const textarea = await renderTwoAgents();

    await fireEvent.input(textarea, { target: { value: "@" } });
    const menu = await screen.findByTestId("recipient-menu");
    const keys = Array.from(menu.querySelectorAll('[data-testid^="recipient-option-"]')).map((el) =>
      el.getAttribute("data-testid"),
    );
    const firstPane = keys.findIndex((k) => k?.startsWith("recipient-option-pane:"));
    const firstAgent = keys.findIndex((k) => k === `recipient-option-${AGENT_A.id}`);
    expect(firstPane).toBeGreaterThanOrEqual(0);
    expect(firstAgent).toBeGreaterThan(firstPane);
  });

  it("marks a recipient chip only when targeted AND hidden", async () => {
    const panes = await importPanes();
    const selection = await importSelection();
    await renderTwoAgents();

    // Hidden but UNSELECTED (the default selection is alice): no hazard, so
    // no warning — a cue that fires without a hazard trains users to ignore it.
    panes.toggleAgentHidden(PROJECT_ID, ROSTER, AGENT_B.id);
    await waitFor(() => {
      expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "false");
    });
    expect(screen.queryByTestId(`recipient-hidden-cue-${AGENT_B.id}`)).not.toBeInTheDocument();
    expect(chip(AGENT_B.id)).not.toHaveAttribute("data-hidden-recipient");

    // Selecting the hidden agent makes it targeted-but-hidden: cue appears.
    selection.setRecipients(PROJECT_ID, [AGENT_A.id, AGENT_B.id]);
    await waitFor(() => {
      expect(screen.getByTestId(`recipient-hidden-cue-${AGENT_B.id}`)).toBeInTheDocument();
      expect(chip(AGENT_B.id)).toHaveAttribute("data-hidden-recipient", "true");
    });

    // Revealing the agent clears the cue while it stays selected.
    panes.showAllAgents(PROJECT_ID, ROSTER);
    await waitFor(() => {
      expect(screen.queryByTestId(`recipient-hidden-cue-${AGENT_B.id}`)).not.toBeInTheDocument();
    });
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "true");
  });

  it("never accents the compose box for pane targeting (the dock treatment was removed)", async () => {
    const panes = await importPanes();
    const selection = await importSelection();
    panes.moveAgentToNewPane(PROJECT_ID, ROSTER, AGENT_B.id);

    await renderTwoAgents();

    // Even the exact-pane match that used to trigger the dock leaves the
    // compose box neutral — the pane's own coverage ring is the one
    // targeting visual.
    selection.setRecipients(PROJECT_ID, [AGENT_B.id]);
    await Promise.resolve();
    expect(screen.getByTestId("compose-box")).not.toHaveAttribute("data-docked-pane");
    expect(screen.getByTestId("compose-box").className).not.toContain("border-accent");
  });

  it("an external pane-targeting write flows into the chips and persists", async () => {
    const selection = await importSelection();
    const composeStore = await loadComposeStore();
    await renderTwoAgents();

    // Simulates a pane header click / Cmd+Alt+N from outside this component.
    selection.setRecipients(PROJECT_ID, [AGENT_B.id]);
    await waitFor(() => {
      expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "true");
      expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "false");
    });
    expect(composeStore.getCompose(PROJECT_ID).selectedIds).toEqual([AGENT_B.id]);
  });

  it("refuses pane targeting while a prompt render is in flight, so the send still dispatches", async () => {
    const panes = await importPanes();
    const selection = await importSelection();
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    panes.moveAgentToNewPane(PROJECT_ID, ROSTER, AGENT_B.id);

    let release!: (v: { text: string }) => void;
    const gate = new Promise<{ text: string }>((res) => {
      release = res;
    });
    mockPromptBackend({ prompts: [SUMMARY], render: () => gate });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    // Default selection is alice; dispatch a prompt send to her.
    await enterPromptMode("prompt-option-tiddly:summary");
    await fireEvent.click(screen.getByTestId("compose-send"));

    // Mid-render, a pane gesture is refused — without this, the post-render
    // recipient check would silently abort the send.
    expect(selection.targetRecipients(PROJECT_ID, [AGENT_B.id])).toBe(false);
    expect(selection.selectionFor(PROJECT_ID)).toEqual([AGENT_A.id]);

    release({ text: "DONE" });
    await waitFor(() => {
      const sends = invokeMock.mock.calls.filter(([c]) => c === "send_message");
      expect(sends).toHaveLength(1);
    });
    expect((state.transcripts[AGENT_A.id] ?? [])[0]).toMatchObject({ text: "DONE" });

    // The freeze lifts with the render: targeting works again.
    expect(selection.targetRecipients(PROJECT_ID, [AGENT_B.id])).toBe(true);
  });

  it("releases the targeting lock even when the prompt render fails", async () => {
    const panes = await importPanes();
    const selection = await importSelection();
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    panes.moveAgentToNewPane(PROJECT_ID, ROSTER, AGENT_B.id);

    mockPromptBackend({
      prompts: [SUMMARY],
      render: () => Promise.reject(new Error("render boom")),
    });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    await enterPromptMode("prompt-option-tiddly:summary");
    await fireEvent.click(screen.getByTestId("compose-send"));
    await waitFor(() => {
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent("render boom");
    });

    // A stuck lock would disable pane targeting forever — the failure path
    // must release it.
    expect(selection.targetRecipients(PROJECT_ID, [AGENT_B.id])).toBe(true);
  });

  it("omits empty panes from the @ menu's pane entries", async () => {
    const panes = await importPanes();
    const paneId = panes.moveAgentToNewPane(PROJECT_ID, ROSTER, AGENT_B.id);
    const pane1 = panes.layoutFor(PROJECT_ID, ROSTER).panes[0]!.id;
    // Empty pane 2 by moving bob back; the pane stays open but has no members.
    panes.moveAgentToPane(PROJECT_ID, ROSTER, AGENT_B.id, pane1);

    const textarea = await renderTwoAgents();
    await fireEvent.input(textarea, { target: { value: "@" } });
    await screen.findByTestId("recipient-menu");

    expect(screen.queryByTestId(`recipient-option-pane:${paneId}`)).not.toBeInTheDocument();
    expect(screen.getByTestId(`recipient-option-pane:${pane1}`)).toBeInTheDocument();
  });
});

describe("ComposeBar — cross-agent forward", () => {
  const AGENT_C: AgentRecord = {
    id: "00000000-0000-7000-8000-000000000ccc",
    project_id: PROJECT_ID,
    name: "carol",
    harness: "claude_code",
    session_locator: { uuid: "00000000-0000-7000-8000-000000000003" },
    created_at: "2026-05-16T00:00:02Z",
  };

  // Give an agent a completed turn so it's a non-empty forward source (else its
  // chip is flagged "no output").
  async function seedCompletedTurn(agentId: string): Promise<void> {
    const state = await loadState();
    state.transcripts[agentId] = [
      {
        role: "agent",
        turn_id: `t-${agentId}`,
        agent_id: agentId,
        started_at: "2026-05-16T00:00:00Z",
        status: "complete",
        items: [{ item_kind: "text", kind: "text", text: "done" }],
      },
    ];
  }

  async function resetHeldForwards(): Promise<void> {
    (await import("$lib/state/heldForwards.svelte"))._testing.reset();
  }

  // Open the `@` menu and pick the "forward from {agent}" entry for `agentId`.
  async function pickForwardSource(agentId: string): Promise<void> {
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "@" } });
    await fireEvent.click(await screen.findByTestId(`forward-option-forward-agent:${agentId}`));
  }

  afterEach(async () => {
    await resetHeldForwards();
  });

  it("@ menu pane row adds missing members, dedups, and disappears once all are attached", async () => {
    const panes = await import("$lib/state/transcriptPanes.svelte");
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await state.registerAgent(AGENT_C);
    const roster = [AGENT_A.id, AGENT_B.id, AGENT_C.id];
    // Split into two non-empty panes (the @ menu only offers panes once split):
    // "reviewers" = bob + carol; the default pane keeps alice.
    const reviewers = panes.moveAgentToNewPane(PROJECT_ID, roster, AGENT_B.id);
    panes.moveAgentToPane(PROJECT_ID, roster, AGENT_C.id, reviewers);
    panes.renamePane(PROJECT_ID, roster, reviewers, "reviewers");

    render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B, AGENT_C] },
    });
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;

    // Attach one member (bob) on its own, then forward the whole pane from the @
    // menu: only the missing member (carol) is added, bob isn't duplicated, and a
    // pane chip never appears.
    await pickForwardSource(AGENT_B.id);
    await fireEvent.input(textarea, { target: { value: "@review" } });
    await fireEvent.click(await screen.findByTestId(`forward-option-forward-pane:${reviewers}`));

    await waitFor(() =>
      expect(screen.getByTestId("forward-source-chip-carol")).toBeInTheDocument(),
    );
    expect(screen.getAllByTestId("forward-source-chip-bob")).toHaveLength(1);
    expect(screen.queryByTestId("forward-source-chip-reviewers")).toBeNull();

    // Both members now attached → the pane row is suppressed (picking it would be a
    // no-op), while the still-forwardable alice keeps the menu open.
    await fireEvent.input(textarea, { target: { value: "@" } });
    await screen.findByTestId(`forward-option-forward-agent:${AGENT_A.id}`);
    expect(screen.queryByTestId(`forward-option-forward-pane:${reviewers}`)).toBeNull();
  });

  it("picks a forward source from the @ menu and dispatches a forward", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "forward_message") {
        return { status: "resolved", body: "composed body", skipped: [] };
      }
      if (cmd === "send_message") return "msg-1";
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    // AGENT_A is the default recipient; forward FROM bob TO alice.
    await pickForwardSource(AGENT_B.id);
    expect(screen.getByTestId("forward-source-chip-bob")).toBeInTheDocument();

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "please aggregate" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    // The backend resolves + composes (no recipients/send_id — it doesn't dispatch).
    await waitFor(() => {
      const calls = invokeMock.mock.calls.filter(([c]) => c === "forward_message");
      expect(calls).toHaveLength(1);
      expect(calls[0]?.[1]).toMatchObject({ sources: [AGENT_B.id], body: "please aggregate" });
      expect(typeof (calls[0]?.[1] as { forwardId?: unknown }).forwardId).toBe("string");
    });
    // The frontend then dispatches the composed body to the recipient via the
    // normal send path (so it groups/cancels like any send).
    await waitFor(() => {
      const sends = invokeMock.mock.calls.filter(([c]) => c === "send_message");
      expect(sends).toHaveLength(1);
      expect(sends[0]?.[1]).toMatchObject({ agentId: AGENT_A.id, prompt: "composed body" });
    });
    // Composer clears on submit.
    await waitFor(() => {
      expect(screen.queryByTestId("forward-source-chip-bob")).toBeNull();
      expect((screen.getByTestId("compose-textarea") as HTMLTextAreaElement).value).toBe("");
    });
  });

  it("a plain forward carries staged attachments through to the dispatched send", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);
    invokeMock.mockImplementation(
      async (cmd: string, args?: Record<string, unknown>): Promise<unknown> => {
        if (cmd === "search_project_files") return [];
        if (cmd === "stage_attachment") {
          const source = String((args as { sourcePath?: unknown })?.sourcePath ?? "drop");
          const name = source.split("/").pop() ?? source;
          return { path: `/proj/.switchboard/attachments/uuid__${name}`, original_name: name };
        }
        if (cmd === "forward_message") return { status: "resolved", body: "composed", skipped: [] };
        if (cmd === "send_message") return "msg-1";
        return null;
      },
    );

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    fireDrop(["/a/diagram.png"]);
    await waitFor(() => expect(screen.getByTestId("attachment-chip-image-1")).toBeInTheDocument());
    await pickForwardSource(AGENT_B.id);
    await fireEvent.click(screen.getByTestId("compose-send"));

    // The forwarded body dispatches with the staged attachment — a forward is a
    // send, so the user's files ride it like any message.
    await waitFor(() => {
      const sends = invokeMock.mock.calls.filter(([c]) => c === "send_message");
      expect(sends).toHaveLength(1);
      const payload = sends[0]?.[1] as { prompt: string; attachments: { original_name: string }[] };
      expect(payload.prompt).toBe("composed");
      expect(payload.attachments).toHaveLength(1);
      expect(payload.attachments[0]?.original_name).toBe("diagram.png");
    });
    await waitFor(() => expect(screen.queryByTestId("attachment-chip-image-1")).toBeNull());
  });

  it("a forward's recipient response groups under the forwarded message (live)", async () => {
    // The §7 live-parity the resolve/dispatch split restored: the backend
    // resolves + composes, the frontend dispatches the body through the normal
    // send path, so the recipient's response turn carries the SAME send_id as the
    // forwarded user message and groups under it — exactly like any send.
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "forward_message")
        return { status: "resolved", body: "composed body", skipped: [] };
      if (cmd === "send_message") return "msg-fwd";
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await pickForwardSource(AGENT_B.id);
    await fireEvent.click(screen.getByTestId("compose-send"));

    // The composed body dispatches to recipient A as a normal send.
    await waitFor(() => {
      expect(invokeMock.mock.calls.filter(([c]) => c === "send_message")).toHaveLength(1);
    });

    // A's response turn (correlated by message_id) must carry the forwarded
    // user message's send_id — so the unified view groups them.
    fireTo(`agent:${AGENT_A.id}`, {
      type: "turn_start",
      turn_id: "t-a",
      message_id: "msg-fwd",
      send_id: "msg-fwd",
      started_at: "2026-05-16T00:00:00Z",
    });
    await waitFor(() => {
      const turns = state.transcripts[AGENT_A.id] ?? [];
      const sendIdOf = (t: (typeof turns)[number] | undefined) =>
        (t as { send_id?: string } | undefined)?.send_id;
      const userTurn = turns.find((t) => t.role === "user");
      const agentTurn = turns.find((t) => t.role === "agent");
      expect(sendIdOf(userTurn)).toBeDefined();
      expect(sendIdOf(agentTurn)).toBe(sendIdOf(userTurn));
    });
  });

  it("flags an idle-empty source on its chip", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    // AGENT_B has no completed turn → nothing to forward yet.

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await pickForwardSource(AGENT_B.id);

    const chipEl = screen.getByTestId("forward-source-chip-bob");
    expect(chipEl).toHaveAttribute("data-empty", "true");
    expect(chipEl).toHaveTextContent("no output");
  });

  it("composes multiple forward sources in declared order", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await state.registerAgent(AGENT_C);
    await seedCompletedTurn(AGENT_B.id);
    await seedCompletedTurn(AGENT_C.id);
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "forward_message") return { status: "resolved", body: "x", skipped: [] };
      if (cmd === "send_message") return "msg-1";
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B, AGENT_C] } });
    await pickForwardSource(AGENT_B.id);
    await pickForwardSource(AGENT_C.id);
    expect(screen.getByTestId("forward-source-chip-bob")).toBeInTheDocument();
    expect(screen.getByTestId("forward-source-chip-carol")).toBeInTheDocument();

    await fireEvent.click(screen.getByTestId("compose-send"));
    await waitFor(() => {
      const calls = invokeMock.mock.calls.filter(([c]) => c === "forward_message");
      expect(calls).toHaveLength(1);
      expect(calls[0]?.[1]).toMatchObject({ sources: [AGENT_B.id, AGENT_C.id] });
    });
  });

  it("restores the composer when a forward is invalidated", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "forward_message") {
        return { status: "invalidated", reason: "bob's turn failed before it could be forwarded" };
      }
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await pickForwardSource(AGENT_B.id);
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "aggregate this" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    // The source chip + typed text return to the composer (nothing was sent).
    await waitFor(() => {
      expect(screen.getByTestId("forward-source-chip-bob")).toBeInTheDocument();
      expect((screen.getByTestId("compose-textarea") as HTMLTextAreaElement).value).toBe(
        "aggregate this",
      );
    });
  });

  it("restores the composer when a held forward is cancelled", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "forward_message") return { status: "cancelled" };
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await pickForwardSource(AGENT_B.id);
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "aggregate this" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => {
      expect(screen.getByTestId("forward-source-chip-bob")).toBeInTheDocument();
      expect((screen.getByTestId("compose-textarea") as HTMLTextAreaElement).value).toBe(
        "aggregate this",
      );
    });
  });

  it("seeds a held forward (no send_message issued during the hold)", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);
    // forward_message never resolves during the test → the forward stays held.
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "forward_message") return new Promise(() => {});
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await pickForwardSource(AGENT_B.id);
    await fireEvent.click(screen.getByTestId("compose-send"));

    const held = await import("$lib/state/heldForwards.svelte");
    await waitFor(() => {
      const forwards = held.heldForwardsFor(PROJECT_ID);
      expect(forwards).toHaveLength(1);
      expect(held.expandForwardSources(forwards[0]?.sources ?? [])).toEqual([AGENT_B.id]);
      expect(forwards[0]?.recipients).toEqual([AGENT_A.id]);
    });
    // While holding, no `send_message` is issued — the frontend dispatches only
    // once `forward_message` resolves. Distinct from a queued send.
    expect(invokeMock.mock.calls.filter(([c]) => c === "send_message")).toHaveLength(0);
  });

  it("a pane-selected manual forward holds individual agent sources (not a pane)", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_A.id);
    await seedCompletedTurn(AGENT_B.id);
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "forward_message") return new Promise(() => {}); // holds forever
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    // Expand the default pane to its members, then submit.
    await fireEvent.keyDown(window, { key: "1", metaKey: true, ctrlKey: true });
    await waitFor(() =>
      expect(screen.getByTestId("forward-source-chip-alice")).toBeInTheDocument(),
    );
    await fireEvent.input(screen.getByTestId("compose-textarea"), { target: { value: "go" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    // The held entry carries one agent source per member — no pane grouping.
    const held = await import("$lib/state/heldForwards.svelte");
    await waitFor(() => {
      const forwards = held.heldForwardsFor(PROJECT_ID);
      expect(forwards).toHaveLength(1);
      expect(forwards[0]?.sources).toEqual([
        { id: AGENT_A.id, name: "alice" },
        { id: AGENT_B.id, name: "bob" },
      ]);
    });
  });

  it("restores individual agent chips when a pane-selected forward is cancelled", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_A.id);
    await seedCompletedTurn(AGENT_B.id);
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "forward_message") return { status: "cancelled" };
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await fireEvent.keyDown(window, { key: "1", metaKey: true, ctrlKey: true });
    await waitFor(() =>
      expect(screen.getByTestId("forward-source-chip-alice")).toBeInTheDocument(),
    );
    await fireEvent.input(screen.getByTestId("compose-textarea"), {
      target: { value: "aggregate" },
    });
    await fireEvent.click(screen.getByTestId("compose-send"));

    // The composer comes back with the member agent chips (not a pane chip) and
    // the typed draft intact.
    await waitFor(() => {
      expect(screen.getByTestId("forward-source-chip-alice")).toBeInTheDocument();
      expect(screen.getByTestId("forward-source-chip-bob")).toBeInTheDocument();
      expect((screen.getByTestId("compose-textarea") as HTMLTextAreaElement).value).toBe(
        "aggregate",
      );
    });
  });

  it("removes the held forward when it resolves after the user switches projects", async () => {
    // Regression: the held "waiting for…" row used to stick forever if the user
    // navigated to another project while a forward was holding (and stack across
    // repeats). The forward's resolve closure outlives the submitting context and
    // must key the global held-forward store by *this* forward's project (a
    // captured id), not the reactive `projectId` prop — which, once the user has
    // navigated, no longer points at the project the forward was submitted from.
    // Re-rendering with a different `projectId` reproduces that prop change under
    // the in-flight closure.
    const OTHER_PROJECT = "00000000-0000-7000-8000-0000000000ee";
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);

    // Spy on the local activity bump: the dispatched send must be attributed to
    // the project the forward was submitted from, not the navigated-to one.
    const workspace = await loadWorkspace();
    const activitySpy = vi.spyOn(workspace, "recordProjectsActivityLocally");

    let resolveForward!: (value: unknown) => void;
    const forwardHold = new Promise<unknown>((resolve) => {
      resolveForward = resolve;
    });
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "forward_message") return forwardHold;
      if (cmd === "send_message") return "msg-1";
      return null;
    });

    const { rerender } = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] },
    });
    await pickForwardSource(AGENT_B.id);
    await fireEvent.click(screen.getByTestId("compose-send"));

    const held = await import("$lib/state/heldForwards.svelte");
    await waitFor(() => expect(held.heldForwardsFor(PROJECT_ID)).toHaveLength(1));

    // Navigate to another project while the forward is still holding.
    await rerender({ projectId: OTHER_PROJECT, agents: [AGENT_A] });
    // The hold settles only after the switch.
    resolveForward({ status: "resolved", body: "composed", skipped: [] });

    // The entry must be gone from the project it was submitted under — not leaked.
    await waitFor(() => expect(held.heldForwardsFor(PROJECT_ID)).toHaveLength(0));
    // …and the activity bump on dispatch must hit the submitting project, never
    // the project navigated to (same stale-prop bug class, one call deeper).
    await waitFor(() => {
      expect(activitySpy).toHaveBeenCalledWith([PROJECT_ID], expect.any(String));
    });
    expect(activitySpy).not.toHaveBeenCalledWith([OTHER_PROJECT], expect.any(String));
  });

  // Manual forwarding into a prompt's arguments — the prompt-composer analogue of
  // the compose-bar forward above. The backend resolves the per-argument sources,
  // composes + fills + renders the prompt, and returns the rendered body; the
  // frontend dispatches it through the normal send path.
  function mockPromptForwardBackend(forward: unknown): void {
    invokeMock.mockImplementation(
      async (cmd: string, args?: Record<string, unknown>): Promise<unknown> => {
        if (cmd === "search_project_files") return [];
        if (cmd === "stage_attachment") {
          const source = String((args as { sourcePath?: unknown })?.sourcePath ?? "drop");
          const name = source.split("/").pop() ?? source;
          return { path: `/proj/.switchboard/attachments/uuid__${name}`, original_name: name };
        }
        if (cmd === "list_prompts") return [REVIEW];
        if (cmd === "render_prompt") return { text: "RENDERED" };
        if (cmd === "forward_prompt") {
          if (forward instanceof Error) throw forward;
          return forward;
        }
        if (cmd === "send_message") return "msg-fwd";
        return null;
      },
    );
  }

  // Open the per-argument forward picker and pick `agentId` as a source.
  async function pickArgForward(argName: string, agentId: string): Promise<void> {
    await fireEvent.click(screen.getByTestId(`prompt-arg-forward-${argName}`));
    await fireEvent.click(await screen.findByTestId(`forward-picker-agent-${agentId}`));
  }

  it("⌘⌃1 forwards pane 1 as one chip per member agent (mirrors ⌘⌥1 targeting)", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    // The default pane "Pane 1" holds every agent; ⌘⌃1 expands it to one chip per
    // member agent — a pane is a selection shortcut, never a stored pane chip.
    await fireEvent.keyDown(window, { key: "1", metaKey: true, ctrlKey: true });

    await waitFor(() => {
      expect(screen.getByTestId("forward-source-chip-alice")).toBeInTheDocument();
      expect(screen.getByTestId("forward-source-chip-bob")).toBeInTheDocument();
    });
    expect(screen.queryByTestId("forward-source-chip-Pane 1")).toBeNull();
  });

  it("re-picking a pane and an already-attached agent does not duplicate chips", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    // Attach one agent via the picker, then expand the whole pane via the chord:
    // alice must not appear twice.
    await pickForwardSource(AGENT_A.id);
    await fireEvent.keyDown(window, { key: "1", metaKey: true, ctrlKey: true });

    await waitFor(() => expect(screen.getByTestId("forward-source-chip-bob")).toBeInTheDocument());
    expect(screen.getAllByTestId("forward-source-chip-alice")).toHaveLength(1);
  });

  it("a pane-expanded forward dispatches its member agent ids", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_A.id);
    await seedCompletedTurn(AGENT_B.id);
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "forward_message") return { status: "resolved", body: "composed", skipped: [] };
      if (cmd === "send_message") return "msg-1";
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await fireEvent.keyDown(window, { key: "1", metaKey: true, ctrlKey: true });
    await waitFor(() =>
      expect(screen.getByTestId("forward-source-chip-alice")).toBeInTheDocument(),
    );
    await fireEvent.input(screen.getByTestId("compose-textarea"), { target: { value: "go" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    // The member agent chips ride the wire as their agent ids.
    await waitFor(() => {
      const calls = invokeMock.mock.calls.filter(([c]) => c === "forward_message");
      expect(calls).toHaveLength(1);
      expect(calls[0]?.[1]).toMatchObject({ sources: [AGENT_A.id, AGENT_B.id] });
    });
  });

  it("hides the compose-bar forward button and chips in prompt mode", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);
    mockPromptForwardBackend({ status: "resolved", body: "x", skipped: [] });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    // Plain mode: the ↪ Forward button shows and a source chip can be added.
    expect(screen.getByTestId("compose-forward-button")).toBeInTheDocument();
    await pickForwardSource(AGENT_B.id);
    expect(screen.getByTestId("forward-source-chips")).toBeInTheDocument();

    // Prompt mode: forwarding is per-field, so the message-level affordances hide
    // (their state is preserved, just not shown).
    await enterPromptMode("prompt-option-local:review");
    expect(screen.queryByTestId("compose-forward-button")).toBeNull();
    expect(screen.queryByTestId("forward-source-chips")).toBeNull();
  });

  it("⌘⌃N forwards a pane into the focused prompt field as member agents (not the hidden message set)", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    mockPromptForwardBackend({ status: "resolved", body: "x", skipped: [] });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await enterPromptMode("prompt-option-local:review");

    // The chord targets whichever field is focused (it's inert otherwise), so
    // focus an argument field, then fire it.
    (screen.getByTestId("prompt-arg-focus") as HTMLTextAreaElement).focus();
    await fireEvent.keyDown(window, { key: "1", metaKey: true, ctrlKey: true });

    // The pane lands as one chip per member agent on that field — not on the
    // whole-message forward set, which stays hidden in prompt mode.
    await waitFor(() => {
      const field = screen.getByTestId("prompt-arg-sources-focus");
      expect(field.querySelector('[data-testid="forward-source-chip-alice"]')).not.toBeNull();
      expect(field.querySelector('[data-testid="forward-source-chip-bob"]')).not.toBeNull();
    });
    expect(screen.queryByTestId("forward-source-chips")).toBeNull();
  });

  it("clears a hidden plain-mode forward source after a successful prompt send", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);
    mockPromptForwardBackend({ status: "resolved", body: "RENDERED", skipped: [] });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    // Add a plain forward source, then switch to a prompt (no per-field forward).
    await pickForwardSource(AGENT_B.id);
    await enterPromptMode("prompt-option-local:review");
    await fireEvent.input(screen.getByTestId("prompt-arg-focus"), { target: { value: "x" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    // Back in plain mode: the stale forward source is gone (a send is a fresh start).
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    expect(screen.queryByTestId("forward-source-chips")).toBeNull();

    // A subsequent plain send is a normal send, not a forward of stale output.
    invokeMock.mockClear();
    await fireEvent.input(screen.getByTestId("compose-textarea"), { target: { value: "next" } });
    await fireEvent.click(screen.getByTestId("compose-send"));
    await waitFor(() =>
      expect(invokeMock.mock.calls.filter(([c]) => c === "send_message")).toHaveLength(1),
    );
    expect(invokeMock.mock.calls.filter(([c]) => c === "forward_message")).toHaveLength(0);
  });

  it("forwards into the appended text and dispatches with appended sources", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);
    mockPromptForwardBackend({ status: "resolved", body: "RENDERED + APPENDED", skipped: [] });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await enterPromptMode("prompt-option-local:review");
    // Required arg satisfied by typed text; forward only into the appended field.
    await fireEvent.input(screen.getByTestId("prompt-arg-focus"), { target: { value: "tests" } });
    await fireEvent.click(screen.getByTestId("prompt-appended-forward"));
    await fireEvent.click(await screen.findByTestId(`forward-picker-agent-${AGENT_B.id}`));
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => {
      const calls = invokeMock.mock.calls.filter(([c]) => c === "forward_prompt");
      expect(calls).toHaveLength(1);
      // No forwarded arguments; the appended text carries the source.
      expect(calls[0]?.[1]).toMatchObject({ forwardArgs: [], appendedSources: [AGENT_B.id] });
    });
    // The backend-combined body dispatches verbatim (no client-side combine).
    await waitFor(() => {
      const sends = invokeMock.mock.calls.filter(([c]) => c === "send_message");
      expect(sends[0]?.[1]).toMatchObject({ prompt: "RENDERED + APPENDED" });
    });
  });

  it("forwards an agent's output into a prompt argument and dispatches the rendered body", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);
    mockPromptForwardBackend({ status: "resolved", body: "RENDERED WITH FORWARD", skipped: [] });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await enterPromptMode("prompt-option-local:review");
    await pickArgForward("focus", AGENT_B.id);
    expect(
      within(screen.getByTestId("prompt-arg-sources-focus")).getByTestId("forward-source-chip-bob"),
    ).toBeInTheDocument();
    // The required `focus` is typed-empty but the source fills it → send enabled.
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(false);
    await fireEvent.click(screen.getByTestId("compose-send"));

    // Backend gets the prompt id, the empty typed args, and the per-arg sources.
    await waitFor(() => {
      const calls = invokeMock.mock.calls.filter(([c]) => c === "forward_prompt");
      expect(calls).toHaveLength(1);
      expect(calls[0]?.[1]).toMatchObject({
        provider: "local",
        name: "review",
        typedArgs: {},
        forwardArgs: [{ name: "focus", sources: [AGENT_B.id], required: true }],
      });
      expect(typeof (calls[0]?.[1] as { forwardId?: unknown }).forwardId).toBe("string");
    });
    // The rendered body dispatches to the recipient via the normal send path.
    await waitFor(() => {
      const sends = invokeMock.mock.calls.filter(([c]) => c === "send_message");
      expect(sends).toHaveLength(1);
      expect(sends[0]?.[1]).toMatchObject({ agentId: AGENT_A.id, prompt: "RENDERED WITH FORWARD" });
    });
    // Composer returns to plain mode on submit.
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
  });

  it("sends the typed lead text as the argument's typed value alongside its source", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);
    mockPromptForwardBackend({ status: "resolved", body: "BODY", skipped: [] });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await enterPromptMode("prompt-option-local:review");
    await fireEvent.input(screen.getByTestId("prompt-arg-focus"), { target: { value: "lead" } });
    await pickArgForward("focus", AGENT_B.id);
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => {
      const calls = invokeMock.mock.calls.filter(([c]) => c === "forward_prompt");
      expect(calls).toHaveLength(1);
      // Typed lead rides as the typed arg; the backend prepends it to the blocks.
      expect(calls[0]?.[1]).toMatchObject({ typedArgs: { focus: "lead" } });
    });
  });

  it("holds the prompt forward and seeds a held entry (no send during the hold)", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);
    mockPromptForwardBackend(new Promise(() => {})); // never resolves

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await enterPromptMode("prompt-option-local:review");
    await pickArgForward("focus", AGENT_B.id);
    await fireEvent.click(screen.getByTestId("compose-send"));

    const held = await import("$lib/state/heldForwards.svelte");
    await waitFor(() => {
      const forwards = held.heldForwardsFor(PROJECT_ID);
      expect(forwards).toHaveLength(1);
      expect(held.expandForwardSources(forwards[0]?.sources ?? [])).toEqual([AGENT_B.id]);
      expect(forwards[0]?.recipients).toEqual([AGENT_A.id]);
    });
    expect(invokeMock.mock.calls.filter(([c]) => c === "send_message")).toHaveLength(0);
  });

  it("restores prompt mode when a prompt forward is invalidated", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);
    mockPromptForwardBackend({
      status: "invalidated",
      reason: 'required argument "focus" had no output to forward',
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await enterPromptMode("prompt-option-local:review");
    await pickArgForward("focus", AGENT_B.id);
    await fireEvent.click(screen.getByTestId("compose-send"));

    // The prompt composer comes back with the source chip and an error.
    await waitFor(() => {
      expect(screen.getByTestId("prompt-composer")).toBeInTheDocument();
      expect(screen.getByTestId("forward-source-chip-bob")).toBeInTheDocument();
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent("no output to forward");
    });
    expect(invokeMock.mock.calls.filter(([c]) => c === "send_message")).toHaveLength(0);
  });

  it("restores prompt mode (and re-stages chips) when a prompt forward is cancelled", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);
    mockPromptForwardBackend({ status: "cancelled" });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    fireDrop(["/a/diagram.png"]);
    await waitFor(() => expect(screen.getByTestId("attachment-chip-image-1")).toBeInTheDocument());
    await enterPromptMode("prompt-option-local:review");
    await pickArgForward("focus", AGENT_B.id);
    await fireEvent.click(screen.getByTestId("compose-send"));

    // A cancelled hold restores the whole composer: prompt, the per-arg source
    // chip, and the attachment chip (rebuilt from the snapshot).
    await waitFor(() => {
      expect(screen.getByTestId("prompt-composer")).toBeInTheDocument();
      expect(screen.getByTestId("forward-source-chip-bob")).toBeInTheDocument();
      expect(screen.getByTestId("attachment-chip-image-1")).toBeInTheDocument();
    });
    expect(invokeMock.mock.calls.filter(([c]) => c === "send_message")).toHaveLength(0);
  });

  it("restores prompt mode and clears the held entry when the forward IPC rejects", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);
    mockPromptForwardBackend(new Error("ipc down"));

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await enterPromptMode("prompt-option-local:review");
    await pickArgForward("focus", AGENT_B.id);
    await fireEvent.click(screen.getByTestId("compose-send"));

    const held = await import("$lib/state/heldForwards.svelte");
    await waitFor(() => {
      expect(screen.getByTestId("prompt-composer")).toBeInTheDocument();
      expect(screen.getByTestId("forward-source-chip-bob")).toBeInTheDocument();
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent("ipc down");
      // No ghost held entry survives an IPC failure.
      expect(held.heldForwardsFor(PROJECT_ID)).toHaveLength(0);
    });
    expect(invokeMock.mock.calls.filter(([c]) => c === "send_message")).toHaveLength(0);
  });

  it("a prompt forward carries staged attachments through to the dispatched send", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);
    mockPromptForwardBackend({ status: "resolved", body: "RENDERED BODY", skipped: [] });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    fireDrop(["/a/diagram.png"]);
    await waitFor(() => expect(screen.getByTestId("attachment-chip-image-1")).toBeInTheDocument());
    await enterPromptMode("prompt-option-local:review");
    await pickArgForward("focus", AGENT_B.id);
    await fireEvent.click(screen.getByTestId("compose-send"));

    // The resolved body dispatches with the staged attachment (a prompt forward is
    // a prompt send, so it carries files like any prompt send).
    await waitFor(() => {
      const sends = invokeMock.mock.calls.filter(([c]) => c === "send_message");
      expect(sends).toHaveLength(1);
      const payload = sends[0]?.[1] as { prompt: string; attachments: { original_name: string }[] };
      expect(payload.prompt).toBe("RENDERED BODY");
      expect(payload.attachments).toHaveLength(1);
      expect(payload.attachments[0]?.original_name).toBe("diagram.png");
    });
    // Chips clear once the forward has dispatched.
    await waitFor(() => expect(screen.queryByTestId("attachment-chip-image-1")).toBeNull());
  });

  it("enters workflow mode, resolves the form, and invokes with declared + derived values", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    const WORKFLOW = {
      name: "review-and-recommend",
      is_builtin: true,
      description: "d",
      inputs: [
        { name: "reviewers", ty: "agent_list", optional: false, description: null },
        { name: "worker", ty: "agent", optional: false, description: null },
      ],
      invocable: true,
      parse_error: null,
    };
    // The descriptor adds the auto-derived `context` arg (optional) from the
    // hardcoded code-review prompt.
    const DESCRIPTOR = {
      name: "review-and-recommend",
      description: "d",
      is_builtin: true,
      invocable: true,
      inputs: WORKFLOW.inputs,
      steps: [],
      derived_args: [
        {
          name: "context",
          required: false,
          description: "Optional background",
          prompts: ["builtin:code-review"],
        },
      ],
      compatibility: { state: "ok" },
    };
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workflows") return [WORKFLOW];
      if (cmd === "describe_workflow_form") return DESCRIPTOR;
      if (cmd === "list_prompts") return [];
      if (cmd === "invoke_workflow") return "run-1";
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    await fireEvent.click(screen.getByTestId("compose-workflow-button"));
    await waitFor(() => screen.getByTestId("workflow-option-builtin:review-and-recommend"));
    await fireEvent.click(screen.getByTestId("workflow-option-builtin:review-and-recommend"));

    // Workflow mode: the composer renders, and the To field + message forward
    // affordance are hidden (the workflow owns routing via its agent inputs).
    expect(screen.getByTestId("workflow-composer")).toBeInTheDocument();
    expect(screen.queryByTestId("recipient-field")).toBeNull();
    expect(screen.queryByTestId("compose-forward-button")).toBeNull();

    // The auto-derived `context` field renders; no prompt-picker control exists.
    await waitFor(() => screen.getByTestId("workflow-arg-input-context"));
    expect(screen.queryByTestId("workflow-prompt-review_prompt")).toBeNull();

    // Fill the required agent inputs and the optional derived arg.
    await fireEvent.click(screen.getByTestId("workflow-agent-reviewers-bob"));
    await fireEvent.click(screen.getByTestId("workflow-agent-worker-alice"));
    await fireEvent.input(screen.getByTestId("workflow-arg-input-context"), {
      target: { value: "focus on error handling" },
    });

    await fireEvent.click(screen.getByTestId("workflow-invoke-button"));
    await waitFor(() => {
      expect(invokeMock.mock.calls.some(([c]) => c === "invoke_workflow")).toBe(true);
    });
    const call = invokeMock.mock.calls.find(([c]) => c === "invoke_workflow");
    expect(call?.[1]).toMatchObject({
      projectId: PROJECT_ID,
      name: "review-and-recommend",
      isBuiltin: true,
      inputs: {
        reviewers: ["bob"],
        worker: "alice",
        context: "focus on error handling",
      },
      // No field had a forward attached, so the map is present but empty.
      forwardSources: {},
    });
  });

  it("runs the workflow on ⌘Enter from inside a workflow form field", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    const WORKFLOW = {
      name: "review-and-recommend",
      is_builtin: true,
      description: "d",
      inputs: [
        { name: "reviewers", ty: "agent_list", optional: false, description: null },
        { name: "worker", ty: "agent", optional: false, description: null },
      ],
      invocable: true,
      parse_error: null,
    };
    const DESCRIPTOR = {
      name: "review-and-recommend",
      description: "d",
      is_builtin: true,
      invocable: true,
      inputs: WORKFLOW.inputs,
      steps: [],
      derived_args: [
        {
          name: "context",
          required: false,
          description: "Optional",
          prompts: ["builtin:code-review"],
        },
      ],
      compatibility: { state: "ok" },
    };
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workflows") return [WORKFLOW];
      if (cmd === "describe_workflow_form") return DESCRIPTOR;
      if (cmd === "list_prompts") return [];
      if (cmd === "invoke_workflow") return "run-1";
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await fireEvent.click(screen.getByTestId("compose-workflow-button"));
    await waitFor(() => screen.getByTestId("workflow-option-builtin:review-and-recommend"));
    await fireEvent.click(screen.getByTestId("workflow-option-builtin:review-and-recommend"));
    await waitFor(() => screen.getByTestId("workflow-arg-input-context"));
    await fireEvent.click(screen.getByTestId("workflow-agent-reviewers-bob"));
    await fireEvent.click(screen.getByTestId("workflow-agent-worker-alice"));

    // ⌘Enter from inside a form field runs it — no click on the invoke button.
    const field = screen.getByTestId("workflow-arg-input-context") as HTMLTextAreaElement;
    field.focus();
    await fireEvent.keyDown(window, { key: "Enter", metaKey: true });

    await waitFor(() =>
      expect(invokeMock.mock.calls.some(([c]) => c === "invoke_workflow")).toBe(true),
    );
    workflowsTesting.reset();
  });

  it("invokes a workflow with a forward source attached to a derived field", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    const WORKFLOW = {
      name: "review-and-recommend",
      is_builtin: true,
      description: "d",
      inputs: [{ name: "worker", ty: "agent", optional: false, description: null }],
      invocable: true,
      parse_error: null,
    };
    const DESCRIPTOR = {
      name: "review-and-recommend",
      description: "d",
      is_builtin: true,
      invocable: true,
      inputs: WORKFLOW.inputs,
      steps: [],
      derived_args: [
        {
          name: "context",
          required: false,
          description: "Optional background",
          prompts: ["builtin:code-review"],
        },
      ],
      compatibility: { state: "ok" },
    };
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workflows") return [WORKFLOW];
      if (cmd === "describe_workflow_form") return DESCRIPTOR;
      if (cmd === "list_prompts") return [];
      if (cmd === "invoke_workflow") return "run-1";
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    await fireEvent.click(screen.getByTestId("compose-workflow-button"));
    await waitFor(() => screen.getByTestId("workflow-option-builtin:review-and-recommend"));
    await fireEvent.click(screen.getByTestId("workflow-option-builtin:review-and-recommend"));

    await waitFor(() => screen.getByTestId("workflow-arg-input-context"));
    await fireEvent.click(screen.getByTestId("workflow-agent-worker-alice"));

    // Forward alice's output into the derived `context` field (in place of typing).
    await fireEvent.click(screen.getByTestId("workflow-forward-picker-context"));
    await fireEvent.click(await screen.findByTestId(`forward-picker-agent-${AGENT_A.id}`));
    await waitFor(() => screen.getByTestId("forward-source-chip-alice"));

    await fireEvent.click(screen.getByTestId("workflow-invoke-button"));
    await waitFor(() => {
      expect(invokeMock.mock.calls.some(([c]) => c === "invoke_workflow")).toBe(true);
    });
    const call = invokeMock.mock.calls.find(([c]) => c === "invoke_workflow");
    // The pane-expanded agent ids land under the field name.
    expect(call?.[1]).toMatchObject({
      name: "review-and-recommend",
      forwardSources: { context: [AGENT_A.id] },
    });
  });

  it("resolves an unresolved workflow form on prompts:synced without a re-pick", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const WORKFLOW = {
      name: "mcp-flow",
      is_builtin: false,
      description: "d",
      inputs: [{ name: "worker", ty: "agent", optional: false, description: null }],
      invocable: true,
      parse_error: null,
    };
    const BASE = {
      name: "mcp-flow",
      description: "d",
      is_builtin: false,
      invocable: true,
      inputs: WORKFLOW.inputs,
      steps: [],
      derived_args: [] as unknown[],
    };
    // The first describe lands while the MCP prompt is still cold (unresolved);
    // after a sync the same call resolves to ok.
    let synced = false;
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workflows") return [WORKFLOW];
      if (cmd === "list_prompts") return [];
      if (cmd === "describe_workflow_form") {
        return synced
          ? { ...BASE, compatibility: { state: "ok" } }
          : { ...BASE, compatibility: { state: "unresolved", prompts: ["tiddly:x"] } };
      }
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await fireEvent.click(screen.getByTestId("compose-workflow-button"));
    await waitFor(() => screen.getByTestId("workflow-option-dir:mcp-flow"));
    await fireEvent.click(screen.getByTestId("workflow-option-dir:mcp-flow"));

    // Pending affordance shown; the agent field is withheld until resolution.
    await waitFor(() => screen.getByTestId("workflow-resolving"));
    expect(screen.queryByTestId("workflow-agent-worker-alice")).toBeNull();

    // A completed sync re-fetches the descriptor, which now resolves → fields render.
    synced = true;
    listeners.get("prompts:synced")?.({ payload: null as unknown as NormalizedEvent });
    await waitFor(() => screen.getByTestId("workflow-agent-worker-alice"));
    expect(screen.queryByTestId("workflow-resolving")).toBeNull();
  });

  it("escalates a still-unresolved workflow to a not-found error after a sync settles", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const WORKFLOW = {
      name: "mcp-gone",
      is_builtin: false,
      description: "d",
      inputs: [{ name: "worker", ty: "agent", optional: false, description: null }],
      invocable: true,
      parse_error: null,
    };
    // The MCP prompt is missing and a sync does not produce it — the descriptor is
    // unresolved before and after the sync.
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workflows") return [WORKFLOW];
      if (cmd === "list_prompts") return [];
      if (cmd === "describe_workflow_form") {
        return {
          name: "mcp-gone",
          description: "d",
          is_builtin: false,
          invocable: true,
          inputs: WORKFLOW.inputs,
          steps: [],
          derived_args: [],
          compatibility: { state: "unresolved", prompts: ["tiddly:gone"] },
        };
      }
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await fireEvent.click(screen.getByTestId("compose-workflow-button"));
    await waitFor(() => screen.getByTestId("workflow-option-dir:mcp-gone"));
    await fireEvent.click(screen.getByTestId("workflow-option-dir:mcp-gone"));

    // Before a sync settles → pending spinner, not an error.
    await waitFor(() => screen.getByTestId("workflow-resolving"));
    expect(screen.queryByTestId("workflow-prompt-missing")).toBeNull();

    // After a sync settles and it's still unresolved → blocking not-found error.
    listeners.get("prompts:synced")?.({ payload: null as unknown as NormalizedEvent });
    await waitFor(() => screen.getByTestId("workflow-prompt-missing"));
    expect(screen.queryByTestId("workflow-resolving")).toBeNull();
  });

  it("drops a stale workflow-form reply that resolves out of order", async () => {
    // The generation-token guard: an older describe reply that lands after a newer
    // one must not overwrite the form. Drive two re-fetches whose replies resolve
    // in reverse order and assert the newer (ok) wins, not the older (unresolved).
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const WORKFLOW = {
      name: "race",
      is_builtin: false,
      description: "d",
      inputs: [{ name: "worker", ty: "agent", optional: false, description: null }],
      invocable: true,
      parse_error: null,
    };
    const base = {
      name: "race",
      description: "d",
      is_builtin: false,
      invocable: true,
      inputs: WORKFLOW.inputs,
      steps: [],
      derived_args: [] as unknown[],
    };
    const pending: Array<(d: unknown) => void> = [];
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workflows") return [WORKFLOW];
      if (cmd === "list_prompts") return [];
      if (cmd === "describe_workflow_form") {
        return new Promise((resolve) => pending.push(resolve));
      }
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await fireEvent.click(screen.getByTestId("compose-workflow-button"));
    await waitFor(() => screen.getByTestId("workflow-option-dir:race"));
    await fireEvent.click(screen.getByTestId("workflow-option-dir:race")); // fetch #1 (gen 1)
    // Two prompt syncs trigger two more re-fetches (gen 2, then gen 3).
    listeners.get("prompts:synced")?.({ payload: null as unknown as NormalizedEvent });
    listeners.get("prompts:synced")?.({ payload: null as unknown as NormalizedEvent });
    await waitFor(() => expect(pending.length).toBe(3));

    // Resolve the NEWEST (gen 3) as ok, then an older (gen 1) as unresolved.
    pending[2]?.({ ...base, compatibility: { state: "ok" } });
    await waitFor(() => screen.getByTestId("workflow-agent-worker-alice"));
    pending[0]?.({ ...base, compatibility: { state: "unresolved", prompts: ["tiddly:x"] } });
    await tick();

    // The stale unresolved reply is ignored — the form stays resolved.
    expect(screen.getByTestId("workflow-agent-worker-alice")).toBeInTheDocument();
    expect(screen.queryByTestId("workflow-resolving")).toBeNull();
  });
});

describe("ComposeBar — workflow run live view (M4 swap / hold / stop)", () => {
  function run(over: Partial<WorkflowRunInfo> = {}): WorkflowRunInfo {
    return {
      run_id: "run-1",
      workflow: "review-and-recommend",
      step: 0,
      total: 3,
      status: "running",
      reason: null,
      steps: [
        {
          kind: "send",
          label: "Send the review",
          description: null,
          prompt: { kind: "named", id: "builtin:code-review" },
          recipients: [{ kind: "literal", name: "alice" }],
          feeds_from: [],
        },
        {
          kind: "wait",
          label: "Wait for reviews",
          description: null,
          prompt: null,
          recipients: [{ kind: "literal", name: "alice" }],
          feeds_from: [],
        },
        {
          kind: "send",
          label: "Hand off",
          description: null,
          prompt: { kind: "inline" },
          recipients: [{ kind: "literal", name: "bob" }],
          feeds_from: [],
        },
      ],
      ...over,
    };
  }

  beforeEach(() => workflowsTesting.reset());
  afterEach(() => workflowsTesting.reset());

  it("replaces compose with the live progress view while a workflow runs", async () => {
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    expect(screen.getByTestId("compose-box")).toBeInTheDocument();

    workflowRuns[PROJECT_ID] = [run()];
    await tick();

    expect(screen.getByTestId("workflow-run-live")).toBeInTheDocument();
    // The compose box (and any send path) is GONE, not merely disabled.
    expect(screen.queryByTestId("compose-box")).toBeNull();
    expect(screen.queryByTestId("compose-textarea")).toBeNull();
    expect(screen.queryByTestId("compose-send")).toBeNull();
    // Labeled steps render, with the active step on step 0.
    expect(screen.getByTestId("workflow-step-0")).toHaveTextContent("Send the review");
    expect(screen.getByTestId("workflow-step-0")).toHaveAttribute("data-step-state", "active");
  });

  it("restores compose when the run completes and drops from state", async () => {
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    workflowRuns[PROJECT_ID] = [run()];
    await tick();
    expect(screen.getByTestId("workflow-run-live")).toBeInTheDocument();

    // complete/cancelled remove the run from state.
    workflowRuns[PROJECT_ID] = [];
    await tick();
    expect(screen.getByTestId("compose-box")).toBeInTheDocument();
    expect(screen.queryByTestId("workflow-run-live")).toBeNull();
  });

  it("holds on a failed run and Dismiss abandons it", async () => {
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    workflowRuns[PROJECT_ID] = [run({ status: "failed", step: 1, reason: "agent is busy" })];
    await tick();

    // Held (no Stop), showing the failed step + reason. The send + its wait
    // collapse into one node [0,1]; a failure at step 1 fails that node (index 0).
    expect(screen.getByTestId("workflow-run-live")).toHaveAttribute("data-run-status", "failed");
    expect(screen.queryByTestId("workflow-run-stop")).toBeNull();
    expect(screen.getByTestId("workflow-step-0")).toHaveAttribute("data-step-state", "failed");
    expect(screen.getByTestId("workflow-step-reason-0")).toHaveTextContent("agent is busy");

    await fireEvent.click(screen.getByTestId("workflow-run-dismiss"));
    const call = invokeMock.mock.calls.find(([c]) => c === "abandon_workflow_run");
    expect(call?.[1]).toMatchObject({ projectId: PROJECT_ID, runId: "run-1" });
  });

  it("Stop cancels the workflow run", async () => {
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    workflowRuns[PROJECT_ID] = [run()];
    await tick();

    await fireEvent.click(screen.getByTestId("workflow-run-stop"));
    const call = invokeMock.mock.calls.find(([c]) => c === "cancel_workflow_run");
    expect(call?.[1]).toMatchObject({ runId: "run-1" });
  });

  it("renders a fallback count line when steps are absent (legacy/pre-refresh)", async () => {
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    workflowRuns[PROJECT_ID] = [run({ steps: [] })];
    await tick();
    expect(screen.getByTestId("workflow-run-fallback")).toHaveTextContent("Step 1 of 3");
  });

  it("holds the lockout via an optimistic row when the post-invoke refresh fails", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    const WORKFLOW = {
      name: "review-and-recommend",
      is_builtin: true,
      description: "d",
      inputs: [
        { name: "reviewers", ty: "agent_list", optional: false, description: null },
        { name: "worker", ty: "agent", optional: false, description: null },
      ],
      invocable: true,
      parse_error: null,
    };
    const DESCRIPTOR = {
      name: "review-and-recommend",
      description: "d",
      is_builtin: true,
      invocable: true,
      inputs: WORKFLOW.inputs,
      steps: [
        {
          kind: "send",
          label: "Send the review",
          description: null,
          prompt: { kind: "named", id: "builtin:code-review" },
          recipients: [{ kind: "slot", input: "reviewers" }],
          feeds_from: [],
        },
      ],
      derived_args: [],
      compatibility: { state: "ok" },
    };
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workflows") return [WORKFLOW];
      if (cmd === "describe_workflow_form") return DESCRIPTOR;
      if (cmd === "list_prompts") return [];
      if (cmd === "invoke_workflow") return "run-opt";
      // The follow-up seed fails — the lockout must NOT depend on it.
      if (cmd === "list_workflow_runs") throw new Error("transient backend error");
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await fireEvent.click(screen.getByTestId("compose-workflow-button"));
    await waitFor(() => screen.getByTestId("workflow-option-builtin:review-and-recommend"));
    await fireEvent.click(screen.getByTestId("workflow-option-builtin:review-and-recommend"));
    await waitFor(() => screen.getByTestId("workflow-composer"));
    await fireEvent.click(screen.getByTestId("workflow-agent-reviewers-bob"));
    await fireEvent.click(screen.getByTestId("workflow-agent-worker-alice"));
    await fireEvent.click(screen.getByTestId("workflow-invoke-button"));

    // Refresh rejected, but the optimistic row keeps the compose box gone.
    await waitFor(() => expect(screen.getByTestId("workflow-run-live")).toBeInTheDocument());
    expect(screen.queryByTestId("compose-box")).toBeNull();
    expect(screen.queryByTestId("compose-textarea")).toBeNull();
    expect(screen.getByTestId("workflow-step-0")).toHaveTextContent("Send the review");
  });

  it("surfaces a Dismiss failure inline and keeps the run held", async () => {
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "abandon_workflow_run") throw new Error("file is gone");
      return null;
    });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    workflowRuns[PROJECT_ID] = [run({ status: "failed", step: 1, reason: "boom" })];
    await tick();

    await fireEvent.click(screen.getByTestId("workflow-run-dismiss"));
    await waitFor(() =>
      expect(screen.getByTestId("workflow-run-error")).toHaveTextContent("Couldn't dismiss"),
    );
    // Still held — the run wasn't cleared.
    expect(screen.getByTestId("workflow-run-live")).toBeInTheDocument();
  });

  it("surfaces a Stop failure inline and keeps the run live", async () => {
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "cancel_workflow_run") throw new Error("backend gone");
      return null;
    });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    workflowRuns[PROJECT_ID] = [run()]; // running → Stop control
    await tick();

    await fireEvent.click(screen.getByTestId("workflow-run-stop"));
    await waitFor(() =>
      expect(screen.getByTestId("workflow-run-error")).toHaveTextContent("Couldn't stop"),
    );
    // Still live — the run wasn't cleared.
    expect(screen.getByTestId("workflow-run-live")).toBeInTheDocument();
  });
});
