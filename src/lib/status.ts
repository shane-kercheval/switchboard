/// Shared run-status vocabulary for the status-colored UI primitives (`Badge`
/// status variant, `StatusDot`). Lives here rather than in either component so
/// neither sibling owns the other's type as more status consumers appear.
export type BadgeStatus = "idle" | "processing" | "failed" | "cancelled";
