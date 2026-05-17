import { describe, it, expect } from "vitest";
import { basename } from "./utils";

describe("basename", () => {
  it("returns the last path component for an absolute path", () => {
    expect(basename("/Users/x/repos/temp")).toBe("temp");
  });

  it("trims a single trailing slash", () => {
    expect(basename("/Users/x/repos/temp/")).toBe("temp");
  });

  it("returns the input when there is no slash", () => {
    expect(basename("just-a-name")).toBe("just-a-name");
  });

  it("handles dot-prefixed components", () => {
    expect(basename("/Users/x/.switchboard")).toBe(".switchboard");
  });
});
