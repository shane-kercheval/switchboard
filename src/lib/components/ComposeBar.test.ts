import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { tick } from "svelte";
import type { AgentRecord, NormalizedEvent, Prompt } from "$lib/types";
// Static import so the component-tree transform happens at module collection,
// not inside the first test's timeout (cold CI transforms have no vite cache).
// `vi.mock` is hoisted above imports, so the mocks below still apply.
import ComposeBar from "./ComposeBar.svelte";

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

beforeEach(() => {
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

  it("recovers a single-agent project from a saved selection whose agent is gone", async () => {
    // Saved "send to bob" against a project that now has only alice: bob is
    // filtered out, and a single-agent project shows no chips — so without the
    // single-agent guard the composer would be unsendable with no recovery UI.
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockResolvedValue("msg-1");
    const store = await loadComposeStore();
    store.setSelection(PROJECT_ID, [AGENT_B.id]);
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
    expect((screen.getByTestId("compose-prompt-button") as HTMLButtonElement).disabled).toBe(true);
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
