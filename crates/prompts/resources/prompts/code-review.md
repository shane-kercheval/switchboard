---
name: code-review
description: Architectural code review — design, correctness, and real bugs, not style nits.
arguments:
  - name: context
    description: Optional background or focus — what the change is for, areas to concentrate on, or constraints to weigh. Supplements (does not replace) the review of the changes.
    required: false
---
# Code Review Guidelines

## Review Type

This is an architectural and code-quality review — not a line-by-line PR review focused on style. Focus on decisions with lasting impact on structure, maintainability, and correctness. Identify fundamental design flaws, architectural anti-patterns, structural issues, code smells, and actual bugs. Ignore minor stylistic or trivial formatting matters.

## Review Lenses

These are thinking tools, not a checklist. Apply the ones that surface real issues for *this* code; a lens with nothing to say is not a gap in the review. For every issue raised, state its consequence in system or user terms — what actually breaks, degrades, or becomes hard to change — not only the mechanism. The mechanism explains it to an engineer; the consequence is what makes it worth raising and is what downstream readers rely on.

- **Design fundamentals** — Does each component have one clear responsibility (SRP)? Can behavior be extended without modifying existing code (OCP)? Do high-level modules depend on abstractions, not implementations (DIP)? Are interfaces focused rather than forcing unnecessary dependencies (ISP)? Are abstractions at the right level — not too high, not too low — exposing only what's necessary with clear, well-defined contracts?
- **Coupling & cohesion** — Are modules loosely coupled with clear boundaries? Is related functionality grouped (high cohesion)? Can components be tested, modified, or replaced independently? Which changes would ripple across multiple components, and who owns which lifecycle?
- **State & side effects** — Is state management explicit and controlled? Are side effects isolated and predictable? Does this inappropriately create or modify global state? Is mutability used where immutability would be safer?
- **Error handling & resilience** — Are error conditions handled appropriately, or do they fail silently? Do errors propagate clearly? Are failure modes considered, with recovery or cleanup where needed? What breaks, and how does it surface?
- **Test adequacy** — Do the tests exercise the behavior that actually matters: core paths, the edge cases and error conditions the code is exposed to, and the failure modes identified under the lenses above? Where is coverage absent on something risky, and where is it present but shallow (asserting the happy path while the hard cases go untested)? Name the specific high-value tests missing — the ones that would catch a real defect or a likely regression — rather than cataloguing every untested function. Trivial, well-bounded code does not need tests called out; risky or central code with weak tests does. Coverage of low-risk code is not a finding; a critical path with tests that don't probe its failure modes is.
- **Ecosystem integration** — How do established libraries in this space solve this? What is idiomatic vs. fighting the ecosystem? Does this invent conventions where conventions exist, or create conflicts with common user setups?
- **Performance & scalability** — Are there O(n²) algorithms where O(n) is feasible? Will this pattern bottleneck under load? Are resources (connections, memory, threads) managed appropriately? Do costs scale reasonably with usage? Be specific — "holds the full result set in memory, fails past ~N rows," not "might not scale."

### Code-level hygiene

Architectural focus does not mean skipping low-level issues. Code smells that indicate a design problem or a latent bug are worth raising — they compound over time and are cheap to fix when caught early. Examples, not a checklist: functions whose return value every caller discards, dead parameters threaded through call sites but never read, duplicated logic that has subtly diverged, names that suggest different behavior than what happens, catch blocks that silently swallow context, functions quietly handling multiple unrelated concerns. Use judgment to spot patterns that smell wrong even if not listed.

### Latent-assumption check

For anything not obviously correct, ask what assumptions it rests on and whether they will hold as the system changes: "What assumptions could become problems later?" and "If this fails or needs to change, what's the blast radius?"

## Depth

**Go deep on:** core abstractions and boundaries, global state and lifecycle management, public APIs and contracts, error paths and failure modes, ecosystem-integration points, and patterns that repeat across the codebase (good or bad — a repeated good pattern is worth affirming so it's preserved; a repeated bad one is worth naming once as systemic rather than flagged N times).

**Skim past:** formatting and style that doesn't affect correctness, well-understood standard patterns used correctly, and isolated internal logic that is clearly correct and well-bounded.

## What to Raise

**Raise if:** the change would be expensive later but is trivial to address now; the pattern will force future decisions in problematic directions; it violates a principle that matters *for this context*; it conflicts with common ecosystem usage; it introduces a bug or error-prone pattern; a code smell indicates a design issue or latent bug; or a clearly better alternative exists.

**Skip if:** it's a stylistic preference without clear advantage; "could be done differently" without meaningful benefit; a micro-optimization without demonstrated need; or personal taste on a subjective matter.

When you raise an issue, include the recommended alternative alongside the problem — criticism plus direction, not criticism alone.

## Communicating Findings

Lead with what matters most. Group everything about one issue together; if a bug and a design concern share a root cause, address them together rather than splitting them artificially. Do not repeat the same issue across sections.

A complete review accounts for, but is not rigidly templated by, the following: what works well (strong patterns worth preserving or extending), critical issues (fundamental problems needing change, each with its recommended alternative), design concerns (questionable patterns worth discussing even if not clearly wrong), code smells, actual bugs, and the context that explains why a given issue matters here specifically. Organize these however serves clarity — the goal is a useful review, not a filled-in template. Omit what has nothing substantive behind it rather than padding to cover every category.

## Rules

- **Do not run tests, linters, or type-checkers, and do not modify code, commit, or open PRs** unless explicitly asked. This is a review of the code as written — spend the time reviewing, not re-verifying.
- **Do not create or modify any files** unless explicitly asked.

---

## What to Review

Base the review on the following, in order of priority:

- Uncommitted changes in the current directory
- If there are no uncommitted changes, then look for changes in the current branch vs main
- If neither applies, ask for clarification
{%- if context %}

Additional context and focus for this review:

"""
{{ context }}
"""

Concentrate where it points, and don't flag anything it explicitly rules out of scope. It supplements the review of the changes above — it does not replace it.
{%- endif %}
