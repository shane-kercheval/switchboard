import { describe, expect, it } from "vitest";
import { classifyKind, nextLabel } from "./attachments";

const labels = (...names: string[]): { label: string }[] => names.map((label) => ({ label }));

describe("nextLabel", () => {
  it("starts each kind at 1 in an empty draft", () => {
    expect(nextLabel("image", [])).toBe("image-1");
    expect(nextLabel("text", [])).toBe("text-1");
    expect(nextLabel("file", [])).toBe("file-1");
  });

  it("counts only labels of the same kind", () => {
    // Kinds number independently: two images and a text file coexist as
    // image-1, image-2, text-1.
    const existing = labels("image-1", "image-2", "text-1");
    expect(nextLabel("image", existing)).toBe("image-3");
    expect(nextLabel("text", existing)).toBe("text-2");
    expect(nextLabel("file", existing)).toBe("file-1");
  });

  it("never reuses a removed chip's number while a higher one survives", () => {
    // image-1 removed; image-2 must keep its name, so the next is image-3.
    expect(nextLabel("image", labels("image-2"))).toBe("image-3");
  });

  it("skips a gap rather than filling it", () => {
    // image-2 was removed from [1,2,3]. Refilling "2" would make the label
    // ambiguous against a message that already references the old image-2.
    expect(nextLabel("image", labels("image-1", "image-3"))).toBe("image-4");
  });

  it("restarts at 1 once the draft is emptied", () => {
    // Deliberate: with no surviving label there is nothing to collide with, and
    // a fresh draft reading `image-1` beats one reading `image-7`.
    expect(nextLabel("image", labels("image-1", "image-2"))).toBe("image-3");
    expect(nextLabel("image", [])).toBe("image-1");
  });

  it("ignores an unparseable suffix instead of poisoning the maximum", () => {
    // Hand-edited localStorage, or a format this build predates. `parseInt` would
    // yield NaN; NaN must not swallow the real maximum or propagate into the label.
    expect(nextLabel("image", labels("image-abc", "image-2"))).toBe("image-3");
    expect(nextLabel("image", labels("image-abc"))).toBe("image-1");
    expect(nextLabel("image", labels("image-"))).toBe("image-1");
  });

  it("is not confused by a kind whose name prefixes another", () => {
    // Guards the `startsWith` check: "file-" must not match "file2-" style names,
    // and an exact-prefix match is required.
    expect(nextLabel("file", labels("files-9"))).toBe("file-1");
  });

  it("tolerates a suffix with trailing junk the way parseInt does", () => {
    // `parseInt("2x")` is 2. Documented rather than defended against: the label is
    // ours to mint, so this only arises from external tampering, and taking the
    // leading digits is a safe reading.
    expect(nextLabel("image", labels("image-2x"))).toBe("image-3");
  });
});

describe("classifyKind", () => {
  it("classifies common image extensions as image", () => {
    for (const name of ["a.png", "B.JPG", "shot.jpeg", "icon.svg", "photo.heic", "x.webp"]) {
      expect(classifyKind(name)).toBe("image");
    }
  });

  it("classifies text/code extensions as text", () => {
    for (const name of ["notes.txt", "README.md", "data.json", "main.rs", "app.ts", "x.py"]) {
      expect(classifyKind(name)).toBe("text");
    }
  });

  it("falls back to file for unknown or extensionless names", () => {
    for (const name of ["archive.zip", "movie.mp4", "binary.bin", "Makefile", "noext"]) {
      expect(classifyKind(name)).toBe("file");
    }
  });

  it("is case-insensitive and ignores any directory portion", () => {
    expect(classifyKind("/Users/me/Pictures/Diagram.PNG")).toBe("image");
    expect(classifyKind("src/lib/main.TS")).toBe("text");
  });
});
