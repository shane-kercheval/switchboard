import { describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render } from "@testing-library/svelte";
import type { ToolCall, Turn } from "$lib/state/types";
import type { ToolFacet } from "$lib/types";
import { copyTextOf } from "$lib/state/unified";
import AgentMessageBody from "./AgentMessageBody.svelte";

vi.mock("$lib/native", () => ({
  copyText: async (): Promise<void> => undefined,
}));

type AgentTurn = Extract<Turn, { role: "agent" }>;

const REVIEW: ToolFacet = {
  facet_kind: "findings",
  level: "low",
  findings: [
    {
      file: "src/app.py",
      line: 12,
      summary: "Off-by-one.",
      failure_scenario: "Drops the last item.",
    },
  ],
  text: "REVIEW",
};

const started: ToolCall = {
  item_kind: "tool",
  tool_use_id: "r1",
  kind: "builtin",
  name: "ReportFindings",
  input: {},
  facet: REVIEW,
  started_at: "2026-05-16T00:00:01Z",
};
const completed = { completed_at: "2026-05-16T00:00:02Z", output: "1 finding reported." };

function turnWith(tool: ToolCall, status: AgentTurn["status"] = "complete"): AgentTurn {
  return {
    role: "agent",
    turn_id: "00000000-0000-7000-8000-000000000001",
    agent_id: "00000000-0000-7000-8000-000000000aaa",
    started_at: "2026-05-16T00:00:00Z",
    status,
    items: [tool, { item_kind: "text", kind: "text", text: "ack" }],
  };
}

function rendered(turn: AgentTurn, settled: boolean): "card" | "row" {
  const { queryByTestId } = render(AgentMessageBody, { turn, settled });
  const card = queryByTestId("findings-card") !== null;
  const row = queryByTestId("turn-tool") !== null;
  expect(card !== row).toBe(true);
  return card ? "card" : "row";
}

describe("AgentMessageBody with a code review call", () => {
  it("renders a confirmed review as the card, and copy includes it", () => {
    const turn = turnWith({ ...started, ...completed, is_error: false });
    expect(rendered(turn, true)).toBe("card");
    expect(copyTextOf(turn, "full_answer")).toBe("REVIEW\n\nack");
  });

  it("renders a review still waiting on its result in a live turn as the card, not yet copied", () => {
    const turn = turnWith(started, "streaming");
    expect(rendered(turn, false)).toBe("card");
    expect(copyTextOf(turn, "full_answer")).toBe("ack");
  });

  it("renders a rejected review as the generic row, excluded from copy", () => {
    const turn = turnWith({ ...started, ...completed, is_error: true });
    expect(rendered(turn, true)).toBe("row");
    expect(copyTextOf(turn, "full_answer")).toBe("ack");
  });

  it.each(["cancelled", "failed"] as const)(
    "renders a review stopped by a %s turn as the generic row, excluded from copy",
    (reason) => {
      const turn = turnWith(
        { ...started, stopped_at: "2026-05-16T00:00:02Z", stop_reason: reason },
        reason,
      );
      expect(rendered(turn, true)).toBe("row");
      expect(copyTextOf(turn, "full_answer")).toBe("ack");
    },
  );

  it("renders a reopened completed turn's unanswered review as the generic row", () => {
    const turn = turnWith(started, "complete");
    expect(rendered(turn, true)).toBe("row");
    expect(copyTextOf(turn, "full_answer")).toBe("ack");
  });

  it("renders a facet this build doesn't know as the generic row", () => {
    const unknown = { facet_kind: "future_kind" } as unknown as ToolFacet;
    const turn = turnWith({ ...started, ...completed, is_error: false, facet: unknown });
    expect(rendered(turn, true)).toBe("row");
  });
});
