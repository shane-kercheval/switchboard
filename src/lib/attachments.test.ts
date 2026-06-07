import { describe, expect, it } from "vitest";
import { classifyKind } from "./attachments";

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
