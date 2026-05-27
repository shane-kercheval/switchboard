import { describe, expect, it } from "vitest";
import { renderMarkdown } from "$lib/markdown";

describe("renderMarkdown", () => {
  it("renders core Markdown constructs", () => {
    const html = renderMarkdown(
      [
        "# Heading",
        "",
        "Some **bold** and *italic* and `inline`.",
        "",
        "- one",
        "- two",
        "  - nested",
        "",
        "> a quote",
        "",
        "| a | b |",
        "| - | - |",
        "| 1 | 2 |",
        "",
        "[link](https://example.com)",
      ].join("\n"),
    );
    expect(html).toContain("<h1");
    expect(html).toContain("<strong>bold</strong>");
    expect(html).toContain("<em>italic</em>");
    expect(html).toContain("<code>inline</code>");
    expect(html).toContain("<ul>");
    expect(html).toContain("<li>");
    expect(html).toContain("<blockquote>");
    expect(html).toContain("<table>");
    expect(html).toContain('href="https://example.com"');
  });

  it("highlights fenced code and the token spans survive sanitization", () => {
    const html = renderMarkdown("```rust\nfn main() {}\n```");
    expect(html).toContain('class="language-rust"');
    // Prism token spans — proves both highlighting ran and DOMPurify kept `class`.
    expect(html).toContain('class="token');
    // Chrome: language badge + copy button (button outside <code>).
    expect(html).toContain("md-code-copy");
    expect(html).toContain("rust");
  });

  it("renders an unknown-language fence as escaped monospace without throwing", () => {
    const html = renderMarkdown("```nonsense-lang\n<not> & 'real'\n```");
    expect(html).toContain("<pre");
    expect(html).toContain("<code");
    // No grammar → escaped plain text, no token spans.
    expect(html).not.toContain('class="token');
    expect(html).toContain("&lt;not&gt;");
  });

  it("renders a fence with no language as monospace labeled 'text'", () => {
    const html = renderMarkdown("```\nplain\n```");
    expect(html).toContain("md-code-lang");
    expect(html).toContain("text");
    expect(html).not.toContain('class="token');
  });

  it("does not throw on incomplete/streaming Markdown", () => {
    expect(() => renderMarkdown("```rust\nlet x = ")).not.toThrow();
    expect(() => renderMarkdown("a paragraph with a dangling [link")).not.toThrow();
    expect(() => renderMarkdown("**bold without close")).not.toThrow();
    expect(renderMarkdown("```rust\nlet x = ")).toBeTruthy();
  });

  it("neutralizes embedded HTML/script payloads", () => {
    const html = renderMarkdown(
      "intro\n\n<script>alert('xss')</script>\n\n<img src=x onerror=alert(1)>",
    );
    expect(html).not.toContain("<script>");
    expect(html).not.toContain("onerror");
  });

  it("strips dangerous link schemes from anchor hrefs", () => {
    const html = renderMarkdown("[click](javascript:alert(1))");
    expect(html).not.toContain("javascript:");
  });
});
