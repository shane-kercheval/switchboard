import { describe, expect, it } from "vitest";
import { messageIdentityForRow } from "./messageIdentity";
import type { UnifiedRow } from "./state/unified";

const AGENT = "agent-a";

function agentRow(
  overrides: Partial<Extract<UnifiedRow, { kind: "agent" }>["turn"]> = {},
): Extract<UnifiedRow, { kind: "agent" }> {
  return {
    kind: "agent",
    rank: 1,
    key: "a:turn-a",
    at: "2026-08-07T12:00:01Z",
    send_id: "send-a",
    turn: {
      role: "agent",
      turn_id: "turn-a",
      agent_id: AGENT,
      send_id: "send-a",
      send_correlation: "live",
      started_at: "2026-08-07T12:00:01Z",
      status: "complete",
      items: [],
      ...overrides,
    },
  };
}

describe("messageIdentityForRow", () => {
  it("uses hydration identity canonically and retains the live send identity as an alias", () => {
    expect(messageIdentityForRow(agentRow({ hydration_key: "msg/one" }), "claude_code")).toEqual({
      kind: "pinnable",
      key: "agent:hydration:agent-a:msg%2Fone",
      aliases: ["agent:send:send-a:agent-a"],
      temporary: false,
    });
  });

  it("allows a temporary send identity only while a hydrating harness is streaming", () => {
    expect(messageIdentityForRow(agentRow({ status: "streaming" }), "codex")).toEqual({
      kind: "pinnable",
      key: "agent:send:send-a:agent-a",
      aliases: [],
      temporary: true,
    });
    expect(messageIdentityForRow(agentRow(), "codex").kind).toBe("unsupported");
    expect(messageIdentityForRow(agentRow({ status: "streaming" }), "antigravity").kind).toBe(
      "unsupported",
    );
  });

  it("never exposes a positional send alias for display or migration", () => {
    expect(
      messageIdentityForRow(
        agentRow({ hydration_key: "message-a", send_correlation: "positional" }),
        "codex",
      ),
    ).toEqual({
      kind: "pinnable",
      key: "agent:hydration:agent-a:message-a",
      aliases: [],
      temporary: false,
    });
    expect(
      messageIdentityForRow(
        agentRow({ status: "streaming", send_correlation: "positional" }),
        "codex",
      ).kind,
    ).toBe("unsupported");
  });

  it("keys a journal-owned user message by send id", () => {
    const user: Extract<UnifiedRow, { kind: "user" }> = {
      kind: "user",
      rank: 0,
      key: "u:send-a",
      at: "2026-08-07T12:00:00Z",
      send_id: "send-a",
      agent_ids: [AGENT],
      text: "hello",
      attachments: [],
      live: false,
    };
    expect(messageIdentityForRow(user)).toEqual({
      kind: "pinnable",
      key: "user:send:send-a",
      aliases: [],
      temporary: false,
    });
  });

  it("rejects imported prompts and permanently keyless completed replies", () => {
    const imported: Extract<UnifiedRow, { kind: "user" }> = {
      kind: "user",
      rank: 0,
      key: "u:parser-generated",
      at: "2026-08-07T12:00:00Z",
      agent_ids: [AGENT],
      text: "hello",
      attachments: [],
      live: false,
    };
    expect(messageIdentityForRow(imported).kind).toBe("unsupported");
    expect(messageIdentityForRow(agentRow(), "antigravity").kind).toBe("unsupported");
  });
});
