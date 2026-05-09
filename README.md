# Switchboard

Manual orchestrator for AI coding agents.

Switchboard lets you spawn multiple Claude Code and Codex sessions in a single project, route messages between them, and define reusable patterns for common multi-agent workflows like dueling code review, plan-and-implement, and fan-out aggregation.

It's built for developers who want explicit, human-in-the-loop control over multi-agent workflows — not an opinionated SDLC engine, not a full agent replacement, just the coordination layer between agents you're already using.

## Status

Early development. Design is being captured in [`/docs`](./docs) before implementation begins. The high-level plan is in [`docs/plan.md`](./docs/plan.md).

This is not yet usable software. Star or watch the repo if you want to follow along.

## Why

Running multiple AI coding agents in parallel — one to plan, others to review, one to implement — produces meaningfully better results than running a single agent, but the manual coordination overhead (copy-paste between terminals, tracking which agent has which context, applying prompt templates by hand) is busywork that should be automated so you can spend that time on the parts that need judgment.

Switchboard removes the coordination overhead while keeping the human in the loop where judgment matters: deciding what to route, when to revise, when to proceed.

## Core ideas

- **Project**: a workspace containing related agents working toward a shared goal (a feature, a refactor, a document).
- **Agent**: a Claude Code or Codex session, named and assigned a role within a project.
- **Pattern**: a reusable, parameterized routing template — for example "fan-out review and aggregate" — defined as a YAML file and invoked by name.
- **Routing**: message passing between agents, optionally wrapped in a prompt template, with support for fan-out (one to many) and fan-in (many to one).

## Non-goals

- Replacing the Claude Code or Codex harness. Switchboard drives them; it doesn't reimplement them.
- Prescribing a software development lifecycle. Patterns are user-defined; Switchboard ships defaults but doesn't impose process.
- Managing git, CI, or PR workflows. Out of scope.
- Cross-session persistent agent memory. Possibly a future addition; not in scope for v1.

## Design and discussion

The architectural decisions, functional requirements, and open questions are being worked through in [`docs/`](./docs). Comments and pushback welcome via issues.

## License

Apache 2.0. See [LICENSE](./LICENSE).
