import type { AgentTranscript, ReducerInput, Turn, TurnId } from "./types";

// Pure transcript reducer. Single source of truth for transcript state —
// component-level effects (heartbeat timer, IPC subscription) push events
// into the reducer rather than mutating transcripts directly.
//
// Cross-turn isolation: events for unknown turn_ids are dropped. Late events
// for turns already in a terminal state (`complete` or `failed`) are also
// dropped — the dispatcher's drain task may continue emitting after the
// frontend has heartbeat-timed-out a turn; without this guard the failed
// turn would resurrect with late content.
export function reduce(transcript: AgentTranscript, input: ReducerInput): AgentTranscript {
  switch (input.type) {
    case "turn_start":
      return appendAgentTurn(transcript, {
        id: input.turn_id,
        role: "agent",
        text: "",
        status: "streaming",
        startedAt: input.started_at,
      });

    case "content_chunk": {
      const existing = findTurn(transcript, input.turn_id);
      if (existing === undefined) return transcript;
      if (existing.role !== "agent") return transcript;
      if (existing.status !== "streaming") return transcript;
      return updateTurn(transcript, input.turn_id, {
        ...existing,
        text: existing.text + input.text,
      });
    }

    case "turn_end": {
      const existing = findTurn(transcript, input.turn_id);
      if (existing === undefined) return transcript;
      if (existing.role !== "agent") return transcript;
      if (existing.status !== "streaming") return transcript;
      if (input.outcome.status === "completed") {
        return updateTurn(transcript, input.turn_id, {
          ...existing,
          status: "complete",
          endedAt: input.ended_at,
        });
      }
      return updateTurn(transcript, input.turn_id, {
        ...existing,
        status: "failed",
        error: input.outcome.message,
        endedAt: input.ended_at,
      });
    }

    case "heartbeat_timeout": {
      const existing = findTurn(transcript, input.turn_id);
      if (existing === undefined) return transcript;
      if (existing.role !== "agent") return transcript;
      if (existing.status !== "streaming") return transcript;
      return updateTurn(transcript, input.turn_id, {
        ...existing,
        status: "failed",
        error: "no response from harness — retry?",
        endedAt: new Date().toISOString(),
      });
    }
  }
}

// Append a user-role turn synchronously at submit time. Separate from the
// reducer because it's caller-driven (the user clicked Send), not
// event-driven from the backend.
export function appendUserTurn(
  transcript: AgentTranscript,
  turnId: TurnId,
  text: string,
): AgentTranscript {
  return {
    ...transcript,
    turns: [
      ...transcript.turns,
      {
        id: turnId,
        role: "user",
        text,
        submittedAt: new Date().toISOString(),
      },
    ],
  };
}

export function emptyTranscript(agentId: string): AgentTranscript {
  return { agentId, turns: [] };
}

function findTurn(transcript: AgentTranscript, turnId: TurnId): Turn | undefined {
  return transcript.turns.find((t) => t.id === turnId);
}

function appendAgentTurn(transcript: AgentTranscript, turn: Turn): AgentTranscript {
  return { ...transcript, turns: [...transcript.turns, turn] };
}

function updateTurn(transcript: AgentTranscript, turnId: TurnId, next: Turn): AgentTranscript {
  return {
    ...transcript,
    turns: transcript.turns.map((t) => (t.id === turnId ? next : t)),
  };
}
