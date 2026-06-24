---
name: analyze-ai-reviews
description: Distills one or more AI reviews into a decision-ready verdict, with a recommendation per finding.
arguments:
  - name: review
    description: The review feedback to analyze — one or more reviewers' findings, pasted or forwarded.
    required: true
---
You're given a review produced by an AI coding agent — either of an implementation plan or of written code. Your job is to evaluate it critically and turn it into a decision-ready analysis. Three parties use that analysis: the engineer makes the call (they know the domain but not necessarily this code), you — the coding agent — carry out the decisions, and the analysis is sometimes pasted back to the review agents to confirm alignment or resolve conflicting recommendations. The principles below govern how you weigh the feedback; the format governs how you present it.

**Core Principle** — Two failure modes, equally bad:

1. **Caving to effort.** Rejecting correct feedback because the better approach is more work. Effort is never a valid counterargument to technical correctness.
2. **Rubber-stamping the reviewer.** Accepting a point because it came from an AI agent that sounds authoritative. AI reviews contain real mistakes, overreach, and misjudged severity. Evaluate each point on its merits, not its confidence.

Both concern *technical* correctness. Neither overrides decisions that are legitimately the engineer's: scope, sequencing across milestones, and whether a user-facing outcome is worth shipping ahead of a cleaner internal design. Effort and timing are invalid as excuses against technical correctness but are *first-class* considerations for those engineer decisions. The format below separates the two so each gets the treatment it deserves — and so the engineer can see at a glance which decisions are actually theirs.

---

"""
{{review}}
"""

---

Apply the Core Principle above to every issue below. Re-examine the underlying code or plan before forming an opinion if needed — we optimize for a codebase that exemplifies best practices, not for minimal changes.

**Two layers, one for each kind of reader.** This analysis has two audiences. The engineer making the call knows the domain but not necessarily this code — they need what each finding means in system or user terms. You and the review agents already hold the technical detail; the engineer is the only reader who needs the functional translation, and the failure mode is you having that understanding and not surfacing it. So give each finding a **decision layer** written for the engineer — impact in system or user terms — and an **implementation layer** that does not re-teach the agents anything but pins the recommendation down precisely: exactly what changes and where, so the commitment is unambiguous when you act on it or a review agent checks it. Keep the implementation layer tight; do not re-explain mechanism already established in the review.

**Who owns the decision.** Tag every finding:

- **Agent's call** — purely technical. The right answer follows from best practices, correctness, or consistency with the existing codebase. The engineer is delegating this; state the call and proceed. *(e.g., replace a repeated manual state-flip with a single idempotent helper)*
- **Engineer's call** — affects what the system does, what users experience, what's in scope, or when it ships. There's a recommendation, but the call belongs to the engineer because it's a product/priority judgment, not a purely technical one. *(e.g., surface parser warnings in the UI now vs. defer to a later milestone)*

If a finding has both a technical fix and a scope or user-facing question, tag it **Engineer's call**, make the decision layer about the part the engineer owns, and put the technical resolution in the implementation layer. Do not tag a finding **Engineer's call** to avoid making a technical call you should make — a purely technical decision stays the agent's even when it's large.

## Output Format

For each issue:

```
### [Descriptive Topic Name] — [Category] — [Owner: Engineer's call | Agent's call]

**For the engineer:**
- **What's wrong & why it matters**: Plain language, in terms of system or user behavior — not code mechanics. Lead with the impact. Someone who knows the domain but not this codebase should finish this line understanding what breaks (or improves) and why it deserves their attention. Not "the dedupe check misses because ids are regenerated at parse time" — instead "reopening a project shows the user the same conversation twice; the fix de-duplicates so it doesn't."
- **Honest assessment**: Is the feedback actually right? Say so plainly — including when the reviewer is wrong, overreaching, or has misjudged severity. Your independent take, not a restatement of theirs.
- **The decision**: If *Agent's call* — state the call in one line and move on. If *Engineer's call* — frame the tradeoff functionally: what's gained, what it costs, and if it could be deferred, what shipping without it means for users or the system. Not "M2.6 vs M2.7" (unanswerable for the engineer) — "users keep hitting X until this is done" (answerable).
- **Recommendation**: `ACCEPT` | `REJECT` | `MODIFY` | `DISCUSS`

**Implementation detail** (engineer may skip):
- **Technical detail**: The precise mechanism — files, call sites, the actual defect or pattern. Preserve the original review's specificity here; this is the implementation record and the place for full technical depth.
- **Proposed change**: Specifically what changes. Reference files and functions for code reviews; plan sections and milestones for plan reviews.
- **Testing**: Which existing tests need updating; which new tests meaningfully cover this (behavior, edge cases, error conditions — not implementation details).
- **Open questions** (if any): What genuinely needs clarification before this can proceed.
```

**Category Definitions:**
- **correctness**: Bugs, wrong behavior, logic errors
- **design**: Architecture, patterns, abstractions, API shape
- **cleanup**: Naming, style, dead code, simplification
- **testing**: Missing tests, test quality, coverage gaps
- **security**: Vulnerabilities, input validation, auth issues
- **performance**: Efficiency, unnecessary work, scaling concerns
- **other**: Doesn't fit the above

For anything you recommend rejecting, explain why it isn't actually a better design — not merely that it isn't worth the effort. That reasoning is invalid against technical correctness; it is valid only as an *Engineer's call* scope decision and must be framed as one.

---

## Summary

| # | Topic | Category | Owner | Recommendation |
|---|-------|----------|-------|----------------|
| 1 | ... | correctness/design/cleanup/testing/security/performance/other | Engineer / Agent | ACCEPT/REJECT/MODIFY/DISCUSS |

---

**Rules:**
- Each topic appears exactly ONCE. Merge related feedback into a single topic.
- Group everything about one topic together; don't scatter it across sections.
- Share this analysis before implementing anything.
