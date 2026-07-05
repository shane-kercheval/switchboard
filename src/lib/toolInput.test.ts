import { describe, expect, it } from "vitest";
import { formatToolInput, toolInputPreview } from "./toolInput";

describe("tool input formatting", () => {
  it("uses a model-authored description for the row preview", () => {
    expect(
      toolInputPreview({
        description: "Commit and push ledger accuracy batch",
        command: "git commit && git push",
      }),
    ).toBe("Commit and push ledger accuracy batch");
  });

  it("falls back to common actionable fields", () => {
    expect(toolInputPreview({ command: "pnpm test\n" })).toBe("pnpm test");
    expect(toolInputPreview({ file_path: "/tmp/example.txt", old_string: "before" })).toBe(
      "/tmp/example.txt",
    );
    expect(toolInputPreview(["git", "status", "--short"])).toBe("git status --short");
  });

  it("uses compact JSON when no preferred field exists", () => {
    expect(toolInputPreview({ limit: 10, recursive: true })).toBe('{"limit":10,"recursive":true}');
  });

  it("pretty-prints full input while redacting obvious sensitive keys", () => {
    expect(
      formatToolInput({
        command: "curl",
        api_key: "sk-123",
        accessToken: "token-123",
        authToken: "token-456",
        sessionCookie: "sid=abc",
        cookie: "theme=light",
        session_id: "non-secret-id",
        nested: { authorization: "Bearer token" },
      }),
    ).toBe(
      JSON.stringify(
        {
          command: "curl",
          api_key: "[redacted]",
          accessToken: "[redacted]",
          authToken: "[redacted]",
          sessionCookie: "[redacted]",
          cookie: "[redacted]",
          session_id: "non-secret-id",
          nested: { authorization: "[redacted]" },
        },
        null,
        2,
      ),
    );
  });

  it("redacts common inline secrets in preview strings", () => {
    expect(
      toolInputPreview({
        command:
          'curl -H "Authorization: Bearer sk-live-abc123456789" -H "Cookie: sid=abc; theme=light" "https://example.test?api_key=secret123&token=secret456"',
      }),
    ).toBe(
      'curl -H "Authorization: Bearer [redacted]" -H "Cookie: [redacted]" "https://example.test?api_key=[redacted]&token=[redacted]"',
    );
  });

  it("redacts common inline secrets in formatted input", () => {
    expect(
      formatToolInput({
        command: "env ACCESS_TOKEN=secret123 curl 'https://example.test?auth_token=secret456'",
        prompt: "Use key sk_test_abc123456789 carefully",
      }),
    ).toBe(
      JSON.stringify(
        {
          command: "env ACCESS_TOKEN=[redacted] curl 'https://example.test?auth_token=[redacted]'",
          prompt: "Use key [redacted] carefully",
        },
        null,
        2,
      ),
    );
  });
});
