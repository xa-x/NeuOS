//! NeuOS app shell — window lifecycle, hotkey, IPC wiring.

mod ai;
mod audit;
mod search;
mod shell;
mod state;
mod web;

use neu_index::FileIndex;
use state::{AppState, ConfirmHub};
use tauri::{Emitter, Manager, RunEvent, WebviewWindow, WindowEvent};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

pub fn launch(exec: &str) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    #[cfg(unix)]
    let mut cmd = {
        let mut c = Command::new("sh");
        c.arg("-c").arg(exec);
        c
    };
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(exec);
        c
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    Ok(())
}

#[tauri::command]
fn hide_window(app: tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.hide();
    }
}

#[tauri::command]
fn active_hotkey(state: tauri::State<AppState>) -> String {
    state.hotkey.lock().map(|h| h.clone()).unwrap_or_default()
}

/// Try hotkey candidates in order — desktop environments often own the
/// first choice (GNOME grabs Alt+Space for its window menu on X11).
fn register_hotkey(app: &tauri::AppHandle) {
    let candidates = ["alt+space", "ctrl+space", "ctrl+alt+space"];
    for candidate in candidates {
        let Ok(shortcut) = candidate.parse::<Shortcut>() else { continue };
        let result = app.global_shortcut().on_shortcut(shortcut, |app, _s, event| {
            if event.state == ShortcutState::Pressed {
                toggle_launcher(app);
            }
        });
        match result {
            Ok(()) => {
                eprintln!("neuos: global hotkey active: {candidate}");
                if let Some(state) = app.try_state::<AppState>() {
                    if let Ok(mut h) = state.hotkey.lock() {
                        *h = candidate.to_string();
                    }
                }
                return;
            }
            Err(e) => eprintln!("neuos: hotkey {candidate} unavailable ({e}), trying next"),
        }
    }
    eprintln!("neuos: WARNING no global hotkey registered — launch the window manually for now");
}

fn toggle_launcher(app: &tauri::AppHandle) {
    let Some(win) = app.get_webview_window("main") else { return };
    if win.is_visible().unwrap_or(false) && win.is_focused().unwrap_or(false) {
        let _ = win.hide();
    } else {
        let first_show = !app
            .try_state::<AppState>()
            .map(|s| s.placed.swap(true, std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(true);
        if first_show {
            position_center_top(&win);
        }
        let _ = win.show();
        let _ = win.set_focus();
        if let Some(state) = app.try_state::<AppState>() {
            if let Ok(mut at) = state.shown_at.lock() {
                *at = Some(std::time::Instant::now());
            }
        }
        let _ = win.emit_to("main", "launcher-shown", ());
    }
}

/// Place the bar horizontally centered, ~22% from the top of the current monitor.
fn position_center_top(win: &WebviewWindow) {
    if let (Ok(Some(monitor)), Ok(size)) = (win.current_monitor(), win.outer_size()) {
        let msize = monitor.size();
        let pos = monitor.position();
        let x = pos.x + msize.width.saturating_sub(size.width) as i32 / 2;
        let y = pos.y + (msize.height as i32 * 22 / 100);
        let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
    }
}

/// Watch ~/.neuos/tools and hot-reload the registry when tools change.
fn watch_tools_dir(app: tauri::AppHandle) {
    let Some(dir) = neu_tools::Registry::tools_dir() else { return };
    let _ = std::fs::create_dir_all(&dir);
    std::thread::spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let Ok(mut watcher) = notify::recommended_watcher(move |res: Result<notify::Event, _>| {
            if res.is_ok() {
                let _ = tx.send(());
            }
        }) else { return };
        use notify::Watcher;
        if watcher.watch(&dir, notify::RecursiveMode::Recursive).is_err() {
            return;
        }
        let mut last = std::time::Instant::now() - std::time::Duration::from_secs(10);
        while rx.recv().is_ok() {
            // debounce bursts
            while rx.recv_timeout(std::time::Duration::from_millis(300)).is_ok() {}
            if last.elapsed() < std::time::Duration::from_secs(1) {
                continue;
            }
            last = std::time::Instant::now();
            let state = app.state::<AppState>();
            *state.registry.write().unwrap_or_else(|e| e.into_inner()) = neu_tools::Registry::load();
            eprintln!("neuos: tools reloaded");
        }
    });
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // second launch: summon the existing window instead
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
                let _ = win.set_focus();
                let _ = win.emit_to("main", "launcher-shown", ());
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            let progress_app = app.handle().clone();
            let index = FileIndex::start(std::sync::Arc::new(move |files, docs, done| {
                let _ = progress_app.emit(
                    "index-progress",
                    serde_json::json!({ "files": files, "docs": docs, "done": done }),
                );
            }));
            app.manage(AppState {
                index,
                hotkey: std::sync::Mutex::new("alt+space".into()),
                shown_at: std::sync::Mutex::new(None),
                had_focus: std::sync::atomic::AtomicBool::new(false),
                placed: std::sync::atomic::AtomicBool::new(false),
                gateway: tokio::sync::RwLock::new(None),
                registry: std::sync::RwLock::new(neu_tools::Registry::load()),
                confirms: ConfirmHub::default(),
            });
            register_hotkey(app.handle());
            watch_tools_dir(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            search::search,
            search::activate,
            search::index_status,
            search::web_search,
            hide_window,
            active_hotkey,
            shell::run_shell,
            ai::agent_ask,
            ai::agent_confirm,
            ai::tools_list,
            ai::tool_run,
            ai::tools_new,
            ai::reload_tools,
            web::open_reader,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let RunEvent::WindowEvent { label, event, .. } = event {
                if label == "main" {
                    match event {
                        WindowEvent::Focused(true) => {
                            if let Some(s) = app.try_state::<AppState>() {
                                s.had_focus.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                        WindowEvent::Focused(false) => {
                            let keep = match app.try_state::<AppState>() {
                                Some(s) => {
                                    let in_grace = s
                                        .shown_at
                                        .lock()
                                        .ok()
                                        .and_then(|at| {
                                            at.map(|t| t.elapsed() < std::time::Duration::from_millis(400))
                                        })
                                        .unwrap_or(false);
                                    let never_focused =
                                        !s.had_focus.load(std::sync::atomic::Ordering::Relaxed);
                                    in_grace || never_focused
                                }
                                None => false,
                            };
                            if !keep {
                                if let Some(win) = app.get_webview_window("main") {
                                    let _ = win.hide();
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        });
}
