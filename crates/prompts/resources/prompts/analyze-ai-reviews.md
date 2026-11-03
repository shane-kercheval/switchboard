---
name: analyze-ai-reviews
description: Evaluates one or more AI reviews of a plan or code change point by point, confirming or refuting each finding against the code or plan, and turns them into a decision-ready analysis that says who owns each decision.
arguments:
  - name: review
    description: The review feedback to analyze, from one or more reviewers, pasted or forwarded.
    required: true
---
# Evaluate AI review feedback

You're given one or more reviews produced by AI agents, of an implementation plan, a project plan, or written code. Evaluate them critically and turn them into a decision-ready analysis. Three parties use the analysis: the engineer makes the call (they know the domain but not necessarily this code), you carry out the decisions, and the analysis is sometimes pasted back to the review agents to confirm alignment or resolve conflicting recommendations.

**The reviews:**

"""
{{review}}
"""

**Don't change anything yet.** Share this analysis before implementing anything.

## Core principle

Two failure modes, equally bad:

1. **Caving to effort.** Rejecting correct feedback because the better approach is more work. Effort is never a valid counterargument to technical correctness.
2. **Rubber-stamping the reviewer.** Accepting a point because it came from an AI agent that sounds authoritative. AI reviews contain real mistakes, overreach, and misjudged severity. Evaluate each point on its merits, not its confidence.

Both concern technical correctness. Neither overrides decisions that are legitimately the engineer's: scope, sequencing across milestones, and whether a user-facing outcome is worth shipping ahead of a cleaner internal design. Effort and timing are invalid as excuses against technical correctness, but they're first-class considerations for those decisions. The format below separates the two, so the engineer can see at a glance which decisions are actually theirs.

We optimize for work that exemplifies best practices, not for minimal changes.

## Check every point

Before forming an opinion on a finding, check it against the actual code or plan. Read the code path or plan section it refers to, and cite it. For a claimed bug, show it where you can: a failing input, a one-off command, a single test, or a short script. Don't run the test suite, linters, or type checkers. Each finding's assessment then starts with one of:

- **Confirmed:** the evidence shows the reviewer is right.
- **Refuted:** the evidence shows the reviewer is wrong. Say what shows it.
- **Couldn't check:** say what would settle it. Never drop a finding because you couldn't check it.

## More than one review

If you're given several reviews, for example from two tools:

- Merge findings that describe the same issue into one topic, and note which reviewers raised it. A finding raised independently by more than one reviewer is a stronger signal.
- Where reviewers disagree, say so, and say which side the evidence supports.

## Two layers, one for each kind of reader

The engineer making the call knows the domain but not necessarily this code. They need what each finding means in system or user terms. You and the review agents already hold the technical detail. The engineer is the only reader who needs the functional translation, and the failure mode is you having that understanding and not surfacing it. So give each finding a **decision layer** written for the engineer (impact in system or user terms) and an **implementation layer** that doesn't re-teach the agents anything but pins the recommendation down precisely: exactly what changes and where, so the commitment is unambiguous when you act on it or a review agent checks it. Keep the implementation layer tight. Don't re-explain mechanism the review already established.

## Who owns the decision

Tag every finding:

- **Agent's call:** purely technical. The right answer follows from best practices, correctness, or consistency with the existing code. The engineer is delegating this. State the call and proceed. For example: replace a repeated manual state flip with a single idempotent helper.
- **Engineer's call:** affects what the system does, what users experience, what's in scope, or when it ships. There's a recommendation, but the call belongs to the engineer because it's a product or priority judgment, not a purely technical one. For example: surface parser warnings in the UI now, or defer them to a later milestone. A fix that would break existing callers, stored data, or a public API is always the engineer's call.

If a finding has both a technical fix and a scope or user-facing question, tag it **Engineer's call**, make the decision layer about the part the engineer owns, and put the technical resolution in the implementation layer. Don't tag a finding **Engineer's call** to avoid making a technical call you should make. A purely technical decision stays the agent's even when it's large.

## Output format

For each issue:

```
### [Descriptive topic name] | [Category] | [Owner: Engineer's call | Agent's call]

**For the engineer:**
- **What's wrong and why it matters:** plain language, in terms of system or user behavior, not code mechanics. Lead with the impact. Someone who knows the domain but not this codebase should finish this line understanding what breaks (or improves) and why it deserves their attention. Not "the dedupe check misses because ids are regenerated at parse time", but "reopening a project shows the user the same conversation twice; the fix de-duplicates so it doesn't."
- **Honest assessment:** Confirmed, Refuted, or Couldn't check, with the evidence. Then your independent take: is the feedback actually right? Say so plainly, including when the reviewer is wrong, overreaching, or has misjudged severity. Name which reviewers raised it, if there are several.
- **The decision:** if Agent's call, state the call in one line and move on. If Engineer's call, frame the tradeoff functionally: what's gained, what it costs, and, if it could be deferred, what shipping without it means for users or the system. Not "M2.6 vs M2.7" (unanswerable for the engineer) but "users keep hitting X until this is done" (answerable).
- **Recommendation:** ACCEPT | REJECT | MODIFY | DISCUSS

**Implementation detail** (the engineer may skip):
- **Technical detail:** the precise mechanism: files, call sites, the actual defect or pattern. Keep the original review's specificity here. This is the implementation record and the place for full technical depth.
- **Proposed change:** specifically what changes. Reference files and functions for code reviews, and plan sections and milestones for plan reviews.
- **Testing:** which existing tests need updating, and which new tests meaningfully cover this (behavior, edge cases, error conditions, not implementation details). For a plan review, how the plan's evidence of done should change.
- **Open questions** (if any): what genuinely needs clarifying before this can proceed.
```

Categories:

- **correctness:** bugs, wrong behavior, logic errors.
- **design:** architecture, patterns, abstractions, API shape, milestone structure.
- **cleanup:** naming, style, dead code, simplification.
- **testing:** missing tests, test quality, coverage gaps, weak evidence of done.
- **security:** vulnerabilities, input validation, auth, data exposure.
- **performance:** efficiency, unnecessary work, scaling.
- **other:** doesn't fit the above.

For anything you recommend rejecting, explain why it isn't actually a better design, not merely that it isn't worth the effort. That reasoning is invalid against technical correctness. It's valid only as an Engineer's call scope decision, and must be framed as one.

## Summary

End with a table:

| # | Topic | Category | Owner | Assessment | Recommendation |
|---|---|---|---|---|---|
| 1 | ... | correctness, design, cleanup, testing, security, performance, or other | Engineer or Agent | Confirmed, Refuted, or Couldn't check | ACCEPT, REJECT, MODIFY, or DISCUSS |

## Rules

- Each topic appears exactly once. Merge related feedback into a single topic.
- Group everything about one topic together. Don't scatter it across sections.
- Share this analysis before implementing anything. Once the engineer decides, keep the rejected findings and their reasons, so the next reviewer doesn't raise them again.
