// Thin wrapper around Tauri's `invoke` for type safety. Each function maps
// 1:1 onto a `#[tauri::command]` in `crates/app/src/lib.rs`.

import { invoke } from "@tauri-apps/api/core";
import type {
  AgentId,
  AgentRecord,
  DirectoryInfo,
  HarnessKind,
  LoadedTranscript,
  MessageId,
  ProjectConversation,
  ProjectId,
  ProjectListing,
  ProjectSummary,
  SendId,
  WorkspaceDirectories,
} from "./types";

export async function checkClaudeBinary(): Promise<void> {
  await invoke<null>("check_claude_binary");
}

export async function checkCodexBinary(): Promise<void> {
  await invoke<null>("check_codex_binary");
}

export async function checkCodexAuth(): Promise<void> {
  await invoke<null>("check_codex_auth");
}

export async function checkGeminiBinary(): Promise<void> {
  await invoke<null>("check_gemini_binary");
}

export async function checkGeminiAuth(): Promise<void> {
  await invoke<null>("check_gemini_auth");
}

export async function checkAntigravityBinary(): Promise<void> {
  await invoke<null>("check_antigravity_binary");
}

export async function checkAntigravityAuth(): Promise<void> {
  await invoke<null>("check_antigravity_auth");
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

export async function loadTranscript(agentId: AgentId): Promise<LoadedTranscript> {
  return await invoke<LoadedTranscript>("load_transcript", { agentId });
}
