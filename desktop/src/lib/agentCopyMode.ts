export type AgentCopyMode = "last_answer_block" | "full_answer";

export const DEFAULT_AGENT_COPY_MODE: AgentCopyMode = "last_answer_block";

export function isAgentCopyMode(value: string | null): value is AgentCopyMode {
  return value === "last_answer_block" || value === "full_answer";
}
