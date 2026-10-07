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
  it("collapses to one line with only actionable status", async () => {
    render(AgentEnvironment, { props: { inventory: CLAUDE } });

    expect(screen.getByTestId("agent-env-toggle")).toHaveTextContent("Environment");
    expect(screen.getByTestId("agent-env-trigger-summary")).toHaveTextContent("1 need auth");
    expect(screen.queryByTestId("agent-env-detail")).toBeNull();

    await expand();
    expect(screen.getByTestId("agent-env-inventory-summary")).toHaveTextContent(
      "MCP 2 · 1 need auth · Agents 2 · Plugins 1 · Skills 1 · Memory 1",
    );
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
    const agents = within(detail).getByTestId("agent-env-agents");
    expect(agents).not.toHaveTextContent("Explore");
    await fireEvent.click(within(detail).getByTestId("agent-env-agents-toggle"));
    expect(within(agents).getByText("Explore")).toBeInTheDocument();
    expect(within(agents).getByText("Plan")).toBeInTheDocument();
    expect(within(detail).getByTestId("agent-env-plugins")).toHaveTextContent(
      "anthropic-skills @ 0.0.1",
    );
    expect(within(detail).getByTestId("agent-env-memory")).toHaveTextContent("memory");
    const settings = within(detail).getByTestId("agent-env-settings");
    expect(within(settings).getByText("Permission mode")).toBeInTheDocument();
    expect(within(settings).getByText("bypassPermissions")).toBeInTheDocument();
    expect(within(settings).getByText("Output style")).toBeInTheDocument();
    expect(within(settings).getByText("default")).toBeInTheDocument();
    const commands = within(detail).getByTestId("agent-env-list-slash_commands");
    expect(
      commands.compareDocumentPosition(agents) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    expect(
      agents.compareDocumentPosition(settings) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it("names the connected dot by its status, not the server beside it", async () => {
    // The sibling text already says "tiddly"; the dot is the sole signal for
    // health, so its accessible name must be the health.
    render(AgentEnvironment, {
      props: { inventory: { mcp_servers: [{ name: "tiddly", status: "connected" }] } },
    });
    await expand();

    const dot = screen.getByTestId("agent-env-mcp-dot");
    expect(dot).toHaveAttribute("aria-label", "connected");
    expect(dot).not.toHaveAttribute("aria-label", "tiddly");
    expect(dot).not.toHaveAttribute("tabindex");
    expect(screen.queryByRole("tooltip")).not.toBeInTheDocument();
  });

  it("keeps the warning dot decorative beside its visible status", async () => {
    render(AgentEnvironment, {
      props: { inventory: { mcp_servers: [{ name: "gmail", status: "needs-auth" }] } },
    });
    await expand();

    const dot = screen.getByTestId("agent-env-mcp-dot");
    expect(dot).toHaveAttribute("aria-hidden", "true");
    expect(screen.getByTestId("agent-env-mcp")).toHaveTextContent("needs-auth");
  });

  it("renders every entry when a harness reports two with the same name", async () => {
    // A recorded Claude `init` lists `deep-research` twice; copying a bundled
    // skill to customize it is the ordinary way that happens. A keyed list
    // threw on this in production. Duplicates are preserved, not merged: the
    // count must match what the harness reported, and two same-named skills
    // from different roots are two skills.
    render(AgentEnvironment, {
      props: {
        inventory: {
          mcp_servers: [
            { name: "tiddly", status: "connected" },
            { name: "tiddly", status: "needs-auth" },
          ],
          plugins: [
            { name: "kit", version: "1" },
            { name: "kit", version: "2" },
          ],
          memory_paths: ["/same", "/same"],
          skills: [
            { name: "deep-research", description: "bundled" },
            { name: "deep-research", description: "customized" },
          ],
        },
      },
    });
    await expand();

    // Servers: both rows survive with their own status, not one row twice.
    const dots = screen.getAllByTestId("agent-env-mcp-dot");
    expect(dots).toHaveLength(2);
    expect(dots[0]).toHaveClass("bg-accent");
    expect(dots[1]).toHaveClass("bg-warning");
    expect(screen.getByTestId("agent-env-plugins")).toHaveTextContent("kit @ 1");
    expect(screen.getByTestId("agent-env-plugins")).toHaveTextContent("kit @ 2");
    expect(screen.getAllByTestId("agent-env-memory-entry")).toHaveLength(2);

    await fireEvent.click(screen.getByTestId("agent-env-skills-toggle"));
    const skills = screen.getByTestId("agent-env-skills");
    expect(skills).toHaveTextContent("Skills · 2");
    expect(within(skills).getAllByText("deep-research")).toHaveLength(2);
    expect(within(skills).getByText("bundled")).toBeInTheDocument();
    expect(within(skills).getByText("customized")).toBeInTheDocument();
  });

  it("shows a needs-auth server as a warning naming its status", async () => {
    render(AgentEnvironment, { props: { inventory: CLAUDE } });
    await expand();

    const mcp = screen.getByTestId("agent-env-mcp");
    expect(mcp).toHaveTextContent("needs-auth");
    const dots = screen.getAllByTestId("agent-env-mcp-dot");
    expect(dots).toHaveLength(2);
    expect(dots[0]).toHaveClass("bg-accent");
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

  it("keeps a long list behind a one-line trigger that opens a popover", async () => {
    render(AgentEnvironment, { props: { inventory: CLAUDE } });
    await expand();

    const tools = screen.getByTestId("agent-env-list-tools");
    expect(tools).toHaveTextContent("Tools · 2");
    expect(tools).not.toHaveTextContent("Bash");

    await fireEvent.click(screen.getByTestId("agent-env-list-toggle-tools"));
    expect(
      within(screen.getByTestId("agent-env-list-tools")).getByText("Bash"),
    ).toBeInTheDocument();
    expect(
      within(screen.getByTestId("agent-env-list-tools")).getByText("Read"),
    ).toBeInTheDocument();
  });

  it("keeps skills behind their own count line, with descriptions", async () => {
    render(AgentEnvironment, { props: { inventory: CODEX } });
    await expand();

    const skills = screen.getByTestId("agent-env-skills");
    expect(skills).toHaveTextContent("Skills · 1");
    expect(skills).not.toHaveTextContent("Build polished reports.");

    await fireEvent.click(screen.getByTestId("agent-env-skills-toggle"));
    expect(
      within(screen.getByTestId("agent-env-skills")).getByText("build-report"),
    ).toBeInTheDocument();
    expect(
      within(screen.getByTestId("agent-env-skills")).getByText("Build polished reports."),
    ).toBeInTheDocument();
  });

  it("expands the approved-command allowlist as command lines", async () => {
    render(AgentEnvironment, { props: { inventory: CODEX } });
    await expand();
    await fireEvent.click(screen.getByTestId("agent-env-list-toggle-approved_commands"));

    const commands = screen.getByTestId("agent-env-list-approved_commands");
    expect(within(commands).getByText("brew install jq")).toBeInTheDocument();
    expect(within(commands).getByText("ls")).toBeInTheDocument();
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
