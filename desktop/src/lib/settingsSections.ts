export const SETTINGS_SECTIONS = [
  { id: "general", label: "General" },
  { id: "agents", label: "Agents" },
  { id: "notifications", label: "Notifications" },
  { id: "prompts", label: "Prompts" },
  { id: "workflows", label: "Workflows" },
  { id: "agent-access", label: "Agent MCP Server" },
  { id: "shortcuts", label: "Keyboard shortcuts" },
] as const;

export type SettingsSection = (typeof SETTINGS_SECTIONS)[number]["id"];
