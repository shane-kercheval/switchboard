<script lang="ts">
  import {
    dispatchUserTurn,
    failSendStart,
    recordSendAccepted,
    runtimes,
    ui,
  } from "$lib/state/index.svelte";
  import * as api from "$lib/api";
  import type { AgentRecord } from "$lib/types";
  import Button from "$lib/components/ui/Button.svelte";
  import Textarea from "$lib/components/ui/Textarea.svelte";
  import { HARNESS_LABEL } from "$lib/harnessDisplay";

  let { agents }: { agents: AgentRecord[] } = $props();

  let prompt = $state<string>("");
  /// Recipient preselect: most-recent recipient (ui.lastRecipientId) if it's
  /// still in the agents list; otherwise the first agent. Recomputed on
  /// every change to agents/lastRecipientId so adding an agent dynamically
  /// (the `create_agent` flow) updates the picker without manual reset.
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
  /// 2. Hydration is complete. Newly-created agents land at "complete"
  ///    immediately; registered/attached agents pass through "loading".
  ///    "failed" is also OK (degraded-dispatch mode — harness session
  ///    still works; banner explains the gap).
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
    const submittedText = prompt.trim();
    sendError = null;

    // Local UUID for the user-turn id — frontend-only, never crosses the
    // IPC boundary. Distinct from the backend-assigned turn_id (which is
    // the agent's response turn).
    const userTurnId = crypto.randomUUID();
    dispatchUserTurn(recipientId, userTurnId, submittedText);

    try {
      const messageId = await api.sendMessage(recipientId, submittedText);
      // Record the accepted-send receipt so the eventual `turn_start` /
      // `message_failed` event (both carry this `message_id`) correlates
      // back to this dispatch. Guarded inside the state module — a no-op
      // if TurnStart already raced the IPC reply and flipped the agent to
      // "processing".
      recordSendAccepted(recipientId, messageId);
      // Clear-only-if-unchanged: if the user typed new text during the
      // in-flight window, preserve it. If the prompt still matches what
      // we submitted, clear it for the next message. Future work may
      // disable the textarea during in-flight to remove this case
      // entirely.
      if (prompt.trim() === submittedText) {
        prompt = "";
      }
      // TurnStart (or message_failed) will arrive on the channel and flip
      // run_status. Nothing further to do here.
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      sendError = message;
      // Restore sendability (starting → idle) so the user can retry.
      // Guarded inside the state module — no-op if TurnStart raced
      // ahead and the agent is already processing (in which case the
      // IPC failure was spurious). Prompt text is preserved
      // unconditionally on failure so the user can retry without
      // retyping.
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

<div class="border-border bg-panel border-t p-3">
  {#if agents.length > 1}
    <div class="mb-2 flex items-center gap-2 text-xs">
      <label for="recipient-picker" class="text-muted">To:</label>
      <select
        id="recipient-picker"
        data-testid="recipient-picker"
        bind:value={recipientId}
        class="border-border bg-raised text-fg rounded border px-2 py-1 font-mono text-xs"
      >
        {#each agents as agent (agent.id)}
          <option value={agent.id}>
            {agent.name} ({HARNESS_LABEL[agent.harness]})
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
    <p class="text-status-failed mt-2 text-xs" data-testid="compose-send-error">
      Send failed: {sendError}
    </p>
  {/if}
</div>
