/// Per-project compose drafts + recipient selection, persisted to localStorage
/// so a half-written message and the chosen agent chips survive both project
/// switches (the ComposeBar is remounted per project via `{#key}`) and app
/// restarts.
///
/// This is pre-durable UI ergonomics — the same category as the theme
/// preference, *not* conversation history. A draft is earlier than even a
/// queued send, which system-design §3 already classes as live-UI-only. It
/// therefore lives in machine-local localStorage rather than the git-tracked
/// `.switchboard/` project state: a half-typed message must not sync to a
/// teammate. localStorage is also origin-scoped, so `make dev DEV_PORT=…`
/// instances get isolated drafts for free.
///
/// Persistence contract: **mutations are synchronous; only persistence defers.**
/// `setContent`/`setSelection` update the in-memory `store` immediately (reads
/// are always current), then schedule one trailing debounced write so typing
/// doesn't pay a serialize+`localStorage.setItem` per keystroke. The debounced
/// write serializes the store *at fire time*, never a snapshot from schedule
/// time — so it cannot write stale state, which is what structurally prevents
/// one project's draft landing in another's slot on a fast project switch.
///
/// Deferring the write creates timing races; each is closed by an explicit
/// `flush()` point rather than by luck:
/// - **send-clear** (`persistContentNow` in ComposeBar): the cleared content is
///   written through immediately and any pending write cancelled, so a stale
///   pre-send draft can never land after the clear and resurrect sent text.
/// - **ComposeBar `onDestroy`**: a project switch remounts the bar (`{#key}`);
///   the outgoing bar flushes so its draft is durable before the next mounts.
/// - **`pagehide`/`beforeunload`** (registered once below): app quit
///   mid-debounce. Best-effort by design: whether either event is delivered
///   during Tauri webview teardown is deliberately unverified. If neither
///   fires, the loss is everything typed since the last ≥200 ms pause or
///   flush — a trailing debounce never fires during continuous typing, so
///   this is NOT bounded at 200 ms of keystrokes. Triggered only by quitting
///   within ~200 ms of the last keystroke; accepted because drafts are
///   ergonomic, not load-bearing, and the common exits (send-clear, project
///   switch) flush synchronously.
///
/// The persisted blob is versioned (`STORAGE_VERSION`) so the snapshot shape can
/// evolve without corrupting older drafts. An unversioned blob (the string-only
/// era) migrates in place: each entry's text becomes a plain-mode draft. Versioned
/// blobs share one envelope shape and every field added since v2 is optional, so a
/// v2 blob parses as a v3 snapshot with the newer fields simply absent.
///
/// **Attachments carry a backend obligation.** A staged attachment file is
/// reclaimed by the load-time GC unless the caller of `load_project_conversation`
/// declares it (see `draftAttachmentPaths`). Persisting a chip without passing its
/// path to that command restores a chip pointing at a deleted file.

import type {
  AgentId,
  Attachment,
  AttachmentKind,
  ProjectId,
  WorkflowInputValue,
} from "$lib/types";
import type { ForwardSource } from "$lib/state/heldForwards.svelte";

const STORAGE_KEY = "switchboard-compose";
const STORAGE_VERSION = 3;

/// The compose area's content, by mode — `kind` is the mode. Plain mode is the
/// normal textarea; prompt mode is the structured prompt composer (a chosen prompt
/// + its argument values + appended free text); workflow mode is a picked workflow
/// and its half-filled invocation form. Each is a distinct persisted state.
export type PlainContent = { kind: "plain"; draft: string };
export type PromptContent = {
  kind: "prompt";
  provider: string;
  name: string;
  args: Record<string, string>;
  appendedText: string;
};
/// `name` alone is not a workflow's identity — a built-in and a same-named copied
/// user workflow share a name — so `isBuiltin` is part of the saved key and both
/// are needed to re-resolve the listing on restore.
export type WorkflowContent = {
  kind: "workflow";
  name: string;
  isBuiltin: boolean;
  inputs: Record<string, WorkflowInputValue>;
};
export type ComposeContent = PlainContent | PromptContent | WorkflowContent;

/// Every forward-source family the composer can hold, in one field because they
/// are written together and are **mode-independent**: switching plain↔prompt↔workflow
/// hides the inapplicable ones but preserves them for the return trip, exactly as
/// `selectedIds` survives a mode switch.
export type ComposeForwards = {
  /// Message-level sources (plain mode).
  message: ForwardSource[];
  /// Prompt mode, keyed by argument name.
  promptArgs: Record<string, ForwardSource[]>;
  /// Prompt mode's appended-text field.
  promptAppended: ForwardSource[];
  /// Workflow mode, keyed by fillable field name.
  workflowFields: Record<string, ForwardSource[]>;
};

/// A project's compose snapshot. `selectedIds === undefined` means "no saved
/// selection — fall through to the default recipient"; an explicit `[]` means
/// "the user deliberately deselected everyone" and is honored on restore. Keep
/// this distinction deliberate — collapsing them loses deselect-all. `selectedIds`
/// is mode-independent (recipients persist across a plain↔prompt switch).
///
/// `attachments` and `forwards` are omitted entirely when empty, so an untouched
/// project's blob stays as small as it was before they existed.
export type ComposeSnapshot = {
  content: ComposeContent;
  selectedIds?: AgentId[];
  attachments?: Attachment[];
  forwards?: ComposeForwards;
};

function emptyPlain(): PlainContent {
  return { kind: "plain", draft: "" };
}

export function emptyForwards(): ComposeForwards {
  return { message: [], promptArgs: {}, promptAppended: [], workflowFields: {} };
}

function forwardsAreEmpty(forwards: ComposeForwards): boolean {
  return (
    forwards.message.length === 0 &&
    forwards.promptAppended.length === 0 &&
    Object.keys(forwards.promptArgs).length === 0 &&
    Object.keys(forwards.workflowFields).length === 0
  );
}

function isStringRecord(value: unknown): value is Record<string, string> {
  return (
    value !== null &&
    typeof value === "object" &&
    !Array.isArray(value) &&
    Object.values(value).every((v) => typeof v === "string")
  );
}

function isWorkflowInputValue(value: unknown): value is WorkflowInputValue {
  return (
    typeof value === "string" || (Array.isArray(value) && value.every((x) => typeof x === "string"))
  );
}

function parseWorkflowInputs(value: unknown): Record<string, WorkflowInputValue> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return {};
  const out: Record<string, WorkflowInputValue> = {};
  for (const [key, item] of Object.entries(value as Record<string, unknown>)) {
    if (isWorkflowInputValue(item)) out[key] = item;
  }
  return out;
}

/// Parse one persisted content blob, degrading anything malformed to an empty
/// plain draft rather than throwing (drafts are ergonomic, not load-bearing).
function parseContent(value: unknown): ComposeContent {
  if (value === null || typeof value !== "object") return emptyPlain();
  const v = value as Record<string, unknown>;
  if (v.kind === "prompt") {
    if (
      typeof v.provider === "string" &&
      typeof v.name === "string" &&
      isStringRecord(v.args) &&
      typeof v.appendedText === "string"
    ) {
      return {
        kind: "prompt",
        provider: v.provider,
        name: v.name,
        args: { ...v.args },
        appendedText: v.appendedText,
      };
    }
    return emptyPlain();
  }
  if (v.kind === "workflow") {
    if (typeof v.name === "string" && typeof v.isBuiltin === "boolean") {
      return {
        kind: "workflow",
        name: v.name,
        isBuiltin: v.isBuiltin,
        inputs: parseWorkflowInputs(v.inputs),
      };
    }
    return emptyPlain();
  }
  // Default/plain: tolerate a missing/non-string draft as empty.
  return { kind: "plain", draft: typeof v.draft === "string" ? v.draft : "" };
}

function parseSelectedIds(value: unknown): AgentId[] | undefined {
  return Array.isArray(value)
    ? value.filter((x): x is AgentId => typeof x === "string")
    : undefined;
}

const ATTACHMENT_KINDS: readonly AttachmentKind[] = ["image", "text", "file", "unknown"];

/// Attachments restore as-written. A chip whose staged file has since vanished is
/// dropped by the consumer at restore time, not here — this layer only rejects
/// structurally malformed entries.
function parseAttachments(value: unknown): Attachment[] | undefined {
  if (!Array.isArray(value)) return undefined;
  const out: Attachment[] = [];
  for (const item of value) {
    if (item === null || typeof item !== "object") continue;
    const a = item as Record<string, unknown>;
    if (
      typeof a.label !== "string" ||
      typeof a.path !== "string" ||
      typeof a.original_name !== "string"
    ) {
      continue;
    }
    // An unrecognized kind (written by a newer build) renders as a generic file
    // rather than dropping the chip — same cross-version posture as the wire type.
    const kind = ATTACHMENT_KINDS.includes(a.kind as AttachmentKind)
      ? (a.kind as AttachmentKind)
      : "unknown";
    out.push({ label: a.label, kind, path: a.path, original_name: a.original_name });
  }
  return out.length > 0 ? out : undefined;
}

function parseForwardSources(value: unknown): ForwardSource[] {
  if (!Array.isArray(value)) return [];
  const out: ForwardSource[] = [];
  for (const item of value) {
    if (item === null || typeof item !== "object") continue;
    const s = item as Record<string, unknown>;
    if (typeof s.id === "string" && typeof s.name === "string") {
      out.push({ id: s.id as AgentId, name: s.name });
    }
  }
  return out;
}

function parseForwardSourceMap(value: unknown): Record<string, ForwardSource[]> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return {};
  const out: Record<string, ForwardSource[]> = {};
  for (const [key, item] of Object.entries(value as Record<string, unknown>)) {
    const sources = parseForwardSources(item);
    if (sources.length > 0) out[key] = sources;
  }
  return out;
}

function parseForwards(value: unknown): ComposeForwards | undefined {
  if (value === null || typeof value !== "object") return undefined;
  const v = value as Record<string, unknown>;
  const forwards: ComposeForwards = {
    message: parseForwardSources(v.message),
    promptArgs: parseForwardSourceMap(v.promptArgs),
    promptAppended: parseForwardSources(v.promptAppended),
    workflowFields: parseForwardSourceMap(v.workflowFields),
  };
  return forwardsAreEmpty(forwards) ? undefined : forwards;
}

function parseSnapshot(value: unknown): ComposeSnapshot | null {
  if (value === null || typeof value !== "object") return null;
  const v = value as {
    content?: unknown;
    selectedIds?: unknown;
    attachments?: unknown;
    forwards?: unknown;
  };
  const snapshot: ComposeSnapshot = { content: parseContent(v.content) };
  const selectedIds = parseSelectedIds(v.selectedIds);
  if (selectedIds !== undefined) snapshot.selectedIds = selectedIds;
  const attachments = parseAttachments(v.attachments);
  if (attachments !== undefined) snapshot.attachments = attachments;
  const forwards = parseForwards(v.forwards);
  if (forwards !== undefined) snapshot.forwards = forwards;
  return snapshot;
}

/// Versioned blobs that `parseSnapshot` can read directly. v2 → v3 added only
/// optional fields (`attachments`, `forwards`) and one `content` variant, so a v2
/// snapshot parses as a v3 one with those absent — no per-version migration
/// function needed, and the next write re-stamps it as v3. Anything not listed
/// here is treated as the legacy unversioned shape.
const READABLE_VERSIONS: readonly number[] = [2, 3];

/// Migrate an unversioned (v1) blob: a flat `Record<ProjectId, { draft, selectedIds? }>`
/// where the text was a bare string. Each entry becomes a plain-mode snapshot.
function migrateUnversioned(parsed: Record<string, unknown>): Record<ProjectId, ComposeSnapshot> {
  const out: Record<ProjectId, ComposeSnapshot> = {};
  for (const [id, value] of Object.entries(parsed)) {
    if (value === null || typeof value !== "object") continue;
    const v = value as { draft?: unknown; selectedIds?: unknown };
    const draft = typeof v.draft === "string" ? v.draft : "";
    const selectedIds = parseSelectedIds(v.selectedIds);
    const content: PlainContent = { kind: "plain", draft };
    out[id] = selectedIds === undefined ? { content } : { content, selectedIds };
  }
  return out;
}

function readStored(): Record<ProjectId, ComposeSnapshot> {
  if (typeof localStorage === "undefined") return {};
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw === null) return {};
    const parsed: unknown = JSON.parse(raw);
    if (parsed === null || typeof parsed !== "object") return {};
    const envelope = parsed as { version?: unknown; projects?: unknown };
    if (typeof envelope.version === "number" && READABLE_VERSIONS.includes(envelope.version)) {
      if (envelope.projects === null || typeof envelope.projects !== "object") return {};
      const out: Record<ProjectId, ComposeSnapshot> = {};
      for (const [id, value] of Object.entries(envelope.projects as Record<string, unknown>)) {
        const snapshot = parseSnapshot(value);
        if (snapshot !== null) out[id] = snapshot;
      }
      return out;
    }
    // No recognized version → the legacy flat shape. Migrate it forward.
    return migrateUnversioned(parsed as Record<string, unknown>);
  } catch {
    return {};
  }
}

// Hydrated once at module load. Not reactive state — no component re-derives
// from it; the ComposeBar reads its snapshot once at construction and writes
// through on change.
const store: Record<ProjectId, ComposeSnapshot> = readStored();

const PERSIST_DEBOUNCE_MS = 200;

let pendingPersist: ReturnType<typeof setTimeout> | undefined;

/// Serialize the current store now, cancelling any pending debounced write
/// (the state it would have written is already covered by this one).
function persistNow(): void {
  if (pendingPersist !== undefined) {
    clearTimeout(pendingPersist);
    pendingPersist = undefined;
  }
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({ version: STORAGE_VERSION, projects: store }),
    );
  } catch {
    // Quota or serialization failure — drafts are ergonomic, not load-bearing,
    // so a failed persist degrades to in-memory-only rather than throwing.
  }
}

/// Trailing debounce: a burst of mutations (typing) pays one serialize+write
/// per pause instead of one per keystroke.
function schedulePersist(): void {
  if (pendingPersist !== undefined) clearTimeout(pendingPersist);
  pendingPersist = setTimeout(persistNow, PERSIST_DEBOUNCE_MS);
}

/// Run any pending debounced write immediately. Each caller is a flush point
/// closing a specific deferral race — see the module comment for the list.
/// The no-pending early return is load-bearing ("no pending ⇒ disk is
/// current") and holds only while every store mutator calls
/// `schedulePersist()` — a mutation path that skips it is un-flushable.
export function flush(): void {
  if (pendingPersist === undefined) return;
  persistNow();
}

// App quit mid-debounce. Both events are registered (delivery varies by
// engine/teardown path); the second flush is a no-op since the first cleared
// the pending write.
if (typeof window !== "undefined") {
  window.addEventListener("pagehide", flush);
  window.addEventListener("beforeunload", flush);
}

/// Current snapshot for a project; an empty plain draft when nothing is saved.
export function getCompose(projectId: ProjectId): ComposeSnapshot {
  return store[projectId] ?? { content: emptyPlain() };
}

/// Replace the compose content (plain or prompt). Recipient selection is left
/// untouched — it persists across a plain↔prompt mode switch.
export function setContent(projectId: ProjectId, content: ComposeContent): void {
  store[projectId] = { ...(store[projectId] ?? { content: emptyPlain() }), content };
  schedulePersist();
}

export function setSelection(projectId: ProjectId, selectedIds: AgentId[]): void {
  store[projectId] = { ...(store[projectId] ?? { content: emptyPlain() }), selectedIds };
  schedulePersist();
}

/// Replace the staged attachment chips. An empty list drops the key entirely so an
/// untouched project persists no `attachments` field.
export function setAttachments(projectId: ProjectId, attachments: Attachment[]): void {
  const next: ComposeSnapshot = { ...(store[projectId] ?? { content: emptyPlain() }) };
  if (attachments.length > 0) next.attachments = [...attachments];
  else delete next.attachments;
  store[projectId] = next;
  schedulePersist();
}

/// Replace every forward-source family at once — they are written together (one
/// composer effect owns all four) and are mode-independent. All-empty drops the key.
export function setForwards(projectId: ProjectId, forwards: ComposeForwards): void {
  const next: ComposeSnapshot = { ...(store[projectId] ?? { content: emptyPlain() }) };
  if (forwardsAreEmpty(forwards)) delete next.forwards;
  else next.forwards = forwards;
  store[projectId] = next;
  schedulePersist();
}

/// Staged paths this project's unsent draft still references. Pass to
/// `loadProjectConversation` — the backend GC deletes any staged attachment the
/// journal doesn't reference, and it cannot see a draft that lives here.
export function draftAttachmentPaths(projectId: ProjectId): string[] {
  return (store[projectId]?.attachments ?? []).map((a) => a.path);
}

/// Test-only API surface. Production hydrates once at module load; tests use
/// `reset` to isolate between cases and `reloadFromStorage` to exercise the
/// restart path (write localStorage, drop the in-memory copy, re-read).
export const _testing = {
  reset(): void {
    // Drop any pending debounced write too — a leftover timer from one test
    // must not fire mid-way through the next and write its store state.
    if (pendingPersist !== undefined) {
      clearTimeout(pendingPersist);
      pendingPersist = undefined;
    }
    for (const key of Object.keys(store)) delete store[key];
    if (typeof localStorage !== "undefined") localStorage.removeItem(STORAGE_KEY);
  },
  reloadFromStorage(): void {
    const next = readStored();
    for (const key of Object.keys(store)) delete store[key];
    Object.assign(store, next);
  },
};
