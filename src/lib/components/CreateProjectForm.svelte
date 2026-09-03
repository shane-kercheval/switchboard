<script lang="ts">
  /// "Add project" form. Owns its own dialog state and commits directly via the
  /// workspace action (the same pattern the rename/remove flows use), so
  /// `App.svelte` only wires open/close.
  ///
  /// `busy` is `$bindable` so the host can keep the modal non-dismissible while a
  /// commit is in flight — load-bearing because the commit kicks off agent
  /// auto-seeding the user must not navigate away from mid-flight. `onClose`
  /// fires on a successful commit or Cancel; `onCreated` fires only after a
  /// successful create (the host uses it to leave the settings view so the
  /// freshly-activated project is shown).
  import type { ProjectSummary } from "$lib/types";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import * as api from "$lib/api";
  import { createProjectAndActivate } from "$lib/state/workspace.svelte";
  import { normalizeProjectName, validateProjectName } from "$lib/projectName";
  import { basename, cn } from "$lib/utils";
  import Input from "$lib/components/ui/Input.svelte";
  import Button from "$lib/components/ui/Button.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { SUPPLEMENTAL_TOOLTIP_DELAY } from "$lib/components/ui/tooltip";

  let {
    onClose,
    onCreated,
    busy = $bindable(false),
  }: { onClose: () => void; onCreated?: () => void; busy?: boolean } = $props();

  let newFolder = $state<string | null>(null);
  let newName = $state<string>("");
  /// Projects already in the chosen folder (read-only probe), for live
  /// duplicate validation. Empty until/unless a probe populates it; the backend
  /// stays authoritative either way.
  let newSiblings = $state<ProjectSummary[]>([]);
  let newError = $state<string | null>(null);
  /// Monotonic stamp for the folder probe. Picking a folder is instant (submit
  /// enables immediately — the probe runs best-effort behind it), so nothing
  /// blocks a second pick while the first probe is in flight. Stamping each pick
  /// and discarding a probe whose stamp is stale prevents an out-of-order probe
  /// (folder A resolving after the user re-picked folder B) from overwriting B's
  /// canonical path / sibling list. Non-reactive guard.
  let newFolderProbeSeq = 0;

  async function pickFolder(): Promise<string | null> {
    const result = await openDialog({ directory: true, multiple: false });
    return typeof result === "string" ? result : null;
  }

  /// The folder is valid the instant it's picked (we're creating fresh), so set
  /// it immediately and enable submit. The read-only probe only enriches
  /// duplicate validation, so it runs best-effort *behind* the selection — a
  /// probe failure doesn't block creating (the backend rejects an unusable
  /// folder at commit), it just forfeits client-side dup detection.
  async function chooseNewFolder(): Promise<void> {
    const folder = await pickFolder();
    if (folder === null) return;
    newFolder = folder;
    if (normalizeProjectName(newName) === "") newName = basename(folder);
    newError = null;
    // Only apply this probe's result if it's still the latest pick — a slower,
    // superseded probe must not clobber a newer selection.
    const seq = ++newFolderProbeSeq;
    try {
      const info = await api.pickDirectory(folder);
      if (seq !== newFolderProbeSeq) return;
      newFolder = info.path; // canonical path the backend keys on
      newSiblings = info.projects;
    } catch (err) {
      if (seq !== newFolderProbeSeq) return;
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
</script>

<div class="space-y-4" data-testid="project-dialog">
  <div class="space-y-4" data-testid="new-project-form">
    <p class="text-muted text-sm leading-relaxed">
      Choose the folder you want to work in — typically your repo or working directory. Nothing is
      written into it: Switchboard keeps the project's state separately, so the folder stays exactly
      as it is.
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
        <Tooltip label={newFolder} delayDuration={SUPPLEMENTAL_TOOLTIP_DELAY} focusable={false}>
          {#snippet trigger(props)}
            <p
              {...props}
              class="text-muted bg-panel truncate rounded px-2 py-1.5 font-mono text-xs"
            >
              {newFolder}
            </p>
          {/snippet}
        </Tooltip>
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
</div>
