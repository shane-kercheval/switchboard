// Thin wrapper around Tauri's `invoke` for type safety. Each function maps
// 1:1 onto a `#[tauri::command]` in `crates/app/src/lib.rs`.

import { invoke } from "@tauri-apps/api/core";
import type {
  AgentId,
  AgentRecord,
  DirectoryInfo,
  HarnessInstallStatus,
  HarnessKind,
  LoadedTranscript,
  McpProviderInfo,
  MessageId,
  ProjectConversation,
  ProjectId,
  ProjectListing,
  ProjectSummary,
  SendId,
  WorkspaceDirectories,
} from "./types";

/// Auth probes exist for the getting-started surface (no-project state)
/// to show ✓/✗ per harness. The working UI does **not** call these —
/// reactive-auth posture means a logged-out harness is discovered on
/// send, surfaced as an `AuthFailure` turn in the transcript, not by a
/// startup probe. Binary presence is no longer probed via dedicated
/// `check_*_binary` commands here — it comes from the shared
/// `harnessAvailability` store (`get_harness_install_status`), which also
/// carries the version.
export async function checkCodexAuth(): Promise<void> {
  await invoke<null>("check_codex_auth");
}

/// See `checkCodexAuth` — same retention rationale.
export async function checkGeminiAuth(): Promise<void> {
  await invoke<null>("check_gemini_auth");
}

/// See `checkCodexAuth` — same retention rationale.
export async function checkAntigravityAuth(): Promise<void> {
  await invoke<null>("check_antigravity_auth");
}

/// Claude auth probe (macOS Keychain presence heuristic). Like the others,
/// consumed only by the getting-started surface — not the working UI.
export async function checkClaudeAuth(): Promise<void> {
  await invoke<null>("check_claude_auth");
}

/// Install status (present-on-PATH + best-effort version) for the
/// getting-started panel. Never throws on a missing binary — that's
/// reported as `{ installed: false, version: null }`.
export async function getHarnessInstallStatus(harness: HarnessKind): Promise<HarnessInstallStatus> {
  return await invoke<HarnessInstallStatus>("get_harness_install_status", { harness });
}

export async function pickDirectory(path: string): Promise<DirectoryInfo> {
  return await invoke<DirectoryInfo>("pick_directory", { path });
}

export async function initDirectory(path: string): Promise<DirectoryInfo> {
  return await invoke<DirectoryInfo>("init_directory", { path });
}

// The flat cross-directory project list. Each row carries its owning directory
// and availability; ordering is left to the caller (the switcher sorts by
// `last_activity`).
export async function listProjects(): Promise<ProjectListing[]> {
  return await invoke<ProjectListing[]>("list_projects");
}

// Every registered workspace directory (including empty ones) plus the
// persistability signal — the switcher's directory rows.
export async function listWorkspaceDirectories(): Promise<WorkspaceDirectories> {
  return await invoke<WorkspaceDirectories>("list_workspace_directories");
}

export async function createProject(name: string, directory: string): Promise<ProjectSummary> {
  return await invoke<ProjectSummary>("create_project", { name, directory });
}

/// Rename a project. The backend re-validates format + per-directory uniqueness
/// (the frontend pre-check is UX only) and returns the updated listing row (or
/// rejects with a collision/invalid-name error).
export async function renameProject(
  projectId: ProjectId,
  newName: string,
): Promise<ProjectListing> {
  return await invoke<ProjectListing>("rename_project", { projectId, newName });
}

/// Permanently delete one project's Switchboard state: drains its agents and
/// removes its index entry, then best-effort removes
/// `<directory>/.switchboard/projects/<id>/`. The working directory and each
/// agent's own harness session files are kept. "Already gone" is benign
/// success; failures that prevent removing the project from the listing reject.
export async function deleteProject(projectId: ProjectId): Promise<void> {
  await invoke("delete_project", { projectId });
}

/// Archive or unarchive a project — a user-global view-state flip
/// (`workspace.yaml`). Display-only: never stops a running agent and works even
/// when the project's directory is offline.
export async function setProjectArchived(projectId: ProjectId, archived: boolean): Promise<void> {
  await invoke("set_project_archived", { projectId, archived });
}

// Removes a directory from the workspace: drains its projects' in-flight turns,
// releases their locks, and drops the entry — leaving `.switchboard/` on disk.
export async function removeDirectory(path: string): Promise<void> {
  await invoke<null>("remove_directory", { path });
}

// The merged post-restart conversation for a project (journal user-messages +
// harness agent content + journal outcome markers). Replaces per-agent
// `loadTranscript` for the unified view.
export async function loadProjectConversation(projectId: ProjectId): Promise<ProjectConversation> {
  return await invoke<ProjectConversation>("load_project_conversation", { projectId });
}

export async function openProject(projectId: ProjectId): Promise<ProjectSummary> {
  return await invoke<ProjectSummary>("open_project", { projectId });
}

export async function setActiveProject(projectId: ProjectId): Promise<void> {
  await invoke<null>("set_active_project", { projectId });
}

export async function createAgent(name: string, harness: HarnessKind): Promise<AgentRecord> {
  return await invoke<AgentRecord>("create_agent", { name, harness });
}

/// Remove an agent: tears down its actor (cancelling any in-flight turn) and
/// deletes its registry record + Switchboard sidecars. Harness-native session
/// files are left intact.
export async function removeAgent(agentId: AgentId): Promise<void> {
  await invoke("remove_agent", { agentId });
}

/// Rename an agent. The backend re-validates format + uniqueness and returns the
/// updated record (or rejects with a collision/invalid-name error).
export async function renameAgent(agentId: AgentId, newName: string): Promise<AgentRecord> {
  return await invoke<AgentRecord>("rename_agent", { agentId, newName });
}

export async function attachAgent(
  name: string,
  harness: HarnessKind,
  existingSessionId: string,
): Promise<AgentRecord> {
  return await invoke<AgentRecord>("attach_agent", {
    name,
    harness,
    existingSessionId,
  });
}

export async function listAgents(projectId?: ProjectId): Promise<AgentRecord[]> {
  return await invoke<AgentRecord[]>("list_agents", { projectId });
}

export async function searchProjectFiles(
  projectId: ProjectId,
  query: string,
  limit: number,
): Promise<string[]> {
  return await invoke<string[]>("search_project_files", { projectId, query, limit });
}

// Returns the accepted-send receipt (`message_id`), NOT the turn_id. The
// turn's real `turn_id` arrives later on the correlated `turn_start` event
// (matched by `message_id`); a failure before the turn starts arrives as a
// `message_failed` event keyed by the same `message_id`. `sendId` is minted
// once per Send and passed on every per-recipient call so a fan-out's turns
// share it.
export async function sendMessage(
  agentId: string,
  prompt: string,
  sendId: SendId,
): Promise<MessageId> {
  return await invoke<MessageId>("send_message", { agentId, prompt, sendId });
}

// Cancel a whole send across its recipients (send-scoped, actor-decided): each
// recipient cancels its in-flight turn iff it belongs to `sendId` and drops any
// still-queued item of the send, never touching a later, unrelated turn. The
// per-turn cancelled terminals flow back over the agent event channels.
export async function cancelSend(sendId: SendId, recipients: AgentId[]): Promise<void> {
  await invoke("cancel_send", { sendId, recipients });
}

export async function cancelTurn(agentId: AgentId): Promise<void> {
  await invoke("cancel_turn", { agentId });
}

export async function cancelAgent(agentId: AgentId): Promise<void> {
  await invoke("cancel_agent", { agentId });
}

/// Per-agent session actions: the openable session-file path and a copy-ready
/// terminal resume command. Each field is null until the agent has a resolvable
/// session.
export interface AgentSessionInfo {
  session_file: string | null;
  resume_command: string | null;
}

export async function agentSessionInfo(agentId: AgentId): Promise<AgentSessionInfo> {
  return await invoke<AgentSessionInfo>("agent_session_info", { agentId });
}

/// Open the agent's harness session file in the OS default app (backend-resolved
/// path, opened Rust-side). Rejects if the agent has no session file yet.
export async function openSessionFile(agentId: AgentId): Promise<void> {
  await invoke("open_session_file", { agentId });
}

/// Open an external URL in the OS default browser. The backend validates the
/// scheme (http/https only) before opening, so a non-web link from transcript
/// content is rejected rather than handed to the OS opener.
export async function openExternalUrl(url: string): Promise<void> {
  await invoke("open_external_url", { url });
}

export async function loadTranscript(agentId: AgentId): Promise<LoadedTranscript> {
  return await invoke<LoadedTranscript>("load_transcript", { agentId });
}

/// Configured MCP prompt-server providers with their status (Settings list).
export async function listMcpProviders(): Promise<McpProviderInfo[]> {
  return await invoke<McpProviderInfo[]>("list_mcp_providers");
}

/// Add a generic MCP server. `bearer` is `null` for an unauthenticated server;
/// when present it is stored in the OS keychain, never in config. Triggers a
/// background cache rebuild.
export async function addMcpProvider(
  name: string,
  url: string,
  bearer: string | null,
): Promise<void> {
  await invoke("add_mcp_provider", { name, url, bearer });
}

/// Remove a configured MCP server (deletes its config entry and stored token).
export async function removeMcpProvider(name: string): Promise<void> {
  await invoke("remove_mcp_provider", { name });
}

/// Probe a candidate server before saving; resolves to the prompt count or
/// rejects with an actionable error.
export async function testMcpConnection(url: string, bearer: string | null): Promise<number> {
  return await invoke<number>("test_mcp_connection", { url, bearer });
}

/// Rebuild the cached prompt list from all providers (the Settings "Sync" action).
export async function syncPrompts(): Promise<void> {
  await invoke("sync_prompts");
}
