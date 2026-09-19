<script lang="ts">
  import type { HarnessKind } from "$lib/types";
  import { HARNESS_LABEL } from "$lib/harnessDisplay";
  import { cn } from "$lib/utils";
  import claudeCodeIcon from "$lib/assets/harness-icons/claude-code.png";
  import codexIcon from "$lib/assets/harness-icons/codex.png";
  import antigravityIcon from "$lib/assets/harness-icons/antigravity.png";

  type Props = {
    harness: HarnessKind;
    size?: "sm" | "md";
    testid?: string;
    class?: string;
  };

  let { harness, size = "sm", testid, class: className }: Props = $props();

  const ICON_SRC: Record<HarnessKind, string> = {
    claude_code: claudeCodeIcon,
    codex: codexIcon,
    antigravity: antigravityIcon,
  };

  const SIZE_CLASS: Record<NonNullable<Props["size"]>, string> = {
    sm: "h-6 w-6",
    md: "h-8 w-8",
  };

  const SCALE_CLASS: Record<HarnessKind, string> = {
    claude_code: "scale-105",
    codex: "scale-125",
    antigravity: "scale-125",
  };
</script>

<span
  class={cn(
    "bg-panel relative shrink-0 [transform:translateZ(0)] overflow-hidden rounded-full [clip-path:circle(50%)]",
    SIZE_CLASS[size],
    className,
  )}
>
  <img
    src={ICON_SRC[harness]}
    alt={HARNESS_LABEL[harness]}
    data-testid={testid}
    class={cn("absolute inset-0 h-full w-full object-cover", SCALE_CLASS[harness])}
  />
</span>
