# Project management — rename, delete, archive, search

**Status:** Planned — not started.
**Branch:** lands as commits on the current M4 branch (`m4-dispatcher-contention-cancel`); per the "all of M4 is one PR" convention, sub-features here are commits, not separate review units. (Engineer's call if this grows large enough to warrant its own branch/PR — see "Sequencing" below.)

## Problem & motivation

Today a project, once created, is permanent and immutable from the UI. You can add a project (new or from an existing directory) and switch between projects, but you cannot **rename** one, **delete** one, **archive** one you're done with, or **search** when the list grows long. The only lifecycle escape hatch is `remove_directory`, which forgets an entire directory's worth of projects at once and leaves all on-disk state behind.

This plan adds basic per-project management — the operations a user reaches for once they have more than a handful of projects:

- **Rename** a project (same directory, new display name).
- **Delete** a project — remove *Switchboard's* state for that one project, never the working directory and never the harness's own session transcripts.
- **Archive** / unarchive a project, with a way to view archived projects without leaving the main list.
- **Search** the project list by name and directory.

These are deliberately "basic management." Cross-project **transcript/content search** is explicitly **out of scope** (see below).

## What we are NOT doing

- **Not** searching transcript/conversation content. The project list is in memory and trivially filterable; searching across every project's harness session files (`~/.claude/projects/...`, `~/.codex/...`) means reading and per-harness-parsing files that are not in memory, with no index — a separate, larger feature that earns its own milestone. In-project transcript filtering already exists in `UnifiedTranscript`; this plan does not touch it.
- **Not** deleting the working directory or any file outside `<directory>/.switchboard/projects/<id>/`. Delete never touches harness-native session files — those are harness-owned (system-design §3).
- **Not** bulk/multi-select operations. One project at a time.
- **Not** a separate archived "tab" or route. Archived projects fold into the same searchable list behind a toggle (rationale in "Decisions locked" below).
- **Not** soft-deleting on "delete." Delete is a hard, irreversible removal of Switchboard's project state. Archive is the reversible operation; delete is not.

## Related work (prerequisite, landed separately): conversation-merge fix

A separate, smaller change lands **before** this plan: the unified-transcript merge currently drops the harness file's user-role turns and sources the user's side **only** from the journal (`crates/app/src/commands.rs::merge_project_conversation`). For an **attached** session with history that predates the attach, those prompts were never journaled, so they vanish on restart even though they render correctly at attach time (the per-agent `hydrateAgent` path keeps them). The fix renders harness user turns for the pre-journaling region the merge already identifies (`turn_offset`), plus `docs/system-design.md` §3/§7 updates.

**Why it matters here:** it corrects the delete blast-radius story. Once user prompts survive restart from the harness file, deleting a project's `.switchboard/` state loses **only the journal-exclusive data — the outcome markers for failed/cancelled turns** (which harnesses persist nothing for). Prompts and completed agent content both live in harness files and are unaffected. The delete confirmation copy (M2) must reflect this corrected, honest scope.

## Required reading before implementing

- `docs/system-design.md` §3 (filesystem layout + split source-of-truth) and §4 (name uniqueness / canonicalization). The per-directory project-name uniqueness rule is load-bearing for rename validation.
- `crates/core/src/name.rs` — `validate_name`, `canonicalize_for_uniqueness`. **Project names use the identical rules as agent names**, scoped per-directory (`crates/core/src/directory.rs::create_project`).
- `crates/core/src/project.rs` — `ProjectConfig` (the canonical `config.yaml` shape), `Project`, `create_on_disk`, and the agent `rename_agent` / `remove_agent` methods (the precedent: read → mutate in memory → **full file rewrite**, not append).
- `crates/core/src/directory.rs` — `create_project`, `open_project`, `list_projects`, `projects_index_path`, `projects_dir`. The index `projects.jsonl` is append-only today; rename/archive/delete need a rewrite path here.
- `crates/core/src/io.rs` — `append_jsonl`, `write_jsonl`, the YAML read/write helpers, and their durability characteristics (note any `write_yaml` fsync gap before relying on it for `config.yaml` rewrites).
- `crates/app/src/commands.rs` — `create_project_impl`, `open_project_impl`, `remove_directory_impl` (the two-phase dispatcher-shutdown + state-prune pattern delete must mirror), `list_projects_impl`, the `ProjectListing` wire type, and the `registry_write` lock + `persist_workspace` flow.
- `crates/app/src/workspace.rs` and `state.rs` — `Workspace`/`DirectoryEntry` (cached `ProjectSummary` snapshots), `AppState.projects` / `project_locks` / `active_project_id`, `persist_workspace`, the documented lock order.
- `src/lib/components/AgentActionsMenu.svelte` — kebab menu + inline-confirm-destructive pattern (the model for the project menu).
- `src/lib/components/Sidebar.svelte` — the agent inline-rename editor (state machine, validation, Enter/save/Escape/blur, `focusSelect`). Reuse wholesale for project rename.
- `src/lib/components/ProjectsSidebar.svelte` — the project-row rendering, where the menu, search box, and archived toggle land.
- `src/lib/state/workspace.svelte.ts` — `projects.list`, `agentsByProject`, `activateProject`, and especially `removeDirectory`'s frontend teardown (the prune delete must reuse).
- `src/lib/agentName.ts` — `validateAgentName` / `normalizeAgentName` (the per-directory project validator generalizes from these).

## Cross-cutting conventions established here

Introduced in the earliest milestone that needs them and **reused, not reinvented**:

1. **`ProjectActionsMenu` is the single host for per-project actions (M1).** A kebab menu on each project row, mirroring `AgentActionsMenu`. Rename (M1), Delete (M2), and Archive/Unarchive (M3) are items on this one menu — later milestones add items, they do not add parallel triggers.
2. **Project metadata mutation lives in `core::Directory`, dual-writing both copies (M1).** Any operation that changes a project's persisted metadata (`rename`, `set_archived`) rewrites **both** the canonical `config.yaml` **and** the denormalized `projects.jsonl` index entry, in one core method. The app layer never hand-edits either file. This makes `projects.jsonl` rewrite-on-mutation, exactly as `registry.jsonl` already is for agents — update the "append-only" comments in core accordingly.
3. **App-layer project mutations follow the `create_project_impl` / `remove_directory_impl` shape (M1–M2).** Serialize under `registry_write`; update `AppState.projects`; refresh the workspace cache (`refresh_cache`) and `persist_workspace`. Destructive ops additionally drain the dispatcher first (the two-phase pattern from `remove_directory_impl`).
4. **The rendered project list derives from `projects.list` through one filter pipeline (M3–M4).** Archived-visibility and search are composed `$derived` filters over the same source array; rows never read project state from anywhere else.

## Decisions locked in discussion (do not re-litigate)

- **Delete is hard and irreversible; archive is the reversible path.** No soft-delete-on-delete, no trash.
- **Delete blast-radius:** remove `<directory>/.switchboard/projects/<id>/` (config, registry, journal, sessions, runs) and its `projects.jsonl` entry. Leave `.switchboard/` and the working directory intact. **Never** touch harness session files. Genuinely unrecoverable: the journal's failed/cancelled **outcome markers** only (prompts + completed content live in harness files).
- **Delete requires confirmation** (it's the one irreversible op) and the copy states honestly what is and isn't lost. Reuse `AgentActionsMenu`'s inline-confirm pattern (no separate dialog), consistent with the no-dialog philosophy.
- **Archive is display-only and does NOT stop running agents.** The project still exists and can be unarchived; archiving must not interfere with dispatch. (Contrast delete, which drains.)
- **Archived state is a plain `bool`**, not a timestamp. "Basic management"; a timestamp only earns its place if we later sort/show "archived N days ago."
- **Archived is a user-global *view-state*, stored in `workspace.yaml`, NOT an on-disk project flag (decision A — supersedes the original on-disk `ProjectConfig`/`ProjectSummary` plan).** Rationale: the only thing that *hides* a project (archive) must be reachable even when the project's directory is offline; an on-disk flag lives in the directory we can't write to while it's unavailable (catch-22), and since `.switchboard/projects/` is gitignored runtime state, an on-disk flag wouldn't travel across machines anyway. Storing archived in the user-global registry makes it offline-capable and per-install — consistent with how `workspace.yaml` already works. **Consequence:** archive doubles as the "remove an (even unavailable) project from my view without deleting data" lever the directory-keyed workspace otherwise lacks. The one trade-off (archived-ness is per-install, doesn't follow the project across machines) is accepted.
- **View-archived = single list + "Show archived" toggle**, default off, archived rows visually de-emphasized with an Unarchive action. Not a separate tab (fragments the list and breaks cross-list search), not a segmented Active/Archived/All control (excess chrome for a binary state).
- **Per-project menu (`ProjectActionsMenu`) availability gating.** Rename and Delete mutate on-disk state in the project's directory, so they require that directory to be **loaded/available**; they are disabled (and the backend errors defensively) on rows whose directory is unavailable. Archive/Unarchive is global (above), so it works regardless of availability. The menu renders its trigger only when ≥1 action is enabled: available rows always; **unavailable rows only once Archive exists (M3)** — through M1–M2 the menu is available-rows-only. Status spinner/checkmark stays always-visible; the kebab is hover/focus/menu-open-revealed at the row's far right.
- **Search is frontend-only**, case-insensitive substring over project name + directory basename, composed with the archived toggle.
- **Rename reuses the agent inline-rename UX verbatim:** inline editor (never a dialog), triggered from the menu *and* double-click; Enter/save-icon commits, Escape/blur cancels (never persist on blur); live validation (red border + tooltip + disabled commit) for empty/invalid/duplicate-within-directory excluding the project's own current name.

---

## Milestone 1 — Rename project

### Goal & Outcome

Introduce the per-project kebab menu and let the user rename a project in place.

- Each project row in the sidebar has a kebab (`⋯`) menu with a **Rename** item.
- Double-clicking a project's name, or choosing Rename, swaps the name to an inline input.
- Live validation blocks empty, malformed, or duplicate-within-the-same-directory names (excluding the project's own current name), with the same red-border + tooltip + disabled-commit treatment as agent rename.
- Enter or a save affordance commits; Escape or blur cancels without persisting.
- The new name persists across restart and updates everywhere the name renders (sidebar row, breadcrumb).

### Implementation Outline

- **Core (`Directory::rename_project(id, new_name) -> Result<ProjectSummary>`).** Mirror `Project::rename_agent`: `validate_name`, then per-directory canonicalized uniqueness against the other projects in this directory's index (excluding `id` so renaming to a case/hyphen variant of the current name is allowed). On success, **dual-write**: rewrite the project's `config.yaml` with the new `name`, and rewrite `projects.jsonl` with the updated summary entry (read all, replace the matching line, `write_jsonl`). Return the updated `ProjectSummary`. Add the rewrite to `projects.jsonl` here and revise the "append-only" doc comments on the index.
- **App (`rename_project_impl(state, project_id, new_name) -> Result<ProjectListing>`).** Under `registry_write` (synchronous — no dispatcher involvement; rename never touches running agents). Resolve the owning directory, call `Directory::rename_project`, update the in-memory `Project`'s config name in `AppState.projects`, `refresh_cache` + `persist_workspace`. Return the wire `ProjectListing` (so the frontend can replace the row without a full relist). Map `DuplicateProjectName` / `InvalidName` to the existing error-string boundary.
- **Frontend validator.** Generalize the agent name helper into a name validator that takes the candidate, the sibling set, and an exclude-id — projects validate against the **other projects in the same directory** (group `projects.list` by `directory`), agents keep their existing call. Either parameterize `validateAgentName` or add a thin `validateProjectName` sharing the core canonicalization rule; do not duplicate the rule.
- **`ProjectActionsMenu.svelte`.** New component modeled on `AgentActionsMenu`: kebab trigger on the row, `onRename` callback (the row owns the inline-edit state, the menu owns the trigger — same split as the agent card). This is the shared host M2/M3 extend.
- **`ProjectsSidebar.svelte`.** Add the menu to each row; lift the agent-rename inline-editor state machine (`editingProjectId`, `draftName`, `renaming`, `renameError`, derived validation/canSave, `startEdit`/`cancelEdit`/`commitEdit`/`onRenameKeydown`/`focusSelect`) to the project row. Double-click on the name enters edit. Backend rejection renders below the input.
- **`workspace.svelte.ts` (`renameProject(projectId, newName)`).** Call the API, then replace the entry in `projects.list` in place with the returned listing.

### Definition of Done

- Core unit tests mirroring the `rename_agent_*` set: persists new name to both `config.yaml` and `projects.jsonl`; own-name case/hyphen variant succeeds; canonical collision with another project in the same directory rejected; **same name allowed in a different directory**; invalid name rejected; nonexistent id → not found.
- App test: `rename_project_impl` updates state + cache and returns the listing; duplicate/invalid surface as errors; in-memory `Project.name()` reflects the change.
- Component tests (ProjectsSidebar): double-click and menu both enter edit; Enter/save commit; Escape/blur revert without persisting; duplicate-in-directory disables commit; empty suppresses message but disables commit; backend error keeps the editor open and shows the message; double-Enter commits once.
- Renders unchanged after restart (covered by the round-trip core test).

---

## Milestone 2 — Delete project

### Goal & Outcome

Let the user permanently remove one project's Switchboard state, with an honest confirmation.

- The project menu gains a **Delete** item; choosing it shows an inline confirm (not a dialog) with copy stating exactly what is removed and what is preserved.
- Confirming deletes `<directory>/.switchboard/projects/<id>/` and the project's `projects.jsonl` entry; the working directory, `.switchboard/` itself, sibling projects, and all harness session files are untouched.
- Any running agents in the project are stopped/drained first.
- The row disappears; if it was the active project, the view returns to the empty/no-project state. The deletion holds across restart.

### Implementation Outline

- **Core (`Directory::delete_project(id) -> Result<()>`).** Rewrite `projects.jsonl` dropping the entry, then recursively remove the project directory (`projects_dir().join(id)`). Order so a crash between steps is benign (prefer index-rewrite-then-rmdir: an orphan dir with no index entry is already a tolerated state per `create_project`'s atomicity note; a dir-less index entry would be a dangling listing). Idempotent on a missing project.
- **App (`delete_project_impl(state, project_id) -> Result<()>`).** Mirror `remove_directory_impl`'s two phases: **(a)** with no locks held, `shutdown_agent(..., CancelSource::Shutdown)` for every agent in the project and drain; **(b)** under `registry_write`: drop the project's `project_locks` handle (so the directory frees on all platforms before removal), call `Directory::delete_project`, remove the `Project` from `AppState.projects`, remove its agents from `agents_by_id`, clear `active_project_id` if it pointed here, `refresh_cache` + `persist_workspace`.
- **Frontend.** `ProjectActionsMenu` gains a Delete item using the inline-confirm sub-view pattern from `AgentActionsMenu` (`confirmingDelete`/`deleting`/`deleteError`, `closeOnSelect={false}`). Confirm copy, honest per the corrected blast-radius: e.g. "Delete '{name}' from Switchboard? This removes its conversation journal and agent list. Your files and each agent's own session history are kept." `workspace.svelte.ts` `deleteProject(projectId)` reuses `removeDirectory`'s teardown for the single project: remove from `projects.list`, drop `agentsByProject[projectId]`, unregister its agents' listeners, delete their conversations/runtimes, clear the project's hydration/load guards, and clear `selection.activeProjectId` if it matches.

### Definition of Done

- Core tests: deletes the project dir and drops only its index entry (siblings remain); harness session files are out of tree and provably untouched (assert nothing under a stubbed `~/.claude`-equivalent is removed — or document that delete only operates within `projects_dir`); idempotent on a re-delete.
- App tests: `delete_project_impl` drains agents, removes project + its agents from state, clears active project when applicable, persists; deleting a non-active project leaves the active one intact; the two-phase ordering does not deadlock (no `.await` under `registry_write`).
- Component tests: Delete shows the confirm sub-view; confirm calls through and removes the row; cancel restores the menu; backend failure surfaces and keeps the project.

---

## Milestone 3 — Archive + view archived

### Goal & Outcome

Let the user hide finished projects from the default list and view/restore them on demand.

- The project menu gains **Archive** (and **Unarchive** when archived).
- Archived projects are hidden from the list by default; a **Show archived** toggle in the Projects header includes them, visually de-emphasized.
- Archiving never interrupts a running agent.
- Archived state persists across restart.

**Storage: decision A — global view-state, not on-disk.** Archived lives in the user-global `workspace.yaml`, not in the project's `config.yaml`/`projects.jsonl`. This is what makes archive reachable for an *unavailable* project (we never touch the offline directory) and is the supported "remove from view without deleting" lever. There is **no on-disk schema change and no migration.**

### Implementation Outline

- **Workspace registry (`crate::workspace`).** Add a user-global archived-project set to `Workspace` (e.g. `archived: BTreeSet<ProjectId>`, `#[serde(default)]` so old `workspace.yaml` reads as empty — no version bump). Add `set_archived(id, archived) -> bool` / `is_archived(id) -> bool` (mutator reports whether the set changed, mirroring `refresh_cache`, so persist-on-change holds). Keyed by `ProjectId`; cleaned up on hard delete (M2) so a re-created id can't inherit a stale flag.
- **Wire type only.** Add `archived: bool` to the `ProjectListing` wire type (+ TS `ProjectListing`). `list_projects_impl` computes it per row from the workspace set, on **both** the fresh-read and cached paths (so unavailable rows still report their archived state). `ProjectConfig` / `ProjectSummary` are **unchanged**.
- **App (`set_project_archived_impl(state, project_id, archived) -> Result<()>`).** Acquire the `workspace` lock, flip the set, `persist_workspace`. **No** `registry_write`, **no** directory resolution, **no** dispatcher interaction, **no** on-disk project write — so it works whether or not the directory is loaded/available. Returns `()`; the frontend already holds the row and flips its `archived` flag locally (the next `list_projects` confirms it from the persisted set). Validates only that `project_id` is known to the workspace (present in some directory's cached snapshot or a loaded index) so a typo'd id can't silently poison the set.
- **Frontend.** `ProjectActionsMenu` shows Archive or Unarchive based on `project.archived`, **enabled regardless of availability** (and is now the reason the menu renders at all on unavailable rows). `workspace.svelte.ts` `setProjectArchived(projectId, archived)` calls through, then flips `archived` on the matching `projects.list` row in place. `ProjectsSidebar` header gets a "Show archived" toggle (`$state` local); the rendered list is a `$derived` filter — exclude archived unless the toggle is on; archived rows get a muted treatment + the Unarchive affordance.

### Definition of Done

- Workspace unit tests: `set_archived` round-trips through `workspace.yaml`; `is_archived` defaults to `false` for an unknown/absent id; old `workspace.yaml` (no `archived` field) loads as empty.
- App tests: `set_project_archived_impl` flips the flag and persists with no `registry_write`/dispatcher/disk-to-directory write; **archiving works for an unavailable project** (directory not loaded); `list_projects_impl` reports `archived` on both fresh and cached rows; a running agent in the project is unaffected.
- Component tests: archived rows hidden by default; toggle reveals them de-emphasized; Archive/Unarchive item reflects current state and calls through; the menu (with Archive) appears on unavailable rows while Rename/Delete stay disabled there.
- `ProjectListing` TS type updated; reducers/consumers default-safe on the new field.

---

## Milestone 4 — Search projects

### Goal & Outcome

Let the user filter the project list as it grows.

- A search input in the Projects header filters the list by case-insensitive substring over project name and directory basename.
- Search composes with the archived toggle (searching with the toggle on searches archived too).
- Clearing the query restores the full (toggle-respecting) list.

### Implementation Outline

- **Frontend only.** `projects.list` is already fully in memory. Add a search `$state` string in `ProjectsSidebar`; the rendered list becomes the existing archived-filter pipeline (M3) followed by a substring match on `name` and `basename(directory)`. No backend, no new types. Empty-result state ("No projects match.") distinct from the existing empty-list state.

### Definition of Done

- Component tests: query filters by name and by directory basename; case-insensitive; composes with the archived toggle; empty query restores the list; no-match state renders.

---

## Sequencing (engineer's call)

The four milestones are independent enough to land in any order after M1 (which introduces the shared menu); M4 has no backend and could land first if desired. Whether this whole plan rides in the current M4 PR as further commits, or becomes its own branch/PR, is the maintainer's call — it adds a new persistent field (`archived`) and the first on-disk project deletion, which is enough surface to justify its own review unit if the M4 PR is being kept minimal.

## Documentation to update

- `docs/system-design.md` §3 — note that `workspace.yaml` carries a user-global archived-projects set (archived is offline-capable *view-state*, per decision A, not an on-disk project flag) and that `projects.jsonl` is rewrite-on-mutation (like `registry.jsonl`, for rename/delete); confirm the delete blast-radius (Switchboard state only, never harness files).
- `docs/implementation_plans/2026-05-12-v1.md` — record project management as a directional addition (the v1 roadmap deferred only the *agent* context-menu reset/remove to M8; project-level lifecycle is new scope).
- `README.md` "Harness support and limitations" — only if a user-facing limitation emerges (none expected; delete/rename/archive are Switchboard-internal).
