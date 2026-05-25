<script lang="ts">
  import type { HarnessKind } from "$lib/types";
  import { HARNESS_LABEL } from "$lib/harnessDisplay";
  import { cn } from "$lib/utils";
  import claudeCodeIcon from "$lib/assets/harness-icons/claude-code.png";
  import codexIcon from "$lib/assets/harness-icons/codex.png";
  import geminiIcon from "$lib/assets/harness-icons/gemini.svg";
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
    gemini: geminiIcon,
    antigravity: antigravityIcon,
  };

  const SIZE_CLASS: Record<NonNullable<Props["size"]>, string> = {
    sm: "h-6 w-6",
    md: "h-8 w-8",
  };
</script>

<img
  src={ICON_SRC[harness]}
  alt={HARNESS_LABEL[harness]}
  title={HARNESS_LABEL[harness]}
  data-testid={testid}
  class={cn("shrink-0 rounded-[5px] object-contain", SIZE_CLASS[size], className)}
/>
