/// Derives the agent card's Environment entry and detail popover from what the
/// harness reported having loaded.
///
/// Kept out of `Sidebar.svelte` for the same reason `usageWindows.ts` is: this
/// is input validation and presentation policy over an undocumented,
/// per-harness shape, and it is worth testing directly rather than only
/// through rendered markup.
///
/// **The governing rule is disclosure, not omission.** The narrow card shows
/// only actionable state; the complete inventory lives in a bounded popover,
/// where long lists collapse to count lines. What *is* dropped is a list the
/// harness never reported and a list it reported as empty: neither has
/// anything to show, so neither draws a section. The two still differ
/// upstream, where only the unreported one may be filled from a config
/// registry.
import { basename } from "$lib/utils";
import type { McpServerStatus, SessionInventory, SettingPair, SkillEntry } from "$lib/types";

/// The status the config loaders stamp on a server read from a config file.
/// Must match `CONFIGURED_STATUS` in the Rust `*/config.rs` loaders.
const CONFIGURED_STATUS = "configured";

/// The status a healthy MCP server reports. Anything else — `needs-auth` is
/// the only other value observed — shows as a warning: the set is unknown
/// beyond those two, so an unrecognized status must *show*, labelled with
/// whatever the harness called it, rather than hide behind a calm dot.
const CONNECTED_STATUS = "connected";

export type ServerTone = "success" | "warning";

export type EnvironmentServer = {
  name: string;
  /// The harness's own status string, verbatim. The connected dot's
  /// accessible name — the one case where the dot is the sole status signal.
  status: string;
  /// Which config scope registered it, when the harness says.
  source?: string;
  /// `undefined` draws **no dot**: the entry came from a config file, and
  /// `"configured"` describes where we read it, not whether the agent can
  /// call it. Rendering a status dot for it would claim a runtime fact we do
  /// not have — the case for every Codex card, and for any harness before its
  /// first turn.
  tone?: ServerTone;
  /// Shown beside the dot when the status is not plain `connected`, so an
  /// unknown value is named rather than merely coloured.
  ///
  /// Equivalent to `status` whenever `tone` is `warning` today, and kept as
  /// its own field rather than derived from the tone deliberately: whether a
  /// status is worth *naming* is not the same question as which colour it
  /// gets. Deriving text visibility from the tone would silently stop naming
  /// any status a future third tone covered.
  statusLabel?: string;
};

/// A list long enough to live behind a count line ("Tools · 109") inside the
/// detail popover.
export type EnvironmentList = {
  key: string;
  label: string;
  items: string[];
};

export type EnvironmentMemory = { label: string; path: string };

export type EnvironmentPlugin = { name: string; version?: string };

/// Everything the Environment row renders. Each section is `null` when there
/// is nothing to draw; the view itself is `null` when that is true of all of
/// them.
export type EnvironmentView = {
  /// The detail popover's inventory overview.
  summary: string;
  /// Actionable state that remains visible on the compact card trigger. The
  /// complete inventory summary belongs inside the detail popover, where it
  /// has enough width to remain readable.
  attentionSummary: string | null;
  servers: EnvironmentServer[] | null;
  agents: string[] | null;
  plugins: EnvironmentPlugin[] | null;
  memory: EnvironmentMemory[] | null;
  skills: SkillEntry[] | null;
  /// Tools, slash commands, and the approved-command allowlist, in that
  /// order, omitting any the harness did not report.
  lists: EnvironmentList[];
  settings: SettingPair[] | null;
};

/// A reported, non-empty list, or `null`. Collapsing "unreported" and
/// "reported empty" is correct **here and only here**: neither draws a
/// section, and the distinction has already done its work upstream, deciding
/// whether a config registry was allowed to fill the list.
function present<T>(list: readonly T[] | undefined): T[] | null {
  return list !== undefined && list.length > 0 ? [...list] : null;
}

function serverTone(status: string): ServerTone | undefined {
  if (status === CONFIGURED_STATUS) return undefined;
  return status === CONNECTED_STATUS ? "success" : "warning";
}

function toServer(server: McpServerStatus): EnvironmentServer {
  return {
    name: server.name,
    status: server.status,
    source: server.source,
    tone: serverTone(server.status),
    statusLabel:
      server.status === CONNECTED_STATUS || server.status === CONFIGURED_STATUS
        ? undefined
        : server.status,
  };
}

/// The one non-healthy status observed so far, and the only one whose cause
/// the collapsed line may name.
const NEEDS_AUTH_STATUS = "needs-auth";

/// How many servers are in a state the user has to act on, split by whether
/// the cause is known. Called out in the collapsed line because it is the one
/// thing about the list that cannot wait for the user to expand it — a card
/// reading "MCP 7" while two of them are unusable is the failure this row
/// exists to prevent.
///
/// Two counts rather than one: "need auth" is actionable copy for the status
/// we have actually seen, and any other warning status is reported as
/// needing attention rather than being folded under an auth instruction that
/// will not fix it. No status *name* is invented for the unobserved ones —
/// the same reasoning that drops unknown usage-window keys rather than
/// labelling them — and the expanded row names each raw status regardless.
function attentionCounts(servers: readonly McpServerStatus[]): {
  needsAuth: number;
  other: number;
} {
  let needsAuth = 0;
  let other = 0;
  for (const server of servers) {
    if (serverTone(server.status) !== "warning") continue;
    if (server.status === NEEDS_AUTH_STATUS) needsAuth += 1;
    else other += 1;
  }
  return { needsAuth, other };
}

function summaryOf(inventory: SessionInventory): string {
  const parts: string[] = [];
  const servers = present(inventory.mcp_servers);
  if (servers !== null) {
    parts.push(`MCP ${servers.length}`);
    const { needsAuth, other } = attentionCounts(servers);
    if (needsAuth > 0) parts.push(`${needsAuth} need auth`);
    if (other > 0) parts.push(`${other} need attention`);
  }
  // Tools, commands and the allowlist are deliberately absent: they run to
  // three digits, would dominate the line, and are one click away.
  const counted: [string, unknown[] | null][] = [
    ["Agents", present(inventory.agents)],
    ["Plugins", present(inventory.plugins)],
    ["Skills", present(inventory.skills)],
    ["Memory", present(inventory.memory_paths)],
  ];
  for (const [label, list] of counted) {
    if (list !== null) parts.push(`${label} ${list.length}`);
  }
  return parts.join(" · ");
}

function attentionSummaryOf(inventory: SessionInventory): string | null {
  const servers = present(inventory.mcp_servers);
  if (servers === null) return null;
  const { needsAuth, other } = attentionCounts(servers);
  const parts: string[] = [];
  if (needsAuth > 0) parts.push(`${needsAuth} need auth`);
  if (other > 0) parts.push(`${other} need attention`);
  return parts.length === 0 ? null : parts.join(" · ");
}

export function environmentView(inventory: SessionInventory | undefined): EnvironmentView | null {
  if (inventory === undefined) return null;

  const servers = present(inventory.mcp_servers);
  const lists: EnvironmentList[] = [];
  const counted: [string, string, string[] | null][] = [
    ["tools", "Tools", present(inventory.tools)],
    ["slash_commands", "Commands", present(inventory.slash_commands)],
    ["approved_commands", "Approved commands", present(inventory.approved_commands)],
  ];
  for (const [key, label, items] of counted) {
    if (items !== null) lists.push({ key, label, items: [...items].sort() });
  }

  const view: EnvironmentView = {
    summary: summaryOf(inventory),
    attentionSummary: attentionSummaryOf(inventory),
    servers: servers === null ? null : servers.map(toServer),
    agents: present(inventory.agents),
    plugins: present(inventory.plugins)?.map(({ name, version }) => ({ name, version })) ?? null,
    memory:
      present(inventory.memory_paths)?.map((path) => ({ label: basename(path), path })) ?? null,
    skills: present(inventory.skills),
    lists,
    settings: present(inventory.settings),
  };

  // Nothing reported anything renderable — the card draws no row at all
  // rather than an empty disclosure the user can open onto nothing.
  const empty =
    view.servers === null &&
    view.agents === null &&
    view.plugins === null &&
    view.memory === null &&
    view.skills === null &&
    view.lists.length === 0 &&
    view.settings === null;
  return empty ? null : view;
}
