//! The search pipeline: intent routing → grouped results, slash commands,
//! and action dispatch.

use crate::state::AppState;
use neu_router::{Action, GroupKind, Intent, ResultGroup, SearchItem};
use tauri::{Manager, State};
use tauri_plugin_opener::OpenerExt;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    pub intent: String,
    pub groups: Vec<ResultGroup>,
}

fn intent_label(intent: &Intent) -> String {
    match intent {
        Intent::Empty => String::new(),
        Intent::SlashCommand { name, .. } => format!("COMMAND:{name}"),
        Intent::Shell { .. } => "SHELL".into(),
        Intent::Ai { .. } => "ASK".into(),
        Intent::Url { .. } => "URL".into(),
        Intent::PathLike => "PATH".into(),
        Intent::Mixed => "MIXED".into(),
    }
}

#[tauri::command]
pub fn search(query: &str, state: State<AppState>) -> SearchResponse {
    let routed = neu_router::route(query);
    let label = intent_label(&routed);
    let is_mixed = matches!(routed, Intent::Mixed);
    let mut groups: Vec<ResultGroup> = Vec::new();

    match routed {
        Intent::Empty => {
            groups.push(ResultGroup {
                kind: GroupKind::Commands,
                items: slash_commands("", &state).into_iter().take(6).collect(),
            });
            groups.push(ResultGroup {
                kind: GroupKind::Apps,
                items: state.index.search_apps("", 8),
            });
        }
        Intent::SlashCommand { name, args } => {
            groups.push(ResultGroup {
                kind: GroupKind::Commands,
                items: slash_commands(&name, &state)
                    .into_iter()
                    .map(|mut c| {
                        if let Action::Command { args: a, .. } = &mut c.action {
                            *a = args.clone();
                        }
                        c
                    })
                    .collect(),
            });
        }
        Intent::Shell { command } => {
            groups.push(ResultGroup {
                kind: GroupKind::Commands,
                items: vec![SearchItem {
                    id: "shell".into(),
                    title: format!("Run: {command}"),
                    subtitle: Some("Shell — full access (destructive steps ask first)".into()),
                    badge: ">_".into(),
                    score: 1.0,
                    action: Action::RunShell { command },
                }],
            });
        }
        Intent::Ai { prompt } => {
            groups.push(ResultGroup {
                kind: GroupKind::Ai,
                items: vec![ai_item(&prompt, true)],
            });
        }
        Intent::Url { url } => {
            groups.push(ResultGroup {
                kind: GroupKind::Web,
                items: vec![SearchItem {
                    id: url.clone(),
                    title: url.clone(),
                    subtitle: Some("Open in clean reader".into()),
                    badge: "↗".into(),
                    score: 1.0,
                    action: Action::OpenReader { url },
                }],
            });
        }
        Intent::PathLike | Intent::Mixed => {
            if is_mixed {
                groups.push(ResultGroup {
                    kind: GroupKind::Apps,
                    items: state.index.search_apps(query, 5),
                });
            }
            groups.push(ResultGroup {
                kind: GroupKind::Files,
                items: state.index.search_names(query, 7),
            });
            groups.push(ResultGroup {
                kind: GroupKind::Content,
                items: state.index.search_content(query, 5),
            });
            if is_mixed {
                groups.push(ResultGroup {
                    kind: GroupKind::Web,
                    items: vec![SearchItem {
                        id: format!("web:{query}"),
                        title: format!("Search the web for \"{query}\""),
                        subtitle: Some("DuckDuckGo — results open in the reader".into()),
                        badge: "W".into(),
                        score: 0.1,
                        action: Action::Command { name: "web".into(), args: query.to_string() },
                    }],
                });
                groups.push(ResultGroup {
                    kind: GroupKind::Ai,
                    items: vec![ai_item(query, true)],
                });
            }
        }
    }
    groups.retain(|g| !g.items.is_empty());
    SearchResponse { intent: label, groups }
}

fn ai_item(prompt: &str, agent: bool) -> SearchItem {
    SearchItem {
        id: "ai".into(),
        title: format!("Ask AI: {prompt}"),
        subtitle: Some(if agent {
            "Agent with system tools — local model, cloud fallback".into()
        } else {
            "local model → cloud fallback".into()
        }),
        badge: "◆".into(),
        score: 0.1,
        action: Action::AskAi { prompt: prompt.to_string() },
    }
}

/// Built-in slash commands + every registered plugin tool as `/<name>`.
fn slash_commands(filter: &str, state: &State<AppState>) -> Vec<SearchItem> {
    let mut all: Vec<(&str, &str, &str, bool)> = vec![
        ("/ai", "Ask the agent — it can use every tool on this machine", "◆", false),
        ("/run", "Run a shell command with full access", ">_", false),
        ("/web", "Search the web — results open in the clean reader", "W", false),
        ("/tools", "List tools — or create one with: /tools new <idea>", "TL", false),
        ("/index", "File index status", "IX", false),
        ("/settings", "Providers, config and app info", "ST", false),
    ];
    for plugin in state.registry.read().unwrap_or_else(|e| e.into_inner()).plugins() {
        all.push((Box::leak(format!("/{}", plugin.name).into_boxed_str()), Box::leak(plugin.description.clone().into_boxed_str()), "PL", plugin.destructive));
    }
    all.iter()
        .filter(|(name, _, _, _)| filter.is_empty() || name.trim_start_matches('/').starts_with(filter))
        .map(|(name, desc, badge, destructive)| SearchItem {
            id: name.to_string(),
            title: format!("{name}  —  {desc}"),
            subtitle: if *destructive { Some("destructive — asks first".into()) } else { None },
            badge: badge.to_string(),
            score: 1.0,
            action: Action::Command {
                name: name.trim_start_matches('/').to_string(),
                args: String::new(),
            },
        })
        .collect()
}

#[tauri::command]
pub fn activate(action: Action, app: tauri::AppHandle) -> Result<String, String> {
    let close_after = |app: &tauri::AppHandle| {
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.hide();
        }
    };
    match action {
        Action::OpenPath { path } => {
            app.opener()
                .open_path(path.clone(), None::<&str>)
                .map_err(|e| format!("failed to open {path}: {e}"))?;
            close_after(&app);
            Ok(String::new())
        }
        Action::OpenUrl { url } => {
            app.opener()
                .open_url(url.clone(), None::<&str>)
                .map_err(|e| format!("failed to open {url}: {e}"))?;
            close_after(&app);
            Ok(String::new())
        }
        Action::OpenReader { url } => {
            close_after(&app);
            tauri::async_runtime::spawn(async move {
                let _ = crate::web::open_reader(tauri::AppHandle::clone(&app), url).await;
            });
            Ok(String::new())
        }
        Action::LaunchApp { exec, app_name } => {
            crate::launch(&exec).map_err(|e| format!("failed to launch {app_name}: {e}"))?;
            close_after(&app);
            Ok(String::new())
        }
        Action::Command { name, args } => match name.as_str() {
            "index" => Ok(index_status_message(&app)),
            "settings" => {
                let msg = settings_message(&app);
                if let Some(dir) = neu_provider::config_path() {
                    let dir = dir.parent().map(|p| p.to_path_buf()).unwrap_or(dir);
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = app
                        .opener()
                        .open_path(dir.to_string_lossy().to_string(), None::<&str>);
                }
                Ok(msg)
            }
            _ => Ok(format!("{name} {args}").trim().to_string()),
        },
        // handled client-side (shell view / answer view / tools)
        Action::RunShell { .. } | Action::AskAi { .. } | Action::NotImplemented { .. } => {
            Ok(String::new())
        }
    }
}

fn index_status_message(app: &tauri::AppHandle) -> String {
    let state = app.state::<AppState>();
    let s = state.index.stats();
    format!(
        "Index: {} files · {} dirs · {} content docs · walk {} · watcher {}",
        s.files,
        s.dirs,
        s.content_docs,
        if s.walk_done { "complete" } else { "running" },
        if s.watcher_alive { "live" } else { "starting" },
    )
}

fn settings_message(app: &tauri::AppHandle) -> String {
    let state = app.state::<AppState>();
    let gateway_blocking = state.gateway.blocking_read();
    let providers = match gateway_blocking.as_ref() {
        Some(g) => g
            .providers
            .iter()
            .map(|p| {
                format!(
                    "{}:{}{}",
                    p.id,
                    p.model,
                    if p.local { " (local)" } else { "" }
                )
            })
            .collect::<Vec<_>>()
            .join(", "),
        None => "not detected yet — ask AI once to trigger discovery".into(),
    };
    let config = neu_provider::config_path()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "~/.config/neuos/config.toml".into());
    format!("Providers: {providers} · config: {config} · hotkey: {}", state.hotkey.lock().map(|h| h.clone()).unwrap_or_default())
}

#[tauri::command]
pub fn index_status(app: tauri::AppHandle) -> String {
    index_status_message(&app)
}

#[tauri::command]
pub async fn web_search(query: String) -> Result<Vec<SearchItem>, String> {
    Ok(crate::web::search_items(&query).await)
}
