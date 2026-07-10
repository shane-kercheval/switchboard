import { beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import PromptComposer from "./PromptComposer.svelte";
import ForwardHarness from "./_PromptComposerForwardHarness.svelte";
import type { AgentRecord, Prompt } from "$lib/types";
import type { TranscriptPane } from "$lib/state/transcriptPanes.svelte";
import type { ForwardSource } from "$lib/state/heldForwards.svelte";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

const PROMPT: Prompt = {
  provider: "local",
  name: "review",
  title: "Code Review",
  description: "Review code",
  arguments: [
    { name: "focus", description: "What to focus on", required: true },
    { name: "tone", description: null, required: false },
  ],
  tags: [],
};

function setup(args: Record<string, string>, appendedText = "", busy = false) {
  const onremove = vi.fn();
  render(PromptComposer, {
    props: { prompt: PROMPT, args, appendedText, onremove, busy },
  });
  return { onremove };
}

beforeEach(() => {
  invokeMock.mockReset();
});

describe("PromptComposer", () => {
  it("renders an input per argument with required/optional markers and descriptions", () => {
    setup({ focus: "", tone: "" });
    expect(screen.getByTestId("prompt-arg-focus")).toBeInTheDocument();
    expect(screen.getByTestId("prompt-arg-required-focus")).toHaveTextContent("required");
    // The optional arg has no required marker.
    expect(screen.queryByTestId("prompt-arg-required-tone")).toBeNull();
    expect(screen.getByText("What to focus on")).toBeInTheDocument();
  });

  it("renders the appended-text field and the chosen prompt", () => {
    setup({ focus: "", tone: "" });
    expect(screen.getByTestId("prompt-appended")).toBeInTheDocument();
    expect(screen.getByTestId("prompt-selector")).toHaveTextContent("Code Review");
  });

  it("keeps prompt fields in a capped scroll region", () => {
    setup({ focus: "", tone: "" });
    expect(screen.getByTestId("prompt-composer")).toHaveClass(
      "max-h-[min(56dvh,34rem)]",
      "overflow-hidden",
    );
    expect(screen.getByTestId("prompt-fields-scroll")).toHaveClass(
      "min-h-0",
      "overflow-y-auto",
      "pl-1",
      "pr-3",
      "[scrollbar-gutter:stable]",
    );
  });

  it("autosizes argument and appended textareas up to their max height", async () => {
    const scrollHeight = vi.spyOn(HTMLTextAreaElement.prototype, "scrollHeight", "get");
    const getComputedStyleSpy = vi.spyOn(window, "getComputedStyle");
    try {
      scrollHeight.mockImplementation(function (this: HTMLTextAreaElement): number {
        return this.value.includes("\n") ? 220 : 60;
      });
      getComputedStyleSpy.mockReturnValue({ maxHeight: "160px" } as CSSStyleDeclaration);

      setup({ focus: "", tone: "" });
      const focus = screen.getByTestId("prompt-arg-focus") as HTMLTextAreaElement;
      const appended = screen.getByTestId("prompt-appended") as HTMLTextAreaElement;

      await fireEvent.input(focus, { target: { value: "one\ntwo\nthree\nfour" } });
      expect(focus.style.height).toBe("160px");
      expect(focus.style.overflowY).toBe("auto");

      await fireEvent.input(appended, { target: { value: "short" } });
      expect(appended.style.height).toBe("60px");
      expect(appended.style.overflowY).toBe("hidden");
    } finally {
      scrollHeight.mockRestore();
      getComputedStyleSpy.mockRestore();
    }
  });

  it("focuses the first prompt field when requested", async () => {
    render(PromptComposer, {
      props: {
        prompt: PROMPT,
        args: { focus: "", tone: "" },
        appendedText: "",
        onremove: vi.fn(),
        focusFirstField: true,
      },
    });

    await waitFor(() => expect(screen.getByTestId("prompt-arg-focus")).toHaveFocus());
  });

  it("previews the combined message (rendered prompt + appended text) as markdown", async () => {
    invokeMock.mockResolvedValue({ text: "# RENDERED BODY" });
    setup({ focus: "tests", tone: "" }, "extra note");

    await fireEvent.click(screen.getByTestId("prompt-preview-button"));

    // Rendered as markdown in a dialog overlay (the heading becomes an <h1>),
    // not inline in the compose box.
    const previewEl = await screen.findByTestId("prompt-preview");
    expect(previewEl).toHaveTextContent("RENDERED BODY");
    expect(previewEl).toHaveTextContent("extra note");
    expect(previewEl.querySelector("h1")).not.toBeNull();
    expect(screen.getByTestId("dialog-content")).toBeInTheDocument();
    const call = invokeMock.mock.calls.find(([c]) => c === "render_prompt");
    // Blank optional `tone` is omitted, not sent as "".
    expect(call?.[1]).toMatchObject({
      provider: "local",
      name: "review",
      args: { focus: "tests" },
    });
    expect((call?.[1] as { args: Record<string, string> }).args).not.toHaveProperty("tone");
  });

  it("shows a spinner while preview rendering is pending", async () => {
    invokeMock.mockReturnValue(new Promise(() => undefined));
    setup({ focus: "tests", tone: "" });

    await fireEvent.click(screen.getByTestId("prompt-preview-button"));

    const loading = await screen.findByTestId("prompt-preview-loading");
    expect(loading).toHaveTextContent("Rendering preview");
    expect(loading.querySelector(".animate-spin")).not.toBeNull();
  });

  it("disables Preview until required arguments are filled", async () => {
    setup({ focus: "", tone: "" });
    expect((screen.getByTestId("prompt-preview-button") as HTMLButtonElement).disabled).toBe(true);
  });

  it("locks prompt controls and shows status while busy", async () => {
    const { onremove } = setup({ focus: "tests", tone: "" }, "tail", true);

    expect(screen.getByTestId("prompt-composer")).toHaveAttribute("aria-busy", "true");
    expect((screen.getByTestId("prompt-arg-focus") as HTMLTextAreaElement).disabled).toBe(true);
    expect((screen.getByTestId("prompt-arg-tone") as HTMLTextAreaElement).disabled).toBe(true);
    expect((screen.getByTestId("prompt-appended") as HTMLTextAreaElement).disabled).toBe(true);
    expect((screen.getByTestId("prompt-preview-button") as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByTestId("prompt-remove") as HTMLButtonElement).disabled).toBe(true);

    const status = screen.getByTestId("prompt-rendering");
    expect(status).toHaveTextContent("Rendering prompt");
    expect(status).toHaveClass("absolute", "inset-0", "backdrop-blur-[1px]");
    expect(screen.getByTestId("prompt-composer-content")).toHaveClass("opacity-55", "blur-[1px]");
    expect(status.querySelector(".animate-spin")).not.toBeNull();
    await fireEvent.click(screen.getByTestId("prompt-remove"));
    expect(onremove).not.toHaveBeenCalled();
  });

  it("surfaces a preview render failure inline", async () => {
    invokeMock.mockRejectedValue(new Error("server is down"));
    setup({ focus: "tests", tone: "" });

    await fireEvent.click(screen.getByTestId("prompt-preview-button"));

    await waitFor(() =>
      expect(screen.getByTestId("prompt-preview-error")).toHaveTextContent("server is down"),
    );
  });

  it("removes the prompt via the Remove control", async () => {
    const { onremove } = setup({ focus: "", tone: "" });
    await fireEvent.click(screen.getByTestId("prompt-remove"));
    expect(onremove).toHaveBeenCalledTimes(1);
  });
});

const BOB: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000bbb",
  project_id: "00000000-0000-7000-8000-0000000000ff",
  name: "bob",
  harness: "codex",
  session_locator: null,
  created_at: "2026-05-16T00:00:00Z",
};
const CAROL: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000ccc",
  project_id: "00000000-0000-7000-8000-0000000000ff",
  name: "carol",
  harness: "claude_code",
  session_locator: null,
  created_at: "2026-05-16T00:00:01Z",
};

function setupForward(
  args: Record<string, string>,
  opts: {
    argSources?: Record<string, ForwardSource[]>;
    panes?: TranscriptPane[];
    agentReadiness?: (id: string) => "ready" | "pending" | "empty";
  } = {},
): void {
  render(ForwardHarness, {
    props: {
      prompt: PROMPT,
      initialArgs: args,
      initialArgSources: opts.argSources ?? {},
      agents: [BOB, CAROL],
      panes: opts.panes ?? [],
      agentReadiness: opts.agentReadiness,
    },
  });
}

describe("PromptComposer per-argument forwarding", () => {
  it("renders a forward picker per argument once agents exist", () => {
    setupForward({ focus: "", tone: "" });
    expect(screen.getByTestId("prompt-arg-forward-focus")).toBeInTheDocument();
    expect(screen.getByTestId("prompt-arg-forward-tone")).toBeInTheDocument();
  });

  it("omits the forward picker when there are no agents to forward from", () => {
    setup({ focus: "", tone: "" });
    expect(screen.queryByTestId("prompt-arg-forward-focus")).toBeNull();
  });

  it("adds a source chip under the argument when an agent is picked", async () => {
    setupForward({ focus: "", tone: "" });
    await fireEvent.click(screen.getByTestId("prompt-arg-forward-focus"));
    await fireEvent.click(await screen.findByTestId(`forward-picker-agent-${BOB.id}`));

    const sources = await screen.findByTestId("prompt-arg-sources-focus");
    expect(within(sources).getByTestId("forward-source-chip-bob")).toBeInTheDocument();
    // The picked source belongs to `focus`, not `tone`.
    expect(screen.queryByTestId("prompt-arg-sources-tone")).toBeNull();
  });

  it("removes a source chip via its remove control", async () => {
    setupForward({ focus: "", tone: "" }, { argSources: { focus: [{ id: BOB.id, name: "bob" }] } });
    expect(screen.getByTestId("forward-source-chip-bob")).toBeInTheDocument();

    await fireEvent.click(screen.getByTestId("forward-source-remove-bob"));
    await waitFor(() => expect(screen.queryByTestId("forward-source-chip-bob")).toBeNull());
  });

  it("warns on a chip whose source will be skipped at dispatch", () => {
    setupForward(
      { focus: "", tone: "" },
      {
        argSources: { focus: [{ id: BOB.id, name: "bob" }] },
        agentReadiness: () => "empty",
      },
    );
    const chip = screen.getByTestId("forward-source-chip-bob");
    expect(chip).toHaveAttribute("data-readiness", "empty");
    expect(chip).toHaveTextContent("will be skipped");
  });

  it("does not warn on a chip whose source is still generating", () => {
    // The send holds for an in-flight turn and forwards it, so a `pending` source
    // is about to contribute normally — flagging it would say the opposite.
    setupForward(
      { focus: "", tone: "" },
      {
        argSources: { focus: [{ id: BOB.id, name: "bob" }] },
        agentReadiness: () => "pending",
      },
    );
    const chip = screen.getByTestId("forward-source-chip-bob");
    expect(chip).toHaveAttribute("data-readiness", "pending");
    expect(chip).toHaveTextContent("still generating");
    expect(chip).not.toHaveTextContent("will be skipped");
    expect(chip.className).not.toContain("status-failed");
  });

  it("treats a required argument as satisfied once it has a forward source", () => {
    // `focus` is required and typed-empty, but a source fills it → Preview enabled
    // and no missing-required highlight.
    setupForward({ focus: "", tone: "" }, { argSources: { focus: [{ id: BOB.id, name: "bob" }] } });
    expect((screen.getByTestId("prompt-preview-button") as HTMLButtonElement).disabled).toBe(false);
    expect(screen.getByTestId("prompt-arg-focus")).not.toHaveClass("border-status-failed");
  });

  it("previews a forwarded argument with a placeholder for the live source", async () => {
    invokeMock.mockResolvedValue({ text: "RENDERED" });
    setupForward(
      { focus: "lead text", tone: "" },
      { argSources: { focus: [{ id: BOB.id, name: "bob" }] } },
    );

    await fireEvent.click(screen.getByTestId("prompt-preview-button"));
    await screen.findByTestId("prompt-preview");

    const call = invokeMock.mock.calls.find(([c]) => c === "render_prompt");
    const sentArgs = (call?.[1] as { args: Record<string, string> }).args;
    // The forwarded arg shows typed lead + a placeholder (real output is resolved
    // server-side at send time, not in the local preview).
    expect(sentArgs.focus).toContain("lead text");
    expect(sentArgs.focus).toContain("«forwarding from bob…»");
  });

  it("expands a pane pick to one chip per member agent (no pane chip)", async () => {
    const pane: TranscriptPane = {
      id: "pane-1",
      name: "Reviewers",
      members: [BOB.id, CAROL.id],
      hidden: [],
    };
    const other: TranscriptPane = { id: "pane-2", name: "Pane 2", members: [CAROL.id], hidden: [] };
    // Two non-empty panes → the picker offers pane rows.
    setupForward({ focus: "", tone: "" }, { panes: [pane, other] });

    await fireEvent.click(screen.getByTestId("prompt-arg-forward-focus"));
    await fireEvent.click(await screen.findByTestId("forward-picker-pane-pane-1"));

    const sources = await screen.findByTestId("prompt-arg-sources-focus");
    // Agents are the first-class unit: the pane resolves to one chip per member,
    // never a pane chip.
    expect(within(sources).getByTestId("forward-source-chip-bob")).toBeInTheDocument();
    expect(within(sources).getByTestId("forward-source-chip-carol")).toBeInTheDocument();
    expect(within(sources).queryByTestId("forward-source-chip-Reviewers")).toBeNull();
  });

  it("dedups a pane pick against a member already attached to the field", async () => {
    const pane: TranscriptPane = {
      id: "pane-1",
      name: "Reviewers",
      members: [BOB.id, CAROL.id],
      hidden: [],
    };
    const other: TranscriptPane = { id: "pane-2", name: "Pane 2", members: [CAROL.id], hidden: [] };
    // `focus` already carries bob; picking the pane must add only carol.
    setupForward(
      { focus: "", tone: "" },
      { argSources: { focus: [{ id: BOB.id, name: "bob" }] }, panes: [pane, other] },
    );

    await fireEvent.click(screen.getByTestId("prompt-arg-forward-focus"));
    await fireEvent.click(await screen.findByTestId("forward-picker-pane-pane-1"));

    const sources = await screen.findByTestId("prompt-arg-sources-focus");
    await waitFor(() =>
      expect(within(sources).getByTestId("forward-source-chip-carol")).toBeInTheDocument(),
    );
    expect(within(sources).getAllByTestId("forward-source-chip-bob")).toHaveLength(1);
  });

  it("clears all of a field's sources with one click", async () => {
    const pane: TranscriptPane = {
      id: "pane-1",
      name: "Reviewers",
      members: [BOB.id, CAROL.id],
      hidden: [],
    };
    const other: TranscriptPane = { id: "pane-2", name: "Pane 2", members: [CAROL.id], hidden: [] };
    setupForward({ focus: "", tone: "" }, { panes: [pane, other] });

    await fireEvent.click(screen.getByTestId("prompt-arg-forward-focus"));
    await fireEvent.click(await screen.findByTestId("forward-picker-pane-pane-1"));
    await screen.findByTestId("forward-source-chip-bob");

    await fireEvent.click(screen.getByTestId("prompt-arg-sources-focus-clear"));
    await waitFor(() => expect(screen.queryByTestId("prompt-arg-sources-focus")).toBeNull());
  });
});
