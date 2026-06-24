---
name: security-review
description: Security review of the current changes — a high bar for real, exploitable vulnerabilities.
arguments:
  - name: context
    description: Optional background or focus — the change's purpose, the trust boundary it touches, or areas to scrutinize. Supplements (does not replace) the review of the changes.
    required: false
---
# Security Review Guidelines

## Review Type

This is a **security** review — not a general code-quality or style pass. Your job is to find vulnerabilities the change **introduces or newly exposes**: places where untrusted input can reach a dangerous operation, where a trust boundary is crossed without a check, or where sensitive data leaks. Hold a high bar for signal — a security review that buries one real, exploitable finding under ten theoretical ones is worse than useless, because the real one gets ignored.

Work from a **threat model**, not a checklist. For the code in scope, ask: *Where does data cross a trust boundary?* (network input, request parameters, file contents, environment, IPC, another service, a less-privileged caller). *What can an attacker control, and what's the most damaging thing they can reach with it?* Then trace concrete paths from attacker-controlled input to impact. A finding is only real if you can name that path.

## Review Lenses

Thinking tools, not a checklist — apply the ones that fit *this* code; a lens with nothing to say is not a gap. For every issue, give the **exploit path**: what an attacker controls, the step-by-step route from that input to impact, and the consequence (RCE, data exfiltration, auth bypass, privilege escalation). Mechanism without a reachable path is not a finding.

- **Input handling & injection** — Untrusted input reaching a sink without proper parameterization/encoding: SQL/NoSQL, OS command, path traversal, XXE, template (SSTI), LDAP, header/CRLF/log injection, and cross-site scripting (reflected, stored, DOM). Is the defense parameterization/encoding at the sink (robust), or input filtering/blocklists (fragile)?
- **Authentication & authorization** — Auth bypass, missing or incorrect access checks, object-level authorization (IDOR — does the code verify the caller owns the object, not just that they're logged in?), privilege escalation, and broken session/token handling (JWT `alg` confusion, missing expiry/signature verification, fixation, predictable tokens).
- **Secrets & cryptography** — Hardcoded credentials or keys, weak or misused algorithms (ECB, MD5/SHA1 for security, static IVs/nonces), predictable randomness where unpredictability matters (tokens, salts), non-constant-time comparison of secrets, disabled TLS/cert verification, and improper key/secret storage or logging.
- **Unsafe execution & deserialization** — `eval`-style or dynamic code execution on untrusted data, unsafe deserialization (pickle, native objects, unsafe YAML, gadget chains), prototype pollution, and untrusted-code load paths.
- **SSRF & outbound requests** — User-controlled URLs/hosts that the server fetches: access to internal services and cloud metadata endpoints, request forgery, and unvalidated redirect targets (open redirect).
- **File & path handling** — Path traversal on read/write, arbitrary file overwrite, archive extraction (zip-slip / symlink escape), unsafe temp-file creation, and unrestricted upload handling (type, size, destination, execution).
- **Sensitive-data exposure** — PII, credentials, tokens, or internal detail in logs, error messages, stack traces, or API responses; debug/admin surfaces left reachable; over-broad responses returning more than the caller should see.
- **Memory & concurrency safety** — In native/`unsafe` code: buffer overflow, use-after-free, integer overflow leading to undersized allocation, and unchecked bounds. Everywhere: TOCTOU races and check-then-use gaps on security-relevant state.
- **Trust-boundary & configuration** — Validation/authorization performed on the client or in a less-trusted layer and trusted downstream; CORS, cookie flags (`HttpOnly`/`Secure`/`SameSite`), CSRF protection, and security headers where they're load-bearing for this change; new dependencies (is the addition reputable, pinned, and free of known-vulnerable usage?).

### Trace the data flow

For each dangerous sink in scope (a query, a shell call, a file path, an HTML render, a deserializer), trace **backward**: can the input be attacker-controlled, and is there effective parameterization/encoding/authorization between the source and the sink? A sink fed only by trusted constants is not a finding; the same sink reachable from a request parameter is. Be explicit about which it is.

## Depth

**Go deep on:** trust boundaries and the code immediately inside them, authentication and authorization checks, anything parsing or transforming untrusted input, cryptographic and secret-handling code, `unsafe` blocks and FFI, and any new dependency that touches untrusted data.

**Skim past:** logic with no security-relevant input or effect, well-understood library calls used correctly and safely, and pure-internal code with no reachable attacker path.

## What to Raise

**Raise only when you can show it's exploitable** — a concrete path from attacker-controlled input to a real impact. State that path. When in doubt about reachability, say so and rate it lower rather than dropping it, but do not invent severity.

**Do not raise:** denial-of-service, rate-limiting, or resource-exhaustion concerns; general hardening or defense-in-depth suggestions with no concrete vulnerability behind them; missing validation on fields with no security consequence; or theoretical issues with no reachable path. Better to miss a theoretical issue than to bury a real one in noise.

When you raise an issue, include the concrete fix alongside it — the vulnerability *and* the remediation, not criticism alone.

## Communicating Findings

Lead with the highest-severity findings. Group everything about one vulnerability together. For each finding give:

- **Location** — file and line/function.
- **Vulnerability & exploit path** — what it is, what the attacker controls, and the step-by-step route from that input to impact.
- **Severity** — `critical` (e.g. unauthenticated RCE, full auth bypass, mass data exfiltration), `high` (authenticated RCE, privilege escalation, significant data exposure), `medium` (real but needs unusual preconditions or has limited impact), `low` (minor, hard to exploit, or narrow blast radius). Rate by realistic impact **and** exploitability, not category.
- **Fix** — the concrete remediation: parameterize this query, encode at this sink, add this authorization check, move this validation server-side.

If the change introduces no real security issues, say so directly rather than inventing concerns. A clean review is a valid result.

## Rules

- **Do not run tests, linters, scanners, or exploit code, and do not modify code, commit, or open PRs** unless explicitly asked. This is a review of the code as written.
- **Do not create or modify any files** unless explicitly asked.

---

## What to Review

Base the review on the following, in order of priority:

- Uncommitted changes in the current directory
- If there are no uncommitted changes, then look for changes in the current branch vs main
- If neither applies, ask for clarification

Focus on vulnerabilities this change **introduces or newly exposes** — not pre-existing issues in untouched code, unless the change makes a previously-unreachable weakness reachable.
{%- if context %}

Additional context and focus for this review:

"""
{{ context }}
"""

Scrutinize the trust boundaries it names and concentrate where it points; don't flag what it explicitly rules out of scope. It supplements the review of the changes above — it does not replace it.
{%- endif %}
