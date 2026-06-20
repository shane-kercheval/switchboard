<script lang="ts">
  /// One tracked repo in the Git view: a collapsible header (repo name/path,
  /// availability, per-repo refresh + fetch-failure indicator) over its branches.
  /// Branch filtering is applied by the parent and passed in, so this node is
  /// pure presentation over the data.
  import { onMount } from "svelte";
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
  import { ICON_BUTTON_CLASS } from "$lib/components/ui/iconButton";
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
  import type { FetchState } from "$lib/state/gitView.svelte";
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
    setViewMode,
  } from "$lib/state/gitView.svelte";
  import { activateProject } from "$lib/state/workspace.svelte";
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

  const repo = $derived(listing.repo);
  const localBranchCount = $derived(repo.local_branches.length);

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

<div
  class="border-border/70 bg-raised min-h-0 shrink-0 overflow-hidden rounded-md border"
  data-testid="git-repo"
  data-repo-root={repo.root}
>
  <div class="group bg-raised flex min-h-10 items-center gap-2 px-3 py-2">
    <button
      type="button"
      class="text-muted hover:bg-border/60 hover:text-fg flex h-[26px] w-[26px] shrink-0 items-center justify-center rounded-full transition-colors"
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
              class="text-warning hover:bg-border/60 inline-flex h-[26px] w-[26px] items-center justify-center rounded-full transition-colors"
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
          class={cn(ICON_BUTTON_CLASS, "hover:bg-border/60 shrink-0 disabled:opacity-50")}
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
              class={cn(ICON_BUTTON_CLASS, "hover:bg-border/60 shrink-0")}
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
              class={cn(ICON_BUTTON_CLASS, "hover:bg-border/60 shrink-0")}
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
              class={cn(ICON_BUTTON_CLASS, "hover:bg-border/60 shrink-0")}
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
    <div class="border-border/60 bg-panel border-t px-2 py-1.5" data-testid="repo-branches">
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
              "hover:bg-raised",
              (selected || actionsOpen) && "bg-raised hover:bg-raised",
            )}
            data-testid="git-branch"
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
                  "hover:bg-border/60 shrink-0 opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 data-[state=open]:opacity-100",
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
              "hover:bg-raised flex min-h-8 items-center gap-2 rounded-md px-2 py-1.5 opacity-80 transition-colors",
              selected && "bg-raised hover:bg-raised",
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
                wt.warning !== "prunable" && "hover:bg-raised",
                dselected && "bg-raised hover:bg-raised",
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
  <div class="border-border mt-1 mb-1 ml-4 border-l pl-2" data-testid="commit-list">
    {#if worktreePath !== null && hasChanges}
      {@const uSel = isUncommittedSelected(worktreePath)}
      <button
        type="button"
        class={cn(
          "flex w-full items-center gap-2 rounded-md px-2 py-1 text-left text-xs transition-colors",
          uSel ? "bg-raised text-fg" : "text-muted hover:bg-raised",
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
      {#each branchCommits.ranges as range (range.kind)}
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
                cSel ? "bg-raised text-fg" : "text-muted hover:bg-raised",
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
              <span class="min-w-0 flex-1 truncate" title={commit.subject}>{commit.subject}</span>
            </button>
          {/each}
          {#if range.truncated}
            <p class="text-muted/70 px-2 py-1 text-[10px]">…older commits not shown</p>
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
