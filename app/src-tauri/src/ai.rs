//! AI: gateway lifecycle, the agent harness, tool execution (built-ins +
//! plugins) with the confirmation gate, and `/tools new` authoring.

use crate::state::AppState;
use neu_agent::{AgentEvent, ToolRunner};
use neu_provider::{Gateway, Msg, ToolDef};
use neu_tools::{ToolExecutor, ToolSpec};
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tauri_plugin_opener::OpenerExt;

pub async fn gateway(state: &tauri::State<'_, AppState>) -> Result<Gateway, String> {
    let mut guard = state.gateway.write().await;
    if let Some(g) = guard.as_ref() {
        return Ok(g.clone());
    }
    let discovered = Gateway::discover().await;
    let _ = guard.insert(discovered.clone());
    Ok(discovered)
}

// ---------------------------------------------------------------------------
// tool runner: built-ins + plugin scripts

pub struct AppToolRunner {
    pub app: tauri::AppHandle,
}

impl ToolRunner for AppToolRunner {
    fn defs(&self) -> Vec<ToolDef> {
        let specs = {
            let app = self.app.clone();
            let state = app.state::<AppState>();
            let specs = state.registry.read().unwrap_or_else(|e| e.into_inner()).all();
            specs
        };
        specs
            .into_iter()
            .map(|t| ToolDef { name: t.name, description: t.description, parameters: t.parameters })
            .collect()
    }

    fn run(&self, name: &str, args: serde_json::Value) -> neu_agent::BoxedToolFuture {
        let app = self.app.clone();
        let name = name.to_string();
        Box::pin(async move {
            let spec = {
                let app = app.clone();
                let state = app.state::<AppState>();
                let found = state.registry.read().unwrap_or_else(|e| e.into_inner()).find(&name);
                found
            };
            let Some(spec) = spec else {
                return Err(format!("unknown tool: {name}"));
            };
            if let ToolExecutor::Script { command, dir } = spec.executor.clone() {
                return run_plugin_script(&app, &command, dir, &args, &name).await;
            }
            run_builtin(&app, &spec, &args).await
        })
    }
}

async fn run_builtin(
    app: &tauri::AppHandle,
    spec: &ToolSpec,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let state = app.state::<AppState>();
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();

    match spec.name.as_str() {
        "open_path" => {
            let path = s("path");
            app.opener()
                .open_path(path.clone(), None::<&str>)
                .map_err(|e| format!("open failed: {e}"))?;
            Ok(serde_json::json!({ "opened": path }))
        }
        "search_files" => {
            let query = s("query");
            let hits: Vec<String> = state
                .index
                .search_names(&query, 10)
                .into_iter()
                .map(|i| {
                    if let neu_router::Action::OpenPath { path } = i.action {
                        path
                    } else {
                        i.title
                    }
                })
                .collect();
            Ok(serde_json::json!({ "results": hits }))
        }
        "list_dir" => {
            let path = s("path");
            let mut names = Vec::new();
            let mut entries = tokio::fs::read_dir(&path)
                .await
                .map_err(|e| format!("read_dir {path}: {e}"))?;
            while let Ok(Some(e)) = entries.next_entry().await {
                names.push(e.file_name().to_string_lossy().to_string());
            }
            names.sort();
            Ok(serde_json::json!({ "path": path, "entries": names }))
        }
        "read_file" => {
            let path = s("path");
            let meta = tokio::fs::metadata(&path)
                .await
                .map_err(|e| format!("stat {path}: {e}"))?;
            let take = meta.len().min(64 * 1024) as usize;
            use tokio::io::AsyncReadExt;
            let mut f = tokio::fs::File::open(&path)
                .await
                .map_err(|e| format!("open {path}: {e}"))?;
            let mut buf = vec![0u8; take];
            let n = f.read(&mut buf).await.map_err(|e| e.to_string())?;
            buf.truncate(n);
            if buf.contains(&0) {
                return Err("binary file".into());
            }
            Ok(serde_json::json!({ "path": path, "content": String::from_utf8_lossy(&buf) }))
        }
        "run_shell" => {
            let command = s("command");
            if let Some(reason) = crate::shell::destructive_reason(&command) {
                let ok = state
                    .confirms
                    .ask(app, "run_shell", &format!("{command} — {reason}"))
                    .await;
                crate::audit::record("agent:run_shell", &command, ok);
                if !ok {
                    return Err("user declined".into());
                }
            } else {
                crate::audit::record("agent:run_shell", &command, false);
            }
            run_capture(&command, 60).await
        }
        "write_file" => {
            let path = s("path");
            let content = s("content");
            let ok = state
                .confirms
                .ask(app, "write_file", &format!("write {} bytes to {}", content.len(), path))
                .await;
            crate::audit::record("agent:write_file", &format!("{path} ({} bytes)", content.len()), ok);
            if !ok {
                return Err("user declined".into());
            }
            if let Some(parent) = std::path::Path::new(&path).parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            tokio::fs::write(&path, &content)
                .await
                .map_err(|e| format!("write {path}: {e}"))?;
            Ok(serde_json::json!({ "written": content.len(), "path": path }))
        }
        "web_fetch" => {
            let url = s("url");
            let text = crate::web::fetch_text(&url).await?;
            let truncated: String = text.chars().take(8000).collect();
            Ok(serde_json::json!({ "url": url, "text": truncated }))
        }
        "web_search" => {
            let query = s("query");
            let results = crate::web::ddg_search(&query).await?;
            Ok(serde_json::Value::Array(
                results
                    .into_iter()
                    .take(6)
                    .map(|(t, u)| serde_json::json!({ "title": t, "url": u }))
                    .collect(),
            ))
        }
        "launch_app" => {
            let name = s("name");
            let hit = state
                .index
                .search_apps(&name, 1)
                .into_iter()
                .next()
                .ok_or("no matching app")?;
            if let neu_router::Action::LaunchApp { exec, .. } = hit.action {
                crate::launch(&exec).map_err(|e| e.to_string())?;
                return Ok(serde_json::json!({ "launched": hit.title }));
            }
            Err("match was not an app".into())
        }
        other => Err(format!("builtin {other} not wired")),
    }
}

async fn run_plugin_script(
    _app: &tauri::AppHandle,
    command: &str,
    dir: std::path::PathBuf,
    args: &serde_json::Value,
    tool_name: &str,
) -> Result<serde_json::Value, String> {
    crate::audit::record(&format!("plugin:{tool_name}"), command, false);
    use tokio::io::AsyncWriteExt;
    let mut child = tokio::process::Command::new("bash")
        .arg("-lc")
        .arg(command)
        .current_dir(&dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin
            .write_all(serde_json::to_string(args).unwrap_or_default().as_bytes())
            .await;
    }
    let out = tokio::time::timeout(std::time::Duration::from_secs(60), child.wait_with_output())
        .await
        .map_err(|_| "tool timed out (60s)".to_string())?
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "exit {}: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    match serde_json::from_str(&stdout) {
        Ok(v) => Ok(v),
        Err(_) => Ok(serde_json::json!({ "output": stdout })),
    }
}

async fn run_capture(command: &str, timeout_secs: u64) -> Result<serde_json::Value, String> {
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        tokio::process::Command::new("bash")
            .arg("-lc")
            .arg(command)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output(),
    )
    .await
    .map_err(|_| format!("timed out ({timeout_secs}s)"))?
    .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "code": out.status.code(),
        "stdout": String::from_utf8_lossy(&out.stdout),
        "stderr": String::from_utf8_lossy(&out.stderr),
    }))
}

// ---------------------------------------------------------------------------
// commands

/// Full agent run: model + tools loop, streamed as `agent-*` events.
#[tauri::command]
pub async fn agent_ask(app: tauri::AppHandle, prompt: String) -> Result<(), String> {
    let gw = gateway(&app.state::<AppState>()).await?;
    let runner = Arc::new(AppToolRunner { app: app.clone() });
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);

    tokio::spawn(async move {
        neu_agent::run(&gw, &prompt, runner, tx).await;
    });

    let forward = {
        let app = app.clone();
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                let (event, payload) = match &ev {
                    AgentEvent::Provider { id, model } => ("agent-meta", serde_json::json!({ "provider": id, "model": model })),
                    AgentEvent::Delta { text } => ("agent-delta", serde_json::json!({ "text": text })),
                    AgentEvent::ToolStart { name, args } => ("agent-tool-start", serde_json::json!({ "name": name, "args": args })),
                    AgentEvent::ToolDone { name, ok, summary } => ("agent-tool-done", serde_json::json!({ "name": name, "ok": ok, "summary": summary })),
                    AgentEvent::Done { answer, steps } => ("agent-done", serde_json::json!({ "answer": answer, "steps": steps })),
                    AgentEvent::Error { message } => ("agent-error", serde_json::json!({ "message": message })),
                };
                let _ = app.emit(event, payload);
                if matches!(ev, AgentEvent::Done { .. } | AgentEvent::Error { .. }) {
                    break;
                }
            }
        })
    };
    let _ = forward;
    Ok(())
}

#[tauri::command]
pub fn agent_confirm(app: tauri::AppHandle, id: u64, approve: bool) -> bool {
    app.state::<AppState>().confirms.resolve(id, approve)
}

#[tauri::command]
pub fn tools_list(app: tauri::AppHandle) -> serde_json::Value {
    let state = app.state::<AppState>();
    let listings = state.registry.read().unwrap_or_else(|e| e.into_inner()).listings();
    serde_json::to_value(listings).unwrap_or_default()
}

/// Run a tool on demand (`/toolname args`), streaming output shell-style.
#[tauri::command]
pub async fn tool_run(
    app: tauri::AppHandle,
    name: String,
    args_json: String,
) -> Result<(), String> {
    let args: serde_json::Value = serde_json::from_str(&args_json)
        .unwrap_or(serde_json::json!({ "input": args_json }));
    let runner = AppToolRunner { app: app.clone() };
    let _ = app.emit("shell-started", serde_json::json!({ "tool": name }));
    crate::audit::record(&format!("tool_run:{name}"), &args_json, false);
    match runner.run(&name, args).await {
        Ok(v) => {
            let _ = app.emit("shell-out", serde_json::json!({ "stream": "out", "line": serde_json::to_string_pretty(&v).unwrap_or_default() }));
            let _ = app.emit("shell-done", serde_json::json!({ "code": 0 }));
        }
        Err(e) => {
            let _ = app.emit("shell-out", serde_json::json!({ "stream": "err", "line": e }));
            let _ = app.emit("shell-done", serde_json::json!({ "code": 1 }));
        }
    }
    Ok(())
}

/// Have the model author a new plugin tool, confirm, install, hot-reload.
#[tauri::command]
pub async fn tools_new(app: tauri::AppHandle, description: String) -> Result<(), String> {
    let gw = gateway(&app.state::<AppState>()).await?;
    let system = "You author plugin tools for NeuOS, a system launcher. \
Respond with ONLY a JSON object (no markdown fence) with keys: \
name (lowercase-with-dashes), description (one sentence), \
parameters (JSON Schema object), script (a bash script that reads \
JSON args from stdin, e.g. via python3 for parsing, and prints JSON \
or plain text to stdout). Keep scripts under 40 lines and safe.";
    let (provider, mut rx) = gw
        .chat(&[Msg::system(system), Msg::user(description.clone())], &[])
        .await?;
    let _ = app.emit("agent-meta", serde_json::json!({ "provider": provider.id, "model": provider.model }));

    let mut raw = String::new();
    while let Some(ev) = rx.recv().await {
        match ev {
            neu_provider::ChatEvent::Delta { text } => {
                let _ = app.emit("agent-delta", serde_json::json!({ "text": &text }));
                raw.push_str(&text);
            }
            neu_provider::ChatEvent::Done { .. } => break,
            neu_provider::ChatEvent::Error { message } => {
                let _ = app.emit("agent-error", serde_json::json!({ "message": message }));
                return Ok(());
            }
            _ => {}
        }
    }

    let cleaned = raw.trim().trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```").trim();
    let parsed: Option<serde_json::Value> = serde_json::from_str(cleaned)
        .ok()
        .or_else(|| {
            // salvage the outermost {...}
            let start = cleaned.find('{')?;
            let end = cleaned.rfind('}')?;
            serde_json::from_str(&cleaned[start..=end]).ok()
        });
    let Some(v) = parsed else {
        let _ = app.emit("agent-error", serde_json::json!({ "message": "model did not return valid JSON for the tool" }));
        return Ok(());
    };
    let name = v["name"].as_str().unwrap_or("").to_string();
    let script = v["script"].as_str().unwrap_or("").to_string();
    if name.is_empty() || script.is_empty() {
        let _ = app.emit("agent-error", serde_json::json!({ "message": "tool JSON missing name/script" }));
        return Ok(());
    }

    let state = app.state::<AppState>();
    let ok = state
        .confirms
        .ask(&app, "write_file", &format!("install tool “{name}” into ~/.neuos/tools (writes manifest + run.sh)"))
        .await;
    if !ok {
        let _ = app.emit("agent-done", serde_json::json!({ "answer": "declined — tool not installed.", "steps": 1 }));
        return Ok(());
    }
    match neu_tools::write_plugin(
        &name,
        v["description"].as_str().unwrap_or(""),
        &v.get("parameters").cloned().unwrap_or(serde_json::json!({})),
        &script,
        false,
    ) {
        Ok(dir) => {
            *state.registry.write().unwrap_or_else(|e| e.into_inner()) = neu_tools::Registry::load();
            let _ = app.emit(
                "agent-done",
                serde_json::json!({ "answer": format!("installed /{name} → {}", dir.display()), "steps": 1 }),
            );
        }
        Err(e) => {
            let _ = app.emit("agent-error", serde_json::json!({ "message": e }));
        }
    }
    Ok(())
}

#[tauri::command]
pub fn reload_tools(app: tauri::AppHandle) {
    let state = app.state::<AppState>();
    *state.registry.write().unwrap_or_else(|e| e.into_inner()) = neu_tools::Registry::load();
}
