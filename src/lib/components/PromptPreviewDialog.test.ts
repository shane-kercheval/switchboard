import { beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, waitFor } from "@testing-library/svelte";
import PromptPreviewDialog from "./PromptPreviewDialog.svelte";
import type { Prompt } from "$lib/types";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

// Route each command to a per-test responder so a case can control source +
// metadata independently (and simulate the null-source / error paths).
function wire(handlers: {
  source?: (args?: Record<string, unknown>) => unknown;
  list?: () => Prompt[];
}): void {
  invokeMock.mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "get_prompt_source") return Promise.resolve(handlers.source?.(args) ?? null);
    if (cmd === "list_prompts") return Promise.resolve(handlers.list?.() ?? []);
    return Promise.reject(new Error(`unexpected command ${cmd}`));
  });
}

const LOCAL_META: Prompt = {
  provider: "local",
  name: "code-review",
  title: null,
  description: "Review the diff.",
  arguments: [{ name: "focus", description: "What to focus on", required: false }],
  tags: [],
};

const MCP_META: Prompt = {
  provider: "tiddly",
  name: "analyze",
  title: "Analyze",
  description: "Analyze reviews.",
  arguments: [{ name: "review", description: "The review text", required: true }],
  tags: [],
};

beforeEach(() => {
  invokeMock.mockReset();
});

describe("PromptPreviewDialog", () => {
  it("shows the raw, unrendered template body for a builtin/local prompt", async () => {
    wire({
      source: () => ({ text: "Review the changes.\n{% if focus %}Focus: {{ focus }}{% endif %}" }),
      list: () => [LOCAL_META],
    });
    render(PromptPreviewDialog, {
      props: { open: true, prompt: { kind: "named", id: "local:code-review" } },
    });

    const body = await screen.findByTestId("prompt-preview-body");
    // Verbatim template — placeholders NOT substituted.
    expect(body).toHaveTextContent("{% if focus %}");
    expect(body).toHaveTextContent("{{ focus }}");
    // The metadata description renders as a header.
    expect(screen.getByTestId("prompt-preview-description")).toHaveTextContent("Review the diff.");
    // No "server-rendered" fallback note when we have the source.
    expect(screen.queryByTestId("prompt-preview-no-source")).toBeNull();
  });

  it("falls back to metadata + arguments for an MCP prompt (no un-rendered source)", async () => {
    wire({ source: () => null, list: () => [MCP_META] });
    render(PromptPreviewDialog, {
      props: { open: true, prompt: { kind: "named", id: "tiddly:analyze" } },
    });

    await waitFor(() => expect(screen.getByTestId("prompt-preview-no-source")).toBeInTheDocument());
    expect(screen.queryByTestId("prompt-preview-body")).toBeNull();
    const args = screen.getByTestId("prompt-preview-arguments");
    expect(args).toHaveTextContent("review");
    expect(args).toHaveTextContent("required");
  });

  it("tells the user a local prompt is unresolved (not 'server-rendered') when its source is gone", async () => {
    // A local prompt still in the cache but whose file was deleted/corrupted after
    // the cache was built: source is null, but this is NOT an MCP prompt, so the
    // "rendered by its server" message would be a false explanation.
    wire({ source: () => null, list: () => [LOCAL_META] });
    render(PromptPreviewDialog, {
      props: { open: true, prompt: { kind: "named", id: "local:code-review" } },
    });

    await waitFor(() =>
      expect(screen.getByTestId("prompt-preview-unresolved")).toBeInTheDocument(),
    );
    expect(screen.queryByTestId("prompt-preview-no-source")).toBeNull();
  });

  it("shows a generic 'unavailable' when source is null and no metadata is cached", async () => {
    // Non-local provider, null source, and no cached metadata: the server-rendered
    // explanation isn't warranted (we can't confirm it's a live MCP prompt).
    wire({ source: () => null, list: () => [] });
    render(PromptPreviewDialog, {
      props: { open: true, prompt: { kind: "named", id: "tiddly:gone" } },
    });

    await waitFor(() =>
      expect(screen.getByTestId("prompt-preview-unavailable")).toBeInTheDocument(),
    );
    expect(screen.queryByTestId("prompt-preview-no-source")).toBeNull();
    expect(screen.queryByTestId("prompt-preview-unresolved")).toBeNull();
  });

  it("still renders the body when the metadata fetch fails (body is primary)", async () => {
    // listPrompts failing must not discard a successfully fetched template body.
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "get_prompt_source") return Promise.resolve({ text: "Body {{ x }}" });
      if (cmd === "list_prompts") return Promise.reject(new Error("cache read failed"));
      return Promise.reject(new Error(`unexpected command ${cmd}`));
    });
    render(PromptPreviewDialog, {
      props: { open: true, prompt: { kind: "named", id: "local:code-review" } },
    });

    const body = await screen.findByTestId("prompt-preview-body");
    expect(body).toHaveTextContent("Body {{ x }}");
    // No error surface, and the (failed) metadata just omits its sections.
    expect(screen.queryByTestId("prompt-preview-error")).toBeNull();
    expect(screen.queryByTestId("prompt-preview-description")).toBeNull();
  });

  it("surfaces an error when the source fetch rejects", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "get_prompt_source") return Promise.reject(new Error("boom"));
      return Promise.resolve([]);
    });
    render(PromptPreviewDialog, {
      props: { open: true, prompt: { kind: "named", id: "local:x" } },
    });

    const err = await screen.findByTestId("prompt-preview-error");
    expect(err).toHaveTextContent("boom");
  });

  it("shows inline text directly, without any backend call", async () => {
    // Inline text travels on the step, so there is nothing to fetch.
    render(PromptPreviewDialog, {
      props: { open: true, prompt: { kind: "inline", text: "Summarize {{ responses }}" } },
    });

    const body = await screen.findByTestId("prompt-preview-body");
    expect(body).toHaveTextContent("Summarize {{ responses }}");
    expect(invokeMock).not.toHaveBeenCalled();
    // No metadata/arguments or server-rendered note for inline text.
    expect(screen.queryByTestId("prompt-preview-no-source")).toBeNull();
    expect(screen.queryByTestId("prompt-preview-arguments")).toBeNull();
  });

  it("does not fetch while closed", async () => {
    wire({ source: () => ({ text: "body" }), list: () => [LOCAL_META] });
    render(PromptPreviewDialog, {
      props: { open: false, prompt: { kind: "named", id: "local:code-review" } },
    });
    // A closed dialog must not hit the backend (the workflow step list mounts one
    // per view; it stays inert until the user opens a chip).
    await Promise.resolve();
    expect(invokeMock).not.toHaveBeenCalled();
  });
});
