import { afterEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import PinsSidebar from "./PinsSidebar.svelte";
import type { AgentRecord } from "$lib/types";

const PROJECT = "00000000-0000-7000-8000-0000000000ff";
const AGENT: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000aaa",
  project_id: PROJECT,
  name: "alice",
  harness: "claude_code",
  session_locator: { uuid: "00000000-0000-7000-8000-000000000001" },
  created_at: "2026-08-07T12:00:00Z",
};
const PIN_KEY = `agent:hydration:${AGENT.id}:message-1`;
let persistedPins = [{ key: PIN_KEY, pinned_at: "2026-08-07T12:01:00Z" }];

const invokeMock = vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
  if (cmd === "list_message_pins") return persistedPins;
  if (cmd === "set_message_pin") {
    if (args?.pinned === true && !persistedPins.some((pin) => pin.key === args.key)) {
      persistedPins = [
        ...persistedPins,
        { key: args.key as string, pinned_at: "2026-08-07T12:01:00Z" },
      ];
    } else if (args?.pinned === false) {
      persistedPins = persistedPins.filter((pin) => pin.key !== args.key);
    }
    return persistedPins;
  }
  if (cmd === "remove_message_pins") {
    const keys = new Set(args?.keys as string[]);
    persistedPins = persistedPins.filter((pin) => !keys.has(pin.key));
    return persistedPins;
  }
  if (cmd === "migrate_message_pin") {
    persistedPins = persistedPins.map((pin) =>
      pin.key === args?.fromKey ? { ...pin, key: args.toKey as string } : pin,
    );
    return persistedPins;
  }
  return null;
});
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => vi.fn()),
}));

const transcript = await import("$lib/state/index.svelte");
const jump = await import("$lib/state/transcriptJump.svelte");
const panes = await import("$lib/state/transcriptPanes.svelte");
const pins = await import("$lib/state/messagePins.svelte");
const layoutStore = await import("$lib/layout.svelte");

afterEach(() => {
  cleanup();
  transcript._testing.reset();
  jump._testing.reset();
  panes._testing.reset();
  pins._testing.reset();
  layoutStore._testing.reset();
  persistedPins = [{ key: PIN_KEY, pinned_at: "2026-08-07T12:01:00Z" }];
  invokeMock.mockClear();
});

describe("PinsSidebar", () => {
  it("renders the full message, collapses independently, navigates, and unpins", async () => {
    await transcript.registerAgent(AGENT);
    transcript.transcripts[AGENT.id] = [
      {
        role: "agent",
        turn_id: "turn-1",
        agent_id: AGENT.id,
        send_id: "send-1",
        started_at: "2026-08-07T12:00:00Z",
        status: "complete",
        hydration_key: "message-1",
        items: [
          { item_kind: "text", kind: "text", text: "first paragraph" },
          { item_kind: "text", kind: "text", text: "important answer" },
        ],
      },
    ];
    const view = render(PinsSidebar, { props: { projectId: PROJECT, agents: [AGENT] } });

    const item = await screen.findByTestId("pinned-message");
    expect(item).toHaveTextContent("alice");
    expect(screen.getByTestId("pinned-message-body")).toHaveTextContent("first paragraph");
    expect(screen.getByTestId("pinned-message-body")).toHaveTextContent("important answer");
    expect(screen.getByTestId("pinned-message-body")).toHaveClass("bg-raised");
    expect(screen.getByTestId("pinned-message-card")).toHaveClass("bg-raised");
    expect(screen.getByTestId("pinned-message-header")).toHaveClass("bg-surface");
    expect(screen.getByTestId("pinned-message-header")).not.toHaveClass("bg-panel");
    expect(screen.queryByTestId("pins-sort")).not.toBeInTheDocument();

    await fireEvent.click(screen.getByTestId("pinned-message"));
    expect(screen.queryByTestId("pinned-message-body")).not.toBeInTheDocument();
    expect(screen.getByTestId("pinned-message-preview")).toHaveTextContent("first paragraph");

    view.unmount();
    render(PinsSidebar, { props: { projectId: PROJECT, agents: [AGENT] } });
    expect(screen.queryByTestId("pinned-message-body")).not.toBeInTheDocument();
    await fireEvent.click(screen.getByTestId("pinned-message-toggle"));
    expect(screen.getByTestId("pinned-message-body")).toHaveTextContent("important answer");

    await fireEvent.click(screen.getByTestId("pinned-message-locate"));
    expect(jump.jumpRequest.rowKey).toBe("a:turn-1");

    await fireEvent.click(screen.getByTestId("pinned-message-unpin"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("remove_message_pins", {
        projectId: PROJECT,
        keys: [PIN_KEY],
      }),
    );
    expect(screen.getByTestId("pins-empty")).toBeInTheDocument();
    expect(screen.getByTestId("pins-empty-icon")).toBeInTheDocument();
    expect(screen.getByTestId("pins-empty")).toHaveTextContent("Keep important messages close");
    expect(screen.getByTestId("pins-empty")).toHaveTextContent(
      "Pinned messages appear here in full",
    );
  });

  it("shows one control for multiple cards and collapses or expands them together", async () => {
    persistedPins = [
      { key: PIN_KEY, pinned_at: "2026-08-07T12:01:00Z" },
      {
        key: `agent:hydration:${AGENT.id}:message-2`,
        pinned_at: "2026-08-07T12:02:00Z",
      },
    ];
    await transcript.registerAgent(AGENT);
    transcript.transcripts[AGENT.id] = [
      {
        role: "agent",
        turn_id: "turn-1",
        agent_id: AGENT.id,
        started_at: "2026-08-07T12:00:00Z",
        status: "complete",
        hydration_key: "message-1",
        items: [{ item_kind: "text", kind: "text", text: "first answer" }],
      },
      {
        role: "agent",
        turn_id: "turn-2",
        agent_id: AGENT.id,
        started_at: "2026-08-07T12:00:00.500Z",
        status: "complete",
        hydration_key: "message-2",
        items: [{ item_kind: "text", kind: "text", text: "second answer" }],
      },
    ];

    render(PinsSidebar, { props: { projectId: PROJECT, agents: [AGENT] } });

    const toggleAll = await screen.findByTestId("pins-toggle-all");
    expect(toggleAll).toHaveAccessibleName("Collapse all pinned messages");
    expect(screen.getAllByTestId("pinned-message-body")).toHaveLength(2);

    await fireEvent.click(toggleAll);
    expect(screen.queryAllByTestId("pinned-message-body")).toHaveLength(0);
    expect(screen.getAllByTestId("pinned-message-preview")).toHaveLength(2);
    expect(toggleAll).toHaveAccessibleName("Expand all pinned messages");

    await fireEvent.click(toggleAll);
    expect(screen.getAllByTestId("pinned-message-body")).toHaveLength(2);
  });

  it("sorts by pin time or message time and places unavailable messages last", async () => {
    const secondKey = `agent:hydration:${AGENT.id}:message-2`;
    const unavailableKey = `agent:hydration:${AGENT.id}:missing`;
    persistedPins = [
      { key: unavailableKey, pinned_at: "2026-08-07T12:04:00Z" },
      { key: PIN_KEY, pinned_at: "2026-08-07T12:03:00Z" },
      { key: secondKey, pinned_at: "2026-08-07T12:02:00Z" },
    ];
    await transcript.registerAgent(AGENT);
    transcript.transcripts[AGENT.id] = [
      {
        role: "agent",
        turn_id: "turn-1",
        agent_id: AGENT.id,
        started_at: "2026-08-07T12:00:00Z",
        status: "complete",
        hydration_key: "message-1",
        items: [{ item_kind: "text", kind: "text", text: "older message" }],
      },
      {
        role: "agent",
        turn_id: "turn-2",
        agent_id: AGENT.id,
        started_at: "2026-08-07T12:00:00.500Z",
        status: "complete",
        hydration_key: "message-2",
        items: [{ item_kind: "text", kind: "text", text: "newer message" }],
      },
    ];

    render(PinsSidebar, { props: { projectId: PROJECT, agents: [AGENT] } });

    await screen.findByTestId("pins-sort");
    const cardKeys = (): (string | null)[] =>
      screen
        .getAllByTestId("pinned-message-card")
        .map((card) => card.getAttribute("data-message-key"));
    expect(screen.getByTestId("pins-sort-pinned")).toHaveAttribute("aria-checked", "true");
    expect(cardKeys()).toEqual([unavailableKey, PIN_KEY, secondKey]);

    await fireEvent.click(screen.getByTestId("pins-sort-message"));

    expect(screen.getByTestId("pins-sort-message")).toHaveAttribute("aria-checked", "true");
    expect(cardKeys()).toEqual([secondKey, PIN_KEY, unavailableKey]);
  });

  it("uses pin time and key to break ties between equivalent message instants", async () => {
    const secondKey = `agent:hydration:${AGENT.id}:message-2`;
    const thirdKey = `agent:hydration:${AGENT.id}:message-3`;
    persistedPins = [
      { key: thirdKey, pinned_at: "2026-08-07T12:01:00.500Z" },
      { key: secondKey, pinned_at: "2026-08-07T12:01:00Z" },
      { key: PIN_KEY, pinned_at: "2026-08-07T12:01:00.500Z" },
    ];
    await transcript.registerAgent(AGENT);
    transcript.transcripts[AGENT.id] = [
      {
        role: "agent",
        turn_id: "turn-1",
        agent_id: AGENT.id,
        started_at: "2026-08-07T12:00:00Z",
        status: "complete",
        hydration_key: "message-1",
        items: [{ item_kind: "text", kind: "text", text: "first" }],
      },
      {
        role: "agent",
        turn_id: "turn-2",
        agent_id: AGENT.id,
        started_at: "2026-08-07T12:00:00.000Z",
        status: "complete",
        hydration_key: "message-2",
        items: [{ item_kind: "text", kind: "text", text: "second" }],
      },
      {
        role: "agent",
        turn_id: "turn-3",
        agent_id: AGENT.id,
        started_at: "2026-08-07T13:00:00+01:00",
        status: "complete",
        hydration_key: "message-3",
        items: [{ item_kind: "text", kind: "text", text: "third" }],
      },
    ];

    render(PinsSidebar, { props: { projectId: PROJECT, agents: [AGENT] } });
    await fireEvent.click(await screen.findByTestId("pins-sort-message"));

    expect(
      screen
        .getAllByTestId("pinned-message-card")
        .map((card) => card.getAttribute("data-message-key")),
    ).toEqual([PIN_KEY, thirdKey, secondKey]);
  });

  it("hides message-time sorting when every pinned message is unavailable", async () => {
    persistedPins = [
      { key: `agent:hydration:${AGENT.id}:missing-1`, pinned_at: "2026-08-07T12:01:00Z" },
      { key: `agent:hydration:${AGENT.id}:missing-2`, pinned_at: "2026-08-07T12:02:00Z" },
    ];

    render(PinsSidebar, { props: { projectId: PROJECT, agents: [AGENT] } });

    await screen.findAllByTestId("pinned-missing");
    expect(screen.queryByTestId("pins-sort")).not.toBeInTheDocument();
  });

  it("keeps a valid message ahead of an invalid timestamp in newest-message mode", async () => {
    const invalidKey = `agent:hydration:${AGENT.id}:message-invalid`;
    persistedPins = [
      { key: invalidKey, pinned_at: "2026-08-07T12:02:00Z" },
      { key: PIN_KEY, pinned_at: "2026-08-07T12:01:00Z" },
    ];
    await transcript.registerAgent(AGENT);
    transcript.transcripts[AGENT.id] = [
      {
        role: "agent",
        turn_id: "turn-valid",
        agent_id: AGENT.id,
        started_at: "2026-08-07T12:00:00Z",
        status: "complete",
        hydration_key: "message-1",
        items: [{ item_kind: "text", kind: "text", text: "valid" }],
      },
      {
        role: "agent",
        turn_id: "turn-invalid",
        agent_id: AGENT.id,
        started_at: "invalid",
        status: "complete",
        hydration_key: "message-invalid",
        items: [{ item_kind: "text", kind: "text", text: "invalid" }],
      },
    ];

    render(PinsSidebar, { props: { projectId: PROJECT, agents: [AGENT] } });
    await fireEvent.click(await screen.findByTestId("pins-sort-message"));

    expect(
      screen
        .getAllByTestId("pinned-message-card")
        .map((card) => card.getAttribute("data-message-key")),
    ).toEqual([PIN_KEY, invalidKey]);
  });

  it("keeps a live pin's full response together after Claude splits it at compaction", async () => {
    await transcript.registerAgent(AGENT);
    transcript.transcripts[AGENT.id] = [
      {
        role: "agent",
        turn_id: "turn-before-compaction",
        agent_id: AGENT.id,
        started_at: "2026-08-07T12:00:01Z",
        status: "complete",
        hydration_key: "message-1",
        items: [{ item_kind: "text", kind: "text", text: "before compaction" }],
      },
      {
        role: "agent",
        turn_id: "turn-after-compaction",
        agent_id: AGENT.id,
        started_at: "2026-08-07T12:00:02Z",
        status: "complete",
        hydration_key: "message-2",
        continuation_of: "message-1",
        items: [{ item_kind: "text", kind: "text", text: "after compaction" }],
      },
    ];

    render(PinsSidebar, { props: { projectId: PROJECT, agents: [AGENT] } });

    const body = await screen.findByTestId("pinned-message-body");
    expect(screen.getAllByTestId("pinned-message-card")).toHaveLength(1);
    expect(body).toHaveTextContent("before compaction");
    expect(body).toHaveTextContent("after compaction");
  });

  it("preserves a temporary live pin through durable migration and a compacted reload", async () => {
    persistedPins = [];
    await transcript.registerAgent(AGENT);
    transcript.transcripts[AGENT.id] = [
      {
        role: "agent",
        turn_id: "live-turn",
        agent_id: AGENT.id,
        send_id: "send-1",
        send_correlation: "live",
        started_at: "2026-08-07T12:00:01Z",
        status: "streaming",
        items: [{ item_kind: "text", kind: "text", text: "before compaction" }],
      },
    ];
    render(PinsSidebar, { props: { projectId: PROJECT, agents: [AGENT] } });
    await waitFor(() => expect(pins.pinsLoaded(PROJECT)).toBe(true));
    const alias = `agent:send:send-1:${AGENT.id}`;
    const canonical = `agent:hydration:${AGENT.id}:message-1`;
    const temporary = { kind: "pinnable" as const, key: alias, aliases: [], temporary: true };
    pins.setMessagePinned(PROJECT, temporary, true);
    await waitFor(() => expect(persistedPins.map((pin) => pin.key)).toEqual([alias]));

    pins.reconcileMessagePinIdentities(PROJECT, [
      { kind: "pinnable", key: canonical, aliases: [alias], temporary: false },
    ]);
    await waitFor(() => expect(persistedPins.map((pin) => pin.key)).toEqual([canonical]));

    transcript.transcripts[AGENT.id] = [
      {
        role: "agent",
        turn_id: "disk-before",
        agent_id: AGENT.id,
        send_id: "send-1",
        send_correlation: "durable_link",
        started_at: "2026-08-07T12:00:01Z",
        status: "complete",
        hydration_key: "message-1",
        items: [{ item_kind: "text", kind: "text", text: "before compaction" }],
      },
      {
        role: "agent",
        turn_id: "disk-after",
        agent_id: AGENT.id,
        send_id: "send-1",
        send_correlation: "durable_link",
        started_at: "2026-08-07T12:00:02Z",
        status: "complete",
        hydration_key: "message-2",
        continuation_of: "message-1",
        items: [{ item_kind: "text", kind: "text", text: "after compaction" }],
      },
    ];

    const body = await screen.findByTestId("pinned-message-body");
    expect(screen.getAllByTestId("pinned-message-card")).toHaveLength(1);
    expect(body).toHaveTextContent("before compaction");
    expect(body).toHaveTextContent("after compaction");
  });

  it("stops at ambiguous compaction continuations instead of guessing", async () => {
    await transcript.registerAgent(AGENT);
    transcript.transcripts[AGENT.id] = [
      {
        role: "agent",
        turn_id: "root",
        agent_id: AGENT.id,
        started_at: "2026-08-07T12:00:01Z",
        status: "complete",
        hydration_key: "message-1",
        items: [{ item_kind: "text", kind: "text", text: "unambiguous root" }],
      },
      {
        role: "agent",
        turn_id: "continuation-a",
        agent_id: AGENT.id,
        started_at: "2026-08-07T12:00:02Z",
        status: "complete",
        hydration_key: "message-2",
        continuation_of: "message-1",
        items: [{ item_kind: "text", kind: "text", text: "candidate a" }],
      },
      {
        role: "agent",
        turn_id: "continuation-b",
        agent_id: AGENT.id,
        started_at: "2026-08-07T12:00:03Z",
        status: "complete",
        hydration_key: "message-3",
        continuation_of: "message-1",
        items: [{ item_kind: "text", kind: "text", text: "candidate b" }],
      },
    ];

    render(PinsSidebar, { props: { projectId: PROJECT, agents: [AGENT] } });

    const body = await screen.findByTestId("pinned-message-body");
    expect(body).toHaveTextContent("unambiguous root");
    expect(body).not.toHaveTextContent("candidate a");
    expect(body).not.toHaveTextContent("candidate b");
  });
});
