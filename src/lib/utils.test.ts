import { describe, it, expect } from "vitest";
import {
  basename,
  compareIsoTimestampsAscending,
  compareIsoTimestampsDescending,
  currentIsoTimestamp,
  formatDuration,
  formatHomePath,
  formatResetCountdown,
  formatTokens,
  formatUsedPercent,
  isIsoTimestampAfter,
  isIsoTimestampBefore,
  relativeTime,
} from "./utils";

describe("RFC 3339 timestamp ordering", () => {
  it("orders whole-second and variable-precision fractions chronologically", () => {
    expect(
      compareIsoTimestampsAscending("2026-08-07T12:00:00Z", "2026-08-07T12:00:00.500Z"),
    ).toBeLessThan(0);
    expect(
      compareIsoTimestampsAscending("2026-08-07T12:00:00.123Z", "2026-08-07T12:00:00.123456Z"),
    ).toBeLessThan(0);
    expect(
      compareIsoTimestampsAscending(
        "2026-08-07T12:00:00.123456Z",
        "2026-08-07T12:00:00.123456789Z",
      ),
    ).toBeLessThan(0);
  });

  it("treats equivalent instants with different offsets and precision as equal", () => {
    expect(
      compareIsoTimestampsAscending("2026-08-07T12:00:00Z", "2026-08-07T13:00:00.000+01:00"),
    ).toBe(0);
  });

  it("preserves sub-millisecond ordering", () => {
    expect(
      compareIsoTimestampsAscending("2026-08-07T12:00:00.123456Z", "2026-08-07T12:00:00.123457Z"),
    ).toBeLessThan(0);
  });

  it("sorts valid instants before invalid values in either direction", () => {
    expect(compareIsoTimestampsAscending("2026-08-07T12:00:00Z", "invalid-b")).toBeLessThan(0);
    expect(compareIsoTimestampsDescending("2026-08-07T12:00:00Z", "invalid-b")).toBeLessThan(0);
    expect(compareIsoTimestampsDescending("invalid-a", "invalid-b")).toBeLessThan(0);
  });

  it("orders valid instants newest-first with the descending comparator", () => {
    expect(
      compareIsoTimestampsDescending("2026-08-07T12:00:00.500Z", "2026-08-07T12:00:00Z"),
    ).toBeLessThan(0);
  });

  it("selects valid earlier and later candidates without promoting invalid values", () => {
    const earlier = "2026-08-07T12:00:00Z";
    const later = "2026-08-07T12:00:00.500Z";
    expect(isIsoTimestampAfter(later, earlier)).toBe(true);
    expect(isIsoTimestampBefore(earlier, later)).toBe(true);
    expect(isIsoTimestampAfter("invalid", earlier)).toBe(false);
    expect(isIsoTimestampBefore("invalid", later)).toBe(false);
    expect(isIsoTimestampAfter(later, "invalid")).toBe(true);
    expect(isIsoTimestampBefore(earlier, "invalid")).toBe(true);
    expect(isIsoTimestampAfter("invalid-a", "invalid-b")).toBe(false);
  });
});

describe("formatDuration", () => {
  it("formats bare seconds under a minute", () => {
    expect(formatDuration(9_000)).toBe("9s");
    expect(formatDuration(59_000)).toBe("59s");
  });

  it("formats minutes and seconds from one minute up, with padded seconds", () => {
    expect(formatDuration(60_000)).toBe("1m 00s");
    expect(formatDuration(2 * 60_000 + 3_000)).toBe("2m 03s");
  });

  it("formats hours and minutes past an hour with padded minutes", () => {
    expect(formatDuration(60 * 60_000 + 4 * 60_000)).toBe("1h 04m");
    expect(formatDuration(2 * 60 * 60_000 + 30 * 60_000)).toBe("2h 30m");
  });

  it("clamps negative and non-finite inputs to zero", () => {
    expect(formatDuration(-5_000)).toBe("0s");
    expect(formatDuration(Number.NaN)).toBe("0s");
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

describe("formatTokens", () => {
  it("renders exact counts below a thousand", () => {
    expect(formatTokens(0)).toBe("0");
    expect(formatTokens(990)).toBe("990");
  });

  it("renders one decimal below 10k and whole thousands above", () => {
    expect(formatTokens(1000)).toBe("1k");
    expect(formatTokens(2340)).toBe("2.3k");
    expect(formatTokens(23_400)).toBe("23k");
    expect(formatTokens(121_100)).toBe("121k");
    expect(formatTokens(200_000)).toBe("200k");
  });

  it("promotes to millions when the rounded thousands reach 1000", () => {
    // The tier has to be picked after rounding, not before: 999,500 scales to
    // 999.5, which rounds to 1000 and would print "1000k" — the exact reading
    // the M suffix exists to avoid. Both sides of the hand-off are pinned.
    expect(formatTokens(999_499)).toBe("999k");
    expect(formatTokens(999_500)).toBe("1M");
    expect(formatTokens(999_999)).toBe("1M");
  });

  it("applies the same rounding rule in millions as in thousands", () => {
    expect(formatTokens(1_000_000)).toBe("1M");
    expect(formatTokens(1_250_000)).toBe("1.3M");
    expect(formatTokens(12_000_000)).toBe("12M");
  });
});

describe("formatUsedPercent", () => {
  it("rounds to a whole percent", () => {
    expect(formatUsedPercent(0)).toBe("0%");
    expect(formatUsedPercent(0.1)).toBe("10%");
    expect(formatUsedPercent(0.666)).toBe("67%");
  });

  it("reports over-full usage rather than clamping", () => {
    // Only a bar's fill clamps; a quota that says 103% used is stating a fact.
    expect(formatUsedPercent(1.03)).toBe("103%");
  });

  it("matches the Codex rate-limit cell's rounding for whole-percent sources", () => {
    // That cell rendered `usedPercent.toFixed(0)` off a 0-100 number; the meter
    // takes a 0-1 fraction. Parity holds for whole percents, which is what the
    // claim is limited to: dividing by 100 and multiplying back is not exact,
    // so a source of 28.5 renders "29" directly and "28" through the fraction
    // (28.5 / 100 * 100 is 28.499999999999996). Not asserted here — when the
    // Codex windows move onto the meter, that conversion picks its own rounding
    // and this should not have frozen the by-product of the current one.
    for (const usedPercent of [0, 7, 42, 99, 100]) {
      expect(formatUsedPercent(usedPercent / 100)).toBe(`${usedPercent}%`);
    }
  });
});

describe("formatResetCountdown", () => {
  const NOW = new Date("2026-09-17T12:00:00Z");
  const ms = (iso: string): number => new Date(iso).getTime();

  it("counts down in minutes under an hour", () => {
    expect(formatResetCountdown(ms("2026-09-17T12:16:00Z"), NOW)).toBe("in 16 min");
  });

  it("rounds a sub-minute reset up rather than showing zero", () => {
    expect(formatResetCountdown(ms("2026-09-17T12:00:20Z"), NOW)).toBe("in 1 min");
  });

  it("counts down in whole hours under a day", () => {
    expect(formatResetCountdown(ms("2026-09-17T15:00:00Z"), NOW)).toBe("in 3 h");
    expect(formatResetCountdown(ms("2026-09-17T15:59:00Z"), NOW)).toBe("in 3 h");
  });

  it("counts down in whole days beyond a day", () => {
    // Relative at every distance, so the weekly window's reset stays short
    // enough to sit beside its label in the card column.
    expect(formatResetCountdown(ms("2026-09-18T12:00:00Z"), NOW)).toBe("in 1 d");
    expect(formatResetCountdown(ms("2026-09-22T12:00:00Z"), NOW)).toBe("in 5 d");
    expect(formatResetCountdown(ms("2026-09-24T08:00:00Z"), NOW)).toBe("in 6 d");
  });

  it("crosses from hours to days at 24 hours", () => {
    expect(formatResetCountdown(ms("2026-09-18T11:59:00Z"), NOW)).toBe("in 23 h");
    expect(formatResetCountdown(ms("2026-09-18T12:00:00Z"), NOW)).toBe("in 1 d");
  });

  it("renders a passed reset as 'now', never a negative countdown", () => {
    expect(formatResetCountdown(ms("2026-09-17T09:30:00Z"), NOW)).toBe("now");
    expect(formatResetCountdown(NOW.getTime(), NOW)).toBe("now");
  });
});
