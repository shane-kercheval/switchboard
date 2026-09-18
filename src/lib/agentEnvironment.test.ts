import { describe, expect, it } from "vitest";
import { environmentView } from "./agentEnvironment";
import type { SessionInventory } from "./types";

function view(inventory: SessionInventory) {
  const v = environmentView(inventory);
  if (v === null) throw new Error("expected a renderable environment view");
  return v;
}

/// The first element, or a failure naming the empty list — keeps the
/// assertions below reading as claims rather than as null-guards.
function first<T>(list: readonly T[] | null | undefined, what: string): T {
  const head = list?.[0];
  if (head === undefined) throw new Error(`expected at least one ${what}`);
  return head;
}

describe("environmentView summary", () => {
  it("counts each reported list, in the card's order", () => {
    expect(
      view({
        mcp_servers: [
          { name: "a", status: "connected" },
          { name: "b", status: "connected" },
        ],
        agents: ["Explore", "Plan"],
        plugins: [{ name: "p" }],
        skills: [{ name: "s" }],
        memory_paths: ["/m"],
      }).summary,
    ).toBe("MCP 2 · Agents 2 · Plugins 1 · Skills 1 · Memory 1");
  });

  it("calls out how many servers need auth", () => {
    expect(
      view({
        mcp_servers: [
          { name: "a", status: "connected" },
          { name: "b", status: "needs-auth" },
          { name: "c", status: "needs-auth" },
        ],
      }).summary,
    ).toBe("MCP 3 · 2 need auth");
  });

  it("does not call a status it cannot diagnose an auth problem", () => {
    // The status set is open. A server reporting `disconnected`, or anything
    // a future CLI adds, must not be counted under an instruction that will
    // not fix it — and no status name is invented for it either; the
    // expanded row names the raw value.
    expect(
      view({
        mcp_servers: [
          { name: "a", status: "connected" },
          { name: "b", status: "disconnected" },
          { name: "c", status: "some-future-status" },
        ],
      }).summary,
    ).toBe("MCP 3 · 2 need attention");
  });

  it("reports both kinds of trouble as separate counts", () => {
    expect(
      view({
        mcp_servers: [
          { name: "a", status: "needs-auth" },
          { name: "b", status: "disconnected" },
        ],
      }).summary,
    ).toBe("MCP 2 · 1 need auth · 1 need attention");
  });

  it("omits the needs-attention clause when every server is connected", () => {
    expect(view({ mcp_servers: [{ name: "a", status: "connected" }] }).summary).toBe("MCP 1");
  });

  it("does not count config-file entries as needing attention", () => {
    // `configured` says where we read the server, not that anything is wrong.
    expect(
      view({
        mcp_servers: [
          { name: "a", status: "configured" },
          { name: "b", status: "configured" },
        ],
      }).summary,
    ).toBe("MCP 2");
  });

  it("leaves tools and commands out of the collapsed line", () => {
    // They run to three digits and would dominate a line meant to be
    // glanceable; both are one click away.
    const v = view({
      mcp_servers: [{ name: "a", status: "connected" }],
      tools: Array.from({ length: 109 }, (_, i) => `tool${i}`),
      slash_commands: ["init"],
      approved_commands: ["ls"],
    });
    expect(v.summary).toBe("MCP 1");
    expect(v.lists.map((l) => l.label)).toEqual(["Tools", "Commands", "Approved commands"]);
  });

  it("skips a list the harness reported as empty", () => {
    expect(view({ mcp_servers: [], agents: ["Explore"] }).summary).toBe("Agents 1");
  });
});

describe("environmentView sections", () => {
  it("renders no row at all when the harness reported nothing", () => {
    expect(environmentView(undefined)).toBeNull();
    expect(environmentView({})).toBeNull();
  });

  it("renders no row when every reported list is empty", () => {
    // An empty disclosure the user can open onto nothing is worse than none.
    expect(environmentView({ mcp_servers: [], skills: [], tools: [], settings: [] })).toBeNull();
  });

  it("renders the row when any single list has something in it", () => {
    expect(
      environmentView({ settings: [{ label: "Sandbox", value: "read-only" }] }),
    ).not.toBeNull();
  });

  it("gives a connected server the calm dot and no status text", () => {
    const server = view({ mcp_servers: [{ name: "a", status: "connected" }] }).servers?.[0];
    expect(server?.tone).toBe("idle");
    expect(server?.statusLabel).toBeUndefined();
    // The raw status still rides along: it is the dot's accessible name in
    // this one case where the dot is the sole status signal.
    expect(server?.status).toBe("connected");
  });

  it("gives an unknown status the warning dot and names it", () => {
    // The status set is unknown beyond `connected` and `needs-auth`, so an
    // unrecognized value must show rather than hide behind a calm dot.
    const servers = view({
      mcp_servers: [
        { name: "a", status: "needs-auth" },
        { name: "b", status: "disconnected" },
      ],
    }).servers;
    expect(servers?.[0]).toMatchObject({ tone: "warning", statusLabel: "needs-auth" });
    expect(servers?.[1]).toMatchObject({ tone: "warning", statusLabel: "disconnected" });
  });

  it("gives a config-file entry no dot at all", () => {
    // Every Codex card, and any harness before its first turn. A dot would
    // claim a runtime fact we do not have.
    const server = view({ mcp_servers: [{ name: "a", status: "configured" }] }).servers?.[0];
    expect(server?.tone).toBeUndefined();
    expect(server?.statusLabel).toBeUndefined();
  });

  it("carries the config scope a server was registered in", () => {
    const server = first(
      view({ mcp_servers: [{ name: "a", status: "connected", source: "claudeai" }] }).servers,
      "server",
    );
    expect(server.source).toBe("claudeai");
  });

  it("labels a memory path by its basename and keeps the full path", () => {
    expect(view({ memory_paths: ["/home/me/.claude/memory"] }).memory).toEqual([
      { label: "memory", path: "/home/me/.claude/memory" },
    ]);
  });

  it("keeps a plugin with no version", () => {
    expect(view({ plugins: [{ name: "bare" }, { name: "v", version: "0.0.1" }] }).plugins).toEqual([
      { name: "bare", version: undefined },
      { name: "v", version: "0.0.1" },
    ]);
  });

  it("sorts the long lists so a 109-row expansion is scannable", () => {
    const tools = first(view({ tools: ["Zebra", "Bash", "Read"] }).lists, "list");
    expect(tools.items).toEqual(["Bash", "Read", "Zebra"]);
  });

  it("omits a long list the harness never reported", () => {
    // Codex reports no tool inventory anywhere, so its card must draw no
    // Tools line — not "Tools · 0".
    const v = view({ slash_commands: ["init"] });
    expect(v.lists.map((l) => l.key)).toEqual(["slash_commands"]);
  });

  it("passes skill descriptions through", () => {
    expect(
      view({ skills: [{ name: "build-report", description: "Build reports.", path: "/p" }] })
        .skills,
    ).toEqual([{ name: "build-report", description: "Build reports.", path: "/p" }]);
  });

  it("does not alias the caller's arrays", () => {
    // The view is handed to a component that may sort or expand it; mutating
    // reducer state through it would be a silent cross-render bug.
    const inventory: SessionInventory = { tools: ["b", "a"], agents: ["Explore"] };
    const v = view(inventory);
    first(v.lists, "list").items.push("c");
    v.agents?.push("Plan");
    expect(inventory.tools).toEqual(["b", "a"]);
    expect(inventory.agents).toEqual(["Explore"]);
  });
});
