<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import * as api from "$lib/api";
  import { appendUserTurn, emptyTranscript, reduce } from "$lib/reducer";
  import {
    HEARTBEAT_TIMEOUT_MS,
    type AgentRecord,
    type AgentTranscript,
    type NormalizedEvent,
    type TurnId,
  } from "$lib/types";
  import ComposeBar from "./ComposeBar.svelte";
  import Transcript from "./Transcript.svelte";

  let { agent }: { agent: AgentRecord } = $props();

  // svelte-ignore state_referenced_locally
  // Initial value only — App.svelte remounts AgentPane on agent change so
  // there is no scenario where `agent` changes after this binding is created.
  let transcript = $state<AgentTranscript>(emptyTranscript(agent.id));
  let inFlightTurnId = $state<TurnId | null>(null);
  // Synchronous "user clicked Send, awaiting IPC reply" flag. Distinct from
  // inFlightTurnId because the event stream can arrive (and complete) before
  // `await sendMessage` resolves — without this gate, a fast mock would let
  // the user double-click Send before the turn_id round-trip lands.
  let sending = $state<boolean>(false);
  let sendError = $state<string | null>(null);

  // Heartbeat timer: when a turn is in flight and no content_chunk has been
  // observed for HEARTBEAT_TIMEOUT_MS, dispatch a heartbeat_timeout into the
  // reducer. The reducer transitions the turn to "failed" with a retry
  // message. Stream-contract ownership stays with the adapter (M1.3); this is
  // the frontend's resilience against any adapter bug that silently truncates
  // a stream (M1.4 §7).
  let heartbeat: ReturnType<typeof setTimeout> | null = null;

  function clearHeartbeat(): void {
    if (heartbeat !== null) {
      clearTimeout(heartbeat);
      heartbeat = null;
    }
  }

  function armHeartbeat(turnId: TurnId): void {
    clearHeartbeat();
    heartbeat = setTimeout(() => {
      heartbeat = null;
      transcript = reduce(transcript, { type: "heartbeat_timeout", turn_id: turnId });
      if (inFlightTurnId === turnId) inFlightTurnId = null;
    }, HEARTBEAT_TIMEOUT_MS);
  }

  let unlisten: UnlistenFn | null = null;

  onMount(async () => {
    unlisten = await listen<NormalizedEvent>(`agent:${agent.id}`, (event) => {
      const ev = event.payload;
      transcript = reduce(transcript, ev);
      if (ev.type === "turn_start") {
        armHeartbeat(ev.turn_id);
      } else if (ev.type === "content_chunk") {
        // Only re-arm if the chunk is for the turn we're tracking. Stale
        // chunks for unrelated turns would otherwise extend the timer wrongly.
        if (inFlightTurnId === ev.turn_id) armHeartbeat(ev.turn_id);
      } else if (ev.type === "turn_end") {
        if (inFlightTurnId === ev.turn_id) {
          inFlightTurnId = null;
          clearHeartbeat();
        }
      }
    });
  });

  onDestroy(() => {
    if (unlisten) unlisten();
    clearHeartbeat();
  });

  async function handleSubmit(prompt: string): Promise<void> {
    sendError = null;
    sending = true;
    // Optimistic: append the user turn synchronously with a *local* id that
    // is distinct from the backend-assigned turn_id (which is the agent
    // turn's id). Sharing the same id would create duplicate keys in the
    // `{#each ... (turn.id)}` block and break Svelte's keyed rendering.
    const userTurnId = crypto.randomUUID();
    transcript = appendUserTurn(transcript, userTurnId, prompt);
    try {
      const turnId = await api.sendMessage(agent.id, prompt);
      // Events may have already arrived and terminated this turn while the
      // IPC reply was in flight (the mock harness emits all events before
      // sendMessage resolves). Only set inFlightTurnId if the agent turn is
      // still streaming (or hasn't arrived yet) — otherwise it would get
      // stuck set with no turn_end coming to clear it.
      const observed = transcript.turns.find((t) => t.id === turnId);
      const stillStreaming =
        observed === undefined || (observed.role === "agent" && observed.status === "streaming");
      if (stillStreaming) inFlightTurnId = turnId;
    } catch (err) {
      sendError = err instanceof Error ? err.message : String(err);
    } finally {
      sending = false;
    }
  }

  // Send is locked while we're awaiting the IPC reply (synchronous "sending"
  // flag) AND while a turn is in flight from a prior send (inFlightTurnId).
  const sendDisabled = $derived(sending || inFlightTurnId !== null);

  const status = $derived(sendDisabled ? "processing" : sendError !== null ? "error" : "idle");
</script>

<div class="flex h-full flex-col">
  <div class="flex items-center justify-between border-b border-neutral-200 px-4 py-2">
    <div class="text-sm">
      <span class="text-neutral-500">Agent:</span>
      <span class="font-mono font-semibold text-neutral-900">{agent.name}</span>
    </div>
    <div class="flex items-center gap-2 text-xs">
      <span
        class={status === "processing"
          ? "text-amber-700"
          : status === "error"
            ? "text-red-700"
            : "text-neutral-500"}
        data-testid="agent-status"
      >
        {status}
      </span>
    </div>
  </div>
  <Transcript {transcript} />
  {#if sendError}
    <div
      class="border-t border-red-200 bg-red-50 px-4 py-2 text-xs text-red-900"
      data-testid="send-error"
    >
      Send failed: {sendError}
    </div>
  {/if}
  <ComposeBar disabled={sendDisabled} onSubmit={handleSubmit} />
</div>
