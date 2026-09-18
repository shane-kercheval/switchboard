import { describe, expect, it } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, within } from "@testing-library/svelte";
import AgentEnvironment from "./AgentEnvironment.svelte";
import type { SessionInventory } from "$lib/types";

/// A Claude-shaped inventory: everything `system/init` reports.
const CLAUDE: SessionInventory = {
  tools: ["Bash", "Read"],
  mcp_servers: [
    { name: "tiddly", status: "connected", source: "user" },
    { name: "gmail", status: "needs-auth", source: "claudeai" },
  ],
  skills: [{ name: "dataviz" }],
  agents: ["Explore", "Plan"],
  plugins: [{ name: "anthropic-skills", version: "0.0.1" }],
  memory_paths: ["/home/me/.claude/memory"],
  slash_commands: ["init", "review"],
  settings: [
    { label: "Permission mode", value: "bypassPermissions" },
    { label: "Output style", value: "default" },
  ],
};

/// A Codex-shaped inventory: rollout skills with descriptions, the approved
/// allowlist, config-file MCP names with no runtime status, and no tools.
const CODEX: SessionInventory = {
  mcp_servers: [{ name: "tiddly", status: "configured" }],
  skills: [{ name: "build-report", description: "Build polished reports.", path: "/p" }],
  approved_commands: ["brew install jq", "ls"],
  settings: [{ label: "Sandbox", value: "read-only" }],
};

async function expand(): Promise<void> {
  await fireEvent.click(screen.getByTestId("agent-env-toggle"));
}

describe("AgentEnvironment", () => {
  it("collapses to one line of counts", () => {
    render(AgentEnvironment, { props: { inventory: CLAUDE } });

    expect(screen.getByTestId("agent-env-summary")).toHaveTextContent(
      "MCP 2 · 1 need auth · Agents 2 · Plugins 1 · Skills 1 · Memory 1",
    );
    expect(screen.queryByTestId("agent-env-detail")).toBeNull();
  });

  it("renders nothing when the harness reported no inventory", () => {
    render(AgentEnvironment, { props: { inventory: undefined } });

    expect(screen.queryByTestId("agent-meta")).toBeNull();
  });

  it("renders nothing when every reported list is empty", () => {
    render(AgentEnvironment, { props: { inventory: { mcp_servers: [], skills: [] } } });

    expect(screen.queryByTestId("agent-meta")).toBeNull();
  });

  it("expands to the sections in the card's order", async () => {
    render(AgentEnvironment, { props: { inventory: CLAUDE } });
    await expand();

    const detail = screen.getByTestId("agent-env-detail");
    expect(within(detail).getByTestId("agent-env-mcp")).toBeInTheDocument();
    expect(within(detail).getByTestId("agent-env-agents")).toHaveTextContent("Explore, Plan");
    expect(within(detail).getByTestId("agent-env-plugins")).toHaveTextContent(
      "anthropic-skills @ 0.0.1",
    );
    expect(within(detail).getByTestId("agent-env-memory")).toHaveTextContent("memory");
    expect(within(detail).getByTestId("agent-env-settings")).toHaveTextContent(
      "Permission mode: bypassPermissions · Output style: default",
    );
  });

  it("shows a needs-auth server as a warning naming its status", async () => {
    render(AgentEnvironment, { props: { inventory: CLAUDE } });
    await expand();

    const mcp = screen.getByTestId("agent-env-mcp");
    expect(mcp).toHaveTextContent("needs-auth");
    const dots = screen.getAllByTestId("agent-env-mcp-dot");
    expect(dots).toHaveLength(2);
    expect(dots[0]).toHaveClass("bg-status-idle");
    expect(dots[1]).toHaveClass("bg-warning");
  });

  it("names an unrecognized status rather than hiding it", async () => {
    // The status set is unknown beyond the two observed values.
    render(AgentEnvironment, {
      props: { inventory: { mcp_servers: [{ name: "x", status: "some-new-state" }] } },
    });
    await expand();

    expect(screen.getByTestId("agent-env-mcp")).toHaveTextContent("some-new-state");
    expect(screen.getByTestId("agent-env-mcp-dot")).toHaveClass("bg-warning");
  });

  it("draws no status dot for config-file servers", async () => {
    // A Codex card, and any harness before its first turn: `configured` says
    // where we read the server, not whether the agent can call it.
    render(AgentEnvironment, { props: { inventory: CODEX } });
    await expand();

    expect(screen.getByTestId("agent-env-mcp")).toHaveTextContent("tiddly");
    expect(screen.queryByTestId("agent-env-mcp-dot")).toBeNull();
    expect(screen.getByTestId("agent-env-mcp")).not.toHaveTextContent("configured");
  });

  it("hides a section the harness reported nothing for", async () => {
    render(AgentEnvironment, { props: { inventory: CODEX } });
    await expand();

    // Codex records its tools nowhere, so there is no Tools line — not
    // "Tools · 0".
    expect(screen.queryByTestId("agent-env-list-tools")).toBeNull();
    expect(screen.queryByTestId("agent-env-agents")).toBeNull();
    expect(screen.queryByTestId("agent-env-plugins")).toBeNull();
    expect(screen.queryByTestId("agent-env-memory")).toBeNull();
    expect(screen.getByTestId("agent-env-list-approved_commands")).toBeInTheDocument();
  });

  it("keeps a long list behind a count line that expands in place", async () => {
    render(AgentEnvironment, { props: { inventory: CLAUDE } });
    await expand();

    const tools = screen.getByTestId("agent-env-list-tools");
    expect(tools).toHaveTextContent("Tools · 2");
    expect(tools).not.toHaveTextContent("Bash");

    await fireEvent.click(screen.getByTestId("agent-env-list-toggle-tools"));
    expect(screen.getByTestId("agent-env-list-tools")).toHaveTextContent("Bash, Read");
  });

  it("keeps skills behind their own count line, with descriptions", async () => {
    render(AgentEnvironment, { props: { inventory: CODEX } });
    await expand();

    const skills = screen.getByTestId("agent-env-skills");
    expect(skills).toHaveTextContent("Skills · 1");
    expect(skills).not.toHaveTextContent("Build polished reports.");

    await fireEvent.click(screen.getByTestId("agent-env-skills-toggle"));
    expect(screen.getByTestId("agent-env-skills")).toHaveTextContent(
      "build-report — Build polished reports.",
    );
  });

  it("expands the approved-command allowlist as command lines", async () => {
    render(AgentEnvironment, { props: { inventory: CODEX } });
    await expand();
    await fireEvent.click(screen.getByTestId("agent-env-list-toggle-approved_commands"));

    expect(screen.getByTestId("agent-env-list-approved_commands")).toHaveTextContent(
      "brew install jq, ls",
    );
  });

  it("says the list is a snapshot only when it was rehydrated", async () => {
    const { unmount } = render(AgentEnvironment, {
      props: { inventory: CLAUDE, asOf: "2026-09-17T12:00:00Z" },
    });
    await expand();
    expect(screen.getByTestId("agent-env-as-of")).toHaveTextContent(/^as of /);
    unmount();

    render(AgentEnvironment, { props: { inventory: CLAUDE, asOf: null } });
    await expand();
    expect(screen.queryByTestId("agent-env-as-of")).toBeNull();
  });

  it("carries the full memory path on hover, not on the card", async () => {
    render(AgentEnvironment, { props: { inventory: CLAUDE } });
    await expand();

    const entry = screen.getByTestId("agent-env-memory-entry");
    expect(entry).toHaveTextContent("memory");
    expect(entry).not.toHaveTextContent("/home/me");
    // The row is the tooltip trigger rather than a nested focus target.
    expect(entry).toHaveAttribute("data-tooltip-trigger");
  });

  it("reports the expanded state to assistive tech at both levels", async () => {
    render(AgentEnvironment, { props: { inventory: CLAUDE } });

    const row = screen.getByTestId("agent-env-toggle");
    expect(row).toHaveAttribute("aria-expanded", "false");
    await expand();
    expect(row).toHaveAttribute("aria-expanded", "true");

    const tools = screen.getByTestId("agent-env-list-toggle-tools");
    expect(tools).toHaveAttribute("aria-expanded", "false");
    await fireEvent.click(tools);
    expect(screen.getByTestId("agent-env-list-toggle-tools")).toHaveAttribute(
      "aria-expanded",
      "true",
    );
  });
});
