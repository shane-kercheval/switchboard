import { describe, it, expect } from "vitest";
import {
  basename,
  currentIsoTimestamp,
  formatDuration,
  formatHomePath,
  relativeTime,
} from "./utils";

describe("formatDuration", () => {
  it("formats minutes and seconds under an hour with padded seconds", () => {
    expect(formatDuration(2 * 60_000 + 3_000)).toBe("2m 03s");
    expect(formatDuration(45_000)).toBe("0m 45s");
  });

  it("formats hours and minutes past an hour with padded minutes", () => {
    expect(formatDuration(60 * 60_000 + 4 * 60_000)).toBe("1h 04m");
    expect(formatDuration(2 * 60 * 60_000 + 30 * 60_000)).toBe("2h 30m");
  });

  it("clamps negative and non-finite inputs to zero", () => {
    expect(formatDuration(-5_000)).toBe("0m 00s");
    expect(formatDuration(Number.NaN)).toBe("0m 00s");
  });
});

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

describe("formatHomePath", () => {
  it("shortens POSIX paths inside the supplied home directory", () => {
    expect(formatHomePath("/Users/shane/repos/switchboard", "/Users/shane")).toBe(
      "~/repos/switchboard",
    );
    expect(formatHomePath("/home/shane/repos/switchboard", "/home/shane/")).toBe(
      "~/repos/switchboard",
    );
  });

  it("renders the home directory itself as tilde", () => {
    expect(formatHomePath("/Users/shane", "/Users/shane")).toBe("~");
  });

  it("does not shorten paths outside the supplied home directory", () => {
    expect(formatHomePath("/Volumes/work/repos/switchboard", "/Users/shane")).toBe(
      "/Volumes/work/repos/switchboard",
    );
    expect(formatHomePath("/Users/shane-other/repos", "/Users/shane")).toBe(
      "/Users/shane-other/repos",
    );
  });

  it("shortens Windows paths case-insensitively while preserving separators", () => {
    expect(formatHomePath("C:\\Users\\Shane\\repos\\switchboard", "c:\\users\\shane")).toBe(
      "~\\repos\\switchboard",
    );
  });

  it("falls back to the full path without a home directory", () => {
    expect(formatHomePath("/Users/shane/repos/switchboard", null)).toBe(
      "/Users/shane/repos/switchboard",
    );
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

describe("currentIsoTimestamp", () => {
  it("accepts an injected clock for deterministic callers", () => {
    expect(currentIsoTimestamp(new Date("2026-05-25T12:00:00Z"))).toBe("2026-05-25T12:00:00.000Z");
  });
});
