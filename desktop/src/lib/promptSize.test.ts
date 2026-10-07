import { describe, expect, it } from "vitest";
import {
  MAX_PROMPT_BYTES,
  oversizedPromptMessage,
  promptTooLarge,
  utf8ByteLength,
} from "$lib/promptSize";

describe("promptTooLarge", () => {
  it("accepts text up to the limit and refuses one byte over", () => {
    expect(promptTooLarge("", [])).toBeNull();
    expect(promptTooLarge("x".repeat(MAX_PROMPT_BYTES), [])).toBeNull();
    const refusal = promptTooLarge("x".repeat(MAX_PROMPT_BYTES + 1), []);
    expect(refusal).toContain("message is 768 KB; the agent CLI accepts at most 768 KB");
    expect(refusal).toContain("Attach the text as a file instead.");
  });

  it("counts UTF-8 bytes, not characters", () => {
    const text = "€".repeat(MAX_PROMPT_BYTES / 3 + 1);
    expect(text.length).toBeLessThan(MAX_PROMPT_BYTES);
    expect(utf8ByteLength(text)).toBeGreaterThan(MAX_PROMPT_BYTES);
    expect(promptTooLarge(text, [])).not.toBeNull();
  });

  it("counts the attachment footer the backend appends", () => {
    const text = "x".repeat(MAX_PROMPT_BYTES);
    expect(promptTooLarge(text, [])).toBeNull();
    expect(
      promptTooLarge(text, [{ label: "text-1", path: "/attachments/uuid__pasted-1.txt" }]),
    ).not.toBeNull();
  });

  it("reports the size in whole KB", () => {
    expect(oversizedPromptMessage(1_354_608)).toContain("message is 1322 KB");
    expect(oversizedPromptMessage(MAX_PROMPT_BYTES)).toBeNull();
  });
});
