# Authoring a local prompt for Switchboard

> **Audience:** an AI coding agent (Claude Code or Codex) being asked to generate a starter local prompt for a Switchboard working directory. If you are a human author, this doc works for you too — you are just not the primary audience.

## What a local prompt is

A **local prompt** is a single markdown file with YAML frontmatter that Switchboard resolves by name when the user invokes a slash command in the compose bar (e.g., `/code-review`) or when a workflow references a prompt ID (e.g., `local:code-review`). Switchboard renders the template (MiniJinja subset, see below) and dispatches the rendered text to one or more agents.

Local prompts are file-based. There is no in-app editor. You are generating a file the user (or you) will save into a local prompt store — a **user-global** directory, not a per-project or per-directory one (see `docs/system-design.md` §6).

## Where local prompts live

Local prompts are **user-global** — configured once at the user level and available in every project, regardless of working directory. Switchboard resolves them from the directories listed in `local_prompt_dirs` in the user-global `config.yaml`, in declared order (an earlier directory shadows a later one on name collision). If `local_prompt_dirs` is unset, Switchboard uses a single default store: the OS-conventional `prompts/` directory under the user-global config dir (e.g. `~/.config/switchboard/prompts/` on Linux, `~/Library/Application Support/switchboard/prompts/` on macOS), which starts empty — example prompts ship as a read-only built-in library baked into the app, not written into this folder.

Default to writing the prompt into the default store (or, if the user keeps a personal prompt library listed in `local_prompt_dirs`, into that directory). If the user has indicated they keep prompts elsewhere, follow their instruction.

## File format

A local prompt is **one markdown file** with YAML frontmatter at the top. The frontmatter declares metadata; the body is the prompt template.

```markdown
---
name: code-review
description: Ask an agent to review the current diff against a checklist.
arguments:
  - name: focus
    description: Optional focus area for the review.
    required: false
tags:
  - review
  - code-quality
---
Please review the current uncommitted changes in this repository.

{% if focus %}Focus area: {{ focus }}{% endif %}

For each issue, identify the file, the concern, and a suggested fix.
```

## Frontmatter fields

| Field | Required | Notes |
|---|---|---|
| `name` | yes | Slug. Lowercase, hyphens allowed (e.g., `code-review`). Becomes the suffix in `local:<name>` references. Must be unique within its directory. Matches MCP's `prompts/list` `name` field and Claude Code's skill `name` field — if the user later moves this file into a Claude Code skills directory, it works as-is. |
| `description` | yes | One-line human description. Surfaced in slash-command autocomplete and invocation UI. |
| `arguments` | optional | List of named arguments the user supplies at invocation. See "Arguments" below. Omit if the prompt takes no arguments. |
| `tags` | optional | Free-form tags for organization. Surfaced in the prompt-library UI (when v2+ ships). Not validated. |

## Arguments

Each argument is a mapping with `name`, `description`, and `required`:

```yaml
arguments:
  - name: focus
    description: Optional focus area for the review.
    required: false
  - name: target_file
    description: Path to the file to review.
    required: true
```

- `name`: lowercase with underscores (matches the MiniJinja variable convention). Becomes the template variable.
- `description`: shown to the user in the invocation form. Be specific.
- `required`: defaults to `true` if omitted. If `false`, the user can leave it blank — the variable will be the empty string. Use `{% if arg %}…{% endif %}` to handle the blank case.

There are no typed arguments in v1 — every argument is a string. (Numeric or list argument types are deferred to v2+.)

## Template syntax

The template is a **MiniJinja subset** (Jinja2-compatible). Switchboard renders it with the supplied arguments before dispatching.

**Supported:**

- Variable substitution: `{{ var }}`
- Member access: `{{ obj.field }}`, `{{ list[0] }}`
- For loops: `{% for x in list %}...{% endfor %}` (including `loop.index`, `loop.first`, `loop.last`)
- If conditions: `{% if expr %}...{% elif %}...{% else %}...{% endif %}` (truthiness checks and equality)
- Whitespace control: `{%-`, `-%}`, `{{-`, `-}}`
- Comments: `{# ... #}`
- Built-in filters: `length`, `lower`, `upper`, `default`, `join`, `trim` (the portable core — the local renderer supports more MiniJinja filters, but only these are guaranteed to behave identically if the prompt is later used by a workflow or moved to Tiddly)

**Stay within that core subset.** It is the same subset Switchboard's workflow DSL enforces on its *own* template strings, and it's what renders consistently across Switchboard and Tiddly's Jinja2. Local-prompt rendering does **not** restrict you to it (it's plain MiniJinja with default features), so the constructs below split two ways:

- **These error even in a local prompt** — don't use them: unregistered/custom filters, and the template-loading tags `{% include %}`, `{% extends %}`, `{% block %}` (no template loader is configured, so they fail at render).
- **These render locally but aren't portable** — avoid them: `{% set %}`, `{% raw %}`, `{% macro %}`, and the `do` tag. They work in a standalone local prompt today, but the workflow DSL rejects them in its own templates and other Jinja2 servers may differ — staying in the core keeps a prompt consistent wherever it's rendered.

If you want richer templating, the user can keep the prompt in Tiddly (an MCP prompt server) instead of a local file.

## Conventions

- **Naming**: short, descriptive, hyphenated. `code-review`, `summarize-pr`, `extract-test-cases`. Avoid prefixes like `prompt-` (the directory already implies it).
- **Body style**: write in second person to the agent ("Review the diff…", not "The agent should review the diff…"). The body is sent verbatim to the agent.
- **Required vs optional arguments**: prefer required arguments unless the prompt makes sense without them. Optional arguments need explicit `{% if %}` handling.
- **Single responsibility**: one prompt = one task. If a prompt has more than ~3 arguments, consider splitting it into multiple prompts.

## Skill-file compatibility

Switchboard's local-prompt format is intentionally compatible with Claude Code skill files in the forward direction: a `.md` file authored as a Claude Code skill (frontmatter with `name`, `description`, body) can be dropped into a Switchboard prompts directory and used as a local prompt as-is. The user's skill library is implicitly a Switchboard prompt library.

The reverse direction holds **only for argument-less prompts**: a Switchboard local prompt that declares no `arguments` works as a Claude Code skill file. A Switchboard prompt with `arguments` does *not* round-trip — Claude Code skills aren't parameterized, so the `arguments` declaration would be ignored and any `{{ var }}` references in the body would render literally. If you need to share a parameterized prompt across both surfaces, keep it in Switchboard's prompts directory and don't expect skill-side parameter handling.

## Worked examples

### Minimal — no arguments

```markdown
---
name: summarize-changes
description: Summarize the uncommitted changes in this repository.
---
Summarize the uncommitted changes in this repository. Group related edits together.
For each group, briefly explain what changed and why it likely changed.
```

### One required argument

```markdown
---
name: explain-function
description: Explain a function in a target file.
arguments:
  - name: function_name
    description: Name of the function to explain.
    required: true
  - name: file_path
    description: Path to the file containing the function.
    required: true
---
Explain the function `{{ function_name }}` in `{{ file_path }}`.

Cover:
- What it does (one sentence).
- Its inputs and outputs.
- Any non-obvious behavior or edge cases.
- How it's used elsewhere in the codebase (look for callers).
```

### Optional argument with conditional

```markdown
---
name: code-review
description: Review uncommitted changes against a checklist.
arguments:
  - name: focus
    description: Optional focus area for the review (e.g., "error handling", "test coverage").
    required: false
---
Please review the current uncommitted changes in this repository.

{% if focus %}Pay particular attention to: {{ focus }}.{% endif %}

For each issue, identify:
- The file and line.
- The concern, in one sentence.
- A suggested fix.
```

### Iteration over a list (when the user provides one)

You can pass a list as a string and parse it, but more commonly the iteration happens on the workflow side (using a `for_each` step). Local prompts themselves rarely need to iterate at the top level. If you need iteration, ask the user whether the loop belongs in the prompt or in a workflow.

## After authoring

1. Save the file in a local prompt store (the default user-global `prompts/` directory, or a directory the user lists in `local_prompt_dirs`). A `.md` filename matching the `name` field is the convention, though the `name` comes from the frontmatter, not the filename. Prompts are user-global — available in every project.
2. The user invokes it by typing `/<name>` in the compose bar.
3. The prompt appears in slash-command autocomplete once Switchboard refreshes its prompt list (the Sync action in Settings, or a restart).

## When to point at the formal spec

This doc covers the common authoring path. For provider behavior (local file store + MCP server resolution), see `docs/system-design.md` §6 ("Prompts and prompt providers"). For the authoritative supported/unsupported MiniJinja subset and the template-variable scoping rules used when these prompts are rendered inside a workflow, see `docs/workflow-spec.md` §Templating. For the workflow DSL that consumes these prompts, see `docs/workflow-spec.md`.
