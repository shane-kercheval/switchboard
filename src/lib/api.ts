// Thin wrapper around Tauri's `invoke` for type safety. Each function maps
// 1:1 onto a `#[tauri::command]` in `crates/app/src/lib.rs`.

import { invoke } from "@tauri-apps/api/core";
import type {
  AgentRecord,
  DirectoryInfo,
  HarnessKind,
  ProjectId,
  ProjectSummary,
  TurnId,
} from "./types";

export async function checkClaudeBinary(): Promise<void> {
  await invoke<null>("check_claude_binary");
}

export async function checkCodexBinary(): Promise<void> {
  await invoke<null>("check_codex_binary");
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

export async function listAgents(projectId?: ProjectId): Promise<AgentRecord[]> {
  return await invoke<AgentRecord[]>("list_agents", { projectId });
}

export async function sendMessage(agentId: string, prompt: string): Promise<TurnId> {
  return await invoke<TurnId>("send_message", { agentId, prompt });
}
