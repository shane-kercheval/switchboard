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

  // Heartbeat timer: when a turn is streaming and no content_chunk has been
  // observed for HEARTBEAT_TIMEOUT_MS, dispatch a heartbeat_timeout into the
  // reducer. The reducer transitions the turn to "failed" with a retry
  // message. Stream-contract ownership stays with the adapter (M1.3); this is
  // the frontend's resilience against any adapter bug that silently truncates
  // a stream (M1.4 §7).
  //
  // The heartbeat tracks its own target turn (`heartbeatTurnId`) independent
  // of `inFlightTurnId` (which only governs Send-disabled UI state). They are
  // separate concerns: in a fast-events race the entire event stream can
  // fire before `inFlightTurnId` is set, and gating the timer-clear on
  // `inFlightTurnId === ev.turn_id` would leak a zombie 60s timer per turn.
  // Similarly, early `content_chunk`s (before IPC resolves) must still extend
  // the timer — keying re-arm on `heartbeatTurnId` ensures they do.
  let heartbeat: ReturnType<typeof setTimeout> | null = null;
  let heartbeatTurnId: TurnId | null = null;

  function clearHeartbeat(): void {
    if (heartbeat !== null) {
      clearTimeout(heartbeat);
      heartbeat = null;
    }
    heartbeatTurnId = null;
  }

  function armHeartbeat(turnId: TurnId): void {
    clearHeartbeat();
    heartbeatTurnId = turnId;
    heartbeat = setTimeout(() => {
      heartbeat = null;
      heartbeatTurnId = null;
      // `at` is supplied here (not inside the reducer) so the reducer
      // stays pure — reducer tests can assert on transcripts without
      // tolerating wall-clock variation in `endedAt`.
      transcript = reduce(transcript, {
        type: "heartbeat_timeout",
        turn_id: turnId,
        at: new Date().toISOString(),
      });
      if (inFlightTurnId === turnId) inFlightTurnId = null;
    }, HEARTBEAT_TIMEOUT_MS);
  }

  let unlisten: UnlistenFn | null = null;
  // True once `listen()` has registered the callback with Tauri. Send is
  // gated on this so a fast click can't fire `send_message` before the
  // listener exists — without the gate, `turn_start` + early chunks could
  // arrive on a channel with no subscriber and be dropped.
  let listenerReady = $state<boolean>(false);
  // Set true if the component unmounts before `await listen()` resolves;
  // the closure then immediately invokes the freshly-returned unlisten so
  // the registration doesn't leak past component lifetime.
  let cancelled = false;

  onMount(async () => {
    const fn = await listen<NormalizedEvent>(`agent:${agent.id}`, (event) => {
      const ev = event.payload;
      transcript = reduce(transcript, ev);
      if (ev.type === "turn_start") {
        armHeartbeat(ev.turn_id);
      } else if (
        ev.type === "content_chunk" ||
        ev.type === "tool_started" ||
        ev.type === "tool_completed"
      ) {
        // Re-arm on any per-turn activity event for the turn the heartbeat
        // is tracking. A long shell tool call legitimately produces zero
        // content_chunks for minutes (e.g., test runs, large greps); without
        // tool-event re-arming, the heartbeat would falsely fail those turns.
        // Stale events for unrelated turns are ignored so the timer doesn't
        // get dragged to the wrong turn's lifetime.
        if (heartbeatTurnId === ev.turn_id) armHeartbeat(ev.turn_id);
      } else if (ev.type === "turn_end") {
        if (heartbeatTurnId === ev.turn_id) clearHeartbeat();
        if (inFlightTurnId === ev.turn_id) inFlightTurnId = null;
      }
    });
    if (cancelled) {
      // Component already unmounted while listen() was in flight — clean
      // up immediately so the registration doesn't outlive the component.
      fn();
      return;
    }
    unlisten = fn;
    listenerReady = true;
  });

  onDestroy(() => {
    cancelled = true;
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
    //
    // v4 here is fine — this id is frontend-only and never crosses the IPC
    // boundary, so the project-wide UUID v7 convention (time-ordered,
    // backend-friendly) serves no purpose for it. Backend ids (AgentId,
    // ProjectId, TurnId) remain v7 per AGENTS.md.
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

  // Send is locked while we're awaiting the IPC reply (synchronous
  // "sending" flag), while a turn is in flight from a prior send
  // (`inFlightTurnId`), AND before the per-agent listener has been
  // registered (`!listenerReady`). The listener-readiness gate prevents a
  // fast click from firing `send_message` before `listen()` has resolved,
  // which would lose `turn_start` and early chunks (they'd hit a channel
  // with no subscriber).
  const sendDisabled = $derived(!listenerReady || sending || inFlightTurnId !== null);

  // While the listener is being set up, present idle (the textarea still
  // accepts input; only Send is gated). After listenerReady, status
  // follows the actual dispatch state.
  const status = $derived(
    !listenerReady
      ? "idle"
      : sending || inFlightTurnId !== null
        ? "processing"
        : sendError !== null
          ? "error"
          : "idle",
  );
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
