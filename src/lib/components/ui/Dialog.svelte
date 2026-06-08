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
  import { cn } from "$lib/utils";
  import { ICON_BUTTON_CLASS } from "$lib/components/ui/iconButton";

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
    /// When false, the modal can't be dismissed (escape, click-outside, or the
    /// header ✕ are all suppressed). Used to keep a modal up while an
    /// irreversible action it kicked off is mid-flight — e.g. the New Project
    /// dialog during agent auto-seeding, so the user can't navigate away into a
    /// partially-created project. Defaults to true (normal dismissible modal).
    dismissible?: boolean;
    /// Override where focus lands when the modal opens. bits-ui otherwise focuses
    /// the first focusable element (the header ✕). Call `event.preventDefault()`
    /// and focus a specific element instead — e.g. the command palette focuses
    /// its search field so the user can type immediately.
    onOpenAutoFocus?: (event: Event) => void;
  };

  let {
    open = $bindable(),
    title,
    children,
    onClose,
    contentClass,
    dismissible = true,
    onOpenAutoFocus,
  }: Props = $props();

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
        "border-border/90 bg-raised fixed top-1/2 left-1/2 z-50 w-[calc(100vw-2rem)] -translate-x-1/2 -translate-y-1/2 rounded-lg border shadow-[0_18px_60px_rgba(0,0,0,0.22)]",
        contentClass ?? "max-w-md",
      ].join(" ")}
      data-testid="dialog-content"
      onEscapeKeydown={(e) => {
        if (!dismissible) e.preventDefault();
      }}
      onInteractOutside={(e) => {
        if (!dismissible) e.preventDefault();
      }}
      {onOpenAutoFocus}
    >
      <div class="border-border/80 flex items-center justify-between gap-3 border-b px-4 py-3">
        <BitsDialog.Title class="text-fg text-sm font-semibold" data-testid="dialog-title">
          {title}
        </BitsDialog.Title>
        {#if dismissible}
          <BitsDialog.Close
            class={cn(ICON_BUTTON_CLASS, "hover:bg-panel")}
            aria-label="Close dialog"
            data-testid="dialog-close"
          >
            <svg
              width="16"
              height="16"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="2"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <path d="M18 6 6 18M6 6l12 12" />
            </svg>
          </BitsDialog.Close>
        {/if}
      </div>
      <div class="p-4">
        {@render children()}
      </div>
    </BitsDialog.Content>
  </BitsDialog.Portal>
</BitsDialog.Root>
