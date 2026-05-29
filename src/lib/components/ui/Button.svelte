<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLButtonAttributes } from "svelte/elements";
  import { cn } from "$lib/utils";

  type Variant = "primary" | "secondary" | "danger" | "ghost";
  type Size = "default" | "sm";

  type Props = HTMLButtonAttributes & {
    variant?: Variant;
    size?: Size;
    children: Snippet;
  };

  let {
    variant = "primary",
    size = "default",
    class: className,
    children,
    ...rest
  }: Props = $props();

  // Pill-shaped (`rounded-full`) to match the app's circular icon language.
  const base =
    "inline-flex items-center justify-center rounded-full font-medium transition-colors " +
    "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent " +
    "disabled:cursor-not-allowed disabled:opacity-50";

  // `primary` uses the brand accent (not a flat black) so the main action
  // carries the app's identity; `secondary` is a clean outline rather than a
  // gray fill. `danger` is the destructive variant — a solid fill via its own
  // `destructive` token (not the soft status-failed chip), so it carries the
  // same visual weight as `primary` and stays legible in both themes.
  const variants: Record<Variant, string> = {
    primary: "bg-accent text-accent-fg hover:bg-accent/90",
    secondary: "border border-border bg-transparent text-fg hover:bg-panel",
    danger: "bg-destructive text-destructive-fg hover:bg-destructive/90",
    ghost: "bg-transparent text-fg hover:bg-panel",
  };

  const BUTTON_SIZE_CLASS: Record<Size, string> = {
    default: "h-8 px-4 text-sm",
    sm: "h-7 px-3 text-xs",
  };
</script>

<button class={cn(base, variants[variant], BUTTON_SIZE_CLASS[size], className)} {...rest}>
  {@render children()}
</button>
