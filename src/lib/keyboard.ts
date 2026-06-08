// Shared keyboard-handling helpers used by the window-level shortcut handlers.

/// Whether an event target is a text-editing element, so a global shortcut
/// should stand down and let the keystroke reach the field. Centralized so the
/// several window-level keydown handlers (App, GitView, ComposeBar) agree on
/// what counts as "the user is typing" — changing the rule here updates them all.
export function isEditableShortcutTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  return (
    target.isContentEditable ||
    target.tagName === "INPUT" ||
    target.tagName === "TEXTAREA" ||
    target.tagName === "SELECT"
  );
}
