// Thin wrapper around Tauri's `invoke` for type safety. Each function maps
// 1:1 onto a `#[tauri::command]` in `crates/app/src/lib.rs`.

import { invoke } from "@tauri-apps/api/core";
import type {
  AgentSessionFingerprint,
  AgentId,
  AgentRecord,
  Attachment,
  BranchKind,
  ChangeKind,
  ChangedFile,
  CommitChanges,
  DirectoryInfo,
  FileDiff,
  ForwardOutcome,
  GitCommitRange,
  HarnessInstallStatus,
  HarnessKind,
  LoadedTranscript,
  McpProviderInfo,
  MessageId,
  ProjectConversation,
  ProjectId,
  Preferences,
  ProjectListing,
  ProjectSummary,
  Prompt,
  RenderedPrompt,
  RepoListing,
  SendId,
  StagedAttachment,
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

// --- Git view ---------------------------------------------------------------

// Track a repo in the Git view. Accepts any path inside a git repo (a
// subdirectory or linked worktree resolves to the same canonical root and
// dedups); rejects a non-git path so the caller can show an inline error.
export async function addTrackedRepo(path: string): Promise<void> {
  await invoke<null>("add_tracked_repo", { path });
}

// Untrack a repo. Registry-only — never touches files or the workspace.
export async function removeTrackedRepo(path: string): Promise<void> {
  await invoke<null>("remove_tracked_repo", { path });
}

// The aggregate Git-view read: every tracked repo's git read-model plus the
// Switchboard projects linked to each worktree. One unreadable repo degrades to
// an `available: false` row rather than failing the whole call.
export async function listTrackedRepos(): Promise<RepoListing[]> {
  return await invoke<RepoListing[]>("list_tracked_repos");
}

// Re-read a single tracked repo (per-repo refresh) without re-walking the rest.
export async function readTrackedRepo(path: string): Promise<RepoListing> {
  return await invoke<RepoListing>("read_tracked_repo", { path });
}

// Shell out `git fetch` for a tracked repo to refresh its remote-tracking refs.
// Best-effort: rejects with git's error on failure (no remote, no network, auth),
// which the caller records as a "fetch failed" state — never a fatal error.
export async function fetchRepo(path: string): Promise<void> {
  await invoke("fetch_repo", { path });
}

// The changed files in a worktree (working-tree changes vs. HEAD — staged,
// unstaged, untracked). Empty for a clean or unreadable worktree.
export async function changedFiles(path: string): Promise<ChangedFile[]> {
  return await invoke<ChangedFile[]>("changed_files", { path });
}

// The structured working-tree diff for one file in a worktree. Empty hunks for a
// clean file; `binary: true` for binary content; `truncated: true` when capped.
export async function fileDiff(path: string, file: string): Promise<FileDiff> {
  return await invoke<FileDiff>("file_diff", { path, file });
}

// Capped commit-summary ranges for one branch (read on demand, never fetches).
// `kind` selects the local vs. remote-tracking ref; rejects an untracked repo.
export async function branchCommits(
  repoRoot: string,
  kind: BranchKind,
  name: string,
): Promise<GitCommitRange[]> {
  return await invoke<GitCommitRange[]>("branch_commits", { repoRoot, kind, name });
}

// The files one commit changed (vs. its first parent). No worktree needed, so it
// serves branches with no local folder and remote-only branches. `found: false`
// means the commit no longer resolves (gc'd / branch force-updated).
export async function commitChangedFiles(repoRoot: string, oid: string): Promise<CommitChanges> {
  return await invoke<CommitChanges>("commit_changed_files", { repoRoot, oid });
}

// The structured diff of one file within one commit (vs. its first parent).
export async function commitFileDiff(
  repoRoot: string,
  oid: string,
  file: string,
): Promise<FileDiff> {
  return await invoke<FileDiff>("commit_file_diff", { repoRoot, oid, file });
}

// Open a worktree folder in the user's configured editor (`editor_command`), or
// the OS folder-open when no editor command is set. Rejects with the opener's
// error on failure.
export async function openInEditor(path: string): Promise<void> {
  await invoke("open_in_editor", { path });
}

// Open a path in the user's configured terminal app.
export async function openInTerminal(path: string): Promise<void> {
  await invoke("open_in_terminal", { path });
}

// Reveal a path in Finder (selects the item in its containing folder).
export async function revealInFinder(path: string): Promise<void> {
  await invoke("reveal_in_finder", { path });
}

export async function openWorktreeFileDifftool(
  worktreePath: string,
  file: string,
  change: ChangeKind,
): Promise<void> {
  await invoke("open_worktree_file_difftool", { worktreePath, file, change });
}

export async function openCommitFileDifftool(
  repoRoot: string,
  oid: string,
  file: string,
): Promise<void> {
  await invoke("open_commit_file_difftool", { repoRoot, oid, file });
}

// Backend-owned personal preferences (`config.yaml`). `getPreferences` always
// returns a value (defaults if unset); `setPreferences` replaces the whole
// object and persists it, surfacing a write failure.
export async function getPreferences(): Promise<Preferences> {
  return await invoke<Preferences>("get_preferences");
}

export async function setPreferences(preferences: Preferences): Promise<void> {
  await invoke("set_preferences", { preferences });
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

// Cheap per-agent session-file freshness check (stat only, no parse) that gates
// the staleness re-read on project re-activation.
export async function projectSessionFingerprints(
  projectId: ProjectId,
): Promise<AgentSessionFingerprint[]> {
  return await invoke<AgentSessionFingerprint[]>("project_session_fingerprints", { projectId });
}

export async function openProject(projectId: ProjectId): Promise<ProjectSummary> {
  return await invoke<ProjectSummary>("open_project", { projectId });
}

export async function setActiveProject(projectId: ProjectId): Promise<void> {
  await invoke<null>("set_active_project", { projectId });
}

export async function createAgent(
  name: string,
  harness: HarnessKind,
  model?: string,
  effort?: string,
): Promise<AgentRecord> {
  return await invoke<AgentRecord>("create_agent", { name, harness, model, effort });
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

/// Change (or clear) an agent's selected model. `model` undefined clears the
/// override back to `None`. The backend re-validates harness capability and
/// returns the updated record (or rejects for an unsupported harness). Takes
/// effect on the agent's next dispatch — never an in-flight turn.
export async function setAgentModel(agentId: AgentId, model?: string): Promise<AgentRecord> {
  return await invoke<AgentRecord>("set_agent_model", { agentId, model });
}

/// Change (or clear) an agent's selected reasoning effort. See `setAgentModel`.
export async function setAgentEffort(agentId: AgentId, effort?: string): Promise<AgentRecord> {
  return await invoke<AgentRecord>("set_agent_effort", { agentId, effort });
}

export async function attachAgent(
  name: string,
  harness: HarnessKind,
  existingSessionId: string,
  model?: string,
  effort?: string,
): Promise<AgentRecord> {
  return await invoke<AgentRecord>("attach_agent", {
    name,
    harness,
    existingSessionId,
    model,
    effort,
  });
}

/// Persist a new roster order for a project. `agentIds` must be an exact
/// permutation of the project's current agents — the backend rejects a stale
/// list (an agent added or removed since the roster was read). Returns the
/// records in their new order.
export async function reorderAgents(
  projectId: ProjectId,
  agentIds: AgentId[],
): Promise<AgentRecord[]> {
  return await invoke<AgentRecord[]>("reorder_agents", { projectId, agentIds });
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
  attachments: Attachment[] = [],
): Promise<MessageId> {
  return await invoke<MessageId>("send_message", { agentId, prompt, attachments, sendId });
}

// Copy a dropped file into the project's attachments dir, returning its staged
// absolute path and original basename. The frontend then assigns the chip's
// `label`/`kind` and builds the full `Attachment` for the send.
export async function stageAttachment(
  projectId: ProjectId,
  sourcePath: string,
): Promise<StagedAttachment> {
  return await invoke<StagedAttachment>("stage_attachment", { projectId, sourcePath });
}

// Cancel a whole send across its recipients (send-scoped, actor-decided): each
// recipient cancels its in-flight turn iff it belongs to `sendId` and drops any
// still-queued item of the send, never touching a later, unrelated turn. The
// per-turn cancelled terminals flow back over the agent event channels.
export async function cancelSend(sendId: SendId, recipients: AgentId[]): Promise<void> {
  await invoke("cancel_send", { sendId, recipients });
}

// Manual cross-agent forward: hold until each `sources` agent's current turn
// finishes, then compose their outputs into the user's `body` and return the
// composed body for the caller to dispatch (the backend resolves but does not
// send — see `ForwardOutcome`). Long-lived by design — the promise resolves only
// once the hold settles (resolved / invalidated / cancelled). `sources` are
// pane-expanded agent ids (panes are frontend-only). `forwardId` correlates a
// later `cancelForward` with this in-flight hold.
export async function forwardMessage(
  body: string,
  sources: AgentId[],
  forwardId: string,
): Promise<ForwardOutcome> {
  return await invoke<ForwardOutcome>("forward_message", { body, sources, forwardId });
}

// One prompt argument being forwarded into: its name, the (pane-expanded) source
// agent ids, and whether the argument is required (the backend fails the forward
// if a required arg resolves fully empty).
export interface ForwardArg {
  name: string;
  sources: AgentId[];
  required: boolean;
}

// Manual forward into a prompt's arguments (M2.5): hold until each forwarded
// argument's sources finish, compose each (typed text + forwarded blocks), fill
// the args map, render the prompt, and return the rendered body for the caller to
// dispatch. `typedArgs` carries every argument's typed value (forwarded args
// included — their typed text leads); `forwardArgs` adds sources + required for
// the arguments being forwarded into. Same hold/cancel/`ForwardOutcome` contract
// as `forwardMessage`.
export async function forwardPrompt(
  provider: string,
  name: string,
  typedArgs: Record<string, string>,
  forwardArgs: ForwardArg[],
  appendedText: string,
  appendedSources: AgentId[],
  forwardId: string,
): Promise<ForwardOutcome> {
  return await invoke<ForwardOutcome>("forward_prompt", {
    provider,
    name,
    typedArgs,
    forwardArgs,
    appendedText,
    appendedSources,
    forwardId,
  });
}

// Cancel a held forward by id, releasing its source wait without dispatching.
// Idempotent — a no-op once the forward has settled. The held `forwardMessage`
// call then resolves `{ status: "cancelled" }`.
export async function cancelForward(forwardId: string): Promise<void> {
  await invoke("cancel_forward", { forwardId });
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

export async function localPromptsDir(): Promise<string> {
  return await invoke<string>("local_prompts_dir");
}

export async function openLocalPromptsDir(): Promise<void> {
  await invoke("open_local_prompts_dir");
}

/// Copy a built-in prompt into the user's prompts folder as an owned, editable
/// file (`<name>.md`), refreshing the prompt cache so it appears. Rejects with an
/// actionable error if a file of that name already exists. Returns the path.
export async function copyBuiltinPrompt(name: string): Promise<string> {
  return await invoke<string>("copy_builtin_prompt", { name });
}

/// Rebuild the cached prompt list from all providers (the Settings "Sync" action).
export async function syncPrompts(): Promise<void> {
  await invoke("sync_prompts");
}

/// All prompts across configured providers, from the build-once cache. Cheap and
/// offline — never hits the network — so the compose-bar prompt picker can open
/// against it instantly.
export async function listPrompts(): Promise<Prompt[]> {
  return await invoke<Prompt[]>("list_prompts");
}

/// Render `name` from `provider` with `args` to its finished text. Serves both
/// the composer's preview and its send (the same args map for both). May touch
/// the network (MCP `prompts/get`), so callers show a pending state.
export async function renderPrompt(
  provider: string,
  name: string,
  args: Record<string, string>,
): Promise<RenderedPrompt> {
  return await invoke<RenderedPrompt>("render_prompt", { provider, name, args });
}
