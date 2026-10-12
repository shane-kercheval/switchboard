import type { InheritedHistoryGap } from "./types";

/// What the transcript tells the user when a Codex fork's earlier conversation
/// can't be shown. The reasons mean different things for the agent itself, so
/// each says what still works: a missing file also stops Codex from running
/// the fork (probed, `docs/harness-behavior.md` §3.5), while a compressed one
/// doesn't.
export function inheritedHistoryNotice(reason: InheritedHistoryGap | string): {
  title: string;
  detail: string;
} {
  switch (reason) {
    case "not_found":
      return {
        title: "Earlier conversation unavailable",
        detail:
          "This agent was forked from a Codex session whose file is no longer in ~/.codex/sessions. Codex can't continue this agent until that file is restored, so its replies fail until then.",
      };
    case "compressed":
      return {
        title: "Earlier conversation not shown",
        detail:
          "This agent was forked from a Codex session that has since been compressed. The agent still has the earlier conversation, but Switchboard can't read compressed session files yet.",
      };
    case "changed":
      return {
        title: "Earlier conversation unavailable",
        detail:
          "The Codex session this agent was forked from has changed since the fork, so Switchboard can't tell which part of it this agent inherited.",
      };
    case "ambiguous":
      return {
        title: "Earlier conversation unavailable",
        detail:
          "More than one Codex session file carries the id of the session this agent was forked from, so Switchboard can't tell which one is the real one.",
      };
    case "cycle":
      return {
        title: "Earlier conversation unavailable",
        detail:
          "The chain of Codex sessions this agent was forked from loops back on itself, so Switchboard can't follow it.",
      };
    case "too_deep":
      return {
        title: "Earlier conversation unavailable",
        detail:
          "This agent is at the end of a longer chain of Codex forks than Switchboard follows (32), so its earlier conversation isn't shown.",
      };
    default:
      return {
        title: "Earlier conversation unavailable",
        detail: "Switchboard couldn't load the Codex session this agent was forked from.",
      };
  }
}
