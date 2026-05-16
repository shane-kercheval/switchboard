<script lang="ts">
  import { dispatchUserTurn, failSendStart, runtimes, ui } from "$lib/state/index.svelte";
  import * as api from "$lib/api";
  import type { AgentRecord } from "$lib/types";
  import Button from "$lib/components/ui/Button.svelte";
  import Textarea from "$lib/components/ui/Textarea.svelte";

  let { agents }: { agents: AgentRecord[] } = $props();

  let prompt = $state<string>("");
  /// Recipient preselect: most-recent recipient (ui.lastRecipientId) if it's
  /// still in the agents list; otherwise the first agent. Recomputed on
  /// every change to agents/lastRecipientId so adding an agent dynamically
  /// (M2.5's create_agent flow) updates the picker without manual reset.
  const defaultRecipient = $derived.by(() => {
    const lastId = ui.lastRecipientId;
    if (lastId !== null && agents.some((a) => a.id === lastId)) return lastId;
    return agents[0]?.id ?? null;
  });
  let recipientId = $state<string | null>(null);
  // Sync recipientId with defaultRecipient on first render and when agents change.
  $effect(() => {
    if (recipientId === null || !agents.some((a) => a.id === recipientId)) {
      recipientId = defaultRecipient;
    }
  });

  const recipient = $derived(agents.find((a) => a.id === recipientId));
  const runtime = $derived(recipientId !== null ? runtimes[recipientId] : undefined);

  /// Compose-bar Send is gated on three conditions:
  /// 1. Recipient exists.
  /// 2. Hydration is complete (M2.6 may flip to "loading"; M2.5 always
  ///    lands "complete" for newly-created agents). "failed" is also OK
  ///    (degraded-dispatch mode — harness session still works; banner
  ///    explains the gap).
  /// 3. Run status is "idle" — closes the pre-TurnStart race (run_status
  ///    flips to "starting" the moment Send is clicked; second click in
  ///    the IPC window finds non-idle and rejects without firing
  ///    send_message).
  /// And the textarea isn't empty.
  const sendDisabled = $derived(
    recipient === undefined ||
      runtime === undefined ||
      runtime.run_status !== "idle" ||
      (runtime.hydration_status !== "complete" && runtime.hydration_status !== "failed") ||
      prompt.trim() === "",
  );

  let sendError = $state<string | null>(null);

  async function handleSubmit(): Promise<void> {
    if (sendDisabled || recipientId === null) return;
    const text = prompt.trim();
    prompt = "";
    sendError = null;

    // Local UUID for the user-turn id — frontend-only, never crosses the
    // IPC boundary. Distinct from the backend-assigned turn_id (which is
    // the agent's response turn).
    const userTurnId = crypto.randomUUID();
    dispatchUserTurn(recipientId, userTurnId, text);

    try {
      await api.sendMessage(recipientId, text);
      // Success: TurnStart will arrive on the channel and flip
      // run_status → "processing". Nothing to do here.
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      sendError = message;
      // Restore sendability (starting → idle) so the user can retry.
      // Guarded inside the state module — no-op if TurnStart raced
      // ahead and the agent is already processing (in which case the
      // IPC failure was spurious).
      failSendStart(recipientId, { message, kind: "adapter_failure" });
    }
  }

  function handleKey(event: KeyboardEvent): void {
    if (event.key === "Enter" && event.metaKey) {
      event.preventDefault();
      void handleSubmit();
    }
  }
</script>

<div class="border-t border-neutral-200 bg-neutral-50 p-3">
  {#if agents.length > 1}
    <div class="mb-2 flex items-center gap-2 text-xs">
      <label for="recipient-picker" class="text-neutral-600">To:</label>
      <select
        id="recipient-picker"
        data-testid="recipient-picker"
        bind:value={recipientId}
        class="rounded border border-neutral-300 bg-white px-2 py-1 font-mono text-xs"
      >
        {#each agents as agent (agent.id)}
          <option value={agent.id}>
            {agent.name} ({agent.harness === "claude_code" ? "Claude" : "Codex"})
          </option>
        {/each}
      </select>
    </div>
  {/if}
  <div class="flex gap-2">
    <Textarea
      data-testid="compose-textarea"
      placeholder="Type a message…  (⌘+Enter to send)"
      rows={3}
      bind:value={prompt}
      onkeydown={handleKey}
    />
    <Button data-testid="compose-send" onclick={handleSubmit} disabled={sendDisabled}>Send</Button>
  </div>
  {#if sendError}
    <p class="mt-2 text-xs text-red-700" data-testid="compose-send-error">
      Send failed: {sendError}
    </p>
  {/if}
</div>
