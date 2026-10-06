<script lang="ts">
  /// The standard three-dot loading indicator: real spans with a staggered
  /// opacity keyframe — never an animated `…` glyph, which swaps characters
  /// and shifts layout. One animation everywhere it appears: live states
  /// (queued, working, no-response) all animate identically, and the label
  /// text beside it does the differentiating — the text a user is trying to
  /// read must never be the element that fades. Dots inherit `currentColor`,
  /// so they take the label's color for free. Under `prefers-reduced-motion`
  /// all three sit at full opacity, static.
  type Props = {
    class?: string;
  };

  let { class: className }: Props = $props();
</script>

<span class={["loading-dots", className].filter(Boolean).join(" ")} aria-hidden="true">
  <span></span>
  <span></span>
  <span></span>
</span>

<style>
  .loading-dots {
    display: inline-flex;
    align-items: center;
    gap: 3px;
  }
  .loading-dots > span {
    width: 4px;
    height: 4px;
    border-radius: 9999px;
    background-color: currentColor;
    animation: loading-dot 1.4s ease-in-out infinite;
  }
  .loading-dots > span:nth-child(2) {
    animation-delay: 0.16s;
  }
  .loading-dots > span:nth-child(3) {
    animation-delay: 0.32s;
  }
  @keyframes loading-dot {
    0%,
    100% {
      opacity: 0.2;
    }
    35% {
      opacity: 1;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .loading-dots > span {
      animation: none;
      opacity: 1;
    }
  }
</style>
