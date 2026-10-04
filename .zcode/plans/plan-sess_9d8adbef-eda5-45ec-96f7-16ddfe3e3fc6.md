# NeuOS v0.1 — "Smart Search Box" App

A cross-platform (Linux/macOS/Windows) launcher app in **Tauri 2**: one borderless search bar summoned by a global hotkey that instantly searches your files and apps, runs `/` commands, executes shell with full system access, answers via AI (local or cloud), opens web results in a clean reader window, and — DeepSeek-Harness-style — treats every capability as a pluggable tool the agent can even **build new tools for**. If this works, it later becomes the shell of the minimal-Linux NeuOS distro (designed so a kiosk-mode boot can wrap it).

## Architecture

```
┌─────────────────────────────────────────────────┐
│ Tauri window (borderless, centered, hotkey)      │
│ UI: React + TypeScript + Tailwind (minimal)      │
│  search bar · grouped results · /command palette │
│  streaming AI answer card · clean web reader     │
└───────────────┬─────────────────────────────────┘
                │ Tauri IPC (invoke + events)
┌───────────────▼─────────────────────────────────┐
│ Rust core (cargo workspace crates)               │
│ neu-index    Tantivy file index + notify watcher │
│ neu-router   intent routing (file/app/web/ai/cmd)│
│ neu-provider model gateway: local + cloud        │
│ neu-agent    harness loop + confirm gate         │
│ neu-tools    plugin tool registry, hot-reload    │
└─────────────────────────────────────────────────┘
```

## Project layout

```
NeuOS/
├── app/
│   ├── src-tauri/        # Tauri shell: IPC commands, hotkey, windows
│   └── src/              # React + TS + Tailwind UI
├── crates/               # neu-index, neu-router, neu-provider, neu-agent, neu-tools
├── tools/                # example plugin tools
└── docs/
```

## Key designs

**1. Core loop (must feel <20ms).** Alt+Space (configurable) toggles the bar. As you type, client-side heuristics route intent instantly: path-like → files; `/` → command palette; `>` → shell exec; URL → clean web window; `?` prefix or natural-language + Enter → AI; otherwise grouped results (files, apps, actions, commands, web). Arrow keys / Enter drive everything; Tab filters a category.

**2. neu-index.** Tantivy full-text index over `$HOME` (configurable roots, default excludes: `node_modules`, `.cache`, `.git` objects; respects `.gitignore`). Incremental reindex via `notify` FS watcher; initial build runs async with progress shown. App discovery: Linux `.desktop` dirs, macOS `/Applications`, Windows Start Menu `.lnk`. Content search for text/code files in v0.1 (PDF/office extractors noted as extensions).

**3. neu-provider (gateway).** One `Provider` trait with streaming chat + normalized tool-calling. Auto-probes localhost OpenAI-compatible servers (Ollama :11434, LM Studio :1234, llama.cpp :8080) via `/v1/models`; cloud providers OpenAI, Anthropic, DeepSeek from config (`~/.config/neuos/config.toml`, keys via env or config with 0600). Routing: default local model, cloud fallback for hard tasks, provider badge shown per answer.

**4. neu-agent + neu-tools (the DSH-inspired harness).** Everything is a tool with a JSON-schema: built-ins in Rust (`open_path`, `search_files`, `run_shell`, `read_file`, `write_file`, `web_fetch`, `web_search`, `open_url`, `launch_app`) plus plugin tools as `~/.neuos/tools/<name>/manifest.toml` + script (bash/python/node; JSON on stdin → JSON on stdout), hot-reloaded. Agent loop streams each step into the answer card; destructive operations (rm, overwrite, install) require an inline user confirmation — full access, visible gate. `/tools new` lets the agent **author and register a new tool itself** (the self-extending property you asked for).

**5. Slash commands.** `/ai`, `/run`, `/web`, `/tools`, `/index`, `/settings` map to built-ins; prompts/skills like `/summarize <file>`; plugin tools register their own.

**6. Clean web window.** Second Tauri window: page fetched server-side in Rust, main content extracted (readability-style), rendered as clean typography; "open in browser" fallback. Web search via DuckDuckGo HTML (no key) with pluggable Brave/Serper keys.

## Milestones (each ends demoable)

- **M0 Scaffold** — Tauri 2 + React app; borderless hotkey-toggled bar; toolchain setup on this machine (Rust aarch64, Node LTS, pnpm, webkit2gtk deps); builds on Linux.
- **M1 Instant local search** — neu-index + apps + actions; keyboard-first UX; sub-20ms feel.
- **M2 Commands + shell** — `/run` (full-access exec, output inline), `/web`, `/settings`, `/index` status.
- **M3 AI answers** — gateway with local auto-detect + cloud keys; streaming answer card; local→cloud fallback.
- **M4 Agent + tools** — tool registry, agent loop with visible steps + confirm gate; plugin hot-reload; 3 example tools; `/tools new` agent-built tool.
- **M5 Clean web window** — reader window + web search results in the bar.
- **M6 Polish + packaging** — first-run wizard (hotkey, roots, keys), settings UI; `.deb`/AppImage, `.dmg`, `.msi`.

Future (out of scope v0.1): the NeuOS distro — boot a minimal Linux into this app as the shell.

## Known risks & mitigations

- **Wayland global hotkeys** don't work via the standard Tauri plugin (X11-only on Linux) → tray-click activation fallback, documented; fine on macOS/Windows.
- **WebKitGTK quirks** (blank window/flicker) → apply Tauri's official Linux graphics env-var mitigations.
- **Anthropic vs OpenAI tool-call shapes differ** → normalized once in neu-provider.
- **Full system access = attack surface** → no network listeners; tools run as the user; confirm gate on destructive patterns.

Research context: DeepSeek Harness = "everything is a plugin" (models/tools/skills/sandboxes) with an Electron desktop wrapper that doesn't support Linux — NeuOS fills that gap with a launcher-first, Rust-fast take. Sources: [dsh-desktop](https://github.com/dataelement/dsh-desktop), [Tauri Wayland hotkey issue](https://github.com/tauri-apps/tauri/issues/3578), [Tauri Linux graphics guide](https://v2.tauri.app), [global-shortcut walkthrough](https://dev.to).

## What I'll do first (M0)

1. Install/verify toolchain (Rust, Node, pnpm, Tauri Linux deps).
2. Scaffold `app/` via create-tauri-app (React + TS), cargo workspace with the five crates.
3. Borderless centered window + global hotkey toggle + hide-on-blur.
4. A walking skeleton: type → echo list from Rust IPC → verify the full loop, then start M1.