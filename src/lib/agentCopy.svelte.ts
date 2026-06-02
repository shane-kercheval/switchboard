import { DEFAULT_AGENT_COPY_MODE, isAgentCopyMode, type AgentCopyMode } from "$lib/agentCopyMode";

const STORAGE_KEY = "switchboard-agent-copy-mode";

function readStoredMode(): AgentCopyMode {
  if (typeof localStorage === "undefined") return DEFAULT_AGENT_COPY_MODE;
  const stored = localStorage.getItem(STORAGE_KEY);
  return isAgentCopyMode(stored) ? stored : DEFAULT_AGENT_COPY_MODE;
}

class AgentCopyPreference {
  mode = $state<AgentCopyMode>(readStoredMode());

  set(mode: AgentCopyMode): void {
    this.mode = mode;
    if (typeof localStorage !== "undefined") {
      localStorage.setItem(STORAGE_KEY, mode);
    }
  }
}

export const agentCopy = new AgentCopyPreference();
