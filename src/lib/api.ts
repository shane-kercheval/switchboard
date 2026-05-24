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
  ProjectId,
  ProjectSummary,
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

export async function listProjects(): Promise<ProjectSummary[]> {
  return await invoke<ProjectSummary[]>("list_projects");
}

export async function createProject(name: string): Promise<ProjectSummary> {
  return await invoke<ProjectSummary>("create_project", { name });
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
// `message_failed` event keyed by the same `message_id`.
export async function sendMessage(agentId: string, prompt: string): Promise<MessageId> {
  return await invoke<MessageId>("send_message", { agentId, prompt });
}

export async function loadTranscript(agentId: AgentId): Promise<LoadedTranscript> {
  return await invoke<LoadedTranscript>("load_transcript", { agentId });
}
