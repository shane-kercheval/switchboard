/// Shared run-status vocabulary for the status-colored UI (`StatusDot` and the
/// `status-*` tokens). Lives here rather than inside `StatusDot` so additional
/// status consumers don't have to import from a sibling component.
export type BadgeStatus = "idle" | "processing" | "failed" | "cancelled";
