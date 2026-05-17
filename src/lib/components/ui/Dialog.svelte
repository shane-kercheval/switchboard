<script lang="ts">
  /// Thin wrapper around `bits-ui` Dialog. Single import surface so future
  /// modals (settings, etc.) reuse the same primitive without each
  /// component re-importing `bits-ui/dialog` directly.
  ///
  /// **What this gives you:** focus trap, escape-key dismissal, click-outside
  /// dismissal, ARIA semantics — all handled by `bits-ui` at the primitive
  /// layer. The wrapper only adds styling (centered overlay, white card,
  /// border) and a `title` slot for the heading.
  ///
  /// **What this doesn't yet give you:** trigger button, separate header /
  /// footer slots, or animations. Add when a second modal needs them — not
  /// preemptively (the AGENTS.md "don't add features beyond what the task
  /// requires" rule). Splitting into `DialogContent` / `DialogHeader` /
  /// `DialogFooter` along the shadcn-svelte pattern is reasonable when that
  /// happens; today the single composite is enough.
  import type { Snippet } from "svelte";
  import { Dialog as BitsDialog } from "bits-ui";

  type Props = {
    /// Two-way bound open state — caller controls the modal's visibility.
    open: boolean;
    title: string;
    /// Body content. Caller supplies whatever they want inside the card.
    children: Snippet;
    /// Optional callback when bits-ui asks to close (escape, click-outside,
    /// or the open binding flipping to false). Modal consumers typically
    /// treat this as "cancel."
    onClose?: () => void;
    /// Optional override for the content max-width. Defaults to `max-w-md`
    /// which matches the standalone CreateAgentForm layout.
    contentClass?: string;
  };

  let { open = $bindable(), title, children, onClose, contentClass }: Props = $props();

  function handleOpenChange(next: boolean): void {
    open = next;
    if (!next) onClose?.();
  }
</script>

<BitsDialog.Root {open} onOpenChange={handleOpenChange}>
  <BitsDialog.Portal>
    <BitsDialog.Overlay class="fixed inset-0 z-40 bg-black/40" data-testid="dialog-overlay" />
    <BitsDialog.Content
      class={[
        "fixed top-1/2 left-1/2 z-50 w-full -translate-x-1/2 -translate-y-1/2 rounded-md border border-neutral-200 bg-white shadow-lg",
        contentClass ?? "max-w-md",
      ].join(" ")}
      data-testid="dialog-content"
    >
      <BitsDialog.Title
        class="border-b border-neutral-200 px-4 py-3 text-sm font-semibold text-neutral-900"
        data-testid="dialog-title"
      >
        {title}
      </BitsDialog.Title>
      <div class="p-4">
        {@render children()}
      </div>
    </BitsDialog.Content>
  </BitsDialog.Portal>
</BitsDialog.Root>
