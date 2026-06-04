<script lang="ts">
  /// Combined "Add project" form — a two-mode toggle (New project / Add
  /// existing), mirroring `CreateAgentForm`'s create/attach structure. Owns its
  /// own dialog state and commits directly via the workspace actions (the same
  /// pattern the rename/remove flows use), so `App.svelte` only wires open/close.
  ///
  /// `busy` is `$bindable` so the host can keep the modal non-dismissible while a
  /// commit is in flight — load-bearing for New project, whose commit kicks off
  /// agent auto-seeding the user must not navigate away from mid-flight.
  /// `onClose` fires on a successful commit or Cancel; `onCreated` fires only
  /// after a successful *new-project* create (the host uses it to leave the
  /// settings view so the freshly-activated project is shown — add-existing
  /// intentionally does not).
  import type { ProjectSummary } from "$lib/types";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import * as api from "$lib/api";
  import { addDirectory, createProjectAndActivate } from "$lib/state/workspace.svelte";
  import { normalizeProjectName, validateProjectName } from "$lib/projectName";
  import { basename, cn } from "$lib/utils";
  import Input from "$lib/components/ui/Input.svelte";
  import Button from "$lib/components/ui/Button.svelte";
  import {
    SEGMENTED_CONTAINER_CLASS,
    SEGMENTED_ITEM_CLASS,
    SEGMENTED_ITEM_ACTIVE_CLASS,
    SEGMENTED_ITEM_INACTIVE_CLASS,
  } from "$lib/components/ui/segmentedControl";

  let {
    onClose,
    onCreated,
    busy = $bindable(false),
  }: { onClose: () => void; onCreated?: () => void; busy?: boolean } = $props();

  let mode = $state<"new" | "existing">("new");

  // New-project sub-state.
  let newFolder = $state<string | null>(null);
  let newName = $state<string>("");
  /// Projects already in the chosen folder (read-only probe), for live
  /// duplicate validation. Empty until/unless a probe populates it; the backend
  /// stays authoritative either way.
  let newSiblings = $state<ProjectSummary[]>([]);
  let newError = $state<string | null>(null);

  // Add-existing sub-state. `addFound` is `null` until a folder is probed, then
  // the projects discovered in it (empty array = none found).
  let addFolder = $state<string | null>(null);
  let addFound = $state<ProjectSummary[] | null>(null);
  let addError = $state<string | null>(null);

  async function pickFolder(): Promise<string | null> {
    const result = await openDialog({ directory: true, multiple: false });
    return typeof result === "string" ? result : null;
  }

  function selectMode(next: "new" | "existing"): void {
    if (next === mode || busy) return;
    mode = next;
    // Reset the outgoing mode so stale results don't linger behind the toggle.
    if (next === "new") {
      addFolder = null;
      addFound = null;
      addError = null;
    } else {
      newFolder = null;
      newName = "";
      newSiblings = [];
      newError = null;
    }
  }

  /// New project: the folder is valid the instant it's picked (we're creating
  /// fresh), so set it immediately and enable submit. The read-only probe only
  /// enriches duplicate validation, so it runs best-effort *behind* the
  /// selection — a probe failure doesn't block creating (the backend rejects an
  /// unusable folder at commit), it just forfeits client-side dup detection.
  async function chooseNewFolder(): Promise<void> {
    const folder = await pickFolder();
    if (folder === null) return;
    newFolder = folder;
    if (normalizeProjectName(newName) === "") newName = basename(folder);
    newError = null;
    try {
      const info = await api.pickDirectory(folder);
      newFolder = info.path; // canonical path the backend keys on
      newSiblings = info.projects;
    } catch (err) {
      newSiblings = [];
      newError = err instanceof Error ? err.message : String(err);
    }
  }

  const newValidation = $derived(validateProjectName(newName, newSiblings, undefined));
  /// Suppress the `empty` message so an empty field disables Create without
  /// nagging mid-edit (mirrors the agent form + the rename editor).
  const newNameMessage = $derived(
    newValidation.ok || newValidation.reason === "empty" ? null : newValidation.message,
  );
  const canCreate = $derived(!busy && newFolder !== null && newValidation.ok);

  async function submitNew(): Promise<void> {
    if (!canCreate || newFolder === null) return;
    newError = null;
    busy = true;
    try {
      await createProjectAndActivate(normalizeProjectName(newName), newFolder);
      onCreated?.();
      onClose();
    } catch (err) {
      newError = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  function submitNewFromName(event: KeyboardEvent): void {
    if (event.key !== "Enter") return;
    event.preventDefault();
    void submitNew();
  }

  /// Add existing: the probe *is* the action (preview which projects will be
  /// added), so the folder is only committable once it discovers ≥1 project.
  async function chooseAddFolder(): Promise<void> {
    const folder = await pickFolder();
    if (folder === null) return;
    // Discard any prior preview up front so a failed probe leaves a clean
    // "nothing to add" state rather than stranding Add on the previous folder.
    addError = null;
    addFolder = null;
    addFound = null;
    busy = true;
    try {
      const info = await api.pickDirectory(folder);
      addFolder = info.path;
      addFound = info.projects;
    } catch (err) {
      addError = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  const canAdd = $derived(!busy && addFolder !== null && addFound !== null && addFound.length > 0);

  async function submitAdd(): Promise<void> {
    if (addFolder === null) return;
    addError = null;
    busy = true;
    try {
      await addDirectory(addFolder);
      onClose();
    } catch (err) {
      addError = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  const tabNewClass = $derived(
    cn(
      SEGMENTED_ITEM_CLASS,
      "flex-1",
      mode === "new" ? SEGMENTED_ITEM_ACTIVE_CLASS : SEGMENTED_ITEM_INACTIVE_CLASS,
    ),
  );
  const tabExistingClass = $derived(
    cn(
      SEGMENTED_ITEM_CLASS,
      "flex-1",
      mode === "existing" ? SEGMENTED_ITEM_ACTIVE_CLASS : SEGMENTED_ITEM_INACTIVE_CLASS,
    ),
  );
</script>

<div class="space-y-4" data-testid="project-dialog">
  <div class={cn(SEGMENTED_CONTAINER_CLASS, "flex")} role="tablist">
    <button
      type="button"
      class={tabNewClass}
      role="tab"
      aria-selected={mode === "new"}
      data-testid="project-dialog-mode-new"
      onclick={() => selectMode("new")}
      disabled={busy}
    >
      New project
    </button>
    <button
      type="button"
      class={tabExistingClass}
      role="tab"
      aria-selected={mode === "existing"}
      data-testid="project-dialog-mode-existing"
      onclick={() => selectMode("existing")}
      disabled={busy}
    >
      Add existing
    </button>
  </div>

  {#if mode === "new"}
    <div class="space-y-4" data-testid="new-project-form">
      <p class="text-muted text-sm leading-relaxed">
        Choose the folder you want to work in — typically your repo or working directory.
        Switchboard will create a <code>.switchboard/</code> folder there to store project state.
      </p>
      <div class="space-y-1.5">
        <span class="text-muted block text-xs">Folder</span>
        <Button
          variant="secondary"
          size="sm"
          data-testid="new-project-choose-folder"
          disabled={busy}
          onclick={chooseNewFolder}
        >
          Choose folder…
        </Button>
        {#if newFolder}
          <p
            class="text-muted bg-panel truncate rounded px-2 py-1.5 font-mono text-xs"
            title={newFolder}
          >
            {newFolder}
          </p>
        {/if}
      </div>
      <div class="space-y-1.5">
        <label for="new-project-name" class="text-muted block text-xs">Name</label>
        <Input
          id="new-project-name"
          data-testid="new-project-name"
          placeholder="project name"
          bind:value={newName}
          disabled={busy}
          class={cn(newNameMessage && "border-status-failed")}
          aria-invalid={!newValidation.ok}
          aria-describedby={newNameMessage ? "new-project-name-error" : undefined}
          title={newNameMessage ?? undefined}
          onkeydown={submitNewFromName}
        />
        {#if newNameMessage}
          <span
            id="new-project-name-error"
            class="text-status-failed block text-xs"
            data-testid="new-project-name-error"
          >
            {newNameMessage}
          </span>
        {/if}
      </div>
      {#if newError}
        <p class="text-status-failed text-xs" data-testid="new-project-error">
          {newError}
        </p>
      {/if}
      <div class="flex justify-end gap-2">
        <Button
          variant="secondary"
          size="sm"
          class="w-24"
          data-testid="new-project-cancel"
          disabled={busy}
          onclick={onClose}
        >
          Cancel
        </Button>
        <Button
          size="sm"
          class="w-24"
          data-testid="new-project-submit"
          disabled={!canCreate}
          onclick={submitNew}
        >
          Create
        </Button>
      </div>
    </div>
  {:else}
    <div class="space-y-4" data-testid="add-existing-form">
      <p class="text-muted text-sm leading-relaxed">
        Choose a folder you've already used with Switchboard — your repo or working directory (the
        one that contains a
        <code class="bg-panel text-fg rounded px-1 font-mono text-xs">.switchboard/</code>
        folder). Switchboard looks there for projects you've created before.
      </p>
      <Button
        variant="secondary"
        size="sm"
        data-testid="add-existing-choose-folder"
        disabled={busy}
        onclick={chooseAddFolder}
      >
        Choose folder…
      </Button>
      {#if addError}
        <p class="text-status-failed text-xs" data-testid="add-existing-error">
          {addError}
        </p>
      {:else if addFound !== null}
        {#if addFound.length > 0}
          <div class="space-y-1.5" data-testid="add-existing-found">
            <p class="text-fg text-sm">
              Found {addFound.length}
              {addFound.length === 1 ? "project" : "projects"} — these will be added:
            </p>
            <ul class="text-muted space-y-0.5 text-xs">
              {#each addFound as found (found.id)}
                <li class="truncate">{found.name}</li>
              {/each}
            </ul>
          </div>
        {:else}
          <p class="text-warning text-xs leading-relaxed" data-testid="add-existing-none">
            No Switchboard projects found in
            <span class="font-mono" title={addFolder ?? ""}>{addFolder}</span>. Make sure you picked
            the working directory that contains a
            <code class="bg-panel text-fg rounded px-1 font-mono">.switchboard/</code>
            folder — or switch to "New project" to create one there.
          </p>
        {/if}
      {/if}
      <div class="flex justify-end gap-2">
        <Button
          variant="secondary"
          size="sm"
          class="w-24"
          data-testid="add-existing-cancel"
          disabled={busy}
          onclick={onClose}
        >
          Cancel
        </Button>
        <Button
          size="sm"
          class="w-24"
          data-testid="add-existing-add"
          disabled={!canAdd}
          onclick={submitAdd}
        >
          {busy ? "Adding…" : "Add"}
        </Button>
      </div>
    </div>
  {/if}
</div>
