---
name: code-review
description: Review the current uncommitted changes for correctness, design, and risk.
arguments:
  - name: context
    description: Optional background — what the change is for, areas to focus on, or constraints to weigh.
    required: false
---
Review the current uncommitted changes in this repository.

{% if context %}Context for this review: {{ context }}

Weigh the review against that context — call out anything that conflicts with it, and don't flag things it explicitly rules out of scope.
{% else %}No extra context was provided. Infer the intent of the change from the diff itself before judging it.
{% endif %}
Work through these dimensions, in order of importance:

1. **Correctness.** Logic errors, off-by-one and boundary mistakes, mishandled error paths, race conditions, and broken assumptions. Does the change do what its surrounding code implies it should?
2. **Design fit.** Does it match the patterns and boundaries already in this codebase, or does it cut across them? Flag new abstractions that aren't yet earned and duplication that should be shared.
3. **Tests.** Are the behaviors that matter covered — including edge cases and failure paths — or is the coverage trivial? Note missing tests for risky logic.
4. **Risk & clarity.** Security-sensitive handling, performance cliffs (needless O(n²), repeated work), and anything a future reader would misread.

For each issue: name the **file and location**, state the **concern** plainly, rate its **severity** (blocker / should-fix / nit), and give a **concrete suggested fix**. Lead with the highest-severity findings. If the change is sound, say so directly rather than inventing nits.
