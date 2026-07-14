<script lang="ts">
  /// One tracked repo in the Git view: a collapsible header (repo name/path,
  /// availability, per-repo refresh + fetch-failure indicator) over its branches.
  /// Branch filtering is applied by the parent and passed in, so this node is
  /// pure presentation over the data.
  import { onMount, tick } from "svelte";
  import { homeDir } from "@tauri-apps/api/path";
  import {
    Code2,
    Copy,
    FolderOpen,
    GitBranch,
    MoreHorizontal,
    Terminal,
    Trash2,
    TriangleAlert,
  } from "@lucide/svelte";
  import { cn, basename } from "$lib/utils";
  import { formatHomePath } from "$lib/utils";
  import Badge from "$lib/components/ui/Badge.svelte";
  import GitStatusIcon from "$lib/components/GitStatusIcon.svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import AsyncIconButton from "$lib/components/ui/AsyncIconButton.svelte";
  import DropdownMenu from "$lib/components/ui/DropdownMenu.svelte";
  import DropdownMenuItem from "$lib/components/ui/DropdownMenuItem.svelte";
  import { ICON_BUTTON_CLASS, ROW_ACTION_ICON_HOVER } from "$lib/components/ui/iconButton";
  import {
    localBranchIndicators,
    remoteBranchIndicators,
    remoteOnlyIndicator,
  } from "$lib/gitStatusIndicators";
  import type {
    BranchView,
    LinkedProject,
    RemoteBranchView,
    RepoListing,
    WorktreeView,
  } from "$lib/types";
  import type { CommitNavItem, FetchState } from "$lib/state/gitView.svelte";
  import {
    refreshRepo,
    fetchRepo,
    removeRepo,
    selectBranch,
    selectCommit,
    selectUncommitted,
    clearBranchSelection,
    branchSelection,
    branchCommits,
    diffTarget,
    navFocus,
    nextCommitSelection,
    hoverSuppressed,
    hoverableClass,
    setWorktreeMenuOpen,
    anyWorktreeMenuOpen,
    setViewMode,
  } from "$lib/state/gitView.svelte";
  import { activateProject } from "$lib/state/workspace.svelte";
  import { palette } from "$lib/state/commandPalette.svelte";
  import { isEditableShortcutTarget } from "$lib/keyboard";
  import { openInEditor, openInTerminal, revealInFinder } from "$lib/api";
  import { copyText } from "$lib/native";

  let {
    listing,
    branchFilter,
    showInactive,
    fetchState,
  }: {
    listing: RepoListing;
    branchFilter: "local" | "remote" | "both";
    showInactive: boolean;
    fetchState: FetchState | undefined;
  } = $props();

  let expanded = $state(true);
  let busy = $state(false);
  let homePath = $state<string | null>(null);
  let actionError = $state<string | null>(null);
  let openWorktreeActionsPath = $state<string | null>(null);
  let commitListEl = $state<HTMLDivElement | null>(null);

  const repo = $derived(listing.repo);
  const localBranchCount = $derived(repo.local_branches.length);

  // Row hover is suppressed right after a keyboard move so the mouse-hovered row
  // doesn't stay lit alongside the keyboard selection; it returns on pointer move.
  // `bg-hover`, not `bg-raised`: these rows sit directly on the raised card (the
  // branches drawer has no fill of its own), where `raised` would be invisible.
  const hoverBg = $derived(hoverableClass("hover:bg-hover"));

  // The on-hover actions trigger (`…`) is keyed on the real mouse `:hover`, so it
  // must be suppressed alongside the row background — otherwise it lingers under
  // the cursor during commit keyboard nav. Focus/open reveals stay.
  const triggerHoverReveal = $derived(hoverableClass("group-hover:opacity-100"));

  // The default branch anchors the branch list even when it has no local folder.
  // Other folderless branches stay hidden until the user asks for inactive ones.
  const localBranches = $derived(
    [...repo.local_branches]
      .filter((b) => {
        const isDefault = b.name === repo.default_branch;
        return branchFilter === "remote"
          ? b.upstream !== null
          : isDefault || showInactive || b.worktree !== null;
      })
      .sort((a, b) => {
        const aDefault = a.name === repo.default_branch ? 0 : 1;
        const bDefault = b.name === repo.default_branch ? 0 : 1;
        const aActive = a.worktree !== null ? 0 : 1;
        const bActive = b.worktree !== null ? 0 : 1;
        return aDefault - bDefault || aActive - bActive || a.name.localeCompare(b.name);
      }),
  );
  const remoteOnlyBranches = $derived(
    repo.remote_branches.filter(
      (remote) => !repo.local_branches.some((local) => local.upstream === remote.name),
    ),
  );
  const visibleRemoteOnlyBranches = $derived(branchFilter === "local" ? [] : remoteOnlyBranches);

  onMount(() => {
    void homeDir()
      .then((path) => {
        homePath = path;
      })
      .catch(() => {
        homePath = null;
      });
  });

  $effect(() => {
    if (
      openWorktreeActionsPath !== null &&
      !localBranches.some((branch) => branch.worktree?.path === openWorktreeActionsPath)
    ) {
      openWorktreeActionsPath = null;
    }
  });

  // Publish this node's open-menu state to the shared set so the commit
  // navigator yields to a worktree-actions menu open in ANY repo node, not only
  // this one. Self-healing: the cleanup clears this node's entry on close and on
  // unmount (idempotent, so order vs. a reset doesn't matter).
  $effect(() => {
    setWorktreeMenuOpen(repo.root, openWorktreeActionsPath !== null);
    return () => setWorktreeMenuOpen(repo.root, false);
  });

  function displayPath(path: string): string {
    return formatHomePath(path, homePath);
  }

  function linkedProjects(worktreePath: string) {
    return listing.linked_projects[worktreePath] ?? [];
  }

  async function onRefresh(): Promise<void> {
    busy = true;
    try {
      await refreshRepo(repo.root);
      await fetchRepo(repo.root);
    } finally {
      busy = false;
    }
  }

  function runAction(action: Promise<void>): void {
    actionError = null;
    void action.catch((e: unknown) => {
      actionError = e instanceof Error ? e.message : String(e);
      console.error("[switchboard] git view open action failed", e);
    });
  }

  function onHeaderActionError(error: unknown): void {
    actionError = error instanceof Error ? error.message : String(error);
    console.error("[switchboard] git view open action failed", error);
  }

  async function openLinkedProject(project: LinkedProject): Promise<void> {
    setViewMode("projects");
    await activateProject(project.id);
  }

  function branchHasChanges(branch: BranchView): boolean {
    return branch.worktree !== null && (branch.worktree.dirty || branch.worktree.untracked);
  }

  // Selection predicates read the shared stores so a row highlights when it (or
  // its commit / uncommitted entry) is the active selection.
  function isLocalSelected(name: string): boolean {
    const s = branchSelection.current;
    return s !== null && s.repoRoot === repo.root && s.kind === "local" && s.name === name;
  }
  function isRemoteSelected(name: string): boolean {
    const s = branchSelection.current;
    return s !== null && s.repoRoot === repo.root && s.kind === "remote" && s.name === name;
  }
  function isCommitSelected(oid: string): boolean {
    const t = diffTarget.current;
    return t !== null && t.kind === "commit" && t.oid === oid;
  }
  function isUncommittedSelected(path: string): boolean {
    const t = diffTarget.current;
    return t !== null && t.kind === "uncommitted" && t.worktreePath === path;
  }

  function onSelectLocal(branch: BranchView): void {
    void selectBranch(
      { repoRoot: repo.root, kind: "local", name: branch.name },
      {
        worktreePath: branch.worktree?.path ?? null,
        hasChanges: branchHasChanges(branch),
        worktreeSubtitle: branch.worktree ? displayPath(branch.worktree.path) : "",
      },
    );
  }
  function onSelectRemote(name: string): void {
    void selectBranch(
      { repoRoot: repo.root, kind: "remote", name },
      { worktreePath: null, hasChanges: false, worktreeSubtitle: "" },
    );
  }
  // A detached worktree has no branch (so no commit history): selecting it just
  // shows its uncommitted changes, collapsing any expanded branch.
  function onSelectDetached(wt: WorktreeView): void {
    clearBranchSelection();
    selectUncommitted(repo.root, wt.path, displayPath(wt.path));
  }

  // The local branch this node currently owns the selection for, or null (the
  // selection is elsewhere, or it's a remote branch with no worktree).
  function selectedLocalBranch(): BranchView | null {
    const s = branchSelection.current;
    if (s === null || s.repoRoot !== repo.root || s.kind !== "local") return null;
    return repo.local_branches.find((b) => b.name === s.name) ?? null;
  }

  // The rendered commit list is capped until the user asks for the rest: it's
  // a navigation aid, not a `git log` dump, and the backend's 50-commit read
  // buried every other repo card under one branch's history. Expansion is
  // keyed to the loaded ref, so selecting another branch re-collapses.
  const COMMIT_PREVIEW_COUNT = 15;

  let allCommitsShownFor = $state<string | null>(null);

  const commitsRefKey = $derived(
    branchCommits.ref === null
      ? null
      : `${branchCommits.ref.repoRoot}\n${branchCommits.ref.kind}\n${branchCommits.ref.name}`,
  );

  /// The ranges as rendered. Only the `recent` (local history) range is
  /// previewed — `incoming` is the highest-signal set in the list (what a pull
  /// brings) and usually small, so it always renders in full rather than being
  /// buried behind the history's "Show more". A capped range drops its own
  /// `truncated` note (the Show-more row supersedes it until expansion) and
  /// carries `hidden` so the row renders inside that range's section.
  const cappedCommits = $derived.by(() => {
    const expanded = commitsRefKey !== null && allCommitsShownFor === commitsRefKey;
    return branchCommits.ranges.map((range) => {
      if (expanded || range.kind !== "recent" || range.commits.length <= COMMIT_PREVIEW_COUNT) {
        return { range, hidden: 0 };
      }
      return {
        range: {
          ...range,
          commits: range.commits.slice(0, COMMIT_PREVIEW_COUNT),
          truncated: false,
        },
        hidden: range.commits.length - COMMIT_PREVIEW_COUNT,
      };
    });
  });

  // The commit pane's navigable entries in display order: the uncommitted row
  // (when the selected branch's worktree is dirty) above the commits, matching
  // what `commitList` renders — including the preview cap, so arrow keys never
  // land on a commit the list isn't showing.
  function commitNavItems(): CommitNavItem[] {
    const items: CommitNavItem[] = [];
    const branch = selectedLocalBranch();
    if (branch?.worktree != null && branchHasChanges(branch)) {
      items.push({ kind: "uncommitted", worktreePath: branch.worktree.path });
    }
    for (const entry of cappedCommits) {
      for (const commit of entry.range.commits) items.push({ kind: "commit", commit });
    }
    return items;
  }

  function moveCommitSelection(delta: number): void {
    const next = nextCommitSelection(commitNavItems(), diffTarget.current, delta);
    if (next === null) return;
    if (next.kind === "uncommitted") {
      selectUncommitted(repo.root, next.worktreePath, displayPath(next.worktreePath));
    } else {
      selectCommit(repo.root, next.commit);
    }
    // Within the commit list, only the active commit or the uncommitted row
    // carries `data-selected="true"` (the selected branch row, which also does,
    // sits outside this container) — so this needs no test-hook selector.
    void tick().then(() => {
      commitListEl?.querySelector('[data-selected="true"]')?.scrollIntoView({ block: "nearest" });
    });
  }

  // Arrow up/down navigate the commit pane when it's the focused selection (the
  // user last picked a commit or the uncommitted row). Only the node that owns
  // the selection acts, so the several mounted nodes don't all respond; an open
  // overlay (command palette, or a worktree-actions menu open in any repo node)
  // or an event a closer handler already consumed yields the keys to that owner.
  function onCommitNavKeydown(event: KeyboardEvent): void {
    if (navFocus.pane !== "commits") return;
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    if (event.metaKey || event.ctrlKey || event.altKey) return;
    if (event.defaultPrevented || palette.open || anyWorktreeMenuOpen()) return;
    if (branchSelection.current?.repoRoot !== repo.root) return;
    if (isEditableShortcutTarget(event.target)) return;
    event.preventDefault();
    hoverSuppressed.value = true;
    moveCommitSelection(event.key === "ArrowDown" ? 1 : -1);
  }

  $effect(() => {
    window.addEventListener("keydown", onCommitNavKeydown);
    return () => window.removeEventListener("keydown", onCommitNavKeydown);
  });

  // Compact fixed-width-ish format keeps dense commit rows scannable.
  function compactCommitTimestamp(commit: {
    authored_at: string | null;
    short_oid: string;
  }): string {
    if (commit.authored_at === null) return commit.short_oid;
    const date = new Date(commit.authored_at);
    if (Number.isNaN(date.getTime())) return commit.short_oid;
    const pad = (n: number): string => String(n).padStart(2, "0");
    const monthDay = `${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
    const time = `${pad(date.getHours())}:${pad(date.getMinutes())}`;
    return date.getFullYear() === new Date().getFullYear()
      ? `${monthDay} ${time}`
      : `${date.getFullYear()}-${monthDay} ${time}`;
  }
</script>

<!-- A flat section, not a card: the repo header is the anchor and the
     branches/commits hang beneath it on left rules, like the transcript's
     rows. The box the card border drew is now drawn by hierarchy alone. -->
<div class="min-h-0 shrink-0" data-testid="git-repo" data-repo-root={repo.root}>
  <div class="group flex min-h-10 items-center gap-2 px-1 py-1">
    <button
      type="button"
      class="text-muted hover:bg-hover hover:text-fg flex h-[26px] w-[26px] shrink-0 items-center justify-center rounded-full transition-colors"
      aria-label={expanded ? "Collapse repo" : "Expand repo"}
      aria-expanded={expanded}
      onclick={() => (expanded = !expanded)}
    >
      <svg
        viewBox="0 0 24 24"
        class={cn("h-3.5 w-3.5 transition-transform", expanded && "rotate-90")}
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <path d="m9 6 6 6-6 6" />
      </svg>
    </button>

    <div class="flex min-w-0 flex-1 items-baseline gap-1.5">
      <span class="text-fg max-w-[65%] shrink-0 truncate text-sm font-semibold" title={repo.root}
        >{repo.name}</span
      >
      <span class="flex max-w-full min-w-0 flex-1 items-baseline gap-1.5 overflow-hidden">
        <span class="text-border shrink-0 text-[11px]">/</span>
        <span class="text-muted min-w-0 truncate font-mono text-[11px] leading-4" title={repo.root}>
          {displayPath(repo.root)}
        </span>
      </span>
      {#if repo.is_bare}
        <Badge testid="repo-bare" class="shrink-0">bare</Badge>
      {/if}
      {#if !repo.available}
        <Badge testid="repo-unavailable" class="bg-warning-soft text-warning shrink-0">
          unavailable
        </Badge>
      {/if}
    </div>

    <div class="flex shrink-0 items-center gap-0.5">
      {#if fetchState?.kind === "failed"}
        <Tooltip side="top" delayDuration={0} skipDelayDuration={0} disableHoverableContent>
          {#snippet trigger(props)}
            <span
              {...props}
              class="text-warning hover:bg-hover inline-flex h-[26px] w-[26px] items-center justify-center rounded-full transition-colors"
              aria-label="Fetch failed"
              data-testid="repo-fetch-failed"
            >
              <TriangleAlert size={14} strokeWidth={1.8} aria-hidden="true" />
            </span>
          {/snippet}

          <div class="max-w-56">
            <div class="text-[13px] leading-4 font-medium">Fetch failed</div>
            <div class="text-primary-fg/70 mt-1 text-xs leading-4">
              Remote branches could not be refreshed. Try refreshing this repository.
            </div>
          </div>
        </Tooltip>
      {/if}
      <div
        class={cn(
          "flex shrink-0 items-center gap-0.5 overflow-hidden transition-[max-width,opacity]",
          busy
            ? "pointer-events-auto max-w-[140px] opacity-100"
            : "pointer-events-none max-w-0 opacity-0 group-focus-within:pointer-events-auto group-focus-within:max-w-[140px] group-focus-within:opacity-100 group-hover:pointer-events-auto group-hover:max-w-[140px] group-hover:opacity-100",
        )}
      >
        <button
          type="button"
          class={cn(ICON_BUTTON_CLASS, ROW_ACTION_ICON_HOVER, "shrink-0 disabled:opacity-50")}
          aria-label="Refresh repo"
          data-testid="repo-refresh"
          disabled={busy}
          onclick={onRefresh}
        >
          {#if busy}
            <Spinner class="h-4 w-4" />
          {:else}
            <svg
              viewBox="0 0 24 24"
              class="h-4 w-4"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <path d="M21 12a9 9 0 1 1-2.64-6.36M21 3v6h-6" />
            </svg>
          {/if}
        </button>

        <Tooltip label="Reveal in Finder" side="top">
          {#snippet trigger(props)}
            <AsyncIconButton
              {...props}
              class={cn(ICON_BUTTON_CLASS, ROW_ACTION_ICON_HOVER, "shrink-0")}
              label="Reveal repo in Finder"
              testid="repo-action-reveal"
              completeAfterMs={700}
              action={() => {
                actionError = null;
                return revealInFinder(repo.root);
              }}
              onError={onHeaderActionError}
            >
              <FolderOpen size={14} strokeWidth={1.8} aria-hidden="true" />
            </AsyncIconButton>
          {/snippet}
        </Tooltip>

        <Tooltip label="Open in editor" side="top">
          {#snippet trigger(props)}
            <AsyncIconButton
              {...props}
              class={cn(ICON_BUTTON_CLASS, ROW_ACTION_ICON_HOVER, "shrink-0")}
              label="Open repo in editor"
              testid="repo-action-editor"
              completeAfterMs={700}
              action={() => {
                actionError = null;
                return openInEditor(repo.root);
              }}
              onError={onHeaderActionError}
            >
              <Code2 size={14} strokeWidth={1.8} aria-hidden="true" />
            </AsyncIconButton>
          {/snippet}
        </Tooltip>

        <Tooltip label="Copy path" side="top">
          {#snippet trigger(props)}
            <AsyncIconButton
              {...props}
              class={cn(ICON_BUTTON_CLASS, ROW_ACTION_ICON_HOVER, "shrink-0")}
              label="Copy repo path"
              testid="repo-action-copy-path"
              action={() => {
                actionError = null;
                return copyText(repo.root);
              }}
              onError={onHeaderActionError}
            >
              <Copy size={14} strokeWidth={1.8} aria-hidden="true" />
            </AsyncIconButton>
          {/snippet}
        </Tooltip>

        <Tooltip label="Remove from view" side="top">
          {#snippet trigger(props)}
            <button
              {...props}
              type="button"
              class={cn(
                ICON_BUTTON_CLASS,
                "hover:bg-status-failed-soft hover:text-status-failed shrink-0",
              )}
              aria-label="Remove repo from view"
              data-testid="repo-action-remove"
              onclick={() => runAction(removeRepo(repo.root))}
            >
              <Trash2 size={14} strokeWidth={1.8} aria-hidden="true" />
            </button>
          {/snippet}
        </Tooltip>
      </div>
    </div>
  </div>

  {#if actionError}
    <p
      class="border-border/60 text-status-failed border-t px-3 py-1.5 text-xs"
      data-testid="repo-action-error"
    >
      {actionError}
    </p>
  {/if}

  {#if expanded}
    <!-- The drawer hangs on a left rule instead of carrying a third neutral
         fill (list → card → drawer was three stacked grays) — same idiom as
         the commit list below it and the transcript's expanded tool rows. -->
    <div
      class="border-border/70 mt-1 mb-1.5 ml-4 border-l py-0.5 pr-2 pl-2"
      data-testid="repo-branches"
    >
      {#if !repo.available}
        <p class="text-muted px-2 py-2 text-xs">
          This repository is unavailable (moved or unmounted).
        </p>
      {:else}
        {#each localBranches as branch (branch.name)}
          {@const selected = isLocalSelected(branch.name)}
          {@const worktreePath = branch.worktree?.path ?? null}
          {@const actionsOpen = worktreePath !== null && openWorktreeActionsPath === worktreePath}
          <div
            class={cn(
              "group flex min-h-8 items-center gap-2 rounded-md px-2 py-1.5 transition-colors",
              branch.worktree === null && "opacity-60",
              selected || actionsOpen ? "bg-selected" : hoverBg,
            )}
            data-testid="git-branch"
            data-selected={selected || actionsOpen}
            data-branch={branch.name}
            data-actions-open={actionsOpen}
          >
            <!-- Clicking the row selects the branch: it expands to show commits
                 and the panel opens on a default target. The actions menu is a
                 sibling button so the two clicks don't nest. -->
            <button
              type="button"
              class="flex min-w-0 flex-1 items-center gap-2 text-left"
              data-testid="branch-select"
              data-selected={selected}
              onclick={() => onSelectLocal(branch)}
            >
              {@render branchInner(branch)}
            </button>
            {#if worktreePath !== null}
              <DropdownMenu
                open={actionsOpen}
                onOpenChange={(open) => {
                  openWorktreeActionsPath = open
                    ? worktreePath
                    : openWorktreeActionsPath === worktreePath
                      ? null
                      : openWorktreeActionsPath;
                }}
                triggerLabel={`Actions for ${branch.name}`}
                triggerTestid="worktree-actions-trigger"
                triggerClass={cn(
                  ICON_BUTTON_CLASS,
                  "shrink-0 opacity-0 group-focus-within:opacity-100 data-[state=open]:opacity-100",
                  triggerHoverReveal,
                  // Stronger gray hover, overridden to the white `bg-raised` fill
                  // on a selected (blue) row so it reads against the blue. Driven
                  // off the row's `data-selected` (a `group-data-` CSS variant),
                  // not a JS class — the trigger lives in a `{#snippet}` that
                  // doesn't re-render when the row's selected state changes.
                  ROW_ACTION_ICON_HOVER,
                  actionsOpen && "opacity-100",
                )}
                contentTestid="worktree-actions-menu"
              >
                {#snippet trigger()}
                  <MoreHorizontal size={14} strokeWidth={1.8} aria-hidden="true" />
                {/snippet}
                <DropdownMenuItem
                  onSelect={() => runAction(openInEditor(worktreePath))}
                  class="gap-2"
                  data-testid="worktree-action-editor"
                >
                  <Code2
                    size={14}
                    strokeWidth={1.8}
                    class="text-muted shrink-0"
                    aria-hidden="true"
                  />
                  Open in editor
                </DropdownMenuItem>
                <DropdownMenuItem
                  onSelect={() => runAction(openInTerminal(worktreePath))}
                  class="gap-2"
                  data-testid="worktree-action-terminal"
                >
                  <Terminal
                    size={14}
                    strokeWidth={1.8}
                    class="text-muted shrink-0"
                    aria-hidden="true"
                  />
                  Open in terminal
                </DropdownMenuItem>
                <DropdownMenuItem
                  onSelect={() => runAction(revealInFinder(worktreePath))}
                  class="gap-2"
                  data-testid="worktree-action-reveal"
                >
                  <FolderOpen
                    size={14}
                    strokeWidth={1.8}
                    class="text-muted shrink-0"
                    aria-hidden="true"
                  />
                  Reveal in Finder
                </DropdownMenuItem>
                <DropdownMenuItem
                  onSelect={() => runAction(copyText(worktreePath))}
                  class="gap-2"
                  data-testid="worktree-action-copy-path"
                >
                  <Copy
                    size={14}
                    strokeWidth={1.8}
                    class="text-muted shrink-0"
                    aria-hidden="true"
                  />
                  Copy path
                </DropdownMenuItem>
                <DropdownMenuItem
                  onSelect={() => runAction(copyText(branch.name))}
                  class="gap-2"
                  data-testid="worktree-action-copy-branch"
                >
                  <GitBranch
                    size={14}
                    strokeWidth={1.8}
                    class="text-muted shrink-0"
                    aria-hidden="true"
                  />
                  Copy branch name
                </DropdownMenuItem>
                {#each linkedProjects(worktreePath) as project (project.id)}
                  <DropdownMenuItem
                    onSelect={() => runAction(openLinkedProject(project))}
                    class="gap-2"
                    data-testid="worktree-action-open-project"
                    title={project.name}
                  >
                    <FolderOpen
                      size={14}
                      strokeWidth={1.8}
                      class="text-muted shrink-0"
                      aria-hidden="true"
                    />
                    <span class="min-w-0 truncate">Open Project: {project.name}</span>
                  </DropdownMenuItem>
                {/each}
              </DropdownMenu>
            {/if}
          </div>
          {#if selected}
            {@render commitList(branch.worktree?.path ?? null, branchHasChanges(branch))}
          {/if}
        {/each}

        {#each visibleRemoteOnlyBranches as branch (branch.name)}
          {@const selected = isRemoteSelected(branch.name)}
          <div
            class={cn(
              "flex min-h-8 items-center gap-2 rounded-md px-2 py-1.5 opacity-80 transition-colors",
              selected ? "bg-selected" : hoverBg,
            )}
            data-testid="git-remote-branch"
            data-branch={branch.name}
          >
            <button
              type="button"
              class="flex min-w-0 flex-1 items-center gap-2 text-left"
              data-testid="branch-select"
              data-selected={selected}
              onclick={() => onSelectRemote(branch.name)}
            >
              {@render remoteInner(branch)}
            </button>
          </div>
          {#if selected}
            {@render commitList(null, false)}
          {/if}
        {/each}

        {#if localBranches.length === 0 && visibleRemoteOnlyBranches.length === 0}
          <p class="text-muted px-2 py-1.5 text-xs">
            {#if branchFilter === "remote"}
              No remote branches.
            {:else if showInactive && localBranchCount === 0}
              No local branches.
            {:else}
              No branches with local folders.
            {/if}
          </p>
        {/if}

        {#if branchFilter !== "remote"}
          {#each repo.detached_worktrees as wt (wt.path)}
            {@const dselected = isUncommittedSelected(wt.path)}
            <div
              class={cn(
                "flex min-h-8 items-center gap-2 rounded-md px-2 py-1.5 transition-colors",
                dselected ? "bg-selected" : wt.warning !== "prunable" && hoverBg,
              )}
              data-testid="git-detached-worktree"
            >
              {#if wt.warning === "prunable"}
                <!-- Directory is gone — nothing to inspect, so not openable. -->
                <div class="flex min-w-0 flex-1 items-center gap-2">
                  {@render detachedInner(wt)}
                </div>
              {:else}
                <button
                  type="button"
                  class="flex min-w-0 flex-1 items-center gap-2 text-left"
                  data-testid="worktree-select"
                  data-selected={dselected}
                  onclick={() => onSelectDetached(wt)}
                >
                  {@render detachedInner(wt)}
                </button>
              {/if}
            </div>
          {/each}
        {/if}
      {/if}
    </div>
  {/if}
</div>

{#snippet commitList(worktreePath: string | null, hasChanges: boolean)}
  <div
    bind:this={commitListEl}
    class="border-border mt-1 mb-1 ml-4 border-l pl-2"
    data-testid="commit-list"
  >
    {#if worktreePath !== null && hasChanges}
      {@const uSel = isUncommittedSelected(worktreePath)}
      <button
        type="button"
        class={cn(
          "flex w-full items-center gap-2 rounded-md px-2 py-1 text-left text-xs transition-colors",
          uSel ? "bg-selected text-fg" : cn("text-muted", hoverBg),
        )}
        data-testid="uncommitted-row"
        data-selected={uSel}
        onclick={() => selectUncommitted(repo.root, worktreePath, displayPath(worktreePath))}
      >
        <span class="text-warning shrink-0" aria-hidden="true">●</span>
        <span class="text-fg min-w-0 flex-1 truncate">Uncommitted changes</span>
      </button>
    {/if}

    {#if branchCommits.status === "loading"}
      <div class="text-muted flex items-center gap-2 px-2 py-1.5 text-xs">
        <Spinner class="h-3.5 w-3.5" /> Loading commits…
      </div>
    {:else if branchCommits.status === "failed"}
      <p class="text-muted px-2 py-1.5 text-xs">Couldn't load commits.</p>
    {:else if branchCommits.ranges.every((range) => range.commits.length === 0)}
      <p class="text-muted px-2 py-1.5 text-xs">No commits.</p>
    {:else}
      {@const hasBranchWork = branchCommits.ranges.some((range) =>
        range.commits.some((commit) => commit.branch_work),
      )}
      {@const hasUnpushed = branchCommits.ranges.some((range) =>
        range.commits.some((commit) => commit.unpushed),
      )}
      {#each cappedCommits as entry (entry.range.kind)}
        {@const range = entry.range}
        {#if range.commits.length > 0}
          <div
            class="text-muted/80 px-2 pt-1.5 pb-0.5 text-[10px] font-semibold tracking-wide uppercase"
          >
            {range.label}
          </div>
          {#each range.commits as commit (commit.oid)}
            {@const cSel = isCommitSelected(commit.oid)}
            <button
              type="button"
              class={cn(
                "flex w-full items-center gap-1.5 rounded-md px-2 py-1 text-left text-xs transition-colors",
                cSel ? "bg-selected text-fg" : cn("text-muted", hoverBg),
              )}
              data-testid="commit-row"
              data-oid={commit.oid}
              data-selected={cSel}
              onclick={() => selectCommit(repo.root, commit)}
            >
              {#if hasUnpushed || hasBranchWork}
                {@render commitDot(commit.unpushed, commit.branch_work)}
              {/if}
              <span class="text-muted shrink-0 font-mono text-[11px]" title={commit.short_oid}>
                {compactCommitTimestamp(commit)}
              </span>
              <!-- The subject is the row's payload — promoted to `text-fg` so
                   the list reads as scannable subjects with recessed
                   timestamps, not a uniform gray wall. Selection color still
                   wins (the row's own `text-fg` when selected). -->
              <span class="text-fg min-w-0 flex-1 truncate" title={commit.subject}
                >{commit.subject}</span
              >
            </button>
          {/each}
          {#if range.truncated}
            <p class="text-muted/70 px-2 py-1 text-[10px]">…older commits not shown</p>
          {/if}
          {#if entry.hidden > 0}
            <button
              type="button"
              class="text-muted hover:text-fg px-2 py-1 text-[11px] transition-colors hover:underline"
              data-testid="commit-show-more"
              onclick={() => (allCommitsShownFor = commitsRefKey)}
            >
              Show {entry.hidden} more
            </button>
          {/if}
        {/if}
      {/each}
    {/if}
  </div>
{/snippet}

{#snippet commitDot(unpushed: boolean, branchWork: boolean)}
  <!-- One dot per commit. Unpushed (amber) takes precedence over branch work
       (black): an unpushed commit shows only the amber dot, never both. The
       fixed-width slot keeps the timestamp column aligned whether or not a dot
       renders. -->
  <span class="inline-flex h-4 w-2 shrink-0 items-center justify-center">
    {#if unpushed}
      <Tooltip side="top" delayDuration={0} skipDelayDuration={0} disableHoverableContent>
        {#snippet trigger(props)}
          <span
            {...props}
            class="inline-flex h-4 w-2 items-center justify-center"
            aria-label="Not pushed"
            data-testid="unpushed-indicator"
          >
            <span class="bg-warning h-1.5 w-1.5 rounded-full" aria-hidden="true"></span>
          </span>
        {/snippet}

        <div class="max-w-56">
          <div class="text-[13px] leading-4 font-medium">Not pushed</div>
          <div class="text-primary-fg/70 mt-1 text-xs leading-4">
            This commit isn't on the upstream branch yet.
          </div>
        </div>
      </Tooltip>
    {:else if branchWork}
      <Tooltip side="top" delayDuration={0} skipDelayDuration={0} disableHoverableContent>
        {#snippet trigger(props)}
          <span
            {...props}
            class="inline-flex h-4 w-2 items-center justify-center"
            aria-label="Branch work"
            data-testid="branch-work-indicator"
          >
            <span class="bg-primary h-1.5 w-1.5 rounded-full" aria-hidden="true"></span>
          </span>
        {/snippet}

        <div class="max-w-56">
          <div class="text-[13px] leading-4 font-medium">Branch work</div>
          <div class="text-primary-fg/70 mt-1 text-xs leading-4">
            This commit is unique work on this branch.
          </div>
        </div>
      </Tooltip>
    {/if}
  </span>
{/snippet}

{#snippet remoteInner(branch: RemoteBranchView)}
  <div class="min-w-0 flex-1">
    <div class="flex min-w-0 items-center gap-1.5">
      <span class="text-muted truncate text-[13px] leading-5" title={branch.name}>
        {branch.name}
      </span>
      <div class="flex shrink-0 items-center gap-1">
        <GitStatusIcon indicator={remoteOnlyIndicator(branch.name)} />
      </div>
    </div>
    <div class="text-muted truncate text-[11px] leading-4">No local folder</div>
  </div>
  <div class="flex shrink-0 items-center gap-1">
    {#each remoteBranchIndicators(branch, repo.default_branch) as indicator (indicator.key)}
      <GitStatusIcon {indicator} />
    {/each}
  </div>
{/snippet}

{#snippet branchInner(branch: BranchView)}
  <div class="min-w-0 flex-1">
    <div class="flex min-w-0 items-center gap-1.5">
      <span class="text-fg truncate text-[13px] leading-5" title={branch.name}>{branch.name}</span>
      <div class="flex shrink-0 items-center gap-1">
        {#each localBranchIndicators(branch, repo.default_branch) as indicator (indicator.key)}
          <GitStatusIcon {indicator} />
        {/each}
      </div>
    </div>
    {#if branch.worktree}
      <div class="text-muted truncate font-mono text-[11px] leading-4" title={branch.worktree.path}>
        {displayPath(branch.worktree.path)}
      </div>
    {:else}
      <div class="text-muted truncate text-[11px] leading-4">No local folder</div>
    {/if}
  </div>
{/snippet}

{#snippet detachedInner(wt: WorktreeView)}
  <div class="min-w-0 flex-1">
    <div class="flex min-w-0 items-center gap-1.5">
      <span class="text-muted truncate font-mono text-[12px] leading-5">
        {wt.detached_hash ?? "detached"}
      </span>
      <div class="flex shrink-0 items-center gap-1">
        {#if wt.warning === "orphaned"}
          <GitStatusIcon
            indicator={{
              key: "orphaned",
              label: "orphaned",
              tone: "warning",
              title: "Orphaned folder",
              description: "The branch this folder was on was deleted.",
            }}
          />
        {:else if wt.warning === "prunable"}
          <GitStatusIcon
            indicator={{
              key: "prunable",
              label: "prunable",
              tone: "warning",
              title: "Missing folder",
              description: "This folder path is gone; the git worktree record can be pruned.",
            }}
          />
        {/if}
      </div>
    </div>
    <div class="text-muted truncate font-mono text-[11px] leading-4" title={wt.path}>
      {basename(wt.path)}
    </div>
  </div>
{/snippet}
