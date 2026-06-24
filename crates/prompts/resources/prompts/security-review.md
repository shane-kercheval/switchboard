---
name: security-review
description: Review the current uncommitted changes for security vulnerabilities, with a high bar for real, exploitable issues.
arguments:
  - name: context
    description: Optional background — what the change does, the trust boundary it touches, or areas to scrutinize.
    required: false
---
Review the current uncommitted changes in this repository for **security vulnerabilities**.

{% if context %}Context for this review: {{ context }}

Weigh the review against that context — scrutinize the trust boundaries it names, and don't flag what it explicitly rules out of scope.
{% else %}No extra context was provided. Infer the change's purpose and trust boundary from the diff itself before judging it.
{% endif %}
Focus on vulnerabilities this change **newly introduces** — not pre-existing issues in untouched code. Examine, in particular:

1. **Input handling.** Injection of every kind — SQL/NoSQL, OS command, path traversal, XXE, template, and cross-site scripting — wherever untrusted input reaches a sink.
2. **Authentication & authorization.** Auth bypass, missing or wrong access checks, privilege escalation, and broken session or token handling.
3. **Secrets & cryptography.** Hardcoded credentials or keys, weak or misused algorithms, predictable randomness, and improper key or secret storage.
4. **Unsafe execution & deserialization.** `eval`-style execution, unsafe deserialization (pickle, YAML, native objects), and untrusted code paths.
5. **Sensitive-data exposure.** PII or secrets in logs, error messages, or API responses, and debug surfaces left reachable.

Hold a high bar: only report an issue when you're confident it is **actually exploitable**, with a concrete path from untrusted input to impact. Do **not** report denial-of-service, rate-limiting, resource-exhaustion, or general hardening suggestions, and do not flag missing validation on fields with no security consequence. Better to miss a theoretical issue than to bury a real one in noise.

For each finding: name the **file and location**, describe the **vulnerability and how it's exploited**, rate its **severity** (critical / high / medium / low), and give a **concrete fix**. Lead with the highest-severity findings. If the change introduces no real security issues, say so directly rather than inventing concerns.
