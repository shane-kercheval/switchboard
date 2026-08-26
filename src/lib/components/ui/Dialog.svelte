<script lang="ts">
  /// Thin wrapper around `bits-ui` Dialog. Single import surface so future
  /// modals (settings, etc.) reuse the same primitive without each
  /// component re-importing `bits-ui/dialog` directly.
  ///
  /// **What this gives you:** focus trap, escape-key dismissal, click-outside
  /// dismissal, ARIA semantics — all handled by `bits-ui` at the primitive
  /// layer. The wrapper adds styling (centered overlay, card, border), a
  /// `title` slot for the heading, and the height discipline every modal needs:
  /// the card is capped to the viewport and its **body** scrolls, so a tall
  /// form can't push its own header and submit button off both screen edges.
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
    /// Extra classes on the backdrop overlay — e.g. `backdrop-blur-sm` to blur
    /// the content behind the modal.
    overlayClass?: string;
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
    overlayClass,
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
    <BitsDialog.Overlay
      class={cn("fixed inset-0 z-40 bg-black/40", overlayClass)}
      data-testid="dialog-overlay"
    />
    <BitsDialog.Content
      class={cn(
        // Height is capped and the *body* scrolls, not the whole card. The
        // card is centered with -translate-y-1/2, so an unbounded tall body
        // overflows off **both** edges at once — the title bar disappears
        // upward while the submit button disappears downward, and neither is
        // reachable. `100dvh` rather than `100vh` so a mobile/dynamic toolbar
        // can't reintroduce the clip.
        "border-border/90 bg-raised fixed top-1/2 left-1/2 z-50 flex max-h-[calc(100dvh-2rem)] w-[calc(100vw-2rem)] -translate-x-1/2 -translate-y-1/2 flex-col rounded-lg border shadow-[0_18px_60px_rgba(0,0,0,0.22)]",
        contentClass ?? "max-w-md",
      )}
      data-testid="dialog-content"
      onEscapeKeydown={(e) => {
        if (!dismissible) e.preventDefault();
      }}
      onInteractOutside={(e) => {
        if (!dismissible) e.preventDefault();
      }}
      {onOpenAutoFocus}
    >
      <!-- `shrink-0` keeps the title and ✕ pinned while the body scrolls; a
           dismiss control that scrolls out of reach is the failure this whole
           arrangement exists to prevent. -->
      <div
        class="border-border/80 flex shrink-0 items-center justify-between gap-3 border-b px-4 py-3"
      >
        <BitsDialog.Title class="text-fg text-sm font-semibold" data-testid="dialog-title">
          {title}
        </BitsDialog.Title>
        {#if dismissible}
          <BitsDialog.Close
            class={ICON_BUTTON_CLASS}
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
      <!-- `min-h-0` is load-bearing, not defensive: a flex child defaults to
           `min-height: auto`, which refuses to shrink below its content, so
           `overflow-y-auto` would never engage and the card would grow past the
           cap above. Consumers that scroll their own sub-regions (the Messages
           navigator, the command palette) size those under this cap and so
           never reach this scrollbar. -->
      <div class="min-h-0 flex-1 overflow-y-auto p-4">
        {@render children()}
      </div>
    </BitsDialog.Content>
  </BitsDialog.Portal>
</BitsDialog.Root>
