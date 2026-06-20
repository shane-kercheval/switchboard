---
name: ai-review-feedback
description: Analyze one or more AI code reviews into a decision-ready, de-duplicated verdict.
arguments:
  - name: review
    description: The review feedback to analyze — one or more reviewers' findings, pasted or forwarded.
    required: true
---
Below is review feedback on a code change, possibly from more than one reviewer. Your job is not to re-review the code — it is to **analyze the feedback itself** and turn it into a decision-ready verdict.

Review feedback:

{{ review }}

Produce your analysis in this structure:

1. **Consensus.** Issues more than one reviewer raised, or that are clearly correct on their face. These carry the most weight.
2. **Disputed or uncertain.** Points where reviewers disagree, or a single reviewer makes a claim you can't fully corroborate from the feedback alone. Say which way you lean and why.
3. **Low-value or wrong.** Findings that are nits, stylistic preference, or likely mistaken. Briefly say why, so they can be set aside with confidence.
4. **Recommended actions.** A prioritized, de-duplicated list of what to actually change — blockers first, then should-fix, then optional. Collapse overlapping findings into one action.

Be decisive. Where reviewers conflict, take a position rather than restating both sides. Distinguish what the evidence supports from what is judgment, and keep the final action list short enough to act on.
