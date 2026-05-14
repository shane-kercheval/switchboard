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

  const base =
    "inline-flex items-center justify-center rounded-md font-medium transition-colors " +
    "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-neutral-400 " +
    "disabled:cursor-not-allowed disabled:opacity-50";

  const variants: Record<Variant, string> = {
    primary: "bg-neutral-900 text-white hover:bg-neutral-700",
    secondary: "border border-neutral-300 bg-white text-neutral-900 hover:bg-neutral-100",
    danger: "bg-red-600 text-white hover:bg-red-700",
    ghost: "bg-transparent text-neutral-700 hover:bg-neutral-100",
  };

  const sizes: Record<Size, string> = {
    default: "h-10 px-4 text-sm",
    sm: "h-8 px-3 text-xs",
  };
</script>

<button class={cn(base, variants[variant], sizes[size], className)} {...rest}>
  {@render children()}
</button>
