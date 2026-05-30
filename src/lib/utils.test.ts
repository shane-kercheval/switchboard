import { describe, it, expect } from "vitest";
import { basename, relativeTime } from "./utils";

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

describe("relativeTime", () => {
  const now = new Date("2026-05-25T12:00:00Z");

  it("renders sub-minute as 'just now'", () => {
    expect(relativeTime("2026-05-25T11:59:30Z", now)).toBe("just now");
  });

  it("renders minutes, hours, days, and weeks", () => {
    expect(relativeTime("2026-05-25T11:30:00Z", now)).toBe("30m ago");
    expect(relativeTime("2026-05-25T09:00:00Z", now)).toBe("3h ago");
    expect(relativeTime("2026-05-23T12:00:00Z", now)).toBe("2d ago");
    expect(relativeTime("2026-05-11T12:00:00Z", now)).toBe("2w ago");
  });

  it("returns empty string for an unparseable timestamp", () => {
    expect(relativeTime("not-a-date", now)).toBe("");
  });
});
