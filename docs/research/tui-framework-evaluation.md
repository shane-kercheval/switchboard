# Research: TUI framework evaluation

> **Status: SUPERSEDED.** This evaluation drove an initial decision to ship Switchboard as a Textual-based TUI. We later re-evaluated after articulating the UX vision more concretely (multi-pane agent dashboards, real-time per-agent status, expand/collapse outputs, slick aesthetics) and pivoted to a desktop application built with Tauri. The current decision is captured in [desktop-framework-evaluation.md](desktop-framework-evaluation.md). This note is preserved for historical context.

**Captured:** 2026-05-09
**Decision (later superseded):** Textual (Python)
**Affects system-design sections:** §10 (Form factor and distribution) — historical only; current decision is documented elsewhere.

## Summary

Evaluated three leading TUI frameworks for Switchboard's UI layer. Picked **Textual (Python)** based on rendering polish, async-native API matching our streaming model, and best-in-class mouse support. Distribution friction (PyPI install vs single binary) was the main argument against; judged acceptable given our likely audience already has Python and given a PyInstaller bundle can close the gap later if needed.

## Frameworks considered

| Framework | Language | Notable apps |
|---|---|---|
| **Textual** | Python | Posting (HTTP client), Toolong (log viewer), Frogmouth (markdown reader), Memray |
| **Bubbletea** (+ lipgloss + bubbles) | Go | gum, glow, slim, soft-serve (Charm ecosystem) |
| **Ratatui** | Rust | gitui, atuin |
| Also noted: tview/Go (k9s, lazydocker), gocui/Go (lazygit) | | |

Sources: [TUI Framework Ranking — OSSInsight](https://ossinsight.io/collections/tui-framework), [Go vs Rust for TUI Development — DEV](https://dev.to/dev-tngsh/go-vs-rust-for-tui-development-a-deep-dive-into-bubbletea-and-ratatui-2b7), [BubbleTea vs Ratatui — Rost Glukhov](https://www.glukhov.org/post/2026/02/tui-frameworks-bubbletea-go-vs-ratatui-rust/), [awesome-tuis — GitHub](https://github.com/rothgar/awesome-tuis).

## Comparison on Switchboard-relevant criteria

| Concern | Textual | Bubbletea | Ratatui |
|---|---|---|---|
| Rendering polish ("feels like an app") | Strongest | Strong | Strong |
| Async / streaming | Native | Strong | Manual loop, more code |
| Mouse support | Best | Good | Good |
| Performance | Fine | Fine | Strongest |
| Single-binary distribution | No (PyPI) | Yes | Yes |
| Learning curve | Low–moderate | Low | Steep (Rust ownership) |
| Ecosystem | First-party suite (Textualize) | Charm: lipgloss + bubbles | Mostly community |

Performance differences between the frameworks are immaterial for Switchboard — we are a coordination layer over LLM API calls that take seconds, not a real-time data dashboard pushing 1000 fps.

## Why Textual won

- **Streaming-from-stdin** maps naturally onto Textual's async event loop. Every `await` we'd write fits the harness-stream consumption model (read line, parse, dispatch normalized event).
- **Mouse support and modal-dialog polish** are most refined; matters for the "feels like an app" experience that broadens our audience beyond hardcore terminal users.
- **Best precedents** for our shape of app: Posting (HTTP client) and Toolong (log viewer) demonstrate "this could be a desktop app but happens to be in the terminal."
- **Python ecosystem alignment** — same language for harness adapters, MCP client, pattern parser, and UI. One language, one toolchain, one debugger, one packaging story.

## Why distribution friction is acceptable

- Likely Switchboard users (people running Claude Code or Codex) have Python, or are comfortable installing it. Both Anthropic and OpenAI's CLI ecosystems are Python-adjacent.
- `uv tool install switchboard` is one command — comparable in friction to `npm install -g` or `cargo install`.
- `uvx switchboard` allows a try-without-installing first run.
- PyInstaller-bundled binary distribution can be added later (single-file install, ~30–100 MB, slightly slower startup) if non-developer users need a friction-free single-binary option.

## Insurance: pivot is possible

The data layer (file-based config, JSONL session files, normalized event stream, per-harness adapters) is UI-agnostic. A future pivot to desktop (Tauri or Electron) or to a different TUI framework (Bubbletea, Ratatui) is a UI-layer rewrite, not an architecture change. We are not betting the architecture on Textual; we are betting v1's UI layer on it.

## Sources

- [TUI Framework Ranking — OSSInsight](https://ossinsight.io/collections/tui-framework)
- [Go vs. Rust for TUI Development: A Deep Dive into Bubbletea and Ratatui — DEV](https://dev.to/dev-tngsh/go-vs-rust-for-tui-development-a-deep-dive-into-bubbletea-and-ratatui-2b7)
- [Terminal UI: BubbleTea (Go) vs Ratatui (Rust) — Rost Glukhov](https://www.glukhov.org/post/2026/02/tui-frameworks-bubbletea-go-vs-ratatui-rust/)
- [awesome-tuis — rothgar/awesome-tuis](https://github.com/rothgar/awesome-tuis)
- [awesome-ratatui — ratatui/awesome-ratatui](https://github.com/ratatui/awesome-ratatui)
- [Textual — textualize/textual](https://github.com/Textualize/textual) (framework)
- [Posting](https://github.com/darrenburns/posting), [Toolong](https://github.com/Textualize/toolong) (Textual app precedents)
