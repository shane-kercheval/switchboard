# Git branch comparison

## Goal

Give each expanded Git branch one aggregate, PR-style view of everything changed since it diverged from a comparison base. The row sits above uncommitted changes and commit history so a user can review the branch as one change set instead of reconstructing it commit by commit.

## User outcome

- **Branch changes** compares the selected branch with the merge base of its comparison branch, matching the shape normally reviewed in a pull request.
- A checked-out branch includes its committed, staged, unstaged, conflicted, and untracked changes. A branch without a worktree compares the merge base with its selected tip.
- The comparison lists every changed file with line counts. Selecting a file shows its structured inline diff, and the existing external-difftool action opens the same comparison endpoint.
- The comparison base defaults through the Git view's existing default-branch resolution and can be changed from the row menu for the current session.
- File and diff sections immediately clear their selection and show loading state when switching between large targets, while the Git tree and already-loaded commit history remain responsive.

## Comparison contract

The backend resolves and returns both `merge_base_oid` and `head_oid` with the file list. Follow-up inline reads and external-difftool launches validate that those committed objects still exist in the same tracked repository. For a worktree comparison, the worktree must still belong to that repository and remain checked out at `head_oid`.

The worktree is intentionally a **live overlay**, not a content snapshot. Staged, unstaged, and untracked files can change while `HEAD` remains fixed. A changed branch tip, mismatched worktree, or missing committed endpoint yields a distinct stale-comparison error; it must never be presented as a legitimate empty file diff. The UI asks the user to refresh.

## Loading and selection state

Commit ranges and the aggregate comparison load independently and publish as soon as each resolves. Default-target selection waits for both only when necessary and never replaces a target the user chose while loading.

Branch selection uses a lifecycle epoch, while commit and comparison reads use independent request generations. Collapse/reopen, refresh, and A→B→A base changes cannot allow an earlier request to publish into a later lifecycle.

Changing the comparison base preserves the last successful comparison until the replacement succeeds. On failure, the previous comparison and its actions remain valid and the attempted change reports an inline error. The successful base, comparison result, and selected comparison target update together.

## Backend implementation

- `switchboard-git` resolves the selected branch, comparison base, merge base, and file list with `git2`.
- Worktree comparisons use `diff_tree_to_workdir_with_index` with untracked recursion and rename detection; branch-only comparisons use `diff_tree_to_tree`.
- Conflicted deltas remain visible as modified files rather than being silently omitted.
- Per-file reads retain global rename detection but materialize and collect only the selected delta's patch. The existing binary, size-gate, line-cap, byte-cap, and truncation behavior remains unchanged.
- The Tauri command boundary validates both the tracked repository and any separately supplied worktree before reading data or launching `git difftool`.

## Frontend implementation

- `gitView.svelte.ts` owns branch-comparison state, request generations, base switching, and aggregate selection.
- `GitRepoNode.svelte` renders the aggregate row above uncommitted changes and commit ranges, reusing the existing tooltip and compact row-action primitives.
- `DiffPanel.svelte` keys file and diff loading to the complete target identity, retains prior DOM only as inert hidden content during the loading paint, and rejects late responses with request tokens.

## Known limitations

- Automatic base resolution follows the existing `origin/HEAD` → local `main` → local `master` convention. Fork workflows that should compare against an `upstream` remote must choose that base explicitly; there is no universal remote heuristic yet.
- The live worktree overlay is not an immutable list-to-file snapshot. Refresh is the reconciliation action when its committed endpoint expires.
- Global rename detection remains proportional to the comparison's delta set even though selected-file patch rendering no longer prints unrelated file content.

## Validation

- Rust integration tests cover merge-base rather than tip-to-tip semantics, explicit bases, unrelated histories, conflicted and untracked files, stale HEADs, foreign worktrees, missing endpoints, binary files, renames, truncation, and command-boundary enforcement.
- Frontend state tests cover independent commit publication, collapse/reopen ABA races, A→B→A base changes, fallback selection, and failed base changes preserving the last successful comparison.
- Component and real-WebKit tests cover row ordering and tooltip copy, immediate file/diff loading presentation, selection races, file-list geometry, and stable hover layout.
