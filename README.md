# NeuOS

A smart search box for your system — one bar (global hotkey) that searches
files, launches apps, runs shell, searches the web in a clean reader window,
and answers through an **agent with full system access** (local model first,
cloud fallback). Cross-platform Tauri 2 app: Rust core, web UI. The
minimal-Linux "AI-first OS" wraps this same app later.

## Try it

```bash
cd app && pnpm tauri dev        # dev (hot reload)
cd app && pnpm tauri build      # binary + .deb + AppImage
```

**Alt+Space** summons the bar (falls back to Ctrl+Space / Ctrl+Alt+Space if
the desktop owns the combo — the active one shows top-right). Hides on Esc
or focus loss. Drag it by the input row or the status strip.

## What it does

| You type | What happens |
|---|---|
| `fire` | Apps + files + **in-file content** matches, fuzzy-ranked |
| `~/code` | Path-intent file search |
| `? explain this repo` | **Agent** answers — may run tools (visible, gated) |
| `/ai <question>` | Same agent, explicit |
| `> ls -la` or `/run <cmd>` | Shell, streams output; destructive commands ask first |
| `github.com` | Opens the **clean reader window** (readable extract) |
| `notes` → web row, or `/web <query>` | DuckDuckGo results → Enter opens the reader |
| `/tools` | Every registered tool; plugin tools are slash-runnable |
| `/tools new <idea>` | **The agent writes a new tool** (manifest + script), you confirm, it's live immediately |
| `/index`, `/settings` | Index stats; providers/config summary |

## AI providers

Auto-detected at first use, no config needed for defaults:

- **Local**: Ollama (:11434), LM Studio (:1234), llama.cpp (:8080), vLLM (:8000) — any OpenAI-compatible `/v1`
- **Cloud**: OpenAI, Anthropic, DeepSeek — set `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` / `DEEPSEEK_API_KEY` (env overrides: `NEUOS_*_MODEL`, `NEUOS_*_BASE`)
- **Config**: `~/.config/neuos/config.toml`

```toml
[ai]
default_provider = "ollama"

[[provider]]
id = "ollama"
base_url = "http://127.0.0.1:11434/v1"
model = "qwen3.8:latest"
```

Routing: first local server found wins by default; cloud keys are fallback.
The answer view shows which provider/model served you.

## The tool system (DeepSeek-Harness-style)

Everything the agent can do is a tool. Built-ins live in Rust:
`open_path · search_files · list_dir · read_file · run_shell · write_file ·
web_fetch · web_search · launch_app`.

Plugin tools live in `~/.neuos/tools/<name>/`:

```
~/.neuos/tools/git-summary/
├── manifest.toml   # name, description, parameters (JSON Schema), command
└── run.sh          # args JSON on stdin → JSON (or text) on stdout
```

Hot-reloaded — `/tools new` writes one and it's callable instantly as
`/git-summary`. Destructive operations (rm, sudo, overwrites, installs) show
an inline confirm: **⏎ approve · tab decline**.

## Layout

```
app/                Tauri shell: windows, hotkey, IPC commands + React UI
  src-tauri/src/
    lib.rs          setup, hotkey, window lifecycle, tools hot-reload
    search.rs       intent → grouped results, slash commands, activate
    shell.rs        streaming shell exec + destructive gate
    ai.rs           gateway lifecycle, agent runner, tool executors, /tools new
    web.rs          DDG search, readability extraction, reader window
    state.rs        shared state + confirm hub
crates/
  neu-index         deep walk (in-memory names) + Tantivy content + fs watcher
  neu-router        intent routing, shared types, fuzzy scoring
  neu-provider      model gateway: local probes, OpenAI-compat + Anthropic SSE
  neu-agent         the harness loop (policy only; tools injected)
  neu-tools         tool registry, plugin manifests, authoring
```

## Notes for this machine (Ubuntu 24.04 / NVIDIA / X11)

- `main.rs` sets `WEBKIT_DISABLE_COMPOSITING_MODE=1` — required or WebKitGTK
  renders the transparent window invisible on NVIDIA.
- GNOME's window menu owns Alt+Space on some setups — the app falls back
  automatically and shows the active combo.
- Indexing walks `$HOME` (respecting `.gitignore`, skipping heavy dirs),
  caps at 400k files; name search stays instant via the in-memory list,
  content search uses Tantivy. Progress shows in the status strip.

## Tests

```bash
cargo test --workspace                      # unit tests
NEUOS_E2E=1 cargo test -p neu-agent --test e2e_local   # needs a local server
```
