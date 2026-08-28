// Thin wrapper around Tauri's `invoke` for type safety. Each function maps
// 1:1 onto a `#[tauri::command]` in `crates/app/src/lib.rs`.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AgentSessionFingerprint,
  ActivationCommandError,
  ActivationFailureKind,
  AgentId,
  AgentProfile,
  AgentProfileSlot,
  AgentRecord,
  ForwardSourceRef,
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
  McpAuth,
  McpProviderInfo,
  MessageId,
  MessagePin,
  ProjectConversation,
  ProjectId,
  NotificationAvailability,
  Preferences,
  ProjectListing,
  ProjectSummary,
  ProviderStatus,
  PathSource,
  Prompt,
  SavedPromptResolution,
  PromptSource,
  RenderPromptOutcome,
  RepoListing,
  SendId,
  StagedAttachment,
  WorkflowFormDescriptor,
  WorkflowInputValue,
  WorkflowListing,
  WorkflowRunInfo,
  WorkspaceDirectories,
} from "./types";

export class ActivationFailureError extends Error {
  readonly type: ActivationFailureKind;

  constructor(type: ActivationFailureKind, message: string) {
    super(message);
    this.name = "ActivationFailureError";
    this.type = type;
  }
}

function activationFailure(error: unknown): ActivationFailureError {
  if (typeof error === "object" && error !== null) {
    const wire = error as Partial<ActivationCommandError>;
    if (typeof wire.message === "string") {
      const type: ActivationFailureKind =
        wire.type === "project_not_loaded" || wire.type === "project_locked" ? wire.type : "other";
      return new ActivationFailureError(type, wire.message);
    }
  }
  const message = error instanceof Error ? error.message : String(error);
  return new ActivationFailureError("other", message);
}

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

/// Discard the backend's cached PATH, re-read it from the user's login shell,
/// and report where the new value came from. Install probes search a PATH
/// captured once per app launch; if that capture failed (a slow login-restore),
/// every harness reads as "not installed" until something forces a re-read.
/// Waits for the shell, so this can take seconds.
export async function recheckHarnessInstalls(): Promise<PathSource> {
  return await invoke<PathSource>("recheck_harness_installs");
}

/// Wait (bounded) for the login-shell PATH to be resolved, then report where it
/// came from. For callers that need a definitive answer — auto-create runs once
/// per project creation and cannot retroactively add agents it skipped.
export async function awaitHarnessPath(): Promise<PathSource> {
  return await invoke<PathSource>("await_harness_path");
}

/// Fires when the backend finishes resolving the login-shell PATH. Detection
/// results taken before then are provisional, so listeners re-probe on receipt.
export async function listenHarnessPathResolved(handler: () => void): Promise<UnlistenFn> {
  return await listen("harness_path_resolved", () => handler());
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

// The selected commit's message body and changed files (vs. its first parent).
// No worktree needed, so it serves branches with no local folder and remote-only
// branches. `found: false` means the commit no longer resolves (gc'd / branch
// force-updated).
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

/// Tell the backend which project's transcript is on screen, or `null` when none
/// is (Settings, the Git view, a project still loading). The notification gate
/// reads it to stay quiet about the one outcome the user can already see.
/// `seq` is a monotonic navigation counter; the backend drops stale writes.
export async function setVisibleProject(projectId: ProjectId | null, seq: number): Promise<void> {
  await invoke("set_visible_project", { projectId, seq });
}

/// Post a notification. The backend applies the suppression policy (window
/// focus, the visible project, the user's preferences), so a call here is a
/// request, not a guarantee.
export async function notify(projectId: ProjectId, title: string, body: string): Promise<void> {
  await invoke("notify", { projectId, title, body });
}

export async function notificationAvailability(): Promise<NotificationAvailability> {
  return await invoke<NotificationAvailability>("notification_availability");
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

export async function listMessagePins(projectId: ProjectId): Promise<MessagePin[]> {
  return await invoke<MessagePin[]>("list_message_pins", { projectId });
}

export async function setMessagePin(
  projectId: ProjectId,
  key: string,
  pinned: boolean,
): Promise<MessagePin[]> {
  return await invoke<MessagePin[]>("set_message_pin", { projectId, key, pinned });
}

export async function removeMessagePins(
  projectId: ProjectId,
  keys: string[],
): Promise<MessagePin[]> {
  return await invoke<MessagePin[]>("remove_message_pins", { projectId, keys });
}

export async function migrateMessagePin(
  projectId: ProjectId,
  fromKey: string,
  toKey: string,
): Promise<MessagePin[]> {
  return await invoke<MessagePin[]>("migrate_message_pin", { projectId, fromKey, toKey });
}

// Removes a directory from the workspace: drains its projects' in-flight turns,
// releases their locks, and drops the entry — leaving `.switchboard/` on disk.
export async function removeDirectory(path: string): Promise<void> {
  await invoke<null>("remove_directory", { path });
}

// The merged post-restart conversation for a project (journal user-messages +
// harness agent content + journal outcome markers). Replaces per-agent
// `loadTranscript` for the unified view.
//
// `draftAttachments` are staged paths the project's unsent compose draft still
// points at. Loading a project garbage-collects every staged attachment the
// journal doesn't reference; the backend cannot see a draft (it lives in this
// process's localStorage), so an undeclared path is reclaimed and the restored
// draft's chip dangles. Pass the draft's paths, or `[]` when there is no draft.
export async function loadProjectConversation(
  projectId: ProjectId,
  draftAttachments: string[] = [],
): Promise<ProjectConversation> {
  return await invoke<ProjectConversation>("load_project_conversation", {
    projectId,
    draftAttachments,
  });
}

// Cheap per-agent session-file freshness check (stat only, no parse) that gates
// the staleness re-read on project re-activation.
export async function projectSessionFingerprints(
  projectId: ProjectId,
): Promise<AgentSessionFingerprint[]> {
  return await invoke<AgentSessionFingerprint[]>("project_session_fingerprints", { projectId });
}

export async function openProject(projectId: ProjectId): Promise<ProjectSummary> {
  try {
    return await invoke<ProjectSummary>("open_project", { projectId });
  } catch (error) {
    throw activationFailure(error);
  }
}

export async function setActiveProject(projectId: ProjectId): Promise<void> {
  try {
    await invoke<null>("set_active_project", { projectId });
  } catch (error) {
    throw activationFailure(error);
  }
}

export async function createAgent(
  name: string,
  harness: HarnessKind,
  model?: string,
  effort?: string,
  secondary?: AgentProfile | null,
): Promise<AgentRecord> {
  return await invoke<AgentRecord>("create_agent", {
    name,
    harness,
    model,
    effort,
    secondaryModel: secondary?.model ?? undefined,
    secondaryEffort: secondary?.effort ?? undefined,
  });
}

/// Branch an agent's conversation into a new agent, returning the new record.
/// Registration only — the branch does not exist as a harness session until its
/// first turn is dispatched (Claude has no copy-a-session operation), so the
/// caller must send its first message immediately. Rejects when the source
/// can't be branched: wrong harness, no session yet, or a turn in flight.
export async function forkAgent(agentId: AgentId): Promise<AgentRecord> {
  return await invoke<AgentRecord>("fork_agent", { agentId });
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

export async function setAgentProfiles(
  agentId: AgentId,
  primary: AgentProfile,
  secondary: AgentProfile | null,
): Promise<AgentRecord> {
  return await invoke<AgentRecord>("set_agent_profiles", { agentId, primary, secondary });
}

export async function setActiveAgentProfile(
  agentId: AgentId,
  active: AgentProfileSlot,
): Promise<AgentRecord> {
  return await invoke<AgentRecord>("set_active_agent_profile", { agentId, active });
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

/// List another project's agents **without loading or locking it** — the read
/// side of the display/activation split. Pickers browse with this; picking calls
/// the ordinary open path. Browsing must not take a project's `instance.lock`
/// (which would outlive the menu and block other Switchboard instances), and a
/// hover is no place to surface a lock conflict.
export async function listProjectAgentsReadonly(
  projectId: ProjectId,
  directory: string,
): Promise<AgentRecord[]> {
  try {
    return await invoke<AgentRecord[]>("list_project_agents_readonly", { projectId, directory });
  } catch (error) {
    // The command returns the structured `ActivationCommandError`; Tauri rejects
    // it as a plain object, so without this the picker's row would render
    // `[object Object]` instead of the reason the project couldn't be read.
    throw activationFailure(error);
  }
}

export async function listAgents(projectId?: ProjectId): Promise<AgentRecord[]> {
  try {
    return await invoke<AgentRecord[]>("list_agents", { projectId });
  } catch (error) {
    throw activationFailure(error);
  }
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

// Narrow staged paths to those that still exist under this project's attachments
// dir. A restored draft prunes its chips through this, so a chip whose file was
// removed out-of-band (a cleaned `.switchboard/`) doesn't dangle in the composer.
export async function existingAttachmentPaths(
  projectId: ProjectId,
  paths: string[],
): Promise<string[]> {
  return await invoke<string[]>("existing_attachment_paths", { projectId, paths });
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
  sources: ForwardSourceRef[],
  forwardId: string,
  projectId: ProjectId,
): Promise<ForwardOutcome> {
  return await invoke<ForwardOutcome>("forward_message", { body, sources, forwardId, projectId });
}

// One prompt argument being forwarded into: its name, the (pane-expanded) source
// refs (agent + owning project), and whether the argument is required (the backend fails the forward
// if a required arg resolves fully empty).
export interface ForwardArg {
  name: string;
  sources: ForwardSourceRef[];
  required: boolean;
}

// Manual forward into a prompt's arguments: hold until each forwarded
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
  appendedSources: ForwardSourceRef[],
  forwardId: string,
  projectId: ProjectId,
): Promise<ForwardOutcome> {
  return await invoke<ForwardOutcome>("forward_prompt", {
    provider,
    name,
    typedArgs,
    forwardArgs,
    appendedText,
    appendedSources,
    forwardId,
    projectId,
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

/// Open a fresh window in the configured Terminal/iTerm app and run the
/// backend-generated interactive resume command. The backend refuses while
/// Switchboard still owns running or queued work for the agent.
export async function resumeAgentInTerminal(agentId: AgentId): Promise<void> {
  await invoke("resume_agent_in_terminal", { agentId });
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

/// Add a generic MCP server. `bearer` applies only to bearer mode and is
/// `null` for an unauthenticated server; when present it is stored in the OS
/// keychain, never in config. An OAuth server is added credential-less and
/// then signed in from its row. Triggers a background cache rebuild.
export async function addMcpProvider(
  name: string,
  url: string,
  auth: McpAuth,
  bearer: string | null,
): Promise<void> {
  await invoke("add_mcp_provider", { name, url, auth, bearer });
}

/// Remove a configured MCP server (deletes its config entry and stored token).
export async function removeMcpProvider(name: string): Promise<void> {
  await invoke("remove_mcp_provider", { name });
}

/// Run the browser sign-in flow for a saved OAuth provider. Long-running —
/// resolves only when the user completes (or abandons) the browser round-trip,
/// so callers need a pending state that survives minutes.
export async function signInMcpProvider(name: string): Promise<void> {
  await invoke("sign_in_mcp_provider", { name });
}

/// Sign out an OAuth provider: clears its tokens, keeps its registration so a
/// later sign-in reuses it.
export async function signOutMcpProvider(name: string): Promise<void> {
  await invoke("sign_out_mcp_provider", { name });
}

/// Probe a saved provider by name with its stored credentials — the row-level
/// Test action. Resolves to a provider status (`ok`/`needs_auth`/`errored`/
/// `store_unavailable`); rejects only for an unknown name.
export async function testSavedMcpProvider(name: string): Promise<ProviderStatus> {
  return await invoke<ProviderStatus>("test_saved_mcp_provider", { name });
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

/// Resolve one saved prompt draft from the latest coherent cache snapshot.
export async function resolveSavedPrompt(
  provider: string,
  name: string,
): Promise<SavedPromptResolution> {
  return await invoke<SavedPromptResolution>("resolve_saved_prompt", { provider, name });
}

/// Explicitly retry a saved MCP prompt with one bounded, coalesced refresh.
export async function resolveSavedPromptFresh(
  provider: string,
  name: string,
): Promise<SavedPromptResolution> {
  return await invoke<SavedPromptResolution>("resolve_saved_prompt_fresh", { provider, name });
}

/// Render `name` from `provider` with `args`, resolving to a typed outcome:
/// the finished text, or a needs-sign-in determination the composer acts on by
/// launching the provider's browser sign-in. Serves both the composer's
/// preview and its send (the same args map for both). May touch the network
/// (MCP `prompts/get`), so callers show a pending state.
export async function renderPrompt(
  provider: string,
  name: string,
  args: Record<string, string>,
): Promise<RenderPromptOutcome> {
  return await invoke<RenderPromptOutcome>("render_prompt", { provider, name, args });
}

/// The raw, unrendered template body of `provider:name`, for a read-only preview.
/// Resolves to `null` for an MCP provider (its template lives server-side and the
/// protocol exposes no un-rendered source) or a prompt that doesn't resolve — the
/// caller then falls back to the cached metadata (description + arguments). Cheap
/// and offline for `builtin`/`local`; never substitutes arguments.
export async function getPromptSource(
  provider: string,
  name: string,
): Promise<PromptSource | null> {
  return await invoke<PromptSource | null>("get_prompt_source", { provider, name });
}

// ── Workflows ─────────────────────────────────────────────────────────────────

/// All workflows: the read-only built-in library (when `show_builtins` is on)
/// merged with the user-global workflows folder. User-global — the same set is
/// available from every project.
export async function listWorkflows(): Promise<WorkflowListing[]> {
  return await invoke<WorkflowListing[]>("list_workflows");
}

/// Resolve a picked workflow's invocation form: declared inputs + auto-derived
/// user-fillable prompt-argument fields + a compatibility verdict. No `projectId`
/// — prompts are user-global. Resolved per-pick (not in `listWorkflows`). An MCP
/// cache miss conditionally awaits/runs a sync before the command replies, so the
/// returned descriptor is settled even when the startup event was missed.
export async function describeWorkflowForm(
  name: string,
  isBuiltin: boolean,
): Promise<WorkflowFormDescriptor> {
  return await invoke<WorkflowFormDescriptor>("describe_workflow_form", { name, isBuiltin });
}

/// Reclassify an open workflow after an independent prompt sync. This command is
/// cache-only; unlike `describeWorkflowForm`, it never initiates another sync.
export async function refreshWorkflowFormFromCache(
  name: string,
  isBuiltin: boolean,
): Promise<WorkflowFormDescriptor> {
  return await invoke<WorkflowFormDescriptor>("refresh_workflow_form_from_cache", {
    name,
    isBuiltin,
  });
}

/// Validate a workflow invocation (capability gate + input/roster/prompt rules)
/// without launching it. Rejects with an actionable error string. `forwardSources`
/// maps a fillable field name → the (pane-expanded) agent ids whose completed
/// output the backend composes into that field's typed text. Empty map = none.
export async function validateWorkflowInvocation(
  projectId: ProjectId,
  name: string,
  isBuiltin: boolean,
  inputs: Record<string, WorkflowInputValue>,
  forwardSources: Record<string, ForwardSourceRef[]>,
): Promise<void> {
  await invoke("validate_workflow_invocation", {
    projectId,
    name,
    isBuiltin,
    inputs,
    forwardSources,
  });
}

/// Validate + launch a workflow run on a background task; returns its run id.
/// `forwardSources` maps a fillable field name → the (pane-expanded) source refs
/// whose completed output the backend composes into that field. Empty map = none.
export async function invokeWorkflow(
  projectId: ProjectId,
  name: string,
  isBuiltin: boolean,
  inputs: Record<string, WorkflowInputValue>,
  forwardSources: Record<string, ForwardSourceRef[]>,
): Promise<string> {
  return await invoke<string>("invoke_workflow", {
    projectId,
    name,
    isBuiltin,
    inputs,
    forwardSources,
  });
}

/// Fire a running workflow's cancel token (no-op if it already finished).
export async function cancelWorkflowRun(runId: string): Promise<void> {
  await invoke("cancel_workflow_run", { runId });
}

/// Active + retained-failed + interrupted runs for a project (the run indicator).
export async function listWorkflowRuns(projectId: ProjectId): Promise<WorkflowRunInfo[]> {
  return await invoke<WorkflowRunInfo[]>("list_workflow_runs", { projectId });
}

/// Clear a failed or interrupted run's file (the Abandon action).
export async function abandonWorkflowRun(projectId: ProjectId, runId: string): Promise<void> {
  await invoke("abandon_workflow_run", { projectId, runId });
}

/// Copy a built-in workflow into the user-global `workflows/` folder as an owned,
/// editable file. Returns the written path; rejects if it would clobber.
export async function copyBuiltinWorkflow(name: string): Promise<string> {
  return await invoke<string>("copy_builtin_workflow", { name });
}

/// Open the user-global `workflows/` folder in Finder.
export async function openWorkflowsDir(): Promise<void> {
  await invoke("open_workflows_dir");
}

/// The user-global workflows folder path, for the Settings display.
export async function workflowsDir(): Promise<string> {
  return await invoke<string>("workflows_dir");
}
