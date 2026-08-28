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
import { WORKFLOW_AUTHORING_GUIDE_URL } from "$lib/workflowAuthoring";

const invokeMock = vi.fn(
  async (_cmd: string, _args?: Record<string, unknown>): Promise<unknown> => null,
);
const copyTextMock = vi.fn(async (_text: string): Promise<void> => undefined);

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));
vi.mock("$lib/native", () => ({
  copyText: (text: string) => copyTextMock(text),
}));

const listeners = new Map<string, (e: { payload: unknown }) => void>();
/// Default: record the callback and succeed. A test can override this to make a
/// specific channel's subscription fail.
const listenMock = vi.fn(async (name: string, cb: (e: { payload: unknown }) => void) => {
  listeners.set(name, cb);
  return vi.fn();
});
type MockUnlisten = Awaited<ReturnType<typeof listenMock>>;
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, cb: (e: { payload: unknown }) => void) => listenMock(name, cb),
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
  // A compose operation is project-scoped and outlives its component, so a test
  // that leaves one in flight would keep every later test's composer busy.
  (await import("$lib/state/composeOperations.svelte"))._testing.reset();
  listeners.clear();
  // `listenMock` is module-level so a test can make one channel fail; reset it
  // here or that override leaks into every test after it.
  listenMock.mockReset();
  listenMock.mockImplementation(async (name: string, cb) => {
    listeners.set(name, cb);
    return vi.fn();
  });
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
      // Restored chips are reconciled against disk on mount. Default: every
      // declared path still exists, so nothing is pruned.
      if (cmd === "existing_attachment_paths") {
        return (args as { paths?: string[] })?.paths ?? [];
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

  it("outlines the compose box while focus is anywhere inside it, not just the textarea", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    const box = screen.getByTestId("compose-box");
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    const send = screen.getByTestId("compose-send");

    // The compose bar is the default keyboard target, so a permanent highlight
    // would say nothing — the focus border must be absent until focus lands here.
    expect(box).not.toHaveClass("border-focus");

    // Focus tracking is container-level (focusin bubbles), so any field inside
    // the box lights the border — this is what makes prompt/workflow-mode fields
    // highlight, not only the plain textarea.
    await fireEvent.focusIn(textarea);
    await tick();
    expect(box).toHaveClass("border-focus");

    // Moving focus to another child of the box (textarea → send button) keeps the
    // border: the containment guard clears only when focus leaves the container.
    await fireEvent.focusOut(textarea, { relatedTarget: send });
    await fireEvent.focusIn(send);
    await tick();
    expect(box).toHaveClass("border-focus");

    // Focus leaving the box entirely clears it.
    await fireEvent.focusOut(send, { relatedTarget: document.body });
    await tick();
    expect(box).not.toHaveClass("border-focus");
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

  it("opens prompt settings from the prompt menu", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const onConfigurePrompts = vi.fn();

    render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A], onConfigurePrompts },
    });

    await fireEvent.click(screen.getByTestId("compose-prompt-button"));
    await fireEvent.click(await screen.findByTestId("prompt-menu-configure"));

    expect(onConfigurePrompts).toHaveBeenCalledOnce();
    expect(screen.queryByTestId("prompt-menu")).toBeNull();
  });

  it("syncs and refreshes prompts in place from the prompt menu", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    let listCalls = 0;
    let syncCalls = 0;
    let releaseSync!: () => void;
    const syncGate = new Promise<void>((resolve) => {
      releaseSync = resolve;
    });
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_prompts") {
        listCalls += 1;
        return listCalls === 1 ? [REVIEW] : [REVIEW, SUMMARY];
      }
      if (cmd === "sync_prompts") {
        syncCalls += 1;
        await syncGate;
        return null;
      }
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await fireEvent.click(screen.getByTestId("compose-prompt-button"));
    await screen.findByTestId("prompt-option-local:review");

    const sync = screen.getByTestId("prompt-menu-sync");
    await fireEvent.click(sync);
    await waitFor(() => expect(sync).toHaveTextContent("Syncing…"));
    expect(sync).toBeDisabled();
    await fireEvent.click(sync);
    expect(syncCalls).toBe(1);

    releaseSync();
    await screen.findByTestId("prompt-option-tiddly:summary");
    expect(screen.getByTestId("prompt-menu")).toBeInTheDocument();
    expect(screen.getByTestId("prompt-menu-sync")).toBeEnabled();
    expect(listCalls).toBe(2);
  });

  it("keeps prompt-menu sync retryable and reports a failed rebuild", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_prompts") return [REVIEW];
      if (cmd === "sync_prompts") throw new Error("provider timed out");
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await fireEvent.click(screen.getByTestId("compose-prompt-button"));
    await fireEvent.click(await screen.findByTestId("prompt-menu-sync"));

    await waitFor(() =>
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent(
        "Couldn't sync prompts: provider timed out",
      ),
    );
    expect(screen.getByTestId("prompt-menu")).toBeInTheDocument();
    expect(screen.getByTestId("prompt-menu-sync")).toBeEnabled();
  });

  it("copies the workflow-authoring prompt from the workflow menu", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workflows" || cmd === "list_prompts" || cmd === "search_project_files")
        return [];
      if (cmd === "workflows_dir") return "/Users/test/workflows";
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    await fireEvent.click(screen.getByTestId("compose-workflow-button"));
    await fireEvent.click(await screen.findByTestId("workflow-menu-copy-authoring-prompt"));

    await waitFor(() => expect(copyTextMock).toHaveBeenCalledOnce());
    expect(copyTextMock.mock.calls[0]?.[0]).toContain(WORKFLOW_AUTHORING_GUIDE_URL);
    expect(copyTextMock.mock.calls[0]?.[0]).toContain("/Users/test/workflows");
    expect(await screen.findByText("Copied")).toBeInTheDocument();
  });

  it("inserts an unmatched slash query into the message without dispatching", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_prompts" || cmd === "search_project_files") return [];
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.keyDown(textarea, { key: "/" });
    const search = await screen.findByTestId("prompt-menu-search");
    await fireEvent.input(search, { target: { value: "plugin" } });
    await fireEvent.click(screen.getByTestId("prompt-menu-insert-message"));

    expect(textarea.value).toBe("/plugin");
    await waitFor(() => expect(textarea).toHaveFocus());
    expect(screen.queryByTestId("prompt-menu")).toBeNull();
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "send_message")).toBe(false);
  });

  it("does not invent a slash insertion when the toolbar opens the prompt menu", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_prompts" || cmd === "search_project_files") return [];
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    await fireEvent.click(screen.getByTestId("compose-prompt-button"));
    const search = await screen.findByTestId("prompt-menu-search");
    await fireEvent.input(search, { target: { value: "plugin" } });
    await fireEvent.keyDown(search, { key: "Enter" });

    expect(screen.queryByTestId("prompt-menu-insert-message")).toBeNull();
    expect(screen.getByTestId("prompt-menu")).toBeInTheDocument();
    expect((screen.getByTestId("compose-textarea") as HTMLTextAreaElement).value).toBe("");
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "send_message")).toBe(false);
  });

  it("dispatches the original slash-leading message after prompt-menu insertion", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_prompts" || cmd === "search_project_files") return [];
      if (cmd === "send_message") return "msg-slash";
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.keyDown(textarea, { key: "/" });
    const search = await screen.findByTestId("prompt-menu-search");
    await fireEvent.input(search, { target: { value: "plugin" } });
    await fireEvent.keyDown(search, { key: "Enter" });
    await fireEvent.keyDown(textarea, { key: "Enter", metaKey: true });

    await waitFor(() => {
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "send_message")).toHaveLength(1);
    });
    const send = invokeMock.mock.calls.find(([cmd]) => cmd === "send_message");
    expect(send?.[1]).toMatchObject({ prompt: "/plugin" });
  });

  it("preserves a pasted slash-leading message without opening the prompt menu", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "/message whatever" } });

    expect(textarea.value).toBe("/message whatever");
    expect(screen.queryByTestId("prompt-menu")).toBeNull();
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

  it("takes focus when the parent bumps focusRequest (pane Cmd+click)", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);

    // Default mount (focusOnMount unset) does not grab focus, so park it
    // elsewhere first to prove the bump — not the mount — pulls it in.
    const { rerender } = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A], focusRequest: 0 },
    });
    const outside = document.createElement("button");
    document.body.appendChild(outside);
    outside.focus();
    expect(document.activeElement).toBe(outside);

    await rerender({ projectId: PROJECT_ID, agents: [AGENT_A], focusRequest: 1 });
    await tick();

    expect(screen.getByTestId("compose-textarea")).toHaveFocus();
    outside.remove();
  });

  it("does not steal focus for the initial focusRequest value (remount baseline)", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);

    // A remount inherits the parent's running count; the effect's first run only
    // records that baseline, so a nonzero starting value must not pull focus.
    render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A], focusRequest: 5 },
    });
    const outside = document.createElement("button");
    document.body.appendChild(outside);
    outside.focus();
    await tick();

    expect(document.activeElement).toBe(outside);
    expect(screen.getByTestId("compose-textarea")).not.toHaveFocus();
    outside.remove();
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

  describe("fork (the send button's second half)", () => {
    const FORK_RECORD: AgentRecord = {
      id: "00000000-0000-7000-8000-000000000f0f",
      project_id: PROJECT_ID,
      name: "alice-fork",
      harness: "claude_code",
      session_locator: { uuid: "00000000-0000-7000-8000-00000000000f" },
      forked_from_session: "00000000-0000-7000-8000-000000000001",
      created_at: "2026-05-16T00:00:02Z",
    };

    /// Fork is an action, not a mode: the half is present exactly when it can be
    /// used. An unlabeled icon that is visible-but-dead explains nothing, so
    /// there is no disabled state to assert — only presence.
    const forkHalf = () => screen.queryByTestId("compose-fork-send");

    /// Give `agentId` hydrated history, so the "has a session to branch from"
    /// derivation is satisfied. Hydration (not a live send) is what an agent
    /// with an existing session looks like on project open — and unlike
    /// `dispatchUserTurn` it leaves the agent idle rather than working.
    function seedTurn(state: Awaited<ReturnType<typeof loadState>>, agentId: string): void {
      state.applyAgentHydrate(agentId, {
        turns: [
          {
            role: "agent",
            turn_id: `disk-${agentId}`,
            agent_id: agentId,
            items: [{ item_kind: "text", kind: "text", text: "earlier reply" }],
            status: "complete",
            started_at: "2026-05-15T00:00:00Z",
            ended_at: "2026-05-15T00:00:01Z",
            hydration_key: `key-${agentId}`,
          },
        ],
      });
    }

    it("is offered only for a single Claude recipient that has something to branch", async () => {
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      await state.registerAgent(AGENT_B);
      seedTurn(state, AGENT_A.id);
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

      await waitFor(() => expect(forkHalf()).not.toBeNull());
      expect(forkHalf()).toHaveAttribute("aria-label", expect.stringMatching(/fork alice/i));

      // Two recipients — a branch has no single source.
      await fireEvent.click(chip(AGENT_B.id));
      await waitFor(() => expect(forkHalf()).toBeNull());

      // A single non-Claude recipient — no harness support.
      await fireEvent.click(chip(AGENT_A.id));
      await waitFor(() => expect(forkHalf()).toBeNull());
    });

    it("is not offered while the agent has no session to branch from", async () => {
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

      await waitFor(() => expect(screen.getByTestId("compose-send")).toBeInTheDocument());
      expect(forkHalf()).toBeNull();
    });

    it("withdraws the offer while the agent is working", async () => {
      // Probe-measured hazard: a branch taken mid-turn inherits a synthesized
      // placeholder instead of the parent's in-flight answer.
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      seedTurn(state, AGENT_A.id);
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
      await waitFor(() => expect(forkHalf()).not.toBeNull());

      fireTo(`agent:${AGENT_A.id}`, {
        type: "turn_start",
        turn_id: "turn-busy",
        message_id: "msg-busy",
        send_id: "send-busy",
        started_at: "2026-05-16T00:00:00Z",
      });

      await waitFor(() => expect(forkHalf()).toBeNull());
    });

    it("stays offered while hydration is loading or has failed", async () => {
      // An empty transcript only means "no session" once hydration has
      // *completed*. Before that — and permanently after a failure — an agent
      // with a long history has an empty transcript, and withdrawing the offer
      // there would hide a branch the user can legitimately take. Unknown means
      // offer it; the backend refuses precisely if it really cannot branch.
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

      state.runtimes[AGENT_A.id] = {
        ...state.runtimes[AGENT_A.id]!,
        hydration_status: "loading",
      };
      await waitFor(() => expect(forkHalf()).not.toBeNull());

      state.runtimes[AGENT_A.id] = {
        ...state.runtimes[AGENT_A.id]!,
        hydration_status: "failed",
      };
      await waitFor(() => expect(forkHalf()).not.toBeNull());
    });

    it("is not offered while forward sources are attached", async () => {
      // The forward branch runs first in submit, so a fork would lose silently
      // and the message would go to the parent — the exact outcome the branch's
      // selection swap exists to prevent.
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      await state.registerAgent(AGENT_B);
      seedTurn(state, AGENT_A.id);
      seedTurn(state, AGENT_B.id);
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
      await waitFor(() => expect(forkHalf()).not.toBeNull());

      const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
      await fireEvent.input(textarea, { target: { value: "@" } });
      await fireEvent.click(
        await screen.findByTestId(`forward-option-forward-agent:${AGENT_B.id}`),
      );

      await waitFor(() => expect(forkHalf()).toBeNull());
    });

    it("returns to a circular cancel button once a send is in flight", async () => {
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      seedTurn(state, AGENT_A.id);
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
      await waitFor(() => expect(forkHalf()).not.toBeNull());

      const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
      await fireEvent.input(textarea, { target: { value: "go" } });
      await fireEvent.click(screen.getByTestId("compose-send"));
      fireTo(`agent:${AGENT_A.id}`, {
        type: "turn_start",
        turn_id: "turn-live",
        message_id: "msg-live",
        send_id: "send-live",
        started_at: "2026-05-16T00:00:00Z",
      });

      await waitFor(() =>
        expect(screen.getByTestId("compose-send")).toHaveAttribute("aria-label", "Cancel send"),
      );
      expect(forkHalf()).toBeNull();
      expect(screen.getByTestId("compose-send").className).toContain("w-7");
      expect(screen.getByTestId("compose-send-group").className).not.toContain("w-[3.25rem]");
    });

    it("withdraws the offer while any send is live, not just the recipient's own", async () => {
      // `showStop` is project-wide: another agent's live send turns this button
      // into a cancel while the selected agent sits idle and perfectly
      // forkable. Pairing "Cancel" with "Fork" in one control is the state the
      // in-flight rule exists to prevent — one half would abort someone else's
      // work, the other would start new work, a few pixels apart.
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      await state.registerAgent(AGENT_B);
      seedTurn(state, AGENT_A.id);
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

      // Send to bob alone, leaving alice idle.
      await fireEvent.click(chip(AGENT_B.id));
      await fireEvent.click(chip(AGENT_A.id));
      const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
      await fireEvent.input(textarea, { target: { value: "go" } });
      await fireEvent.click(screen.getByTestId("compose-send"));
      fireTo(`agent:${AGENT_B.id}`, {
        type: "turn_start",
        turn_id: "turn-bob",
        message_id: "msg-bob",
        send_id: "send-bob",
        started_at: "2026-05-16T00:00:00Z",
      });
      await waitFor(() =>
        expect(screen.getByTestId("compose-send")).toHaveAttribute("aria-label", "Cancel send"),
      );

      // Re-select alice: idle, Claude, has history — forkable in every respect
      // except that the button in front of the user is currently a cancel.
      await fireEvent.click(chip(AGENT_A.id));
      await fireEvent.click(chip(AGENT_B.id));
      await waitFor(() => expect(state.runtimes[AGENT_A.id]?.run_status).toBe("idle"));
      expect(forkHalf()).toBeNull();
    });

    it("is single-flight: a second click during the fork cannot create a second branch", async () => {
      // Two fork-sends across the await would each register a branch and each
      // dispatch the same text — two agents, message sent twice.
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      seedTurn(state, AGENT_A.id);
      let releaseFork!: (r: AgentRecord) => void;
      const pendingFork = new Promise<AgentRecord>((resolve) => {
        releaseFork = resolve;
      });
      invokeMock.mockImplementation(async (cmd: string) => {
        if (cmd === "fork_agent") return await pendingFork;
        if (cmd === "send_message") return "msg-fork";
        return null;
      });
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
      await waitFor(() => expect(forkHalf()).not.toBeNull());

      const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
      await fireEvent.input(textarea, { target: { value: "branch" } });
      await fireEvent.click(forkHalf()!);
      await fireEvent.click(screen.getByTestId("compose-fork-send"));

      releaseFork(FORK_RECORD);
      await waitFor(() => {
        expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "send_message")).toHaveLength(1);
      });
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "fork_agent")).toHaveLength(1);
    });

    it("keeps text typed while the fork is in flight", async () => {
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      seedTurn(state, AGENT_A.id);
      let releaseFork!: (r: AgentRecord) => void;
      const pendingFork = new Promise<AgentRecord>((resolve) => {
        releaseFork = resolve;
      });
      invokeMock.mockImplementation(async (cmd: string) => {
        if (cmd === "fork_agent") return await pendingFork;
        if (cmd === "send_message") return "msg-fork";
        return null;
      });
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
      await waitFor(() => expect(forkHalf()).not.toBeNull());

      const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
      await fireEvent.input(textarea, { target: { value: "branch" } });
      await fireEvent.click(forkHalf()!);
      await fireEvent.input(textarea, { target: { value: "next message" } });

      releaseFork(FORK_RECORD);
      await waitFor(() => {
        expect(invokeMock).toHaveBeenCalledWith("fork_agent", { agentId: AGENT_A.id });
      });
      await tick();
      expect((screen.getByTestId("compose-textarea") as HTMLTextAreaElement).value).toBe(
        "next message",
      );
    });

    it("forks, moves the selection to the branch, and sends there", async () => {
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      seedTurn(state, AGENT_A.id);
      invokeMock.mockImplementation(async (cmd: string) => {
        if (cmd === "fork_agent") return FORK_RECORD;
        if (cmd === "send_message") return "msg-fork";
        return null;
      });
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
      await waitFor(() => expect(forkHalf()).not.toBeNull());

      const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
      await fireEvent.input(textarea, { target: { value: "branch from here" } });
      await fireEvent.click(forkHalf()!);

      // The branch was created, and the message went to IT — not the parent.
      await waitFor(() => {
        expect(invokeMock).toHaveBeenCalledWith("fork_agent", { agentId: AGENT_A.id });
      });
      await waitFor(() => {
        expect(invokeMock).toHaveBeenCalledWith(
          "send_message",
          expect.objectContaining({ agentId: FORK_RECORD.id }),
        );
      });
      expect(invokeMock).not.toHaveBeenCalledWith(
        "send_message",
        expect.objectContaining({ agentId: AGENT_A.id }),
      );
    });

    it("the plain send button never forks, even when the fork half is offered", async () => {
      // The two halves sit against each other; pressing send must continue the
      // existing conversation, never branch it.
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      seedTurn(state, AGENT_A.id);
      invokeMock.mockImplementation(async (cmd: string) => {
        if (cmd === "send_message") return "msg-plain";
        return null;
      });
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
      await waitFor(() => expect(forkHalf()).not.toBeNull());

      const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
      await fireEvent.input(textarea, { target: { value: "keep going" } });
      await fireEvent.click(screen.getByTestId("compose-send"));

      await waitFor(() => {
        expect(invokeMock).toHaveBeenCalledWith(
          "send_message",
          expect.objectContaining({ agentId: AGENT_A.id }),
        );
      });
      expect(invokeMock).not.toHaveBeenCalledWith("fork_agent", expect.anything());
    });

    it("fork-sends from the keyboard without touching the send button", async () => {
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      seedTurn(state, AGENT_A.id);
      invokeMock.mockImplementation(async (cmd: string) => {
        if (cmd === "fork_agent") return FORK_RECORD;
        if (cmd === "send_message") return "msg-fork";
        return null;
      });
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
      await waitFor(() => expect(forkHalf()).not.toBeNull());

      const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
      await fireEvent.input(textarea, { target: { value: "branch by keyboard" } });
      await fireEvent.keyDown(textarea, { key: "Enter", metaKey: true, shiftKey: true });

      await waitFor(() => {
        expect(invokeMock).toHaveBeenCalledWith("fork_agent", { agentId: AGENT_A.id });
      });
    });

    it("stays silent on an empty composer, whatever else is blocking", async () => {
      // An empty composer has nothing to fork. Explaining "still loading X's
      // history" to someone who typed nothing answers a question they did not
      // ask, so the empty check has to sit above every readiness check.
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      seedTurn(state, AGENT_A.id);
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
      await waitFor(() => expect(forkHalf()).not.toBeNull());

      const rt = state.runtimes[AGENT_A.id];
      if (rt === undefined) throw new Error("runtime missing");
      state.runtimes[AGENT_A.id] = { ...rt, hydration_status: "loading" };

      const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
      await fireEvent.keyDown(textarea, { key: "Enter", metaKey: true, shiftKey: true });
      await tick();

      expect(screen.queryByTestId("compose-send-error")).toBeNull();
      expect(invokeMock).not.toHaveBeenCalledWith("fork_agent", expect.anything());
    });

    it("leaves nothing behind in the originating project when unmounted mid-fork", async () => {
      // Switching projects mid-fork destroys this bar, and `onDestroy` flushes
      // whatever compose state it holds at that moment. Clearing after the await
      // would persist the message the user already sent — they come back to find
      // it in the box, addressed to the parent, one keystroke from a duplicate
      // send. Clearing before the await means the flush persists cleared state.
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      seedTurn(state, AGENT_A.id);
      let releaseFork!: (r: AgentRecord) => void;
      const pendingFork = new Promise<AgentRecord>((resolve) => {
        releaseFork = resolve;
      });
      invokeMock.mockImplementation(async (cmd: string) => {
        if (cmd === "fork_agent") return await pendingFork;
        if (cmd === "send_message") return "msg-fork";
        return null;
      });
      const view = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
      await waitFor(() => expect(forkHalf()).not.toBeNull());

      const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
      await fireEvent.input(textarea, { target: { value: "branch from here" } });
      await fireEvent.click(forkHalf()!);

      view.unmount();
      releaseFork(FORK_RECORD);

      // The branch still receives its first message — abandoning it would leave
      // a promptless fork, which cannot materialize.
      await waitFor(() => {
        expect(invokeMock).toHaveBeenCalledWith(
          "send_message",
          expect.objectContaining({ agentId: FORK_RECORD.id }),
        );
      });
      // And the originating project is not holding the sent text.
      const compose = await import("$lib/state/composeStore");
      const content = compose.getCompose(PROJECT_ID).content;
      expect(content.kind === "plain" ? content.draft : "unexpected mode").toBe("");
    });

    it("loses nothing when the fork is refused after the user has typed again", async () => {
      // The textarea is not disabled during the await — typing while a fork
      // registers is expected. Restoring only into an empty composer silently
      // destroyed whichever message the user didn't get back.
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      seedTurn(state, AGENT_A.id);
      let rejectFork!: (e: Error) => void;
      const pendingFork = new Promise<AgentRecord>((_, reject) => {
        rejectFork = reject;
      });
      invokeMock.mockImplementation(async (cmd: string) => {
        if (cmd === "fork_agent") return await pendingFork;
        return null;
      });
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
      await waitFor(() => expect(forkHalf()).not.toBeNull());

      const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
      await fireEvent.input(textarea, { target: { value: "keep me" } });
      await fireEvent.click(forkHalf()!);
      await fireEvent.input(textarea, { target: { value: "also this" } });

      rejectFork(new Error("alice is working"));
      await waitFor(() => {
        expect(screen.getByTestId("compose-send-error")).toHaveTextContent("alice is working");
      });

      const value = (screen.getByTestId("compose-textarea") as HTMLTextAreaElement).value;
      expect(value).toContain("keep me");
      expect(value).toContain("also this");
    });

    it("does not overwrite a replacement composer for the same project", async () => {
      // Project scoping separates A from B, but not an obsolete A instance from
      // a newly mounted A instance. The dead one must not write its captured
      // state over the live one's.
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      seedTurn(state, AGENT_A.id);
      let rejectFork!: (e: Error) => void;
      const pendingFork = new Promise<AgentRecord>((_, reject) => {
        rejectFork = reject;
      });
      invokeMock.mockImplementation(async (cmd: string) => {
        if (cmd === "fork_agent") return await pendingFork;
        return null;
      });
      const first = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
      await waitFor(() => expect(forkHalf()).not.toBeNull());
      await fireEvent.input(screen.getByTestId("compose-textarea"), {
        target: { value: "original" },
      });
      await fireEvent.click(forkHalf()!);

      // Leave the project and come back: a new bar for the same project.
      first.unmount();
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
      await fireEvent.input(screen.getByTestId("compose-textarea"), {
        target: { value: "typed in the new composer" },
      });

      rejectFork(new Error("alice is working"));
      await tick();

      // The newer text survives, and the submitted one is still recoverable.
      const compose = await import("$lib/state/composeStore");
      const content = compose.getCompose(PROJECT_ID).content;
      const stored = content.kind === "plain" ? content.draft : "";
      expect(stored).toContain("typed in the new composer");
      expect(stored).toContain("original");
    });

    it("does not send into a branch whose updates it cannot hear", async () => {
      // The branch commits before the frontend subscribes. If subscribing fails,
      // dispatching anyway spends real quota on a turn whose events never arrive:
      // it sits at "starting" forever, the reply never renders, and inherited
      // history never loads. Tauri has no event replay, so subscribing later
      // cannot recover it. Keep the branch, keep the message, send nothing.
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      seedTurn(state, AGENT_A.id);
      listenMock.mockImplementation(async (channel: string) => {
        if (channel === `agent:${FORK_RECORD.id}`) throw new Error("channel unavailable");
        return vi.fn();
      });
      invokeMock.mockImplementation(async (cmd: string) => {
        if (cmd === "fork_agent") return FORK_RECORD;
        if (cmd === "send_message") return "msg-fork";
        return null;
      });
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
      await waitFor(() => expect(forkHalf()).not.toBeNull());

      const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
      await fireEvent.input(textarea, { target: { value: "branch from here" } });
      await fireEvent.click(forkHalf()!);

      await waitFor(() => {
        expect(invokeMock).toHaveBeenCalledWith("fork_agent", { agentId: AGENT_A.id });
      });
      // Wait for the *terminal* state before asserting what did not happen —
      // `send_message` is several async hops past `fork_agent`, so an absence
      // check taken any earlier passes vacuously.
      await waitFor(() =>
        expect(screen.getByTestId("compose-send-error")).toHaveTextContent(/created/i),
      );

      expect(invokeMock).not.toHaveBeenCalledWith("send_message", expect.anything());
      // The message is still the user's, and the error says what actually happened.
      expect((screen.getByTestId("compose-textarea") as HTMLTextAreaElement).value).toBe(
        "branch from here",
      );
      expect(screen.getByTestId("compose-send-error")).not.toHaveTextContent(/Fork failed/i);
    });

    it("keeps the user's text when the fork is refused", async () => {
      // The one send path that can fail before any optimistic turn exists. The
      // user must be able to wait or cancel and retry without retyping.
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      seedTurn(state, AGENT_A.id);
      invokeMock.mockImplementation(async (cmd: string) => {
        if (cmd === "fork_agent") throw new Error("alice is working");
        return null;
      });
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
      await waitFor(() => expect(forkHalf()).not.toBeNull());

      const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
      await fireEvent.input(textarea, { target: { value: "keep me" } });
      await fireEvent.click(forkHalf()!);

      await waitFor(() => {
        expect(screen.getByTestId("compose-send-error")).toHaveTextContent("alice is working");
      });
      expect((screen.getByTestId("compose-textarea") as HTMLTextAreaElement).value).toBe("keep me");
      expect(invokeMock).not.toHaveBeenCalledWith("send_message", expect.anything());
    });
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
    render?: () => Promise<unknown>;
    signIn?: () => Promise<unknown>;
    resolve?: () => Promise<unknown>;
  } = {},
): void {
  invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
    if (cmd === "search_project_files") return [];
    if (cmd === "list_prompts") return opts.prompts ?? [];
    if (cmd === "resolve_saved_prompt" || cmd === "resolve_saved_prompt_fresh") {
      if (opts.resolve) return await opts.resolve();
      const prompt = (opts.prompts ?? [])[0];
      return prompt
        ? { state: "available", prompt, generation: 0 }
        : { state: "confirmed_missing", generation: 0 };
    }
    if (cmd === "render_prompt")
      return opts.render ? await opts.render() : { kind: "rendered", text: "RENDERED" };
    if (cmd === "sign_in_mcp_provider") return opts.signIn ? await opts.signIn() : null;
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

describe("fork is confined to composers whose message it can actually dispatch", () => {
  // Every fork test used to run in plain mode and none asked whether the control
  // leaks out of it. The control moved into the send button and lost the
  // `{#if mode === "plain"}` wrapper the chip row had; nothing noticed, because
  // nothing looked. Prompt mode is now supported; the two shapes below are not.
  function seedTurn(state: Awaited<ReturnType<typeof loadState>>, agentId: string): void {
    state.applyAgentHydrate(agentId, {
      turns: [
        {
          role: "agent",
          turn_id: `disk-${agentId}`,
          agent_id: agentId,
          items: [{ item_kind: "text", kind: "text", text: "earlier reply" }],
          status: "complete",
          started_at: "2026-05-15T00:00:00Z",
          ended_at: "2026-05-15T00:00:01Z",
          hydration_key: `key-${agentId}`,
        },
      ],
    });
  }

  it("hides the fork half once a prompt field is filled from another agent", async () => {
    // A forward-backed prompt composes server-side and dispatches whenever its
    // sources settle — a different path (`dispatchForwardPrompt`) with an
    // unbounded hold. Branching there would either leave a promptless agent for
    // the duration or register one against a long-stale busy-parent gate.
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    seedTurn(state, AGENT_A.id);
    // bob is the forward source, and a source with no output is unpickable.
    seedTurn(state, AGENT_B.id);
    mockPromptBackend({ prompts: [REVIEW] });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    await waitFor(() => expect(screen.queryByTestId("compose-fork-send")).not.toBeNull());
    await enterPromptMode("prompt-option-local:review");
    // Still offered: an ordinary prompt is forkable.
    await waitFor(() => expect(screen.queryByTestId("compose-fork-send")).not.toBeNull());

    await fireEvent.click(screen.getByTestId("prompt-arg-forward-focus"));
    await fireEvent.click(await screen.findByTestId(`forward-picker-agent-${AGENT_B.id}`));

    await waitFor(() => expect(screen.queryByTestId("compose-fork-send")).toBeNull());
  });

  it("refuses the fork shortcut for a forward-backed prompt instead of sending it normally", async () => {
    // The dangerous fallback: the chord asks to branch, and the prompt-mode
    // Enter handler would send the composed prompt to the parent — the exact
    // agent the user was branching away from. The shortcut stays live where the
    // control is hidden, so this is the only surface the reason can reach.
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    seedTurn(state, AGENT_A.id);
    // bob is the forward source, and a source with no output is unpickable.
    seedTurn(state, AGENT_B.id);
    mockPromptBackend({ prompts: [REVIEW] });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await enterPromptMode("prompt-option-local:review");
    await fireEvent.click(screen.getByTestId("prompt-arg-forward-focus"));
    await fireEvent.click(await screen.findByTestId(`forward-picker-agent-${AGENT_B.id}`));

    // The handler gates on `document.activeElement`. `prompt-composer` is a
    // plain div with no tabindex, so calling `.focus()` on it does nothing —
    // focus a real control inside it and assert it landed, or this test passes
    // on whatever the previous click happened to leave focused.
    const field = screen.getByTestId("prompt-arg-focus") as HTMLElement;
    field.focus();
    expect(document.activeElement).toBe(field);
    await fireEvent.keyDown(field, { key: "Enter", metaKey: true, shiftKey: true });

    await waitFor(() =>
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent(/another agent/i),
    );
    expect(invokeMock).not.toHaveBeenCalledWith("fork_agent", expect.anything());
    expect(invokeMock).not.toHaveBeenCalledWith("send_message", expect.anything());
    expect(invokeMock).not.toHaveBeenCalledWith("render_prompt", expect.anything());
    expect(screen.getByTestId("prompt-composer")).toBeInTheDocument();
  });
});

describe("prompt-mode fork", () => {
  // Forking with a saved prompt is render → branch → dispatch. The render is the
  // long, fallible step (it can open a browser for an MCP sign-in and wait
  // minutes), so nothing durable happens until it succeeds — and nothing is
  // cleared until the send has actually dispatched, so every failure path hands
  // the prompt back by never having taken it.
  const FORK: AgentRecord = {
    id: "00000000-0000-7000-8000-000000000f0f",
    project_id: PROJECT_ID,
    name: "alice-fork",
    harness: "claude_code",
    session_locator: { uuid: "00000000-0000-7000-8000-00000000000f" },
    forked_from_session: "00000000-0000-7000-8000-000000000001",
    created_at: "2026-05-16T00:00:02Z",
  };

  const forkHalf = () => screen.queryByTestId("compose-fork-send");

  function seedTurn(state: Awaited<ReturnType<typeof loadState>>, agentId: string): void {
    state.applyAgentHydrate(agentId, {
      turns: [
        {
          role: "agent",
          turn_id: `disk-${agentId}`,
          agent_id: agentId,
          items: [{ item_kind: "text", kind: "text", text: "earlier reply" }],
          status: "complete",
          started_at: "2026-05-15T00:00:00Z",
          ended_at: "2026-05-15T00:00:01Z",
          hydration_key: `key-${agentId}`,
        },
      ],
    });
  }

  /// `render` / `fork` / `signIn` let a test hold any await open or fail it.
  function mockForkPromptBackend(
    opts: {
      render?: () => Promise<unknown>;
      fork?: () => Promise<unknown>;
      signIn?: () => Promise<unknown>;
    } = {},
  ): void {
    invokeMock.mockImplementation(
      async (cmd: string, args?: Record<string, unknown>): Promise<unknown> => {
        if (cmd === "search_project_files") return [];
        if (cmd === "stage_attachment") {
          const source = String((args as { sourcePath?: unknown })?.sourcePath ?? "drop");
          const name = source.split("/").pop() ?? source;
          return { path: `/proj/.switchboard/attachments/uuid__${name}`, original_name: name };
        }
        if (cmd === "list_prompts") return [SUMMARY, REVIEW];
        if (cmd === "resolve_saved_prompt" || cmd === "resolve_saved_prompt_fresh") {
          return {
            state: "available",
            prompt: args?.name === REVIEW.name ? REVIEW : SUMMARY,
            generation: 0,
          };
        }
        if (cmd === "render_prompt")
          return opts.render ? await opts.render() : { kind: "rendered", text: "RENDERED" };
        if (cmd === "sign_in_mcp_provider") return opts.signIn ? await opts.signIn() : null;
        if (cmd === "fork_agent") return opts.fork ? await opts.fork() : FORK;
        if (cmd === "send_message") return "msg-fork";
        return null;
      },
    );
  }

  /// First render demands a browser sign-in; the second (after it) succeeds.
  /// This is the only window in the lifecycle where pane targeting is
  /// deliberately unfrozen, because the wait is unbounded.
  function signInThenRender(): () => Promise<unknown> {
    let calls = 0;
    return () => {
      calls += 1;
      return Promise.resolve(
        calls === 1
          ? { kind: "needs_sign_in", provider: "tiddly" }
          : { kind: "rendered", text: "RENDERED" },
      );
    };
  }

  /// Mount with `alice` selected, hydrated, and composing the argument-free
  /// prompt — the shortest path to a forkable prompt-mode composer.
  async function mountComposingSummary(): Promise<Awaited<ReturnType<typeof loadState>>> {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    seedTurn(state, AGENT_A.id);
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await enterPromptMode("prompt-option-tiddly:summary");
    await waitFor(() => expect(forkHalf()).not.toBeNull());
    return state;
  }

  const sends = () => invokeMock.mock.calls.filter(([c]) => c === "send_message");
  const forks = () => invokeMock.mock.calls.filter(([c]) => c === "fork_agent");

  /// Press ⇧⌘↵ the way a user does. The window-level handler only acts when focus
  /// is *inside* the compose surface, so firing at `window` from a blurred
  /// document silently does nothing — a shortcut test that skips the focus step
  /// passes regardless of what the handler decides.
  async function pressForkShortcut(): Promise<void> {
    const field = (screen.queryByTestId("prompt-appended") ??
      screen.getByTestId("prompt-arg-focus")) as HTMLElement;
    field.focus();
    await fireEvent.keyDown(field, { key: "Enter", metaKey: true, shiftKey: true });
  }

  it("renders the prompt, branches, and sends the rendered text to the branch", async () => {
    mockForkPromptBackend();
    await mountComposingSummary();
    await fireEvent.input(screen.getByTestId("prompt-appended"), { target: { value: "tail" } });

    await fireEvent.click(forkHalf()!);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("fork_agent", { agentId: AGENT_A.id }),
    );
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "send_message",
        expect.objectContaining({ agentId: FORK.id, prompt: "RENDERED\n\ntail" }),
      ),
    );
    // Never the parent — that is the whole point of the branch.
    expect(invokeMock).not.toHaveBeenCalledWith(
      "send_message",
      expect.objectContaining({ agentId: AGENT_A.id }),
    );
    // A completed send returns to the plain composer, as any prompt send does.
    await waitFor(() => expect(screen.queryByTestId("prompt-composer")).toBeNull());
  });

  it("renders before branching, so a render failure leaves no agent behind", async () => {
    // Fork is a registry append: branching first and failing the render would put
    // a visibly empty agent in the roster that received nothing.
    mockForkPromptBackend({ render: () => Promise.reject(new Error("render boom")) });
    await mountComposingSummary();
    await fireEvent.input(screen.getByTestId("prompt-appended"), { target: { value: "tail" } });

    await fireEvent.click(forkHalf()!);

    await waitFor(() =>
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent("render boom"),
    );
    expect(forks()).toHaveLength(0);
    expect(sends()).toHaveLength(0);
    // And the prompt is exactly as the user left it.
    expect(screen.getByTestId("prompt-composer")).toBeInTheDocument();
    expect((screen.getByTestId("prompt-appended") as HTMLTextAreaElement).value).toBe("tail");
  });

  it("is single-flight across the render await", async () => {
    let releaseRender!: (v: unknown) => void;
    const pending = new Promise<unknown>((resolve) => {
      releaseRender = resolve;
    });
    mockForkPromptBackend({ render: () => pending });
    await mountComposingSummary();

    await fireEvent.click(forkHalf()!);
    await fireEvent.click(screen.getByTestId("compose-fork-send"));
    releaseRender({ kind: "rendered", text: "RENDERED" });

    await waitFor(() => expect(sends()).toHaveLength(1));
    expect(invokeMock.mock.calls.filter(([c]) => c === "render_prompt")).toHaveLength(1);
    expect(forks()).toHaveLength(1);
  });

  it("is single-flight across the registration await", async () => {
    // `sending` must span BOTH awaits with no gap. Released after the render, a
    // second submit would register a second branch and dispatch twice.
    let releaseFork!: (v: AgentRecord) => void;
    const pending = new Promise<AgentRecord>((resolve) => {
      releaseFork = resolve;
    });
    mockForkPromptBackend({ fork: () => pending });
    await mountComposingSummary();

    await fireEvent.click(forkHalf()!);
    await waitFor(() => expect(forks()).toHaveLength(1));
    // The prompt is still there — nothing clears until the send dispatches — and
    // the composer is visibly busy rather than accepting another submit.
    expect(screen.getByTestId("prompt-composer")).toHaveAttribute("aria-busy", "true");
    await pressForkShortcut();
    await tick();
    expect(forks()).toHaveLength(1);

    releaseFork(FORK);
    await waitFor(() => expect(sends()).toHaveLength(1));
    expect(forks()).toHaveLength(1);
  });

  it("stays single-flight for a bar remounted while the prompt is still rendering", async () => {
    // `sending` is component-local, so a replacement bar starts at `false`. Only
    // project-scoped operation state can refuse the second submit — and the
    // prompt is still on screen there, which is exactly what invites it.
    let releaseRender!: (v: unknown) => void;
    const pending = new Promise<unknown>((resolve) => {
      releaseRender = resolve;
    });
    mockForkPromptBackend({ render: () => pending });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    seedTurn(state, AGENT_A.id);
    const first = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await enterPromptMode("prompt-option-tiddly:summary");
    await waitFor(() => expect(forkHalf()).not.toBeNull());
    await fireEvent.click(forkHalf()!);

    first.unmount();
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await waitFor(() => expect(screen.queryByTestId("prompt-appended")).not.toBeNull());
    // The replacement shows the operation's own busy state, not a fresh idle one.
    expect(screen.getByTestId("prompt-composer")).toHaveAttribute("aria-busy", "true");
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(true);

    await pressForkShortcut();
    await tick();
    releaseRender({ kind: "rendered", text: "RENDERED" });

    await waitFor(() => expect(sends()).toHaveLength(1));
    expect(forks()).toHaveLength(1);
    expect(invokeMock.mock.calls.filter(([c]) => c === "render_prompt")).toHaveLength(1);
  });

  it("refuses when the parent starts working while the prompt is rendering", async () => {
    // The sign-in detour makes this window minutes wide, not milliseconds. A
    // branch taken mid-turn permanently inherits a placeholder instead of the
    // parent's real answer.
    let releaseRender!: (v: unknown) => void;
    const pending = new Promise<unknown>((resolve) => {
      releaseRender = resolve;
    });
    mockForkPromptBackend({ render: () => pending });
    await mountComposingSummary();
    await fireEvent.input(screen.getByTestId("prompt-appended"), { target: { value: "tail" } });

    await fireEvent.click(forkHalf()!);
    fireTo(`agent:${AGENT_A.id}`, {
      type: "turn_start",
      turn_id: "turn-busy",
      message_id: "msg-busy",
      send_id: "send-busy",
      started_at: "2026-05-16T00:00:00Z",
    });
    releaseRender({ kind: "rendered", text: "RENDERED" });

    await waitFor(() =>
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent(/alice is working/i),
    );
    expect(forks()).toHaveLength(0);
    expect(sends()).toHaveLength(0);
    expect((screen.getByTestId("prompt-appended") as HTMLTextAreaElement).value).toBe("tail");
  });

  it("refuses when a recipient is added during the sign-in wait", async () => {
    // Fork's precondition is *one* recipient, and the ordinary prompt send only
    // checks that its captured recipients are still selected — a subset test an
    // added recipient passes. The sign-in wait is the window where this is
    // reachable: pane targeting is deliberately unfrozen there, because the wait
    // is a browser round trip with no bound.
    let releaseSignIn!: () => void;
    const pendingSignIn = new Promise<unknown>((resolve) => {
      releaseSignIn = () => resolve(null);
    });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    seedTurn(state, AGENT_A.id);
    mockForkPromptBackend({ render: signInThenRender(), signIn: () => pendingSignIn });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await enterPromptMode("prompt-option-tiddly:summary");
    await waitFor(() => expect(forkHalf()).not.toBeNull());

    await fireEvent.click(forkHalf()!);
    const selection = await import("$lib/state/recipientSelection.svelte");
    await waitFor(() => expect(selection.targetRecipients(PROJECT_ID, [AGENT_A.id])).toBe(true));
    expect(selection.selectAgent(PROJECT_ID, AGENT_B.id)).toBe(true);
    releaseSignIn();

    await waitFor(() =>
      expect(screen.getByTestId("compose-send-notice")).toHaveTextContent(/composer changed/i),
    );
    expect(forks()).toHaveLength(0);
    expect(sends()).toHaveLength(0);
  });

  it("does not branch for a prompt that renders to nothing", async () => {
    // Claude refuses a promptless fork, so this would fail at the harness after
    // the branch was already committed. The check is on the combined transport
    // text, not the renderer's output alone.
    mockForkPromptBackend({ render: () => Promise.resolve({ kind: "rendered", text: "   " }) });
    await mountComposingSummary();

    await fireEvent.click(forkHalf()!);

    await waitFor(() =>
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent(/empty message/i),
    );
    expect(forks()).toHaveLength(0);
    expect(screen.getByTestId("prompt-composer")).toBeInTheDocument();
  });

  it("leaves the whole prompt untouched when registration fails", async () => {
    // Nothing was cleared, so there is nothing to restore and nothing that can
    // be lost to a collision with whatever the user did in the meantime.
    mockForkPromptBackend({ fork: () => Promise.reject(new Error("alice is working")) });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    seedTurn(state, AGENT_A.id);
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await enterPromptMode("prompt-option-local:review");
    await fireEvent.input(screen.getByTestId("prompt-arg-focus"), { target: { value: "tests" } });
    await fireEvent.input(screen.getByTestId("prompt-appended"), { target: { value: "tail" } });
    await waitFor(() => expect(forkHalf()).not.toBeNull());

    await fireEvent.click(forkHalf()!);

    await waitFor(() =>
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent("alice is working"),
    );
    expect(screen.getByTestId("prompt-composer")).toBeInTheDocument();
    expect((screen.getByTestId("prompt-arg-focus") as HTMLInputElement).value).toBe("tests");
    expect((screen.getByTestId("prompt-appended") as HTMLTextAreaElement).value).toBe("tail");
    expect(sends()).toHaveLength(0);
  });

  it("keeps hidden plain-mode forward sources when registration fails", async () => {
    // Message-level forwards are hidden in prompt mode but preserved for the
    // return trip. Clearing the composer wipes them, so a failure path that
    // cleared first would silently discard a forwarding setup the user built
    // before ever choosing the prompt.
    mockForkPromptBackend({ fork: () => Promise.reject(new Error("alice is working")) });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    seedTurn(state, AGENT_A.id);
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    // ⌘⌃1 forwards pane 1 as one chip per member agent.
    await fireEvent.keyDown(window, { key: "1", metaKey: true, ctrlKey: true });
    await waitFor(() => expect(screen.queryByTestId("forward-source-chip-alice")).not.toBeNull());
    await enterPromptMode("prompt-option-tiddly:summary");
    await waitFor(() => expect(forkHalf()).not.toBeNull());

    await fireEvent.click(forkHalf()!);

    await waitFor(() =>
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent("alice is working"),
    );
    expect(screen.getByTestId("prompt-composer")).toBeInTheDocument();
    const compose = await loadComposeStore();
    expect(compose.getCompose(PROJECT_ID).forwards?.message ?? []).toHaveLength(1);
  });

  it("clears hidden plain-mode forward sources only after the branch is sent", async () => {
    mockForkPromptBackend();
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    seedTurn(state, AGENT_A.id);
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await fireEvent.keyDown(window, { key: "1", metaKey: true, ctrlKey: true });
    await waitFor(() => expect(screen.queryByTestId("forward-source-chip-alice")).not.toBeNull());
    await enterPromptMode("prompt-option-tiddly:summary");
    await waitFor(() => expect(forkHalf()).not.toBeNull());

    await fireEvent.click(forkHalf()!);

    await waitFor(() => expect(sends()).toHaveLength(1));
    const compose = await loadComposeStore();
    await waitFor(() =>
      expect(compose.getCompose(PROJECT_ID).forwards?.message ?? []).toHaveLength(0),
    );
  });

  it("keeps the prompt when the branch commits but its updates cannot be reached", async () => {
    // Committed is not reachable: dispatching into a branch whose event channel
    // failed spends real work on a turn that never renders, and Tauri has no
    // replay. The branch stays visible with its retry; the prompt stays put.
    listenMock.mockImplementation(async (name: string, cb) => {
      if (name === `agent:${FORK.id}`) throw new Error("channel refused");
      listeners.set(name, cb);
      return vi.fn();
    });
    mockForkPromptBackend();
    await mountComposingSummary();
    await fireEvent.input(screen.getByTestId("prompt-appended"), { target: { value: "tail" } });

    await fireEvent.click(forkHalf()!);

    await waitFor(() =>
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent(/couldn't connect/i),
    );
    expect(forks()).toHaveLength(1);
    expect(sends()).toHaveLength(0);
    expect(screen.getByTestId("prompt-composer")).toBeInTheDocument();
    expect((screen.getByTestId("prompt-appended") as HTMLTextAreaElement).value).toBe("tail");
  });

  it("leaves nothing behind in the originating project when unmounted mid-fork", async () => {
    // `onDestroy` flushes whatever compose state the bar holds at unmount, and
    // the continuation dispatches regardless. The dispatched prompt must not be
    // left sitting in the project's saved state, one keystroke from a duplicate.
    let releaseFork!: (v: AgentRecord) => void;
    const pending = new Promise<AgentRecord>((resolve) => {
      releaseFork = resolve;
    });
    mockForkPromptBackend({ fork: () => pending });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    seedTurn(state, AGENT_A.id);
    const view = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await enterPromptMode("prompt-option-tiddly:summary");
    await fireEvent.input(screen.getByTestId("prompt-appended"), { target: { value: "tail" } });
    await waitFor(() => expect(forkHalf()).not.toBeNull());
    await fireEvent.click(forkHalf()!);
    await waitFor(() => expect(forks()).toHaveLength(1));

    view.unmount();
    releaseFork(FORK);

    // The branch still gets its first message: abandoning it leaves a promptless
    // fork, which can never materialize.
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "send_message",
        expect.objectContaining({ agentId: FORK.id }),
      ),
    );
    const compose = await loadComposeStore();
    const saved = compose.getCompose(PROJECT_ID);
    expect(saved.content).toEqual({ kind: "plain", draft: "" });
    expect(saved.attachments ?? []).toHaveLength(0);
  });

  it("aborts rather than branching when the store diverges under the render", async () => {
    // The defect this round exists to fix. A ComposeBar reads the store once at
    // mount and pushes its locals down, so an obsolete instance finishing later
    // cannot see the replacement's edits — it would clear the store over them,
    // dispatch text the user had already replaced, and retarget their recipients.
    // Nothing is committed before registration, so the correct answer is to stop.
    let releaseRender!: (v: unknown) => void;
    const pending = new Promise<unknown>((resolve) => {
      releaseRender = resolve;
    });
    mockForkPromptBackend({ render: () => pending });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    seedTurn(state, AGENT_A.id);
    const first = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await enterPromptMode("prompt-option-tiddly:summary");
    await waitFor(() => expect(forkHalf()).not.toBeNull());
    await fireEvent.click(forkHalf()!);

    // Leave and return while the render is still open. The composer is frozen —
    // that is the point of the project-scoped busy state — so the divergence is
    // driven through the store the way a late-resolving attachment staging or an
    // external write would, which is what this test is actually about.
    first.unmount();
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await waitFor(() => expect(screen.queryByTestId("prompt-appended")).not.toBeNull());
    const store = await loadComposeStore();
    store.setContent(PROJECT_ID, {
      kind: "prompt",
      provider: "tiddly",
      name: "summary",
      args: {},
      appendedText: "EDITED",
    });

    releaseRender({ kind: "rendered", text: "RENDERED" });
    await waitFor(() =>
      expect(screen.getByTestId("compose-send-notice")).toHaveTextContent(/composer changed/i),
    );

    expect(forks()).toHaveLength(0);
    expect(sends()).toHaveLength(0);
    const sel = await import("$lib/state/recipientSelection.svelte");
    expect(sel.selectionFor(PROJECT_ID)).toEqual([AGENT_A.id]);
    const compose = await loadComposeStore();
    expect(compose.getCompose(PROJECT_ID).content).toEqual({
      kind: "prompt",
      provider: "tiddly",
      name: "summary",
      args: {},
      appendedText: "EDITED",
    });
    // The store holds the newer content; nothing overwrote it.
    expect((compose.getCompose(PROJECT_ID).content as { appendedText: string }).appendedText).toBe(
      "EDITED",
    );
  });

  it("dispatches but preserves a replacement composer edited after registration began", async () => {
    // Past registration the branch exists, so the send is no longer optional —
    // abandoning it leaves a promptless fork. The newer composer wins instead,
    // and the user is told the branch got the message.
    let releaseFork!: (v: AgentRecord) => void;
    const pending = new Promise<AgentRecord>((resolve) => {
      releaseFork = resolve;
    });
    mockForkPromptBackend({ fork: () => pending });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    seedTurn(state, AGENT_A.id);
    const first = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] },
    });
    await enterPromptMode("prompt-option-tiddly:summary");
    await waitFor(() => expect(forkHalf()).not.toBeNull());
    await fireEvent.click(forkHalf()!);
    await waitFor(() => expect(forks()).toHaveLength(1));

    first.unmount();
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await waitFor(() => expect(screen.queryByTestId("prompt-appended")).not.toBeNull());
    const store = await loadComposeStore();
    store.setContent(PROJECT_ID, {
      kind: "prompt",
      provider: "tiddly",
      name: "summary",
      args: {},
      appendedText: "NEWER",
    });
    const sel = await import("$lib/state/recipientSelection.svelte");
    sel.setRecipients(PROJECT_ID, [AGENT_B.id]);

    releaseFork(FORK);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "send_message",
        expect.objectContaining({ agentId: FORK.id }),
      ),
    );
    await waitFor(() =>
      expect(screen.getByTestId("compose-send-notice")).toHaveTextContent(/newer draft/i),
    );
    expect((store.getCompose(PROJECT_ID).content as { appendedText?: string }).appendedText).toBe(
      "NEWER",
    );
    expect(sel.selectionFor(PROJECT_ID)).toEqual([AGENT_B.id]);
  });

  it("clears a replacement composer it did consume, and selects the branch there", async () => {
    // The mirror case: nothing changed while the operation ran, so the prompt on
    // screen IS the one that was just sent. Clearing the store alone would leave
    // it displayed — the replacement holds its own copy in local state — one
    // keystroke from sending it twice.
    let releaseFork!: (v: AgentRecord) => void;
    const pending = new Promise<AgentRecord>((resolve) => {
      releaseFork = resolve;
    });
    mockForkPromptBackend({ fork: () => pending });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    seedTurn(state, AGENT_A.id);
    const first = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await enterPromptMode("prompt-option-tiddly:summary");
    await waitFor(() => expect(forkHalf()).not.toBeNull());
    await fireEvent.click(forkHalf()!);
    await waitFor(() => expect(forks()).toHaveLength(1));

    first.unmount();
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, FORK] } });
    await waitFor(() => expect(screen.queryByTestId("prompt-composer")).not.toBeNull());

    releaseFork(FORK);
    await waitFor(() => expect(sends()).toHaveLength(1));

    await waitFor(() => expect(screen.queryByTestId("prompt-composer")).toBeNull());
    const compose = await loadComposeStore();
    expect(compose.getCompose(PROJECT_ID).content).toEqual({ kind: "plain", draft: "" });
    const sel = await import("$lib/state/recipientSelection.svelte");
    expect(sel.selectionFor(PROJECT_ID)).toEqual([FORK.id]);
  });

  it("moves the selection to the branch even when it finishes off-screen", async () => {
    // The persisted recipient list is synced by an effect that only runs while a
    // bar is mounted. A fork finishing while the user is in another project used
    // to move only the live selection; remounting then seeded from the stale
    // saved copy and put the parent back — so the next message went to the agent
    // they had just branched away from.
    let releaseFork!: (v: AgentRecord) => void;
    const pending = new Promise<AgentRecord>((resolve) => {
      releaseFork = resolve;
    });
    mockForkPromptBackend({ fork: () => pending });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    seedTurn(state, AGENT_A.id);
    const first = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await enterPromptMode("prompt-option-tiddly:summary");
    await waitFor(() => expect(forkHalf()).not.toBeNull());
    await fireEvent.click(forkHalf()!);
    await waitFor(() => expect(forks()).toHaveLength(1));

    // Leave project A entirely and let the branch finish with nothing mounted.
    first.unmount();
    releaseFork(FORK);
    await waitFor(() => expect(sends()).toHaveLength(1));

    const compose = await loadComposeStore();
    const sel = await import("$lib/state/recipientSelection.svelte");
    expect(compose.getCompose(PROJECT_ID).selectedIds).toEqual([FORK.id]);

    // And coming back does not undo it.
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, FORK] } });
    await waitFor(() => expect(screen.queryByTestId("compose-textarea")).not.toBeNull());
    expect(sel.selectionFor(PROJECT_ID)).toEqual([FORK.id]);
  });

  it("moves the selection to an unreachable branch that finished off-screen", async () => {
    // Same rule on the committed-but-unsubscribed path: the retry has to target
    // the branch, so the selection must survive the round trip there too.
    listenMock.mockImplementation(async (name: string, cb) => {
      if (name === `agent:${FORK.id}`) throw new Error("channel refused");
      listeners.set(name, cb);
      return vi.fn();
    });
    let releaseFork!: (v: AgentRecord) => void;
    const pending = new Promise<AgentRecord>((resolve) => {
      releaseFork = resolve;
    });
    mockForkPromptBackend({ fork: () => pending });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    seedTurn(state, AGENT_A.id);
    const first = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await enterPromptMode("prompt-option-tiddly:summary");
    await waitFor(() => expect(forkHalf()).not.toBeNull());
    await fireEvent.click(forkHalf()!);
    await waitFor(() => expect(forks()).toHaveLength(1));

    first.unmount();
    releaseFork(FORK);
    const compose = await loadComposeStore();
    await waitFor(() => expect(compose.getCompose(PROJECT_ID).selectedIds).toEqual([FORK.id]));
    expect(sends()).toHaveLength(0);
  });

  it("freezes every compose control in a bar remounted mid-operation", async () => {
    // `sending` resets to false in a replacement bar, so gating on it left
    // controls that look live but mutate the snapshot the in-flight operation is
    // comparing against — recipients, staged files, and the mode buttons. The
    // rule is one policy for every compose-mutating control.
    let releaseRender!: (v: unknown) => void;
    const pending = new Promise<unknown>((resolve) => {
      releaseRender = resolve;
    });
    mockForkPromptBackend({ render: () => pending });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    seedTurn(state, AGENT_A.id);
    const first = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] },
    });
    await enterPromptMode("prompt-option-tiddly:summary");
    await waitFor(() => expect(forkHalf()).not.toBeNull());
    await fireEvent.click(forkHalf()!);

    first.unmount();
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await waitFor(() => expect(screen.queryByTestId("prompt-composer")).not.toBeNull());

    // The mode-switch buttons live inside `{#if mode === "plain"}`, so prompt
    // mode never renders them; what a remounted bar does still show is the
    // recipient row, which mutates the very selection the snapshot compares.
    const disabled = (id: string) => (screen.getByTestId(id) as HTMLButtonElement).disabled;
    expect(disabled("recipient-clear")).toBe(true);
    expect(disabled(`recipient-chip-${AGENT_B.id}`)).toBe(true);

    // Keyboard paths too: select-all and per-agent toggle both mutate recipients.
    const before = (await import("$lib/state/recipientSelection.svelte")).selectionFor(PROJECT_ID);
    await fireEvent.keyDown(window, { key: "a", metaKey: true, shiftKey: true });
    await fireEvent.keyDown(window, { key: "2", metaKey: true });
    expect((await import("$lib/state/recipientSelection.svelte")).selectionFor(PROJECT_ID)).toEqual(
      before,
    );

    releaseRender({ kind: "rendered", text: "RENDERED" });
    await waitFor(() => expect(sends()).toHaveLength(1));
  });

  it("delivers a failure once, to whichever bar is mounted, and does not repeat it", async () => {
    // The outcome is project-scoped so it can reach a replacement composer, but
    // rendering straight from that shared state kept a stale "Fork failed" alive
    // through every later action and every revisit of the project.
    let rejectFork!: (e: Error) => void;
    const pending = new Promise<AgentRecord>((_, reject) => {
      rejectFork = reject;
    });
    mockForkPromptBackend({ fork: () => pending });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    seedTurn(state, AGENT_A.id);
    const first = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await enterPromptMode("prompt-option-tiddly:summary");
    await waitFor(() => expect(forkHalf()).not.toBeNull());
    await fireEvent.click(forkHalf()!);
    await waitFor(() => expect(forks()).toHaveLength(1));

    // Fail while nothing is mounted, then come back.
    first.unmount();
    rejectFork(new Error("alice is working"));
    const second = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await waitFor(() =>
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent("alice is working"),
    );

    // Leaving and returning again must not resurrect it.
    second.unmount();
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await waitFor(() => expect(screen.queryByTestId("prompt-composer")).not.toBeNull());
    expect(screen.queryByTestId("compose-send-error")).toBeNull();
  });

  it("delivers a failure to a composer that was already mounted when it landed", async () => {
    // A mount-time read would miss this — and this is the common shape, since a
    // failure usually lands after the user has navigated back.
    let rejectFork!: (e: Error) => void;
    const pending = new Promise<AgentRecord>((_, reject) => {
      rejectFork = reject;
    });
    mockForkPromptBackend({ fork: () => pending });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    seedTurn(state, AGENT_A.id);
    const first = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await enterPromptMode("prompt-option-tiddly:summary");
    await waitFor(() => expect(forkHalf()).not.toBeNull());
    await fireEvent.click(forkHalf()!);
    await waitFor(() => expect(forks()).toHaveLength(1));

    first.unmount();
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await waitFor(() => expect(screen.queryByTestId("prompt-composer")).not.toBeNull());
    rejectFork(new Error("alice is working"));

    await waitFor(() =>
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent("alice is working"),
    );
  });

  it("holds the project against a second send or a fork while an ordinary send renders", async () => {
    // The sequence that made this worth doing: an ordinary prompt send is still
    // rendering, the user switches away and back, and the replacement composer
    // shows the same prompt with an enabled Send and an available Fork. Pressing
    // Send sent it twice; pressing Fork sent it to the parent *and* to a new
    // branch — one action, two sends, two agents.
    let releaseRender!: (v: unknown) => void;
    const pending = new Promise<unknown>((resolve) => {
      releaseRender = resolve;
    });
    mockForkPromptBackend({ render: () => pending });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    seedTurn(state, AGENT_A.id);
    const first = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await enterPromptMode("prompt-option-tiddly:summary");
    await fireEvent.click(screen.getByTestId("compose-send"));
    await waitFor(() =>
      expect(invokeMock.mock.calls.filter(([c]) => c === "render_prompt")).toHaveLength(1),
    );

    first.unmount();
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await waitFor(() => expect(screen.queryByTestId("prompt-composer")).not.toBeNull());

    // Neither second operation may start. The fork half is still *rendered* —
    // visibility tracks shape, and the shape is fine — but busy is readiness, so
    // both halves are disabled.
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(true);
    expect((forkHalf() as HTMLButtonElement).disabled).toBe(true);
    await pressForkShortcut();
    await tick();
    expect(forks()).toHaveLength(0);

    releaseRender({ kind: "rendered", text: "RENDERED" });

    // Exactly one dispatch, to the captured recipient, and the composer retires.
    await waitFor(() => expect(sends()).toHaveLength(1));
    expect((sends()[0]![1] as { agentId: string }).agentId).toBe(AGENT_A.id);
    expect(invokeMock.mock.calls.filter(([c]) => c === "render_prompt")).toHaveLength(1);
    const compose = await loadComposeStore();
    await waitFor(() =>
      expect(compose.getCompose(PROJECT_ID).content).toEqual({ kind: "plain", draft: "" }),
    );
  });

  it("delivers an ordinary prompt send's failure to a bar that replaced its own", async () => {
    // Its error used to be written into a destroyed component: the replacement
    // kept the prompt and got no explanation for why nothing happened.
    let rejectRender!: (e: Error) => void;
    const pending = new Promise<unknown>((_, reject) => {
      rejectRender = reject;
    });
    mockForkPromptBackend({ render: () => pending });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    seedTurn(state, AGENT_A.id);
    const first = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await enterPromptMode("prompt-option-tiddly:summary");
    await fireEvent.click(screen.getByTestId("compose-send"));
    await waitFor(() =>
      expect(invokeMock.mock.calls.filter(([c]) => c === "render_prompt")).toHaveLength(1),
    );

    first.unmount();
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await waitFor(() => expect(screen.queryByTestId("prompt-composer")).not.toBeNull());
    rejectRender(new Error("provider unreachable"));

    await waitFor(() =>
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent("provider unreachable"),
    );
    // And the prompt is still there to retry with.
    expect(screen.getByTestId("prompt-composer")).toBeInTheDocument();
  });

  it("still retires the prompt when a recipient was added mid-send", async () => {
    // Retiring consumed content asks about content, not recipients: the ordinary
    // send deliberately tolerates a recipient being added during the sign-in
    // window, and folding recipients into the clear decision left the just-sent
    // prompt in the composer, ready to be sent again.
    let releaseSignIn!: () => void;
    const pendingSignIn = new Promise<unknown>((resolve) => {
      releaseSignIn = () => resolve(null);
    });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    seedTurn(state, AGENT_A.id);
    mockForkPromptBackend({ render: signInThenRender(), signIn: () => pendingSignIn });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await enterPromptMode("prompt-option-tiddly:summary");
    await fireEvent.click(screen.getByTestId("compose-send"));

    const selection = await import("$lib/state/recipientSelection.svelte");
    await waitFor(() => expect(selection.targetRecipients(PROJECT_ID, [AGENT_A.id])).toBe(true));
    expect(selection.selectAgent(PROJECT_ID, AGENT_B.id)).toBe(true);
    releaseSignIn();

    await waitFor(() => expect(sends()).toHaveLength(1));
    // Sent to the captured recipient only, and the prompt is gone rather than
    // left behind for an accidental second send.
    expect((sends()[0]![1] as { agentId: string }).agentId).toBe(AGENT_A.id);
    await waitFor(() => expect(screen.queryByTestId("prompt-composer")).toBeNull());
  });

  it("does not let an ordinary send erase a replacement composer's newer prompt", async () => {
    // The bug this fix exists for, in the path it was overlooked in: the clear
    // at the end of an ordinary prompt send ran unconditionally in a
    // continuation that outlives its component, writing a dead instance's
    // emptied locals over whatever the live composer holds.
    let releaseRender!: (v: unknown) => void;
    const pending = new Promise<unknown>((resolve) => {
      releaseRender = resolve;
    });
    mockForkPromptBackend({ render: () => pending });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    seedTurn(state, AGENT_A.id);
    const first = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await enterPromptMode("prompt-option-tiddly:summary");
    await fireEvent.click(screen.getByTestId("compose-send"));
    await waitFor(() =>
      expect(invokeMock.mock.calls.filter(([c]) => c === "render_prompt")).toHaveLength(1),
    );

    first.unmount();
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await waitFor(() => expect(screen.queryByTestId("prompt-composer")).not.toBeNull());
    // The composer is frozen while the operation owns it, so the divergence is
    // seeded at the store — this is an algorithm-level test of the compare-and-set.
    const compose = await loadComposeStore();
    compose.setContent(PROJECT_ID, {
      kind: "prompt",
      provider: "tiddly",
      name: "summary",
      args: {},
      appendedText: "NEWER",
    });

    releaseRender({ kind: "rendered", text: "RENDERED" });
    await waitFor(() => expect(sends()).toHaveLength(1));

    // Dispatched, and the newer content survived.
    expect((compose.getCompose(PROJECT_ID).content as { appendedText?: string }).appendedText).toBe(
      "NEWER",
    );
  });

  it("replaces a stale status rather than stacking a notice under an error", async () => {
    // A blocked shortcut sets "Already sending"; the operation then finishes with
    // a notice. Rendering both leaves the composer describing two states at once.
    let releaseFork!: (v: AgentRecord) => void;
    const pending = new Promise<AgentRecord>((resolve) => {
      releaseFork = resolve;
    });
    mockForkPromptBackend({ fork: () => pending });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    seedTurn(state, AGENT_A.id);
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await enterPromptMode("prompt-option-tiddly:summary");
    await waitFor(() => expect(forkHalf()).not.toBeNull());
    await fireEvent.click(forkHalf()!);
    await waitFor(() => expect(forks()).toHaveLength(1));

    await pressForkShortcut();
    await waitFor(() =>
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent(/already sending/i),
    );

    // Diverge so the operation finishes with the preserve-newer-composer notice.
    const compose = await loadComposeStore();
    compose.setContent(PROJECT_ID, { kind: "plain", draft: "something else" });
    releaseFork(FORK);

    await waitFor(() =>
      expect(screen.getByTestId("compose-send-notice")).toHaveTextContent(/newer draft/i),
    );
    expect(screen.queryByTestId("compose-send-error")).toBeNull();

    // And the other direction: a fresh error retires the notice. (The composer
    // kept prompt mode — its content diverged, so it was preserved, not retired.)
    await fireEvent.click(chip(AGENT_B.id));
    await pressForkShortcut();

    await waitFor(() =>
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent(/single recipient/i),
    );
    expect(screen.queryByTestId("compose-send-notice")).toBeNull();
  });

  it("lets the user stop waiting on a stuck sign-in, and keeps their prompt", async () => {
    // The sign-in's final step is deliberately un-timed in the backend, so a
    // stall there would otherwise hold this project's composer until the app
    // restarts — the busy state is project-scoped, so switching away no longer
    // hides it.
    const neverSettles = new Promise<unknown>(() => {});
    mockForkPromptBackend({ render: signInThenRender(), signIn: () => neverSettles });
    await mountComposingSummary();
    await fireEvent.input(screen.getByTestId("prompt-appended"), { target: { value: "tail" } });
    await fireEvent.click(forkHalf()!);

    await waitFor(() => expect(screen.queryByTestId("compose-abandon-wait")).not.toBeNull());
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(true);

    await fireEvent.click(screen.getByTestId("compose-abandon-wait"));

    // The composer is usable again, in the bar that started it, with the prompt
    // still there to retry with.
    await waitFor(() =>
      expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(false),
    );
    expect(screen.getByTestId("prompt-composer")).toBeInTheDocument();
    expect((screen.getByTestId("prompt-appended") as HTMLTextAreaElement).value).toBe("tail");
    expect(screen.getByTestId("compose-send-notice")).toHaveTextContent(/stopped waiting/i);
  });

  it("lets a replacement bar stop waiting on an operation it did not start", async () => {
    const neverSettles = new Promise<unknown>(() => {});
    mockForkPromptBackend({ render: signInThenRender(), signIn: () => neverSettles });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    seedTurn(state, AGENT_A.id);
    const first = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await enterPromptMode("prompt-option-tiddly:summary");
    await waitFor(() => expect(forkHalf()).not.toBeNull());
    await fireEvent.click(forkHalf()!);
    await waitFor(() => expect(screen.queryByTestId("compose-abandon-wait")).not.toBeNull());

    first.unmount();
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    // The wait belongs to the project, so the replacement shows it and can end it.
    await waitFor(() => expect(screen.queryByTestId("compose-abandon-wait")).not.toBeNull());
    await fireEvent.click(screen.getByTestId("compose-abandon-wait"));

    await waitFor(() =>
      expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(false),
    );
  });

  it("ignores a sign-in that succeeds after the user stopped waiting", async () => {
    // The abandoned continuation must not render again, register, dispatch,
    // clear, retarget, or publish a status — it no longer owns the composer.
    let releaseSignIn!: () => void;
    const pendingSignIn = new Promise<unknown>((resolve) => {
      releaseSignIn = () => resolve(null);
    });
    mockForkPromptBackend({ render: signInThenRender(), signIn: () => pendingSignIn });
    await mountComposingSummary();
    await fireEvent.click(forkHalf()!);
    await waitFor(() => expect(screen.queryByTestId("compose-abandon-wait")).not.toBeNull());
    await fireEvent.click(screen.getByTestId("compose-abandon-wait"));

    const rendersBefore = invokeMock.mock.calls.filter(([c]) => c === "render_prompt").length;
    releaseSignIn();
    await tick();
    await tick();

    expect(invokeMock.mock.calls.filter(([c]) => c === "render_prompt")).toHaveLength(
      rendersBefore,
    );
    expect(forks()).toHaveLength(0);
    expect(sends()).toHaveLength(0);
    expect(screen.getByTestId("prompt-composer")).toBeInTheDocument();
    // The abandon notice is still the current status — nothing stamped over it.
    expect(screen.getByTestId("compose-send-notice")).toHaveTextContent(/stopped waiting/i);
  });

  it("lets a new operation start after an abandonment, untouched by the old one", async () => {
    let releaseSignIn!: () => void;
    const pendingSignIn = new Promise<unknown>((resolve) => {
      releaseSignIn = () => resolve(null);
    });
    let calls = 0;
    mockForkPromptBackend({
      render: () => {
        calls += 1;
        return Promise.resolve(
          calls === 1
            ? { kind: "needs_sign_in", provider: "tiddly" }
            : { kind: "rendered", text: "RENDERED" },
        );
      },
      signIn: () => pendingSignIn,
    });
    await mountComposingSummary();
    await fireEvent.click(forkHalf()!);
    await waitFor(() => expect(screen.queryByTestId("compose-abandon-wait")).not.toBeNull());
    await fireEvent.click(screen.getByTestId("compose-abandon-wait"));

    // Retry: a fresh operation claims the slot and completes normally.
    await waitFor(() => expect(forkHalf()).not.toBeNull());
    await fireEvent.click(forkHalf()!);
    await waitFor(() => expect(sends()).toHaveLength(1));
    expect(forks()).toHaveLength(1);

    // The abandoned sign-in resolving afterwards changes nothing.
    releaseSignIn();
    await tick();
    await tick();
    expect(sends()).toHaveLength(1);
    expect(forks()).toHaveLength(1);
  });

  it("holds the project across a remount while a plain fork registers", async () => {
    // Plain fork is the third caller of the boundary. Outside it, a replacement
    // bar accepted a send that the fork then invalidated by moving the recipient.
    let releaseFork!: (v: AgentRecord) => void;
    const pending = new Promise<AgentRecord>((resolve) => {
      releaseFork = resolve;
    });
    mockForkPromptBackend({ fork: () => pending });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    seedTurn(state, AGENT_A.id);
    const first = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await waitFor(() => expect(forkHalf()).not.toBeNull());
    await fireEvent.input(screen.getByTestId("compose-textarea"), {
      target: { value: "branch from here" },
    });
    await fireEvent.click(forkHalf()!);
    await waitFor(() => expect(forks()).toHaveLength(1));

    first.unmount();
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await waitFor(() => expect(screen.queryByTestId("compose-textarea")).not.toBeNull());

    // Every submit path is refused while the branch is being created.
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByTestId("compose-prompt-button") as HTMLButtonElement).disabled).toBe(true);
    await fireEvent.input(screen.getByTestId("compose-textarea"), { target: { value: "second" } });
    await fireEvent.keyDown(screen.getByTestId("compose-textarea"), {
      key: "Enter",
      metaKey: true,
    });
    await tick();
    expect(sends()).toHaveLength(0);

    releaseFork(FORK);
    await waitFor(() => expect(sends()).toHaveLength(1));
    // The one dispatch went to the branch, carrying the original message.
    expect(sends()[0]![1] as { agentId: string; prompt: string }).toMatchObject({
      agentId: FORK.id,
      prompt: "branch from here",
    });
  });

  it("delivers a plain fork's failure to a bar that replaced its own", async () => {
    let rejectFork!: (e: Error) => void;
    const pending = new Promise<AgentRecord>((_, reject) => {
      rejectFork = reject;
    });
    mockForkPromptBackend({ fork: () => pending });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    seedTurn(state, AGENT_A.id);
    const first = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await waitFor(() => expect(forkHalf()).not.toBeNull());
    await fireEvent.input(screen.getByTestId("compose-textarea"), {
      target: { value: "branch from here" },
    });
    await fireEvent.click(forkHalf()!);
    await waitFor(() => expect(forks()).toHaveLength(1));

    first.unmount();
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await waitFor(() => expect(screen.queryByTestId("compose-textarea")).not.toBeNull());
    rejectFork(new Error("alice is working"));

    await waitFor(() =>
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent("alice is working"),
    );
    // And the message came back rather than being lost with the old bar.
    expect((screen.getByTestId("compose-textarea") as HTMLTextAreaElement).value).toContain(
      "branch from here",
    );
  });

  it("explains an ordinary prompt send refused because its context changed", async () => {
    // A send the UI accepted that then vanishes with no trace is worse than one
    // that refuses — the user cannot tell it from a bug.
    let releaseSignIn!: () => void;
    const pendingSignIn = new Promise<unknown>((resolve) => {
      releaseSignIn = () => resolve(null);
    });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    seedTurn(state, AGENT_A.id);
    mockForkPromptBackend({ render: signInThenRender(), signIn: () => pendingSignIn });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await enterPromptMode("prompt-option-tiddly:summary");
    await fireEvent.click(screen.getByTestId("compose-send"));

    const selection = await import("$lib/state/recipientSelection.svelte");
    await waitFor(() => expect(selection.targetRecipients(PROJECT_ID, [AGENT_B.id])).toBe(true));
    releaseSignIn();

    // The sign-in route has its own wording; what matters is that it speaks.
    await waitFor(() =>
      expect(screen.getByTestId("compose-send-notice")).toHaveTextContent(/press Send/i),
    );
    expect(sends()).toHaveLength(0);
  });

  it("explains a send refused because a recipient disappeared, with no sign-in involved", async () => {
    // The branch that used to return silently. A removed agent is pruned from the
    // selection mid-render, which is exactly the case the freeze deliberately
    // lets through — and the send then vanished with no explanation at all.
    let releaseRender!: (v: unknown) => void;
    const pending = new Promise<unknown>((resolve) => {
      releaseRender = resolve;
    });
    mockForkPromptBackend({ render: () => pending });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    seedTurn(state, AGENT_A.id);
    const view = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] },
    });
    await enterPromptMode("prompt-option-tiddly:summary");
    await fireEvent.click(screen.getByTestId("compose-send"));
    await waitFor(() =>
      expect(invokeMock.mock.calls.filter(([c]) => c === "render_prompt")).toHaveLength(1),
    );

    // The captured recipient leaves the roster while the message is being built.
    await view.rerender({ projectId: PROJECT_ID, agents: [AGENT_B] });
    await tick();
    releaseRender({ kind: "rendered", text: "RENDERED" });

    await waitFor(() =>
      expect(screen.getByTestId("compose-send-notice")).toHaveTextContent(/changed/i),
    );
    expect(sends()).toHaveLength(0);
  });

  it("releases the project-scoped busy state when a fork outlives the bar that started it", async () => {
    // After a send that spanned a project switch finishes, the composer must be
    // usable again rather than stuck busy on a claim nobody released.
    //
    // Deliberately does *not* also assert that pane targeting works here: the
    // claim is gone by this point, so `operationBlocksTargeting` returns false
    // for any implementation and the assertion would pass whether the phase
    // policy were right, wrong, or absent. Targeting is pinned where it can
    // actually fail — while an operation is live (the abandonment test below)
    // and directly against the predicate (`recipientSelection.svelte.test.ts`).
    let releaseFork!: (v: AgentRecord) => void;
    const pending = new Promise<AgentRecord>((resolve) => {
      releaseFork = resolve;
    });
    mockForkPromptBackend({ fork: () => pending });
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    seedTurn(state, AGENT_A.id);
    const first = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await waitFor(() => expect(forkHalf()).not.toBeNull());
    await fireEvent.input(screen.getByTestId("compose-textarea"), { target: { value: "x" } });
    await fireEvent.click(forkHalf()!);
    await waitFor(() => expect(forks()).toHaveLength(1));

    first.unmount();
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, FORK] } });
    await waitFor(() => expect(screen.queryByTestId("compose-textarea")).not.toBeNull());

    releaseFork(FORK);
    await waitFor(() => expect(sends()).toHaveLength(1));

    await fireEvent.input(screen.getByTestId("compose-textarea"), { target: { value: "next" } });
    await waitFor(() =>
      expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(false),
    );
  });

  it("keeps targeting blocked for a newer send when an abandoned one settles late", async () => {
    // Abandonment deliberately lets the old work run on beside a new claim, so
    // "only one operation exists" was never true. With the freeze derived rather
    // than written, a late continuation has nothing to release.
    let releaseSignIn!: () => void;
    const pendingSignIn = new Promise<unknown>((resolve) => {
      releaseSignIn = () => resolve(null);
    });
    let holdSecond!: (v: unknown) => void;
    let calls = 0;
    mockForkPromptBackend({
      render: () => {
        calls += 1;
        if (calls === 1) return Promise.resolve({ kind: "needs_sign_in", provider: "tiddly" });
        return new Promise<unknown>((resolve) => {
          holdSecond = resolve;
        });
      },
      signIn: () => pendingSignIn,
    });
    await mountComposingSummary();
    await fireEvent.click(screen.getByTestId("compose-send"));
    await waitFor(() => expect(screen.queryByTestId("compose-abandon-wait")).not.toBeNull());
    await fireEvent.click(screen.getByTestId("compose-abandon-wait"));

    await waitFor(() =>
      expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(false),
    );
    await fireEvent.click(screen.getByTestId("compose-send"));
    await waitFor(() =>
      expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(true),
    );

    const sel = await import("$lib/state/recipientSelection.svelte");
    expect(sel.targetRecipients(PROJECT_ID, [AGENT_A.id])).toBe(false);

    releaseSignIn();
    await tick();
    await tick();
    // The abandoned operation settling must not unfreeze the live one.
    expect(sel.targetRecipients(PROJECT_ID, [AGENT_A.id])).toBe(false);
    holdSecond({ kind: "rendered", text: "RENDERED" });
    await waitFor(() => expect(sends()).toHaveLength(1));
  });

  it("freezes the attachment set for the whole fork, not just the render", async () => {
    // The attachment set is frozen for the duration of a send — drops ignored,
    // remove button disabled — and `sending` is what expresses that. Because it
    // now spans the registration await too, there is no window in which a file
    // can join a fork after its attachments were captured, or be captured by a
    // fork the user staged it after. Weakening `sending`'s span reopens both.
    let releaseFork!: (v: AgentRecord) => void;
    const pending = new Promise<AgentRecord>((resolve) => {
      releaseFork = resolve;
    });
    mockForkPromptBackend({ fork: () => pending });
    await mountComposingSummary();
    fireDrop(["/tmp/before.png"]);
    await waitFor(() => expect(screen.queryByTestId("attachment-chip-image-1")).not.toBeNull());
    const stagedBefore = invokeMock.mock.calls.filter(([c]) => c === "stage_attachment").length;

    await fireEvent.click(forkHalf()!);
    await waitFor(() => expect(forks()).toHaveLength(1));
    fireDrop(["/tmp/during.png"]);
    releaseFork(FORK);

    await waitFor(() => expect(sends()).toHaveLength(1));
    expect(invokeMock.mock.calls.filter(([c]) => c === "stage_attachment")).toHaveLength(
      stagedBefore,
    );
    // The fork carried exactly the file that was staged when it was submitted.
    const carried = (sends()[0]![1] as { attachments: { original_name: string }[] }).attachments;
    expect(carried.map((a) => a.original_name)).toEqual(["before.png"]);
  });

  it("stays silent on the shortcut while a required argument is empty", async () => {
    // The primary send is disabled and silent in this state; inventing an error
    // for the fork half alone is an inconsistency the user has to learn. The
    // form's own field markers do the explaining.
    mockForkPromptBackend();
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    seedTurn(state, AGENT_A.id);
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await enterPromptMode("prompt-option-local:review");
    await waitFor(() => expect(forkHalf()).not.toBeNull());

    await pressForkShortcut();
    await tick();

    expect(screen.queryByTestId("compose-send-error")).toBeNull();
    expect(forks()).toHaveLength(0);
    expect(invokeMock).not.toHaveBeenCalledWith("render_prompt", expect.anything());
  });
});

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

  it("a focus request in prompt mode is a safe no-op (focus assist is plain-mode only)", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    mockPromptBackend({ prompts: [REVIEW] });
    const { rerender } = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A], focusRequest: 0 },
    });

    await enterPromptMode("prompt-option-local:review");
    const focusArg = screen.getByTestId("prompt-arg-focus");
    expect(focusArg).toHaveFocus();
    // No plain textarea exists in prompt mode — the focus consumer must degrade
    // gracefully rather than reach for a missing element.
    expect(screen.queryByTestId("compose-textarea")).toBeNull();

    // A pane Cmd+click bumps focusRequest; with no textarea it must no-op and
    // leave the prompt field's focus untouched (not throw, not steal focus).
    await rerender({ projectId: PROJECT_ID, agents: [AGENT_A], focusRequest: 1 });
    await tick();

    expect(focusArg).toHaveFocus();
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
    let release!: (v: { kind: "rendered"; text: string }) => void;
    const gate = new Promise<{ kind: "rendered"; text: string }>((res) => {
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

    release({ kind: "rendered", text: "DONE" });
    await waitFor(() => {
      const sends = invokeMock.mock.calls.filter(([c]) => c === "send_message");
      expect(sends).toHaveLength(2);
    });
    // Successful send returns to the plain composer.
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    expect((state.transcripts[AGENT_A.id] ?? [])[0]).toMatchObject({ text: "DONE" });
    expect((state.transcripts[AGENT_B.id] ?? [])[0]).toMatchObject({ text: "DONE" });
  });

  it("a needs-sign-in render launches the browser sign-in and completes the send", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    let renders = 0;
    let releaseSignIn!: () => void;
    const signInGate = new Promise<null>((res) => {
      releaseSignIn = () => res(null);
    });
    mockPromptBackend({
      prompts: [SUMMARY],
      // First render: the provider needs sign-in. After the sign-in resolves,
      // the retry renders normally.
      render: async () =>
        ++renders === 1
          ? { kind: "needs_sign_in", provider: "tiddly" }
          : { kind: "rendered", text: "DONE" },
      signIn: () => signInGate,
    });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    await enterPromptMode("prompt-option-tiddly:summary");
    await fireEvent.click(screen.getByTestId("compose-send"));

    // The browser wait: a visible waiting line naming the provider, send held,
    // nothing dispatched, and the sign-in invoked exactly once.
    await waitFor(() =>
      expect(screen.getByTestId("compose-signing-in")).toHaveTextContent(
        "Waiting for browser sign-in to tiddly",
      ),
    );
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(true);
    expect(invokeMock.mock.calls.some(([c]) => c === "send_message")).toBe(false);
    expect(invokeMock).toHaveBeenCalledWith("sign_in_mcp_provider", { name: "tiddly" });

    // The user approves in the browser: the send completes itself — no
    // "press send again", no message.
    releaseSignIn();
    await waitFor(() => {
      const sends = invokeMock.mock.calls.filter(([c]) => c === "send_message");
      expect(sends).toHaveLength(1);
    });
    expect((state.transcripts[AGENT_A.id] ?? [])[0]).toMatchObject({ text: "DONE" });
    expect(screen.queryByTestId("compose-signing-in")).not.toBeInTheDocument();
    expect(screen.queryByTestId("compose-send-error")).not.toBeInTheDocument();
  });

  it("a denied mid-send sign-in surfaces the failure and dispatches nothing", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    mockPromptBackend({
      prompts: [SUMMARY],
      render: async () => ({ kind: "needs_sign_in", provider: "tiddly" }),
      signIn: () => Promise.reject(new Error("the authorization server reported access_denied")),
    });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    await enterPromptMode("prompt-option-tiddly:summary");
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() =>
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent("access_denied"),
    );
    expect(invokeMock.mock.calls.some(([c]) => c === "send_message")).toBe(false);
    // The composer kept the prompt: the user can retry after fixing things.
    expect(screen.getByTestId("prompt-composer")).toBeInTheDocument();
    expect((state.transcripts[AGENT_A.id] ?? []).length).toBe(0);
  });

  it("a failure after a successful mid-send sign-in says the sign-in stuck", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    let renders = 0;
    mockPromptBackend({
      prompts: [SUMMARY],
      // Sign-in succeeds; the retry render then fails (e.g. the server).
      render: async () => {
        if (++renders === 1) return { kind: "needs_sign_in", provider: "tiddly" };
        throw new Error("timed out after 10s");
      },
      signIn: () => Promise.resolve(null),
    });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    await enterPromptMode("prompt-option-tiddly:summary");
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() =>
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent(
        "Signed in, but the send then failed: timed out after 10s",
      ),
    );
    expect(invokeMock.mock.calls.some(([c]) => c === "send_message")).toBe(false);
    expect((state.transcripts[AGENT_A.id] ?? []).length).toBe(0);
  });

  it("a failure after a successful mid-send sign-in says the sign-in stuck", async () => {
    // The live-run confusion this pins: the user approves in the browser,
    // the retry then fails (e.g. a server stall) — the message must say the
    // sign-in itself succeeded, not read as a wasted approval.
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    let renders = 0;
    mockPromptBackend({
      prompts: [SUMMARY],
      render: async () => {
        if (++renders === 1) return { kind: "needs_sign_in", provider: "tiddly" };
        throw new Error("timed out after 10s");
      },
      signIn: () => Promise.resolve(null),
    });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    await enterPromptMode("prompt-option-tiddly:summary");
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() =>
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent(
        "Signed in, but the send then failed: timed out after 10s",
      ),
    );
    expect(invokeMock.mock.calls.some(([c]) => c === "send_message")).toBe(false);
    expect((state.transcripts[AGENT_A.id] ?? []).length).toBe(0);
  });

  it("a second needs-sign-in after a successful sign-in stops — never a loop", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    mockPromptBackend({
      prompts: [SUMMARY],
      // Pathological backend: still needs sign-in after a successful flow.
      render: async () => ({ kind: "needs_sign_in", provider: "tiddly" }),
      signIn: () => Promise.resolve(null),
    });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    await enterPromptMode("prompt-option-tiddly:summary");
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() =>
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent("needs sign-in"),
    );
    // Exactly one browser launch, no dispatch, composer intact.
    expect(invokeMock.mock.calls.filter(([c]) => c === "sign_in_mcp_provider")).toHaveLength(1);
    expect(invokeMock.mock.calls.some(([c]) => c === "send_message")).toBe(false);
    expect((state.transcripts[AGENT_A.id] ?? []).length).toBe(0);
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

    let resolution: unknown = { state: "temporarily_unavailable", generation: 0 };
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "search_project_files") return [];
      if (cmd === "resolve_saved_prompt") return resolution;
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    // Cold: shows a recoverable restore failure (not plain), and must NOT clobber
    // the saved snapshot in storage.
    await waitFor(() =>
      expect(screen.getByTestId("compose-prompt-restore-failed")).toBeInTheDocument(),
    );
    expect(screen.queryByTestId("prompt-composer")).toBeNull();
    expect(store.getCompose(PROJECT_ID).content).toMatchObject({ kind: "prompt", name: "review" });

    // Sync completes with the prompt present → restore with args intact.
    const callsBeforeEvent = invokeMock.mock.calls.filter(
      ([cmd]) => cmd === "resolve_saved_prompt",
    ).length;
    resolution = { state: "available", prompt: REVIEW, generation: 1 };
    listeners.get("prompts:changed")?.({ payload: { generation: 1 } });

    await waitFor(() => expect(screen.getByTestId("prompt-composer")).toBeInTheDocument());
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "resolve_saved_prompt")).toHaveLength(
      callsBeforeEvent + 1,
    );
    expect((screen.getByTestId("prompt-arg-focus") as HTMLTextAreaElement).value).toBe(
      "saved focus",
    );
    expect((screen.getByTestId("prompt-appended") as HTMLTextAreaElement).value).toBe("tail");
  });

  it("settles saved-prompt restoration when event registration never settles", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const store = await loadComposeStore();
    store.setContent(PROJECT_ID, {
      kind: "prompt",
      provider: "tiddly",
      name: "summary",
      args: {},
      appendedText: "",
    });
    store.flush();
    store._testing.reloadFromStorage();

    listenMock.mockImplementation(() => new Promise<MockUnlisten>(() => undefined));
    mockPromptBackend({
      resolve: async () => ({ state: "temporarily_unavailable", generation: 0 }),
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    await waitFor(() =>
      expect(screen.getByTestId("compose-prompt-restore-failed")).toBeInTheDocument(),
    );
    expect(
      listenMock.mock.calls.map(([name]) => name).filter((name) => name.startsWith("prompts:")),
    ).toEqual(["prompts:changed"]);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "resolve_saved_prompt")).toHaveLength(1);
  });

  it("coalesces publication during registration into one post-registration read", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const store = await loadComposeStore();
    store.setContent(PROJECT_ID, {
      kind: "prompt",
      provider: "tiddly",
      name: "summary",
      args: {},
      appendedText: "tail",
    });
    store.flush();
    store._testing.reloadFromStorage();

    let finishRegistration: ((unlisten: MockUnlisten) => void) | undefined;
    listenMock.mockImplementation((name: string, cb) => {
      listeners.set(name, cb);
      return new Promise<MockUnlisten>((resolve) => (finishRegistration = resolve));
    });
    let resolution: unknown = { state: "temporarily_unavailable", generation: 0 };
    mockPromptBackend({ resolve: async () => resolution });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await waitFor(() =>
      expect(screen.getByTestId("compose-prompt-restore-failed")).toBeInTheDocument(),
    );

    resolution = { state: "available", prompt: SUMMARY, generation: 1 };
    listeners.get("prompts:changed")?.({ payload: { generation: 1 } });
    finishRegistration?.(vi.fn());

    await waitFor(() => expect(screen.getByTestId("prompt-composer")).toBeInTheDocument());
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "resolve_saved_prompt")).toHaveLength(2);
  });

  it("cleans up a prompt subscription that registers after unmount", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const store = await loadComposeStore();
    store.setContent(PROJECT_ID, {
      kind: "prompt",
      provider: "tiddly",
      name: "summary",
      args: {},
      appendedText: "",
    });
    store.flush();
    store._testing.reloadFromStorage();

    let finishRegistration: ((unlisten: MockUnlisten) => void) | undefined;
    listenMock.mockImplementation((name: string, cb) => {
      listeners.set(name, cb);
      return new Promise<MockUnlisten>((resolve) => (finishRegistration = resolve));
    });
    mockPromptBackend({
      resolve: async () => ({ state: "available", prompt: SUMMARY, generation: 1 }),
    });
    const { unmount } = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A] },
    });
    await waitFor(() => expect(finishRegistration).toBeDefined());
    await waitFor(() =>
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "resolve_saved_prompt")).toHaveLength(
        1,
      ),
    );

    unmount();
    const unlistenChanged = vi.fn();
    finishRegistration?.(unlistenChanged);

    await waitFor(() => expect(unlistenChanged).toHaveBeenCalledOnce());
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "resolve_saved_prompt")).toHaveLength(1);
  });

  it("settles restoration when prompt listener registration rejects", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const store = await loadComposeStore();
    store.setContent(PROJECT_ID, {
      kind: "prompt",
      provider: "tiddly",
      name: "summary",
      args: {},
      appendedText: "",
    });
    store.flush();
    store._testing.reloadFromStorage();

    listenMock.mockRejectedValue(new Error("listener unavailable"));
    mockPromptBackend({
      resolve: async () => ({ state: "temporarily_unavailable", generation: 1 }),
    });

    render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A] },
    });
    await waitFor(() =>
      expect(screen.getByTestId("compose-prompt-restore-failed")).toBeInTheDocument(),
    );
  });

  it("settles restoration when prompt listener creation throws synchronously", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const store = await loadComposeStore();
    store.setContent(PROJECT_ID, {
      kind: "prompt",
      provider: "tiddly",
      name: "summary",
      args: {},
      appendedText: "",
    });
    store.flush();
    store._testing.reloadFromStorage();

    listenMock.mockImplementation(() => {
      throw new Error("native listen failed");
    });
    mockPromptBackend({
      resolve: async () => ({ state: "temporarily_unavailable", generation: 1 }),
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await waitFor(() =>
      expect(screen.getByTestId("compose-prompt-restore-failed")).toBeInTheDocument(),
    );
  });

  it("preserves a confirmed-missing prompt draft until the user starts over", async () => {
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
    let resolution: unknown = { state: "temporarily_unavailable", generation: 0 };
    mockPromptBackend({ resolve: async () => resolution });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    await waitFor(() =>
      expect(screen.getByTestId("compose-prompt-restore-failed")).toBeInTheDocument(),
    );
    listeners.get("prompts:changed")?.({ payload: { generation: 1 } });
    await waitFor(() =>
      expect(screen.getByTestId("compose-prompt-restore-failed")).toBeInTheDocument(),
    );
    expect(store.getCompose(PROJECT_ID).content).toMatchObject({ kind: "prompt", name: "ghost" });

    resolution = { state: "confirmed_missing", generation: 2 };
    listeners.get("prompts:changed")?.({ payload: { generation: 2 } });

    await waitFor(() =>
      expect(
        screen.getByText("This prompt is no longer available from its provider."),
      ).toBeInTheDocument(),
    );
    expect(store.getCompose(PROJECT_ID).content).toMatchObject({
      kind: "prompt",
      name: "ghost",
      args: { focus: "x" },
    });
    await fireEvent.click(screen.getByTestId("prompt-restore-discard"));
    expect(screen.getByTestId("compose-textarea")).toBeInTheDocument();
    expect((screen.getByTestId("compose-textarea") as HTMLTextAreaElement).value).toBe(
      "leftover text",
    );
  });

  it("preserves a saved MCP prompt draft through provider failure and restores it later", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const store = await loadComposeStore();
    store.setContent(PROJECT_ID, {
      kind: "prompt",
      provider: "tiddly",
      name: "summary",
      args: { focus: "keep this" },
      appendedText: "tail",
    });
    store.flush();
    store._testing.reloadFromStorage();
    let resolution: unknown = { state: "temporarily_unavailable", generation: 1 };
    mockPromptBackend({ resolve: async () => resolution });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await waitFor(() => screen.getByTestId("compose-prompt-restore-failed"));
    expect(store.getCompose(PROJECT_ID).content).toMatchObject({
      kind: "prompt",
      provider: "tiddly",
      name: "summary",
      args: { focus: "keep this" },
    });

    resolution = {
      state: "available",
      prompt: { ...SUMMARY, arguments: REVIEW.arguments },
      generation: 2,
    };
    await fireEvent.click(screen.getByTestId("prompt-restore-retry"));
    await waitFor(() => screen.getByTestId("prompt-composer"));
    expect(invokeMock).toHaveBeenCalledWith("resolve_saved_prompt_fresh", {
      provider: "tiddly",
      name: "summary",
    });
    expect(screen.getByTestId("prompt-arg-focus")).toHaveValue("keep this");
  });

  it("lets the user leave an unavailable saved prompt without losing appended text", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const store = await loadComposeStore();
    store.setContent(PROJECT_ID, {
      kind: "prompt",
      provider: "tiddly",
      name: "summary",
      args: { focus: "structured value" },
      appendedText: "keep this tail",
    });
    store.flush();
    store._testing.reloadFromStorage();
    mockPromptBackend({
      resolve: async () => ({ state: "temporarily_unavailable", generation: 1 }),
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await screen.findByTestId("compose-prompt-restore-failed");
    await fireEvent.click(screen.getByTestId("prompt-restore-discard"));

    expect(screen.getByTestId("compose-textarea")).toHaveValue("keep this tail");
    await waitFor(() =>
      expect(store.getCompose(PROJECT_ID).content).toEqual({
        kind: "plain",
        draft: "keep this tail",
      }),
    );
  });

  it("does not discard a saved draft from a restoration reply older than auth invalidation", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const store = await loadComposeStore();
    store.setContent(PROJECT_ID, {
      kind: "prompt",
      provider: "tiddly",
      name: "summary",
      args: { focus: "keep this" },
      appendedText: "tail",
    });
    store.flush();
    store._testing.reloadFromStorage();
    let calls = 0;
    let resolveOld!: (value: unknown) => void;
    mockPromptBackend({
      resolve: async () => {
        calls++;
        if (calls === 1) return { state: "temporarily_unavailable", generation: 0 };
        if (calls === 2) return await new Promise((resolve) => (resolveOld = resolve));
        return { state: "temporarily_unavailable", generation: 2 };
      },
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await waitFor(() => expect(calls).toBe(2));
    listeners.get("prompts:changed")?.({ payload: { generation: 2 } });
    await waitFor(() => expect(calls).toBe(3));
    resolveOld({ state: "confirmed_missing", generation: 1 });
    await tick();

    expect(screen.getByTestId("compose-prompt-restore-failed")).toBeInTheDocument();
    expect(store.getCompose(PROJECT_ID).content).toMatchObject({
      kind: "prompt",
      name: "summary",
      args: { focus: "keep this" },
    });
  });

  it("keeps prompt removal locked while the send render is in flight", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    let release!: (v: { kind: "rendered"; text: string }) => void;
    const gate = new Promise<{ kind: "rendered"; text: string }>((res) => {
      release = res;
    });
    mockPromptBackend({ prompts: [SUMMARY], render: () => gate });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    await enterPromptMode("prompt-option-tiddly:summary");
    await fireEvent.click(screen.getByTestId("compose-send"));
    expect((screen.getByTestId("prompt-remove") as HTMLButtonElement).disabled).toBe(true);
    await fireEvent.click(screen.getByTestId("prompt-remove"));
    release({ kind: "rendered", text: "DONE" });
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
    let release!: (v: { kind: "rendered"; text: string }) => void;
    const gate = new Promise<{ kind: "rendered"; text: string }>((res) => {
      release = res;
    });
    mockPromptBackend({ prompts: [SUMMARY], render: () => gate });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    await fireEvent.click(chip(AGENT_B.id)); // select both A + B
    await enterPromptMode("prompt-option-tiddly:summary");
    await fireEvent.click(screen.getByTestId("compose-send"));
    expect((chip(AGENT_A.id) as HTMLButtonElement).disabled).toBe(true);
    await fireEvent.click(chip(AGENT_A.id));
    release({ kind: "rendered", text: "DONE" });
    await waitFor(() => {
      const sends = invokeMock.mock.calls.filter(([c]) => c === "send_message");
      expect(sends).toHaveLength(2);
    });

    expect((state.transcripts[AGENT_A.id] ?? [])[0]).toMatchObject({ text: "DONE" });
    expect((state.transcripts[AGENT_B.id] ?? [])[0]).toMatchObject({ text: "DONE" });
  });

  describe("cross-project forward sources", () => {
    const OTHER_PROJECT = "00000000-0000-7000-8000-0000000000aa";
    const FOREIGN_AGENT = {
      id: "00000000-0000-7000-8000-0000000000bb",
      project_id: OTHER_PROJECT,
      name: "oracle",
      harness: "claude_code" as const,
      session_locator: null,
      created_at: "2026-05-16T00:00:00Z",
    };

    async function withOtherProject() {
      const ws = await loadWorkspace();
      ws.projects.list = [
        {
          id: PROJECT_ID,
          name: "here",
          created_at: "2026-05-16T00:00:00Z",
          directory: "/work/here",
          available: true,
          last_activity: "2026-05-16T00:00:00Z",
          archived: false,
        },
        {
          id: OTHER_PROJECT,
          name: "backend",
          created_at: "2026-05-16T00:00:00Z",
          directory: "/work/backend",
          available: true,
          last_activity: "2026-05-16T00:00:00Z",
          archived: false,
        },
      ];
    }

    /// Walk the picker's nested submenus down to one project's agent rows. Each
    /// level mounts on demand, so the project rows don't exist until `Projects`
    /// is open, and the agent rows don't exist until the project is.
    async function openProjectSubmenu(projectId: string): Promise<void> {
      await fireEvent.click(await screen.findByTestId("forward-picker-projects-trigger"));
      await fireEvent.click(
        await screen.findByTestId(`forward-picker-project-toggle-${projectId}`),
      );
    }

    /// Close one submenu level. bits-ui closes a submenu on its `dir`-relative
    /// back key, which is `ArrowLeft` under the default `ltr` — independent of
    /// which side the content is rendered on.
    async function closeSubmenu(content: HTMLElement): Promise<void> {
      await fireEvent.keyDown(content, { key: "ArrowLeft" });
    }

    it("refreshes a restored foreign chip's stale agent and project names on mount", async () => {
      // The two halves come from different sources — the agent name from the other
      // project's roster, the project name from the workspace listing — so a test
      // that stales only one leaves the other unprotected.
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      await withOtherProject();
      const composeStore = await loadComposeStore();
      composeStore.setForwards(PROJECT_ID, {
        ...composeStore.emptyForwards(),
        message: [
          {
            id: FOREIGN_AGENT.id,
            name: "old-agent-name",
            projectId: OTHER_PROJECT,
            projectName: "old-project-name",
          },
        ],
      });
      invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
        if (cmd === "list_project_agents_readonly") return [FOREIGN_AGENT];
        return undefined;
      });
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

      // Both names catch up to the current ones without the user browsing.
      const chip = await screen.findByTestId(`forward-source-chip-${FOREIGN_AGENT.name}`);
      await waitFor(() => expect(chip).toHaveTextContent("backend · oracle"));
      const reads = () =>
        invokeMock.mock.calls.filter((c) => c[0] === "list_project_agents_readonly");
      expect(reads()).toHaveLength(1);
      expect(reads()[0]?.[1]).toEqual({
        projectId: OTHER_PROJECT,
        directory: "/work/backend",
      });
    });

    it("keeps a restored chip's stored label when the refresh read fails", async () => {
      // A chip mutating (or vanishing) under the user because another project is
      // momentarily unreadable is worse than a stale name; the backend refuses the
      // send with a clear error if the source really is gone.
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      await withOtherProject();
      const composeStore = await loadComposeStore();
      composeStore.setForwards(PROJECT_ID, {
        ...composeStore.emptyForwards(),
        message: [
          {
            id: FOREIGN_AGENT.id,
            name: "stored-name",
            projectId: OTHER_PROJECT,
            projectName: "stored-project",
          },
        ],
      });
      invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
        if (cmd === "list_project_agents_readonly")
          throw { type: "project_locked", message: "locked" };
        return undefined;
      });
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

      const chip = await screen.findByTestId("forward-source-chip-stored-name");
      await waitFor(() =>
        expect(
          invokeMock.mock.calls.filter((c) => c[0] === "list_project_agents_readonly"),
        ).toHaveLength(1),
      );
      expect(chip).toHaveTextContent("stored-project · stored-name");
    });

    it("keeps every project behind one Projects row until it is opened", async () => {
      // The picker's first screen is the local agents the user came for. An
      // inline list of every other project pushed them off the bottom of a
      // scrolling popover, which is what nesting fixes.
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      await withOtherProject();
      invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
        if (cmd === "list_project_agents_readonly") return [FOREIGN_AGENT];
        return undefined;
      });
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

      await fireEvent.click(screen.getByTestId("compose-forward-button"));
      await screen.findByTestId("forward-picker-projects-trigger");
      expect(
        screen.queryByTestId(`forward-picker-project-toggle-${OTHER_PROJECT}`),
      ).not.toBeInTheDocument();
      // Merely listing the projects must not read any of their rosters.
      expect(invokeMock.mock.calls.map((c) => c[0])).not.toContain("list_project_agents_readonly");

      await fireEvent.click(screen.getByTestId("forward-picker-projects-trigger"));
      await screen.findByTestId(`forward-picker-project-toggle-${OTHER_PROJECT}`);
      expect(
        screen.queryByTestId(`forward-picker-foreign-agent-${FOREIGN_AGENT.id}`),
      ).not.toBeInTheDocument();
    });

    it("browsing a project's roster neither loads nor locks it", async () => {
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      await withOtherProject();
      invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
        if (cmd === "list_project_agents_readonly") return [FOREIGN_AGENT];
        return undefined;
      });
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

      await fireEvent.click(screen.getByTestId("compose-forward-button"));
      await openProjectSubmenu(OTHER_PROJECT);
      await screen.findByTestId(`forward-picker-foreign-agent-${FOREIGN_AGENT.id}`);

      // The read-only command served the menu; the *activation* path — which
      // takes the project's `instance.lock` for the app's lifetime — must not
      // have run for a project the user has only hovered.
      const commands = invokeMock.mock.calls.map((c) => c[0]);
      expect(commands).toContain("list_project_agents_readonly");
      expect(commands).not.toContain("open_project");
    });

    it("renders an unreadable project as an unpickable row, not a thrown error", async () => {
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      await withOtherProject();
      invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
        if (cmd === "list_project_agents_readonly") throw new Error("project is locked");
        return undefined;
      });
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

      await fireEvent.click(screen.getByTestId("compose-forward-button"));
      await openProjectSubmenu(OTHER_PROJECT);

      const row = await screen.findByTestId(`forward-picker-project-error-${OTHER_PROJECT}`);
      expect(row).toHaveTextContent("project is locked");
    });

    it("picking a foreign agent chips it with its project and sends its owner", async () => {
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      await withOtherProject();
      invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
        if (cmd === "list_project_agents_readonly") return [FOREIGN_AGENT];
        if (cmd === "open_project") return { id: OTHER_PROJECT, name: "backend" };
        if (cmd === "forward_message") return { status: "resolved", body: "composed" };
        if (cmd === "send_message") return "m1";
        return undefined;
      });
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

      await fireEvent.click(screen.getByTestId("compose-forward-button"));
      await openProjectSubmenu(OTHER_PROJECT);
      await fireEvent.click(
        await screen.findByTestId(`forward-picker-foreign-agent-${FOREIGN_AGENT.id}`),
      );

      // The chip names the project, so a same-named local agent stays distinct.
      const chip = await screen.findByTestId(`forward-source-chip-${FOREIGN_AGENT.name}`);
      expect(chip).toHaveTextContent("backend · oracle");

      await fireEvent.input(screen.getByTestId("compose-textarea"), {
        target: { value: "please aggregate" },
      });
      await fireEvent.click(screen.getByTestId("compose-send"));

      await waitFor(() => {
        const call = invokeMock.mock.calls.find((c) => c[0] === "forward_message");
        // Assert the **payload keys**, not just `sources`. A required Tauri
        // argument that the wrapper omits fails deserialization before the
        // handler runs — invisible to a suite that mocks `invoke`, which is
        // exactly how every forward was broken at runtime once.
        expect(call?.[1]).toMatchObject({
          sources: [{ agent_id: FOREIGN_AGENT.id, project_id: OTHER_PROJECT }],
          projectId: PROJECT_ID,
        });
        expect(Object.keys(call?.[1] ?? {}).sort()).toEqual(
          ["body", "forwardId", "projectId", "sources"].sort(),
        );
      });
      // `{ status: "resolved" }` must actually drive the dispatch half.
      await waitFor(() => {
        expect(invokeMock.mock.calls.some((c) => c[0] === "send_message")).toBe(true);
      });
    });

    it("renders a foreign chip with no readiness warning", async () => {
      // Readiness is read from the *current* project's transcripts, so a foreign
      // agent is absent and the naive classification is `empty` — which renders a
      // red "this will block your send" warning that is the inverse of the truth
      // for a healthy foreign source. It must render no marker at all.
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      await withOtherProject();
      invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
        if (cmd === "list_project_agents_readonly") return [FOREIGN_AGENT];
        if (cmd === "open_project") return { id: OTHER_PROJECT, name: "backend" };
        return undefined;
      });
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

      await fireEvent.click(screen.getByTestId("compose-forward-button"));
      await openProjectSubmenu(OTHER_PROJECT);
      await fireEvent.click(
        await screen.findByTestId(`forward-picker-foreign-agent-${FOREIGN_AGENT.id}`),
      );

      const chip = await screen.findByTestId(`forward-source-chip-${FOREIGN_AGENT.name}`);
      expect(chip).toHaveAttribute("data-readiness", "unknown");
      expect(chip).not.toHaveTextContent("no forwardable output");
    });

    it("picking opens the project and refuses at the pick site when it is locked", async () => {
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      await withOtherProject();
      invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
        if (cmd === "list_project_agents_readonly") return [FOREIGN_AGENT];
        // Tauri rejects a structured command error with a plain **object**, not an
        // `Error`. Mocking an `Error` here would hide the `[object Object]` bug
        // this assertion exists to catch.
        if (cmd === "open_project")
          throw { type: "project_locked", message: "project is open in another window" };
        return undefined;
      });
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

      await fireEvent.click(screen.getByTestId("compose-forward-button"));
      await openProjectSubmenu(OTHER_PROJECT);
      await fireEvent.click(
        await screen.findByTestId(`forward-picker-foreign-agent-${FOREIGN_AGENT.id}`),
      );

      const err = await screen.findByTestId(`forward-picker-pick-error-${FOREIGN_AGENT.id}`);
      expect(err).toHaveTextContent("project is open in another window");
      // A refused pick must not leave a chip the user would then try to send.
      expect(
        screen.queryByTestId(`forward-source-chip-${FOREIGN_AGENT.name}`),
      ).not.toBeInTheDocument();
    });

    it("retries a failed roster read when the project's submenu is reopened", async () => {
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      await withOtherProject();
      let attempt = 0;
      invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
        if (cmd === "list_project_agents_readonly") {
          attempt += 1;
          if (attempt === 1) throw { type: "other", message: "transient" };
          return [FOREIGN_AGENT];
        }
        return undefined;
      });
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

      await fireEvent.click(screen.getByTestId("compose-forward-button"));
      await openProjectSubmenu(OTHER_PROJECT);
      const menu = await screen.findByTestId(`forward-picker-project-menu-${OTHER_PROJECT}`);
      expect(menu).toHaveTextContent("transient");

      // A transient read failure must not make the project permanently unpickable.
      await closeSubmenu(menu);
      await fireEvent.click(
        await screen.findByTestId(`forward-picker-project-toggle-${OTHER_PROJECT}`),
      );
      await screen.findByTestId(`forward-picker-foreign-agent-${FOREIGN_AGENT.id}`);
    });

    it("sends the exact argument set on a read-only roster call", async () => {
      // `directory` was added in the same change as this test; the command it
      // feeds is the one whose signature drift is otherwise unguarded.
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      await withOtherProject();
      invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
        if (cmd === "list_project_agents_readonly") return [FOREIGN_AGENT];
        return undefined;
      });
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

      await fireEvent.click(screen.getByTestId("compose-forward-button"));
      await openProjectSubmenu(OTHER_PROJECT);

      await waitFor(() => {
        const call = invokeMock.mock.calls.find((c) => c[0] === "list_project_agents_readonly");
        expect(call?.[1]).toEqual({ projectId: OTHER_PROJECT, directory: "/work/backend" });
      });
    });

    it("does not re-read a foreign roster on every chip edit", async () => {
      // The refresh is `onMount`, not `$effect`: as an effect it read all four
      // source families and wrote them back, so every chip add/remove re-ran it
      // and re-read each referenced project's registry. This mounts with no saved
      // draft, so the mount pass itself does nothing here — the read counted
      // below is the user browsing. The mount pass has its own test above.
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      // The local source is picked below, so it needs forwardable output — an
      // empty agent's row is disabled.
      state.transcripts[AGENT_A.id] = [
        {
          role: "agent",
          turn_id: "t-alice",
          agent_id: AGENT_A.id,
          started_at: "2026-05-16T00:00:00Z",
          status: "complete",
          items: [{ item_kind: "text", kind: "text", text: "done" }],
        },
      ];
      await withOtherProject();
      invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
        if (cmd === "list_project_agents_readonly") return [FOREIGN_AGENT];
        if (cmd === "open_project") return { id: OTHER_PROJECT, name: "backend" };
        return undefined;
      });
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

      // Pick a foreign source, then add and remove a local one.
      await fireEvent.click(screen.getByTestId("compose-forward-button"));
      await openProjectSubmenu(OTHER_PROJECT);
      await fireEvent.click(
        await screen.findByTestId(`forward-picker-foreign-agent-${FOREIGN_AGENT.id}`),
      );
      await screen.findByTestId(`forward-source-chip-${FOREIGN_AGENT.name}`);
      await fireEvent.click(screen.getByTestId("compose-forward-button"));
      await fireEvent.click(await screen.findByTestId(`forward-picker-agent-${AGENT_A.id}`));

      // Chip edits must not add reads on top of the browse.
      const reads = () =>
        invokeMock.mock.calls.filter((c) => c[0] === "list_project_agents_readonly").length;
      const after = reads();
      await fireEvent.click(screen.getByTestId(`forward-source-remove-${AGENT_A.name}`));
      await waitFor(() => expect(reads()).toBe(after));
    });

    it("open, close, reopen does not re-read the roster", async () => {
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      await withOtherProject();
      invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
        if (cmd === "list_project_agents_readonly") return [FOREIGN_AGENT];
        return undefined;
      });
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

      await fireEvent.click(screen.getByTestId("compose-forward-button"));
      await openProjectSubmenu(OTHER_PROJECT);
      await screen.findByTestId(`forward-picker-foreign-agent-${FOREIGN_AGENT.id}`);
      const reads = invokeMock.mock.calls.filter(
        (c) => c[0] === "list_project_agents_readonly",
      ).length;

      // Closing unmounts the rows but keeps the cache.
      await closeSubmenu(await screen.findByTestId(`forward-picker-project-menu-${OTHER_PROJECT}`));
      await waitFor(() => {
        expect(
          screen.queryByTestId(`forward-picker-foreign-agent-${FOREIGN_AGENT.id}`),
        ).not.toBeInTheDocument();
      });

      await fireEvent.click(
        await screen.findByTestId(`forward-picker-project-toggle-${OTHER_PROJECT}`),
      );
      await screen.findByTestId(`forward-picker-foreign-agent-${FOREIGN_AGENT.id}`);
      expect(
        invokeMock.mock.calls.filter((c) => c[0] === "list_project_agents_readonly").length,
      ).toBe(reads);
    });

    it("keeps a foreign chip across a remount", async () => {
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      await withOtherProject();
      invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
        if (cmd === "list_project_agents_readonly") return [FOREIGN_AGENT];
        if (cmd === "open_project") return { id: OTHER_PROJECT, name: "backend" };
        return undefined;
      });
      const view = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

      await fireEvent.click(screen.getByTestId("compose-forward-button"));
      await openProjectSubmenu(OTHER_PROJECT);
      await fireEvent.click(
        await screen.findByTestId(`forward-picker-foreign-agent-${FOREIGN_AGENT.id}`),
      );
      await screen.findByTestId(`forward-source-chip-${FOREIGN_AGENT.name}`);

      // A project switch / Git-view toggle remounts the bar and restores the
      // draft. A foreign source is absent from *this* project's roster, so a
      // roster-match restore would silently delete it.
      view.unmount();
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

      expect(
        await screen.findByTestId(`forward-source-chip-${FOREIGN_AGENT.name}`),
      ).toHaveTextContent("backend · oracle");
    });
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

describe("ComposeBar — attachments survive navigation", () => {
  it("restores a staged chip after an unmount/remount (project switch, Git view)", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const { unmount } = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    fireDrop(["/a/diagram.png"]);
    await screen.findByTestId("attachment-chip-image-1");
    unmount();

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    expect(await screen.findByTestId("attachment-chip-image-1")).toBeInTheDocument();
  });

  it("does not restore chips after a successful send", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "stage_attachment") {
        return { path: "/p/attachments/uuid__diagram.png", original_name: "diagram.png" };
      }
      if (cmd === "send_message") return "msg-1";
      if (cmd === "existing_attachment_paths") return (args as { paths?: string[] })?.paths ?? [];
      return null;
    });
    const { unmount } = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    fireDrop(["/a/diagram.png"]);
    await screen.findByTestId("attachment-chip-image-1");
    await fireEvent.click(screen.getByTestId("compose-send"));
    await waitFor(() =>
      expect(invokeMock.mock.calls.some(([c]) => c === "send_message")).toBe(true),
    );
    unmount();

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await tick();
    expect(screen.queryByTestId("attachment-chip-image-1")).toBeNull();
  });

  it("commits a staging result that resolves after unmount to the originating project", async () => {
    // A project switch tears down the bar mid-copy. The file was staged *for this
    // project*, so it belongs in this project's snapshot — not discarded, and not
    // leaked into whichever project the user switched to.
    const state = await loadState();
    const composeStore = await loadComposeStore();
    await state.registerAgent(AGENT_A);

    let releaseStage: (() => void) | undefined;
    const staged = new Promise<void>((r) => (releaseStage = r));
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "stage_attachment") {
        await staged;
        return { path: "/p/attachments/uuid__late.png", original_name: "late.png" };
      }
      return null;
    });

    const { unmount } = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    fireDrop(["/a/late.png"]);
    unmount();

    releaseStage?.();
    await waitFor(() =>
      expect(composeStore.getCompose(PROJECT_ID).attachments).toEqual([
        {
          label: "image-1",
          kind: "image",
          path: "/p/attachments/uuid__late.png",
          original_name: "late.png",
        },
      ]),
    );
  });

  it("numbers a chip attached after a restore without colliding with the restored label", async () => {
    // A per-component counter would restart at 1 on remount and produce two
    // `image-1` chips — and the label is what the user types into the message.
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const { unmount } = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    fireDrop(["/a/first.png"]);
    await screen.findByTestId("attachment-chip-image-1");
    unmount();

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await screen.findByTestId("attachment-chip-image-1");
    fireDrop(["/a/second.png"]);

    expect(await screen.findByTestId("attachment-chip-image-2")).toBeInTheDocument();
  });

  it("prunes a restored chip whose staged file no longer exists", async () => {
    const state = await loadState();
    const composeStore = await loadComposeStore();
    await state.registerAgent(AGENT_A);
    composeStore.setAttachments(PROJECT_ID, [
      {
        label: "image-1",
        kind: "image",
        path: "/p/attachments/gone.png",
        original_name: "gone.png",
      },
    ]);
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "existing_attachment_paths") return []; // the file vanished
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    await waitFor(() => expect(screen.queryByTestId("attachment-chip-image-1")).toBeNull());
    expect(composeStore.getCompose(PROJECT_ID).attachments).toBeUndefined();
  });

  it("does not prune a chip attached while the existence probe was in flight", async () => {
    // The probe answers about the *restored* paths only. A stale answer must not
    // sweep away a file the user dropped a moment ago.
    const state = await loadState();
    const composeStore = await loadComposeStore();
    await state.registerAgent(AGENT_A);
    composeStore.setAttachments(PROJECT_ID, [
      {
        label: "image-1",
        kind: "image",
        path: "/p/attachments/gone.png",
        original_name: "gone.png",
      },
    ]);

    let releaseProbe: (() => void) | undefined;
    const probe = new Promise<void>((r) => (releaseProbe = r));
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "existing_attachment_paths") {
        await probe;
        return [];
      }
      if (cmd === "stage_attachment") {
        return { path: "/p/attachments/uuid__fresh.png", original_name: "fresh.png" };
      }
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    fireDrop(["/a/fresh.png"]);
    await screen.findByTestId("attachment-chip-image-2");

    releaseProbe?.();

    // The restored (missing) chip goes; the freshly attached one stays.
    await waitFor(() => expect(screen.queryByTestId("attachment-chip-image-1")).toBeNull());
    expect(screen.getByTestId("attachment-chip-image-2")).toBeInTheDocument();
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
    const ops = await import("$lib/state/composeOperations.svelte");
    const blockingOp = ops.beginOperation(PROJECT_ID, { kind: "prompt_send" })!;

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

    ops.finishOperation(PROJECT_ID, blockingOp);
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

    // Simulates a pane Cmd+click / Cmd+Alt+N from outside this component.
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

    let release!: (v: { kind: "rendered"; text: string }) => void;
    const gate = new Promise<{ kind: "rendered"; text: string }>((res) => {
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

    release({ kind: "rendered", text: "DONE" });
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

  // Give an agent a completed turn so it's a non-empty forward source. Without
  // one it is "no output" — which the pickers render *disabled*, so a test that
  // forwards from an unseeded agent can't click it at all.
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

  async function seedStreamingTurn(agentId: string, alsoCompleted = false): Promise<void> {
    const state = await loadState();
    const turns = alsoCompleted
      ? [
          {
            role: "agent" as const,
            turn_id: `t-done-${agentId}`,
            agent_id: agentId,
            started_at: "2026-05-16T00:00:00Z",
            status: "complete" as const,
            items: [{ item_kind: "text" as const, kind: "text" as const, text: "old" }],
          },
        ]
      : [];
    state.transcripts[agentId] = [
      ...turns,
      {
        role: "agent",
        turn_id: `t-live-${agentId}`,
        agent_id: agentId,
        started_at: "2026-05-16T00:00:01Z",
        status: "streaming",
        items: [],
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

  it("@ menu pane rows carry the pane glyph in both Send to and Forward from sections", async () => {
    const panes = await import("$lib/state/transcriptPanes.svelte");
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    const roster = [AGENT_A.id, AGENT_B.id];
    const paneId = panes.moveAgentToNewPane(PROJECT_ID, roster, AGENT_B.id);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "@" } });

    const sendToPane = await screen.findByTestId(`recipient-option-pane:${paneId}`);
    expect(within(sendToPane).getByTestId("pane-glyph")).toBeInTheDocument();
    const forwardPane = await screen.findByTestId(`forward-option-forward-pane:${paneId}`);
    expect(within(forwardPane).getByTestId("pane-glyph")).toBeInTheDocument();
  });

  it("picks a forward source from the @ menu and dispatches a forward", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "forward_message") {
        return { status: "resolved", body: "composed body" };
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
      expect(calls[0]?.[1]).toMatchObject({
        sources: [{ agent_id: AGENT_B.id, project_id: PROJECT_ID }],
        body: "please aggregate",
      });
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
        if (cmd === "forward_message") return { status: "resolved", body: "composed" };
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
      if (cmd === "forward_message") return { status: "resolved", body: "composed body" };
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

  it("dispatches the resolved forward body verbatim to a busy recipient", async () => {
    // The frontend half of the forward↔queue seam: the backend resolves a forward
    // into literal text, and the frontend dispatches *that string* through the
    // normal send path. A busy recipient therefore queues a send whose payload is
    // already the source's snapshotted output — `send_message` never carries a
    // reference to the source, so nothing can be re-resolved when the queued send
    // finally dispatches.
    const FORWARDED_BODY =
      "=== START forwarded from bob ===\nREVIEWER-OLD\n=== END forwarded from bob ===";
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);

    // Put recipient A genuinely mid-turn: its send is dispatched *and* the backend
    // has confirmed the turn started. `turn_start` is what moves the runtime to
    // "processing" and consumes the running send's pending entry — without it A
    // would sit in "starting" with its entry still queued, a different state.
    state.dispatchUserTurn(AGENT_A.id, "user-running", "long task", [], "send-running");
    fireTo(`agent:${AGENT_A.id}`, {
      type: "turn_start",
      turn_id: "t-running",
      message_id: "msg-running",
      send_id: "send-running",
      started_at: "2026-05-16T00:00:00Z",
    });
    await tick();
    expect(state.runtimes[AGENT_A.id]?.run_status).toBe("processing");
    expect(state.runtimes[AGENT_A.id]?.pending_sends ?? []).toHaveLength(0);

    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "forward_message") return { status: "resolved", body: FORWARDED_BODY };
      if (cmd === "send_message") return "msg-fwd";
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await pickForwardSource(AGENT_B.id);
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => {
      expect(invokeMock.mock.calls.filter(([c]) => c === "send_message")).toHaveLength(1);
    });

    const [, args] = invokeMock.mock.calls.find(([c]) => c === "send_message") as [
      string,
      Record<string, unknown>,
    ];
    expect(args.prompt).toBe(FORWARDED_BODY);
    expect(args.agentId).toBe(AGENT_A.id);

    // It lines up behind the running turn rather than replacing it: exactly one
    // queued send, and A keeps streaming its current response. The optimistic user
    // turn shows the same literal body.
    expect(state.runtimes[AGENT_A.id]?.pending_sends ?? []).toHaveLength(1);
    expect(state.runtimes[AGENT_A.id]?.run_status).toBe("processing");
    const queuedTurn = (state.transcripts[AGENT_A.id] ?? [])
      .filter((t) => t.role === "user")
      .at(-1);
    expect(queuedTurn && "text" in queuedTurn ? queuedTurn.text : undefined).toBe(FORWARDED_BODY);
  });

  it("warns on an idle-empty source, naming the consequence", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await pickForwardSource(AGENT_B.id);
    // The menus refuse to attach an already-empty source, so the state the chip
    // has to render is a source that goes empty *after* it was picked — a
    // restored draft, or a refresh that replaces the turn it was picked for.
    state.transcripts[AGENT_B.id] = [];
    await tick();

    const chipEl = screen.getByTestId("forward-source-chip-bob");
    expect(chipEl).toHaveAttribute("data-readiness", "empty");
    // State is a colour + trailing icon (with a tooltip), not inline text; the
    // consequence still reaches assistive tech as visually-hidden text.
    expect(screen.getByTestId("forward-source-state-bob")).toHaveAttribute(
      "data-state-readiness",
      "empty",
    );
    expect(within(chipEl).getByText(/has no forwardable output/i)).toBeInTheDocument();
  });

  it("does not warn on a source that is still streaming", async () => {
    // The reported bug: a streaming agent rendered in the failed status colour with
    // "no output", when the send in fact holds for that turn and forwards it.
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedStreamingTurn(AGENT_B.id);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await pickForwardSource(AGENT_B.id);

    const chipEl = screen.getByTestId("forward-source-chip-bob");
    expect(chipEl).toHaveAttribute("data-readiness", "pending");
    // Pending shows the "still generating" icon (tooltip + sr-only text), not the
    // empty warning; and never the failed status colour that read as a warning.
    const stateIcon = screen.getByTestId("forward-source-state-bob");
    expect(stateIcon).toHaveAttribute("data-state-readiness", "pending");
    expect(within(chipEl).getByText(/still generating; sending will wait/i)).toBeInTheDocument();
    expect(chipEl.className).not.toContain("status-failed");
  });

  it("treats a completed turn plus a newer streaming one as pending, not ready", async () => {
    // The forward awaits the in-flight turn and takes the *latest* completed output,
    // so this agent is not ready — the send holds. A `hasCompleted || isStreaming`
    // predicate would call it ready and mislead the user about what gets forwarded.
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedStreamingTurn(AGENT_B.id, true);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await pickForwardSource(AGENT_B.id);

    expect(screen.getByTestId("forward-source-chip-bob")).toHaveAttribute(
      "data-readiness",
      "pending",
    );
  });

  /// Put an agent in the state a project-open hydration leaves it in before (or
  /// instead of) succeeding: registered, empty transcript, history not read.
  async function withHydrationStatus(agentId: string, status: "loading" | "failed"): Promise<void> {
    const state = await loadState();
    state.runtimes[agentId] = { ...state.runtimes[agentId]!, hydration_status: status };
  }

  it.each(["loading", "failed"] as const)(
    "keeps an agent pickable while its history is %s, instead of calling it spent",
    async (status) => {
      // The trap: every agent is seeded with an empty transcript at registration,
      // and a failed read leaves it that way until the user retries hydration. If
      // "empty transcript" gates picking, forwarding is dead for an agent that may
      // have months of history — and the UI says it has no output.
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      await state.registerAgent(AGENT_B);
      await seedCompletedTurn(AGENT_A.id);
      await withHydrationStatus(AGENT_B.id, status);

      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

      const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
      await fireEvent.input(textarea, { target: { value: "@" } });
      const row = await screen.findByTestId(`forward-option-forward-agent:${AGENT_B.id}`);
      expect(row).not.toBeDisabled();
      expect(row).not.toHaveTextContent("no output");

      // And it actually attaches — the chip carries no verdict either way.
      await fireEvent.click(row);
      expect(await screen.findByTestId("forward-source-chip-bob")).toHaveAttribute(
        "data-readiness",
        "unknown",
      );
    },
  );

  it("still reports a streaming turn while the history read is in flight", async () => {
    // A streaming turn arrives on the live event channel, not from disk, so
    // withholding it behind hydration would hide an agent that is visibly working.
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedStreamingTurn(AGENT_B.id);
    await withHydrationStatus(AGENT_B.id, "loading");

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "@" } });

    expect(
      await screen.findByTestId(`forward-option-forward-agent:${AGENT_B.id}`),
    ).toHaveTextContent("still generating");
  });

  it("makes a spent agent unpickable in both forward surfaces, not merely annotated", async () => {
    // Picking a source with no output can only end in the backend refusing the
    // whole send, so the menus decline the choice rather than describing a
    // consequence the user then has to avoid.
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_A.id);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "@" } });
    const row = await screen.findByTestId(`forward-option-forward-agent:${AGENT_B.id}`);
    expect(row).toBeDisabled();
    expect(row).toHaveTextContent("no output");
    // The old copy explained the failure instead of preventing it.
    expect(row).not.toHaveTextContent("blocks the send");
    await fireEvent.click(row);
    expect(screen.queryByTestId("forward-source-chip-bob")).not.toBeInTheDocument();

    // The ↪ picker is the other surface onto the same question.
    await fireEvent.input(textarea, { target: { value: "" } });
    await fireEvent.click(screen.getByTestId("compose-forward-button"));
    const item = await screen.findByTestId(`forward-picker-agent-${AGENT_B.id}`);
    expect(item).toHaveAttribute("data-disabled");
    expect(item).toHaveTextContent("no output");
  });

  it("keyboard forward selection skips the spent agent", async () => {
    // Disabling only the click would leave Enter picking the very source the
    // menu refuses to click — `menuItems` is what arrow keys and Enter walk, so
    // the row has to be absent from it, not just styled out.
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await state.registerAgent(AGENT_C);
    await seedCompletedTurn(AGENT_C.id);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B, AGENT_C] } });
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "@" } });
    await screen.findByTestId(`forward-option-forward-agent:${AGENT_B.id}`);

    // Walk to the end of the list; the only forward entry reachable is carol's.
    for (let i = 0; i < 12; i++) {
      await fireEvent.keyDown(textarea, { key: "ArrowDown" });
      if (
        screen
          .getByTestId(`forward-option-forward-agent:${AGENT_C.id}`)
          .getAttribute("aria-selected") === "true"
      ) {
        break;
      }
    }
    expect(screen.getByTestId(`forward-option-forward-agent:${AGENT_B.id}`)).toHaveAttribute(
      "aria-selected",
      "false",
    );
    await fireEvent.keyDown(textarea, { key: "Enter" });
    expect(await screen.findByTestId("forward-source-chip-carol")).toBeInTheDocument();
  });

  it("the @-menu row and the chip agree about the same agent", async () => {
    // Four surfaces asked this question independently before; a shared derivation
    // is what stops them drifting.
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedStreamingTurn(AGENT_B.id);

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "@" } });
    const row = await screen.findByTestId(`forward-option-forward-agent:${AGENT_B.id}`);
    expect(row).toHaveTextContent("still generating");
    expect(row).not.toHaveTextContent("blocks the send");

    await fireEvent.click(row);
    // The chip carries the same pending state the row advertised.
    expect(screen.getByTestId("forward-source-state-bob")).toHaveAttribute(
      "data-state-readiness",
      "pending",
    );
  });

  it("restores forward sources after an unmount/remount (project switch, Git view)", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);
    const { unmount } = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] },
    });
    await pickForwardSource(AGENT_B.id);
    await screen.findByTestId("forward-source-chip-bob");
    unmount();

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    expect(await screen.findByTestId("forward-source-chip-bob")).toBeInTheDocument();
  });

  it("drops a restored forward source whose agent no longer exists", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);
    const { unmount } = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] },
    });
    await pickForwardSource(AGENT_B.id);
    await screen.findByTestId("forward-source-chip-bob");
    unmount();

    // AGENT_B removed from the roster while we were away.
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await tick();
    expect(screen.queryByTestId("forward-source-chip-bob")).toBeNull();
  });

  it("does not restore forward sources after a successful forward send", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "forward_message") return { status: "resolved", body: "x" };
      if (cmd === "send_message") return "msg-1";
      return null;
    });
    const { unmount } = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] },
    });
    await pickForwardSource(AGENT_B.id);
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "go" } });
    await fireEvent.click(screen.getByTestId("compose-send"));
    await waitFor(() =>
      expect(invokeMock.mock.calls.some(([c]) => c === "forward_message")).toBe(true),
    );
    unmount();

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await tick();
    expect(screen.queryByTestId("forward-source-chip-bob")).toBeNull();
  });

  it("composes multiple forward sources in declared order", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await state.registerAgent(AGENT_C);
    await seedCompletedTurn(AGENT_B.id);
    await seedCompletedTurn(AGENT_C.id);
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "forward_message") return { status: "resolved", body: "x" };
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
      expect(calls[0]?.[1]).toMatchObject({
        sources: [
          { agent_id: AGENT_B.id, project_id: PROJECT_ID },
          { agent_id: AGENT_C.id, project_id: PROJECT_ID },
        ],
      });
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

  it("flags a completed tools-only turn as empty, matching the backend rule", async () => {
    // Completed is not enough: the backend forwards the newest completed
    // turn's *text*, which is blank for a tools/thinking-only turn — the chip
    // must warn before submit rather than claim ready and fail at dispatch.
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    // Picked while it had text, then replaced by a tools-only turn — the route a
    // live chip actually takes into this state, now that an already-empty source
    // can't be picked in the first place.
    await seedCompletedTurn(AGENT_B.id);
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await pickForwardSource(AGENT_B.id);
    state.transcripts[AGENT_B.id] = [
      {
        role: "agent",
        turn_id: "t-tools-only",
        agent_id: AGENT_B.id,
        started_at: "2026-05-16T00:00:00Z",
        status: "complete",
        items: [
          {
            item_kind: "tool",
            tool_use_id: "t1",
            kind: "builtin",
            name: "Bash",
            input: {},
            facet: { facet_kind: "other" },
            output: "did things",
            is_error: false,
            started_at: "2026-05-16T00:00:00Z",
          },
          { item_kind: "text", kind: "thinking", text: "pondering" },
        ],
      },
    ];
    await tick();

    const chipEl = screen.getByTestId("forward-source-chip-bob");
    expect(chipEl).toHaveAttribute("data-readiness", "empty");
  });

  it("surfaces the empty-source invalidation reason and dispatches nothing", async () => {
    // The any-empty policy's frontend half: the backend blocks the send and
    // the user sees why in the compose-bar error, with the composer restored.
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);
    const dispatched: string[] = [];
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "forward_message") {
        return {
          status: "invalidated",
          reason: "bob has no forwardable text available; nothing was sent",
        };
      }
      if (cmd === "send_message") dispatched.push(cmd);
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await pickForwardSource(AGENT_B.id);
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "aggregate this" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => {
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent(
        "no forwardable text available",
      );
    });
    expect(dispatched).toEqual([]);
    // Restored, not consumed: chip and typed text return to the composer.
    expect(screen.getByTestId("forward-source-chip-bob")).toBeInTheDocument();
    expect((screen.getByTestId("compose-textarea") as HTMLTextAreaElement).value).toBe(
      "aggregate this",
    );
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

  it("restored attachments after an invalidated forward reach the store, not just the DOM", async () => {
    // Regression: the restore path assigned `attachmentChips` directly and
    // `persistComposeNow` never wrote attachments, so the chip showed but the
    // store stayed empty — and the next project load GC'd the staged file. Assert
    // the store, because the DOM assertion already passed with the bug present.
    const composeStore = await loadComposeStore();
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "forward_message") return { status: "invalidated", reason: "bob's turn failed" };
      if (cmd === "stage_attachment") {
        const source = String((args as { sourcePath?: unknown })?.sourcePath ?? "drop");
        const name = source.split("/").pop() ?? source;
        return {
          path: `/proj/.switchboard/projects/p/attachments/uuid__${name}`,
          original_name: name,
        };
      }
      if (cmd === "existing_attachment_paths") return (args as { paths?: string[] })?.paths ?? [];
      return null;
    });

    const { unmount } = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] },
    });
    fireDrop(["/a/diagram.png"]);
    await screen.findByTestId("attachment-chip-image-1");
    await pickForwardSource(AGENT_B.id);
    await fireEvent.input(screen.getByTestId("compose-textarea") as HTMLTextAreaElement, {
      target: { value: "aggregate this" },
    });
    await fireEvent.click(screen.getByTestId("compose-send"));

    // The chip is back in the DOM *and* mirrored to the store, so the load-time GC
    // will spare its file.
    await waitFor(() => expect(screen.getByTestId("attachment-chip-image-1")).toBeInTheDocument());
    await waitFor(() => expect(composeStore.getCompose(PROJECT_ID).attachments?.length).toBe(1));
    expect(composeStore.draftAttachmentPaths(PROJECT_ID)).toEqual([
      "/proj/.switchboard/projects/p/attachments/uuid__diagram.png",
    ]);

    // And it survives the unmount/remount the whole milestone is about.
    unmount();
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    expect(await screen.findByTestId("attachment-chip-image-1")).toBeInTheDocument();
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
      expect(held.forwardSourceIds(forwards[0]?.sources ?? [])).toEqual([AGENT_B.id]);
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
        { id: AGENT_A.id, name: "alice", projectId: PROJECT_ID },
        { id: AGENT_B.id, name: "bob", projectId: PROJECT_ID },
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
    resolveForward({ status: "resolved", body: "composed" });

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
        if (cmd === "render_prompt") return { kind: "rendered", text: "RENDERED" };
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

  /// The parent/child seam for cross-project sources.
  ///
  /// The child composers are tested standalone (a foreign pick lands in the right
  /// field) and `ComposeBar` is tested with same-project sources (a pick reaches
  /// IPC). Nothing joined the two — which is exactly how a shared commit callback
  /// once routed prompt- and workflow-field picks into the compose bar's hidden
  /// plain-message list while every component test and the type checker stayed
  /// green. These drive the real nested menu and read the final payload.
  describe("a foreign pick reaches IPC with its owning project", () => {
    const OTHER_PROJECT = "00000000-0000-7000-8000-0000000000aa";
    const FOREIGN_AGENT: AgentRecord = {
      id: "00000000-0000-7000-8000-0000000000bb",
      project_id: OTHER_PROJECT,
      name: "oracle",
      harness: "claude_code",
      session_locator: null,
      created_at: "2026-05-16T00:00:00Z",
    };

    async function withOtherProject(): Promise<void> {
      const ws = await loadWorkspace();
      ws.projects.list = [
        {
          id: PROJECT_ID,
          name: "here",
          created_at: "2026-05-16T00:00:00Z",
          directory: "/work/here",
          available: true,
          last_activity: "2026-05-16T00:00:00Z",
          archived: false,
        },
        {
          id: OTHER_PROJECT,
          name: "backend",
          created_at: "2026-05-16T00:00:00Z",
          directory: "/work/backend",
          available: true,
          last_activity: "2026-05-16T00:00:00Z",
          archived: false,
        },
      ];
    }

    /// Drive the picker that `triggerTestid` opens down to the foreign agent.
    async function pickForeignThrough(triggerTestid: string): Promise<void> {
      await fireEvent.click(screen.getByTestId(triggerTestid));
      await fireEvent.click(await screen.findByTestId("forward-picker-projects-trigger"));
      await fireEvent.click(
        await screen.findByTestId(`forward-picker-project-toggle-${OTHER_PROJECT}`),
      );
      await fireEvent.click(
        await screen.findByTestId(`forward-picker-foreign-agent-${FOREIGN_AGENT.id}`),
      );
    }

    it("carries a prompt argument's foreign source through to forward_prompt", async () => {
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      await withOtherProject();
      invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
        if (cmd === "list_prompts") return [REVIEW];
        if (cmd === "list_project_agents_readonly") return [FOREIGN_AGENT];
        if (cmd === "open_project") return { id: OTHER_PROJECT, name: "backend" };
        if (cmd === "forward_prompt") return { status: "resolved", body: "RENDERED" };
        if (cmd === "send_message") return "msg-1";
        return null;
      });
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
      await enterPromptMode("prompt-option-local:review");

      await pickForeignThrough("prompt-arg-forward-focus");
      await screen.findByTestId(`forward-source-chip-${FOREIGN_AGENT.name}`);
      await fireEvent.click(screen.getByTestId("compose-send"));

      await waitFor(() => {
        const calls = invokeMock.mock.calls.filter(([c]) => c === "forward_prompt");
        expect(calls).toHaveLength(1);
        // The source is scoped to *its own* project, not the composing one — an
        // agent id alone names no project the backend could open.
        expect(calls[0]?.[1]).toMatchObject({
          projectId: PROJECT_ID,
          forwardArgs: [
            {
              name: "focus",
              sources: [{ agent_id: FOREIGN_AGENT.id, project_id: OTHER_PROJECT }],
            },
          ],
        });
      });
    });

    it("carries a workflow field's foreign source through to invoke_workflow", async () => {
      const WORKFLOW = {
        name: "review-and-recommend",
        is_builtin: true,
        description: "d",
        inputs: [{ name: "worker", ty: "agent", optional: false, description: null }],
        invocable: true,
        parse_error: null,
      };
      const DESCRIPTOR = {
        ...WORKFLOW,
        steps: [],
        derived_args: [
          { name: "context", required: false, description: null, prompts: ["builtin:code-review"] },
        ],
        compatibility: { state: "ok" },
      };
      const state = await loadState();
      await state.registerAgent(AGENT_A);
      await withOtherProject();
      invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
        if (cmd === "list_workflows") return [WORKFLOW];
        if (cmd === "list_prompts") return [];
        if (cmd === "describe_workflow_form") return DESCRIPTOR;
        if (cmd === "refresh_workflow_form_from_cache") return DESCRIPTOR;
        if (cmd === "list_project_agents_readonly") return [FOREIGN_AGENT];
        if (cmd === "open_project") return { id: OTHER_PROJECT, name: "backend" };
        if (cmd === "invoke_workflow") return "run-1";
        return null;
      });
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

      await fireEvent.click(screen.getByTestId("compose-workflow-button"));
      await fireEvent.click(
        await screen.findByTestId("workflow-option-builtin:review-and-recommend"),
      );
      await waitFor(() => screen.getByTestId("workflow-arg-input-context"));
      await fireEvent.click(screen.getByTestId("workflow-agent-worker-alice"));

      await pickForeignThrough("workflow-forward-picker-context");
      await screen.findByTestId(`forward-source-chip-${FOREIGN_AGENT.name}`);
      await fireEvent.click(screen.getByTestId("workflow-invoke-button"));

      await waitFor(() => {
        const call = invokeMock.mock.calls.find(([c]) => c === "invoke_workflow");
        expect(call?.[1]).toMatchObject({
          name: "review-and-recommend",
          forwardSources: {
            context: [{ agent_id: FOREIGN_AGENT.id, project_id: OTHER_PROJECT }],
          },
        });
      });
    });
  });

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
      if (cmd === "forward_message") return { status: "resolved", body: "composed" };
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
      expect(calls[0]?.[1]).toMatchObject({
        sources: [
          { agent_id: AGENT_A.id, project_id: PROJECT_ID },
          { agent_id: AGENT_B.id, project_id: PROJECT_ID },
        ],
      });
    });
  });

  it("hides the compose-bar forward button and chips in prompt mode", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);
    mockPromptForwardBackend({ status: "resolved", body: "x" });

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
    mockPromptForwardBackend({ status: "resolved", body: "x" });

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
    mockPromptForwardBackend({ status: "resolved", body: "RENDERED" });

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
    copyTextMock.mockClear();
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
    mockPromptForwardBackend({ status: "resolved", body: "RENDERED + APPENDED" });

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
      expect(calls[0]?.[1]).toMatchObject({
        forwardArgs: [],
        appendedSources: [{ agent_id: AGENT_B.id, project_id: PROJECT_ID }],
        projectId: PROJECT_ID,
      });
      // Exact argument set. `forward_prompt` carries eight arguments and gained
      // `projectId` in the same change that broke `forward_message` by omitting
      // it — a required Tauri argument the wrapper doesn't send fails
      // deserialization before the handler runs, which no mocked-`invoke` test
      // sees unless it looks at the keys.
      expect(Object.keys(calls[0]?.[1] ?? {}).sort()).toEqual(
        [
          "appendedSources",
          "appendedText",
          "forwardArgs",
          "forwardId",
          "name",
          "projectId",
          "provider",
          "typedArgs",
        ].sort(),
      );
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
    mockPromptForwardBackend({ status: "resolved", body: "RENDERED WITH FORWARD" });

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
        forwardArgs: [
          {
            name: "focus",
            sources: [{ agent_id: AGENT_B.id, project_id: PROJECT_ID }],
            required: true,
          },
        ],
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
    mockPromptForwardBackend({ status: "resolved", body: "BODY" });

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
      expect(held.forwardSourceIds(forwards[0]?.sources ?? [])).toEqual([AGENT_B.id]);
      expect(forwards[0]?.recipients).toEqual([AGENT_A.id]);
      // The prompt's body is never pre-composed (it renders server-side after
      // sources resolve), so the held entry carries the prompt's display name
      // — the only content the transcript can show while it waits.
      expect(forwards[0]?.promptName).toBe("review");
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
    mockPromptForwardBackend({ status: "resolved", body: "RENDERED BODY" });

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

  it("keeps workflow loading and failure states out of the plain composer", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    await seedCompletedTurn(AGENT_B.id);
    const WORKFLOW = {
      name: "slow-workflow",
      is_builtin: true,
      description: "d",
      inputs: [],
      invocable: true,
      parse_error: null,
    };
    let rejectDescribe: ((error: Error) => void) | undefined;
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workflows") return [WORKFLOW];
      if (cmd === "list_prompts") return [];
      if (cmd === "stage_attachment") {
        return {
          path: "/proj/.switchboard/projects/p/attachments/uuid__diagram.png",
          original_name: "diagram.png",
        };
      }
      if (cmd === "describe_workflow_form") {
        return await new Promise<never>((_resolve, reject) => {
          rejectDescribe = reject;
        });
      }
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    fireDrop(["/a/diagram.png"]);
    await waitFor(() => expect(screen.getByTestId("attachment-chip-image-1")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("compose-forward-button"));
    await fireEvent.click(await screen.findByTestId(`forward-picker-agent-${AGENT_B.id}`));
    await waitFor(() => expect(screen.getByTestId("forward-source-chips")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("compose-workflow-button"));
    await fireEvent.click(await screen.findByTestId("workflow-option-builtin:slow-workflow"));

    await waitFor(() => expect(screen.getByTestId("compose-workflow-loading")).toBeInTheDocument());
    expect(screen.getAllByTestId("attachment-chips")).toHaveLength(1);
    expect(screen.queryByTestId("compose-textarea")).toBeNull();
    expect(screen.queryByTestId("recipient-field")).toBeNull();
    expect(screen.queryByTestId("forward-source-chips")).toBeNull();
    expect(screen.queryByTestId("compose-action-rail")).toBeNull();
    expect(screen.queryByTestId("compose-send")).toBeNull();

    rejectDescribe?.(new Error("descriptor unavailable"));
    await waitFor(() =>
      expect(screen.getByTestId("compose-workflow-load-failed")).toHaveTextContent(
        "descriptor unavailable",
      ),
    );
    expect(screen.getAllByTestId("attachment-chips")).toHaveLength(1);
    expect(screen.queryByTestId("compose-textarea")).toBeNull();
    expect(screen.queryByTestId("compose-send")).toBeNull();

    await fireEvent.click(screen.getByTestId("workflow-form-start-over"));
    expect(screen.getByTestId("compose-textarea")).toBeInTheDocument();
    expect(screen.getByTestId("compose-action-rail")).toBeInTheDocument();
  });

  it("retries a workflow form that initially fails to load", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const WORKFLOW = {
      name: "retry-workflow",
      is_builtin: true,
      description: "d",
      inputs: [],
      invocable: true,
      parse_error: null,
    };
    const DESCRIPTOR = {
      ...WORKFLOW,
      steps: [],
      derived_args: [],
      compatibility: { state: "ok" },
    };
    let describeCalls = 0;
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workflows") return [WORKFLOW];
      if (cmd === "list_prompts") return [];
      if (cmd === "describe_workflow_form") {
        describeCalls += 1;
        if (describeCalls === 1) throw new Error("temporary failure");
        return DESCRIPTOR;
      }
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await fireEvent.click(screen.getByTestId("compose-workflow-button"));
    await fireEvent.click(await screen.findByTestId("workflow-option-builtin:retry-workflow"));
    await screen.findByTestId("compose-workflow-load-failed");

    await fireEvent.click(screen.getByTestId("workflow-form-retry"));
    await waitFor(() => expect(screen.getByTestId("workflow-composer")).toBeInTheDocument());
    expect(describeCalls).toBe(2);
    expect(screen.queryByTestId("compose-workflow-load-failed")).toBeNull();
  });

  it("signs in from an unavailable workflow and refreshes its form exactly once", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const WORKFLOW = {
      name: "oauth-workflow",
      is_builtin: true,
      description: "d",
      inputs: [],
      invocable: true,
      parse_error: null,
    };
    const unavailable = {
      ...WORKFLOW,
      steps: [],
      derived_args: [],
      compatibility: {
        state: "unavailable",
        issues: [
          {
            prompt: "tiddly:review",
            provider: "tiddly",
            kind: "needs_auth",
            message: null,
          },
        ],
      },
    };
    const available = {
      ...WORKFLOW,
      steps: [],
      derived_args: [],
      compatibility: { state: "ok" },
    };
    let describeCalls = 0;
    let releaseSignIn!: () => void;
    const signInGate = new Promise<void>((resolve) => {
      releaseSignIn = resolve;
    });
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workflows") return [WORKFLOW];
      if (cmd === "list_prompts") return [];
      if (cmd === "describe_workflow_form") {
        describeCalls += 1;
        return describeCalls === 1 ? unavailable : available;
      }
      if (cmd === "sign_in_mcp_provider") return await signInGate;
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await fireEvent.click(screen.getByTestId("compose-workflow-button"));
    await fireEvent.click(await screen.findByTestId("workflow-option-builtin:oauth-workflow"));
    const signIn = await screen.findByTestId("workflow-prompt-sign-in-tiddly");

    await fireEvent.click(signIn);
    await waitFor(() => expect(signIn).toHaveTextContent("Waiting for tiddly sign-in"));
    expect(signIn).toBeDisabled();
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "sign_in_mcp_provider")).toHaveLength(1);
    await fireEvent.click(signIn);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "sign_in_mcp_provider")).toHaveLength(1);

    releaseSignIn();
    await waitFor(() => expect(screen.queryByTestId("workflow-prompt-unavailable")).toBeNull());
    expect(describeCalls).toBe(2);
    expect(screen.getByTestId("workflow-invoke-button")).toBeEnabled();
  });

  it("does not refresh or surface an error after unmounting during workflow sign-in", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const WORKFLOW = {
      name: "oauth-workflow",
      is_builtin: true,
      description: "d",
      inputs: [],
      invocable: true,
      parse_error: null,
    };
    const unavailable = {
      ...WORKFLOW,
      steps: [],
      derived_args: [],
      compatibility: {
        state: "unavailable",
        issues: [
          {
            prompt: "tiddly:review",
            provider: "tiddly",
            kind: "needs_auth",
            message: null,
          },
        ],
      },
    };
    let describeCalls = 0;
    let releaseSignIn!: () => void;
    let markSignInComplete!: () => void;
    const signInGate = new Promise<void>((resolve) => {
      releaseSignIn = resolve;
    });
    const signInComplete = new Promise<void>((resolve) => {
      markSignInComplete = resolve;
    });
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workflows") return [WORKFLOW];
      if (cmd === "list_prompts") return [];
      if (cmd === "describe_workflow_form") {
        describeCalls += 1;
        return unavailable;
      }
      if (cmd === "sign_in_mcp_provider") {
        await signInGate;
        markSignInComplete();
        return null;
      }
      return null;
    });

    const view = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A] },
    });
    await fireEvent.click(screen.getByTestId("compose-workflow-button"));
    await fireEvent.click(await screen.findByTestId("workflow-option-builtin:oauth-workflow"));
    await fireEvent.click(await screen.findByTestId("workflow-prompt-sign-in-tiddly"));
    await waitFor(() =>
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "sign_in_mcp_provider")).toHaveLength(
        1,
      ),
    );

    view.unmount();
    releaseSignIn();
    await signInComplete;
    await Promise.resolve();
    expect(describeCalls).toBe(1);
  });

  it("keeps an unavailable workflow retryable when browser sign-in fails", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const WORKFLOW = {
      name: "oauth-workflow",
      is_builtin: true,
      description: "d",
      inputs: [],
      invocable: true,
      parse_error: null,
    };
    const unavailable = {
      ...WORKFLOW,
      steps: [],
      derived_args: [],
      compatibility: {
        state: "unavailable",
        issues: [
          {
            prompt: "tiddly:review",
            provider: "tiddly",
            kind: "needs_auth",
            message: null,
          },
        ],
      },
    };
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workflows") return [WORKFLOW];
      if (cmd === "list_prompts") return [];
      if (cmd === "describe_workflow_form") return unavailable;
      if (cmd === "sign_in_mcp_provider") throw new Error("access_denied");
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await fireEvent.click(screen.getByTestId("compose-workflow-button"));
    await fireEvent.click(await screen.findByTestId("workflow-option-builtin:oauth-workflow"));
    await fireEvent.click(await screen.findByTestId("workflow-prompt-sign-in-tiddly"));

    await waitFor(() =>
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent(
        "Couldn't sign in to tiddly: access_denied",
      ),
    );
    expect(screen.getByTestId("workflow-prompt-unavailable")).toBeInTheDocument();
    expect(screen.getByTestId("workflow-prompt-sign-in-tiddly")).toBeEnabled();
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
    await seedCompletedTurn(AGENT_A.id);
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
      forwardSources: { context: [{ agent_id: AGENT_A.id, project_id: PROJECT_ID }] },
    });
  });

  it("uses the authoritative workflow-form reply without waiting for prompts:synced", async () => {
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
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workflows") return [WORKFLOW];
      if (cmd === "list_prompts") return [];
      if (cmd === "describe_workflow_form") {
        // The backend owns cold-cache recovery now: it conditionally syncs before
        // returning, so this reply is settled even if the global startup event
        // fired before the component subscribed.
        return { ...BASE, compatibility: { state: "ok" } };
      }
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await fireEvent.click(screen.getByTestId("compose-workflow-button"));
    await waitFor(() => screen.getByTestId("workflow-option-dir:mcp-flow"));
    await fireEvent.click(screen.getByTestId("workflow-option-dir:mcp-flow"));

    // No prompts:synced event is fired after the pick. The command reply alone
    // resolves the form, so a missed startup event cannot strand the spinner.
    await waitFor(() => screen.getByTestId("workflow-agent-worker-alice"));
    expect(screen.queryByTestId("workflow-resolving")).toBeNull();
  });

  it("treats a still-unresolved authoritative reply as settled instead of spinning", async () => {
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

    // This fallback protects against an older backend or an unexpected unresolved
    // reply: a completed describe call is treated as settled, never an unbounded
    // wait for a future global event.
    await waitFor(() => screen.getByTestId("workflow-prompt-missing"));
    expect(screen.queryByTestId("workflow-resolving")).toBeNull();
  });

  it("coalesces sync events during a fresh describe into one cache-only refresh", async () => {
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
    };
    let resolveDescribe: ((descriptor: unknown) => void) | undefined;
    let freshCalls = 0;
    let cacheOnlyCalls = 0;
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workflows") return [WORKFLOW];
      if (cmd === "list_prompts") return [];
      if (cmd === "describe_workflow_form") {
        freshCalls++;
        return new Promise((resolve) => (resolveDescribe = resolve));
      }
      if (cmd === "refresh_workflow_form_from_cache") {
        cacheOnlyCalls++;
        return {
          ...base,
          derived_args: [
            {
              name: "new",
              required: false,
              description: null,
              prompts: ["tiddly:review"],
            },
          ],
          compatibility: { state: "ok" },
        };
      }
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await fireEvent.click(screen.getByTestId("compose-workflow-button"));
    await waitFor(() => screen.getByTestId("workflow-option-dir:race"));
    await fireEvent.click(screen.getByTestId("workflow-option-dir:race"));
    await waitFor(() => expect(freshCalls).toBe(1));

    // Multiple events during one fresh request coalesce. The fresh cache hit may
    // represent the preceding completed generation, so one cache-only pass must
    // apply the newer schema after the authoritative response settles.
    listeners.get("prompts:changed")?.({ payload: { generation: 1 } });
    listeners.get("prompts:changed")?.({ payload: { generation: 2 } });
    await tick();
    expect(freshCalls).toBe(1);
    expect(cacheOnlyCalls).toBe(0);

    resolveDescribe?.({
      ...base,
      derived_args: [
        {
          name: "old",
          required: false,
          description: null,
          prompts: ["tiddly:review"],
        },
      ],
      compatibility: { state: "ok" },
    });
    await waitFor(() => screen.getByTestId("workflow-arg-input-new"));
    expect(cacheOnlyCalls).toBe(1);
    expect(screen.queryByTestId("workflow-arg-input-old")).toBeNull();
  });

  it("uses only cache reclassification after an independent sync", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const WORKFLOW = {
      name: "cache-refresh",
      is_builtin: false,
      description: "d",
      inputs: [{ name: "worker", ty: "agent", optional: false, description: null }],
      invocable: true,
      parse_error: null,
    };
    const base = {
      name: WORKFLOW.name,
      description: "d",
      is_builtin: false,
      invocable: true,
      inputs: WORKFLOW.inputs,
      steps: [],
      derived_args: [] as unknown[],
    };
    let freshCalls = 0;
    let cacheOnlyCalls = 0;
    const cacheReplies: Array<(descriptor: unknown) => void> = [];
    const unavailable = {
      ...base,
      compatibility: {
        state: "unavailable",
        issues: [
          {
            prompt: "tiddly:review",
            provider: "tiddly",
            kind: "provider_error",
            message: "timed out",
          },
        ],
      },
    };
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workflows") return [WORKFLOW];
      if (cmd === "list_prompts") return [];
      if (cmd === "describe_workflow_form") {
        freshCalls++;
        return unavailable;
      }
      if (cmd === "refresh_workflow_form_from_cache") {
        cacheOnlyCalls++;
        return new Promise((resolve) => cacheReplies.push(resolve));
      }
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await fireEvent.click(screen.getByTestId("compose-workflow-button"));
    await waitFor(() => screen.getByTestId("workflow-option-dir:cache-refresh"));
    await fireEvent.click(screen.getByTestId("workflow-option-dir:cache-refresh"));
    await waitFor(() => screen.getByTestId("workflow-prompt-unavailable"));

    listeners.get("prompts:changed")?.({ payload: { generation: 1 } });
    await waitFor(() => expect(cacheOnlyCalls).toBe(1));
    // A newer sync must supersede an in-flight cache-only classification instead
    // of being dropped. Neither event is allowed to call the fresh endpoint.
    listeners.get("prompts:changed")?.({ payload: { generation: 2 } });
    await waitFor(() => expect(cacheOnlyCalls).toBe(2));
    cacheReplies[1]?.({ ...base, compatibility: { state: "ok" } });
    await waitFor(() => screen.getByTestId("workflow-agent-worker-alice"));
    cacheReplies[0]?.(unavailable);
    await tick();

    expect(freshCalls).toBe(1);
    expect(cacheOnlyCalls).toBe(2);
    expect(screen.getByTestId("workflow-agent-worker-alice")).toBeInTheDocument();
  });

  it("prunes obsolete derived values and forwards after a successful schema refresh", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await seedCompletedTurn(AGENT_A.id);
    const WORKFLOW = {
      name: "schema-change",
      is_builtin: false,
      description: "d",
      inputs: [{ name: "worker", ty: "agent", optional: false, description: null }],
      invocable: true,
      parse_error: null,
    };
    const base = {
      name: WORKFLOW.name,
      description: "d",
      is_builtin: false,
      invocable: true,
      inputs: WORKFLOW.inputs,
      steps: [],
    };
    const derived = (name: string) => ({
      name,
      required: false,
      description: null,
      prompts: ["tiddly:review"],
    });
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workflows") return [WORKFLOW];
      if (cmd === "list_prompts") return [];
      if (cmd === "describe_workflow_form") {
        return { ...base, derived_args: [derived("old")], compatibility: { state: "ok" } };
      }
      if (cmd === "refresh_workflow_form_from_cache") {
        return { ...base, derived_args: [derived("new")], compatibility: { state: "ok" } };
      }
      if (cmd === "invoke_workflow") return "run-1";
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await fireEvent.click(screen.getByTestId("compose-workflow-button"));
    await waitFor(() => screen.getByTestId("workflow-option-dir:schema-change"));
    await fireEvent.click(screen.getByTestId("workflow-option-dir:schema-change"));
    const oldInput = (await screen.findByTestId("workflow-arg-input-old")) as HTMLTextAreaElement;
    await fireEvent.input(oldInput, { target: { value: "obsolete" } });
    await fireEvent.click(screen.getByTestId("workflow-forward-picker-old"));
    await fireEvent.click(await screen.findByTestId(`forward-picker-agent-${AGENT_A.id}`));
    await waitFor(() => screen.getByTestId("workflow-forward-sources-old"));

    listeners.get("prompts:changed")?.({ payload: { generation: 1 } });
    const newInput = (await screen.findByTestId("workflow-arg-input-new")) as HTMLTextAreaElement;
    expect(screen.queryByTestId("workflow-arg-input-old")).toBeNull();
    expect(screen.queryByTestId("workflow-forward-sources-old")).toBeNull();
    await fireEvent.input(newInput, { target: { value: "current" } });
    await fireEvent.click(screen.getByTestId("workflow-agent-worker-alice"));
    await fireEvent.click(screen.getByTestId("workflow-invoke-button"));
    await waitFor(() => {
      expect(invokeMock.mock.calls.some(([cmd]) => cmd === "invoke_workflow")).toBe(true);
    });
    const invocation = invokeMock.mock.calls.find(([cmd]) => cmd === "invoke_workflow");
    expect(invocation?.[1]).toMatchObject({
      inputs: { worker: AGENT_A.name, new: "current" },
      forwardSources: {},
    });
    expect(invocation?.[1]).not.toMatchObject({ inputs: { old: expect.anything() } });
  });

  it("preserves derived drafts and forwards through a transient unavailable refresh", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await seedCompletedTurn(AGENT_A.id);
    const WORKFLOW = {
      name: "transient-outage",
      is_builtin: false,
      description: "d",
      inputs: [{ name: "worker", ty: "agent", optional: false, description: null }],
      invocable: true,
      parse_error: null,
    };
    const argument = {
      name: "context",
      required: false,
      description: null,
      prompts: ["tiddly:review"],
    };
    const base = {
      name: WORKFLOW.name,
      description: "d",
      is_builtin: false,
      invocable: true,
      inputs: WORKFLOW.inputs,
      steps: [],
    };
    let cacheCalls = 0;
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workflows") return [WORKFLOW];
      if (cmd === "list_prompts") return [];
      if (cmd === "describe_workflow_form") {
        return { ...base, derived_args: [argument], compatibility: { state: "ok" } };
      }
      if (cmd === "refresh_workflow_form_from_cache") {
        cacheCalls++;
        return cacheCalls <= 2
          ? {
              ...base,
              derived_args: [],
              compatibility: {
                state: "unavailable",
                issues: [
                  {
                    prompt: "tiddly:review",
                    provider: "tiddly",
                    kind: "provider_error",
                    message: "timed out",
                  },
                ],
              },
            }
          : { ...base, derived_args: [argument], compatibility: { state: "ok" } };
      }
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await fireEvent.click(screen.getByTestId("compose-workflow-button"));
    await waitFor(() => screen.getByTestId("workflow-option-dir:transient-outage"));
    await fireEvent.click(screen.getByTestId("workflow-option-dir:transient-outage"));
    const input = (await screen.findByTestId("workflow-arg-input-context")) as HTMLTextAreaElement;
    await fireEvent.input(input, { target: { value: "keep me" } });
    await fireEvent.click(screen.getByTestId("workflow-forward-picker-context"));
    await fireEvent.click(await screen.findByTestId(`forward-picker-agent-${AGENT_A.id}`));

    listeners.get("prompts:changed")?.({ payload: { generation: 1 } });
    await waitFor(() => screen.getByTestId("workflow-prompt-unavailable"));
    listeners.get("prompts:changed")?.({ payload: { generation: 2 } });
    await waitFor(() => expect(cacheCalls).toBe(2));
    listeners.get("prompts:changed")?.({ payload: { generation: 3 } });
    const recovered = (await screen.findByTestId(
      "workflow-arg-input-context",
    )) as HTMLTextAreaElement;
    expect(recovered).toHaveValue("keep me");
    expect(screen.getByTestId("workflow-forward-sources-context")).toBeInTheDocument();
  });

  it("clears a declared value when its semantic input type changes", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const WORKFLOW = {
      name: "input-type-change",
      is_builtin: false,
      description: "d",
      inputs: [{ name: "target", ty: "agent", optional: false, description: null }],
      invocable: true,
      parse_error: null,
    };
    const descriptorBase = {
      name: WORKFLOW.name,
      description: "d",
      is_builtin: false,
      invocable: true,
      steps: [],
      derived_args: [],
      compatibility: { state: "ok" },
    };
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workflows") return [WORKFLOW];
      if (cmd === "list_prompts") return [];
      if (cmd === "describe_workflow_form") return { ...descriptorBase, inputs: WORKFLOW.inputs };
      if (cmd === "refresh_workflow_form_from_cache") {
        return {
          ...descriptorBase,
          inputs: [{ name: "target", ty: "text", optional: false, description: null }],
        };
      }
      return null;
    });

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await fireEvent.click(screen.getByTestId("compose-workflow-button"));
    await waitFor(() => screen.getByTestId("workflow-option-dir:input-type-change"));
    await fireEvent.click(screen.getByTestId("workflow-option-dir:input-type-change"));
    await fireEvent.click(await screen.findByTestId(`workflow-agent-target-${AGENT_A.name}`));
    await waitFor(() =>
      expect(screen.getByTestId(`workflow-agent-target-${AGENT_A.name}`)).toHaveAttribute(
        "aria-checked",
        "true",
      ),
    );

    listeners.get("prompts:changed")?.({ payload: { generation: 1 } });
    const textInput = (await screen.findByTestId("workflow-text-target")) as HTMLTextAreaElement;
    expect(textInput).toHaveValue("");
  });
});

describe("ComposeBar — workflow invocation survives navigation", () => {
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
      { name: "context", required: false, description: "Optional background", prompts: [] },
    ],
    compatibility: { state: "ok" },
  };

  function mockWorkflows(list: unknown[] = [WORKFLOW]): void {
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workflows") return list;
      if (cmd === "describe_workflow_form") return DESCRIPTOR;
      if (cmd === "list_prompts") return [];
      return null;
    });
  }

  async function enterWorkflowModeAndFill(): Promise<void> {
    await fireEvent.click(screen.getByTestId("compose-workflow-button"));
    await waitFor(() => screen.getByTestId("workflow-option-builtin:review-and-recommend"));
    await fireEvent.click(screen.getByTestId("workflow-option-builtin:review-and-recommend"));
    await waitFor(() => screen.getByTestId("workflow-arg-input-context"));
    await fireEvent.click(screen.getByTestId("workflow-agent-worker-alice"));
    await fireEvent.input(screen.getByTestId("workflow-arg-input-context"), {
      target: { value: "focus on error handling" },
    });
  }

  it("restores workflow mode and its field values after an unmount/remount", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    mockWorkflows();
    const { unmount } = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] },
    });
    await enterWorkflowModeAndFill();
    unmount();

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    // Back in workflow mode, not the plain textarea, with the typed value intact.
    await waitFor(() => screen.getByTestId("workflow-composer"));
    const context = (await screen.findByTestId(
      "workflow-arg-input-context",
    )) as HTMLTextAreaElement;
    expect(context.value).toBe("focus on error handling");
  });

  it("falls back to plain mode when the saved workflow no longer exists", async () => {
    // `list_workflows` reads the local filesystem, so a miss is a real deletion —
    // not a cold cache — and must not strand the composer in an uninvocable form.
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    mockWorkflows();
    const { unmount } = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] },
    });
    await enterWorkflowModeAndFill();
    unmount();

    mockWorkflows([]); // the workflow was deleted while we were away
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    expect(screen.queryByTestId("workflow-composer")).toBeNull();
    expect(screen.queryByTestId("compose-restoring")).toBeNull();
  });

  it("does not restore a same-named workflow of the other builtin-ness", async () => {
    // A built-in and a copied user workflow can share a name; `isBuiltin` is part
    // of the saved identity, so a copy must not silently stand in for the original.
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    mockWorkflows();
    const { unmount } = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] },
    });
    await enterWorkflowModeAndFill();
    unmount();

    mockWorkflows([{ ...WORKFLOW, is_builtin: false }]);
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    expect(screen.queryByTestId("workflow-composer")).toBeNull();
  });

  it("does not restore workflow mode after the workflow was invoked", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workflows") return [WORKFLOW];
      if (cmd === "describe_workflow_form") return DESCRIPTOR;
      if (cmd === "list_prompts") return [];
      if (cmd === "invoke_workflow") return "run-1";
      if (cmd === "list_workflow_runs") return [];
      return null;
    });
    const { unmount } = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] },
    });
    await enterWorkflowModeAndFill();
    await fireEvent.click(screen.getByTestId("workflow-agent-reviewers-bob"));
    await fireEvent.click(screen.getByTestId("workflow-invoke-button"));
    await waitFor(() =>
      expect(invokeMock.mock.calls.some(([c]) => c === "invoke_workflow")).toBe(true),
    );
    unmount();

    workflowsTesting.reset();
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    expect(screen.queryByTestId("workflow-composer")).toBeNull();
  });

  it("preserves the saved workflow when list_workflows FAILS (not a confirmed deletion)", async () => {
    // A transient FS/IPC error must not destroy a half-filled invocation. This is
    // the case a bare `find(...) === undefined` conflated with a real deletion.
    const composeStore = await loadComposeStore();
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    mockWorkflows();
    const { unmount } = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] },
    });
    await enterWorkflowModeAndFill();
    unmount();

    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workflows") throw new Error("filesystem hiccup");
      if (cmd === "list_prompts") return [];
      return null;
    });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    // Retry/discard offered; NOT dropped into a plain composer.
    await waitFor(() =>
      expect(screen.getByTestId("compose-workflow-restore-failed")).toBeInTheDocument(),
    );
    expect(screen.queryByTestId("compose-textarea")).toBeNull();
    // The snapshot is untouched in the store.
    expect(composeStore.getCompose(PROJECT_ID).content.kind).toBe("workflow");
  });

  it("recovers the workflow when Retry succeeds after a failed list", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    mockWorkflows();
    const { unmount } = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] },
    });
    await enterWorkflowModeAndFill();
    unmount();

    let listShouldFail = true;
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workflows") {
        if (listShouldFail) throw new Error("filesystem hiccup");
        return [WORKFLOW];
      }
      if (cmd === "describe_workflow_form") return DESCRIPTOR;
      if (cmd === "list_prompts") return [];
      return null;
    });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await screen.findByTestId("compose-workflow-restore-failed");

    listShouldFail = false;
    await fireEvent.click(screen.getByTestId("workflow-restore-retry"));

    await waitFor(() => screen.getByTestId("workflow-composer"));
    const context = (await screen.findByTestId(
      "workflow-arg-input-context",
    )) as HTMLTextAreaElement;
    expect(context.value).toBe("focus on error handling");
  });

  it("clears the saved workflow when a SUCCESSFUL list confirms it is absent", async () => {
    // The other direction of the distinction: a successful empty list is
    // authoritative and *should* release the snapshot.
    const composeStore = await loadComposeStore();
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    mockWorkflows();
    const { unmount } = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] },
    });
    await enterWorkflowModeAndFill();
    unmount();

    mockWorkflows([]); // successful, empty
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    await waitFor(() => expect(composeStore.getCompose(PROJECT_ID).content.kind).toBe("plain"));
  });

  it("Start over on a failed restore drops to a usable plain composer", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    mockWorkflows();
    const { unmount } = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] },
    });
    await enterWorkflowModeAndFill();
    unmount();

    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workflows") throw new Error("filesystem hiccup");
      if (cmd === "list_prompts") return [];
      return null;
    });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await screen.findByTestId("compose-workflow-restore-failed");

    await fireEvent.click(screen.getByTestId("workflow-restore-discard"));
    expect(await screen.findByTestId("compose-textarea")).toBeInTheDocument();
    expect(screen.queryByTestId("compose-workflow-restore-failed")).toBeNull();
  });
});

describe("ComposeBar — workflow run live view (swap / hold / stop)", () => {
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
          prompt: { kind: "inline", text: "go" },
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
