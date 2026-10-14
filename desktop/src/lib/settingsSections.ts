export const SETTINGS_SECTIONS = [
  { id: "general", label: "General" },
  { id: "agents", label: "Agents" },
  { id: "notifications", label: "Notifications" },
  { id: "prompts", label: "Prompts & workflows" },
  { id: "agent-access", label: "Agent access" },
  { id: "shortcuts", label: "Keyboard shortcuts" },
] as const;

export type SettingsSection = (typeof SETTINGS_SECTIONS)[number]["id"];
