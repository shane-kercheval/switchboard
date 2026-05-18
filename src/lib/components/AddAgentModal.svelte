<script lang="ts">
  /// Modal wrapper around `CreateAgentForm` used by the loaded-phase
  /// "+ Add agent" affordance in `Sidebar`. The no-agent first-time flow
  /// keeps using `CreateAgentForm` standalone (full-screen card layout);
  /// this modal is for adding agents **after** a project already has at
  /// least one.
  ///
  /// **ESC and click-outside behavior.** `Dialog` (bits-ui underneath)
  /// handles both at the primitive layer — flipping `open` to false. The
  /// `onClose` callback on `Dialog` is wired to `onCancel` here, so any
  /// dismiss path (ESC, overlay click, Cancel button, the form's own
  /// onCancel) collapses to the same handler.
  import type { AgentFormSubmit } from "./CreateAgentForm.types";
  import type { HarnessAvailability } from "$lib/types";
  import CreateAgentForm from "./CreateAgentForm.svelte";
  import Dialog from "./ui/Dialog.svelte";

  type Props = {
    /// Two-way bound open state.
    open: boolean;
    busy?: boolean;
    error?: string | null;
    onSubmit: (submission: AgentFormSubmit) => void;
    onCancel: () => void;
    claudeAvailability?: HarnessAvailability;
    codexAvailability?: HarnessAvailability;
    geminiAvailability?: HarnessAvailability;
  };

  let {
    open = $bindable(),
    busy = false,
    error = null,
    onSubmit,
    onCancel,
    claudeAvailability,
    codexAvailability,
    geminiAvailability,
  }: Props = $props();
</script>

<Dialog bind:open title="Add an agent" onClose={onCancel}>
  <CreateAgentForm
    {busy}
    {error}
    {onSubmit}
    {onCancel}
    {claudeAvailability}
    {codexAvailability}
    {geminiAvailability}
  />
</Dialog>
