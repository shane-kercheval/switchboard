import { describe, expect, it } from "vitest";

import { inheritedHistoryNotice } from "./inheritedHistory";

describe("inheritedHistoryNotice", () => {
  it("gives a reason this build doesn't know general wording instead of nothing", () => {
    // A newer backend can add a reason; the marker must still explain itself.
    const notice = inheritedHistoryNotice("some_future_reason");
    expect(notice.title).toBe("Earlier conversation unavailable");
    expect(notice.detail).toContain("earlier conversation comes from");
  });

  it("names any session in the chain, not only the immediate parent", () => {
    // The chain can break at a grandparent, so no wording may claim the agent
    // was forked directly from the session that's missing.
    for (const reason of ["not_found", "changed", "ambiguous", "cycle"]) {
      expect(inheritedHistoryNotice(reason).detail).not.toContain("forked from");
    }
  });

  it("only tells the user the agent can't reply when the parent file is missing", () => {
    // Probed: Codex refuses a fork whose parent file is gone. No other reason
    // is known to stop the agent, so none may say it does.
    for (const reason of ["changed", "ambiguous", "cycle", "too_deep"]) {
      expect(inheritedHistoryNotice(reason).detail).not.toContain("can't continue");
    }
    expect(inheritedHistoryNotice("not_found").detail).toContain("can't continue");
  });
});
