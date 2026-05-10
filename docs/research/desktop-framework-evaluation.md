# Research: Desktop framework evaluation for Switchboard

**Captured:** 2026-05-09
**Decision:** Tauri (Rust core + WebView frontend) with Svelte + Tailwind CSS for the UI
**Affects system-design sections:** §10 (Form factor and distribution).
**Supersedes:** [tui-framework-evaluation.md](tui-framework-evaluation.md) — initial decision was a Textual TUI; pivoted after re-evaluating the UX vision.

## Why we pivoted from TUI

The initial decision was a TUI built with Textual (Python). We pivoted after articulating the UX vision more concretely:

- Multi-pane viewing of multiple agent outputs simultaneously
- Real-time per-agent status (which agent is working, context utilization, expand/collapse, etc.)
- Per-agent options menus (compact, fork, etc.)
- "Slick" aesthetic suitable for a broad audience, not just terminal-natives

These are desktop-shaped requirements. Modern TUIs can approximate them but always feel cramped at the high end of polish. The "anyone who wants" audience the README commits to is better served by a desktop app than by a TUI.

## Frameworks considered (this round)

| Framework | Backend lang | Bundle size | Startup | UI tech |
|---|---|---|---|---|
| **Tauri** | Rust (native) | ~3 MB | <500 ms | Web (OS WebView) |
| **Electron** | JS/TS (Node) | ~150 MB | 1–2 s | Web (bundled Chromium) |
| **Local web UI** | Any (Python, Node, Rust) | n/a (server + browser) | Browser-dependent | Web (browser tab) |
| **Native per-platform** (Cocoa/SwiftUI, WPF, GTK) | Per-platform | Small per platform | Fast | Native widgets |

## Why Tauri won

- **Single-binary distribution.** ~3 MB Hello World. No Python/Node prereq. No browser tab. Just a desktop app the user double-clicks.
- **Slick UX without bundling Chromium.** The OS-native WebView (WebKit on macOS, WebView2 on Windows, WebKitGTK on Linux) handles ~99% of modern web tech identically across platforms. Bundle is ~50× smaller than Electron.
- **Rust backend is architecturally simple.** Single-process app: Rust core + WebView frontend, talking via Tauri's typed command system. No subprocess split, no two-process IPC plumbing.
- **Native OS integration via Tauri plugins** — system tray, notifications, file dialogs, auto-updater, etc.
- **Tauri 2.x** (released 2024) is mature with cross-platform support and a growing plugin ecosystem.

Sources: [Tauri vs Electron 2026 — tech-insider](https://tech-insider.org/tauri-vs-electron-2026/), [Tauri vs Electron — DoltHub](https://www.dolthub.com/blog/2025-11-13-electron-vs-tauri/), [Tauri docs](https://tauri.app/).

## Why not Electron

- 50× larger bundle, slower startup, more memory.
- Easier with a JS/TS backend, but our Rust backend works just as well — we're letting the AI agent write the code, so language choice is decoupled from human-developer ergonomics.
- Mature ecosystem advantage doesn't compensate for the distribution cost.
- Historical pain points (renderer-process file access, `nodeIntegration`, etc.) wouldn't apply to our architecture either way (filesystem work happens in the backend regardless of wrapper) — but Electron's overhead vs Tauri remains decisive.

## Why not local web UI

- "Lives in a browser tab" works for Jupyter-style power tools but doesn't match the polished-app aesthetic we want.
- We'd lose dock icon, native notifications, native menus, native window management.
- A future Tauri wrap is possible but means we'd ship two delivery shells; better to commit to Tauri now.

## Why not native per-platform

- Three codebases (Cocoa/SwiftUI, Win32/WPF, GTK/Qt) is too much surface area for v1.
- Not where productivity-tooling development lives in 2026.

## Frontend stack: Svelte + Tailwind CSS

Standard web technology inside Tauri's WebView. **Svelte 5** chosen for:

- Smaller bundles than React
- Less boilerplate (good for AI-agent-written code)
- Excellent developer experience
- Production-ready and stable

**Tailwind CSS** for styling, **shadcn-svelte** for design-system primitives (modal, tabs, accordion) when needed.

**React + Tailwind + shadcn/ui** is a viable alternative. The architectural decisions (Tauri shell, Rust core) don't depend on the frontend framework — a future contributor or rewrite could swap Svelte for React without affecting anything else.

## MCP SDK considerations

The Rust MCP SDK (`rmcp`) is **Tier 2** per [MCP's SDK Tiering System](https://modelcontextprotocol.io/community/sdk-tiers).

What Tier 2 means specifically:

| Concern | Tier 1 | Tier 2 |
|---|---|---|
| Conformance tests | 100% pass | 80% pass |
| New protocol features | Same release | Within 6 months |
| Issue triage | 2 business days | Within a month |
| Critical bug fix | 7 days | 2 weeks |
| Documentation | Comprehensive with examples for all features | Basic documentation covering core features |
| Roadmap | Published | Published plan toward Tier 1 OR explanation for staying Tier 2 |

Tier 2 means slower SLAs and a smaller community for examples — **not** feature gaps. Verified via the [Rust SDK README](https://github.com/modelcontextprotocol/rust-sdk):

- ✅ MCP client connections (stdio + HTTP transports)
- ✅ `prompts/list` (`client.list_all_prompts().await?`)
- ✅ `prompts/get` (`get_prompt()` with `GetPromptRequestParams`)
- Latest release: **rmcp-v1.6.0** (May 2026)
- 76 total releases — actively maintained
- Full feature support: tools, resources, prompts, sampling, roots, logging, completions, notifications, subscriptions

For Switchboard's use case (MCP client fetching prompts), the SDK is functionally adequate.

**Risks to track:**

- Slower upstream evolution if a brand-new MCP feature is needed urgently (up to 6 months wait for Rust support).
- Slower bug response if a critical SDK bug surfaces (2 weeks vs 7 days). Mitigation: it's open-source, we can contribute upstream if blocked.

Both acceptable trade-offs for the architectural wins of Tauri.

## Distribution

Tauri's bundling pipeline produces signed native binaries per platform:

- **macOS**: `.dmg`, `.app`. Signed with Apple Developer ID. Distribution via Homebrew tap (`brew install switchboard`) and direct download.
- **Linux**: `.deb`, `.rpm`, `.AppImage`. Direct download for other distros.
- **Windows**: `.msi` installer signed with Authenticode. Direct `.exe` for portable use.

Tauri's built-in updater is wired in from day one so users get version updates inside the app.

Code signing for macOS and Windows is real release-infrastructure work (Apple Developer Program enrollment, Authenticode certificate). Worth the polish — friction-free installs are a major UX win for a "looks slick" desktop app.

## Sources

- [Tauri docs — tauri.app](https://tauri.app/)
- [Tauri vs Electron 2026: 96% Smaller Apps — tech-insider](https://tech-insider.org/tauri-vs-electron-2026/)
- [Tauri vs Electron — DoltHub Blog](https://www.dolthub.com/blog/2025-11-13-electron-vs-tauri/)
- [Tauri vs Electron Practical Guide — RaftLabs](https://raftlabs.medium.com/tauri-vs-electron-a-practical-guide-to-picking-the-right-framework-5df80e360f26)
- [SDK Tiering System — modelcontextprotocol.io](https://modelcontextprotocol.io/community/sdk-tiers)
- [MCP SDKs — modelcontextprotocol.io/docs/sdk](https://modelcontextprotocol.io/docs/sdk)
- [Rust MCP SDK — modelcontextprotocol/rust-sdk](https://github.com/modelcontextprotocol/rust-sdk)
- [Svelte](https://svelte.dev/), [Tailwind CSS](https://tailwindcss.com/), [shadcn-svelte](https://www.shadcn-svelte.com/)
