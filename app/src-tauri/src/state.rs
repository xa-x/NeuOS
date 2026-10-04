//! Shared app state: index, gateway, tool registry, confirm hub.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use neu_index::FileIndex;

pub struct AppState {
    pub index: Arc<FileIndex>,
    pub hotkey: Mutex<String>,
    /// When the launcher was last shown — ignore blur events within the
    /// grace window (WM focus race would otherwise hide it instantly).
    pub shown_at: Mutex<Option<Instant>>,
    /// True once the window has ever held focus. Blur-triggered hiding only
    /// applies after this — a show that never got focus must stay visible.
    pub had_focus: AtomicBool,
    /// Set after the first show; a user-dragged position is then respected.
    pub placed: AtomicBool,
    pub gateway: tokio::sync::RwLock<Option<neu_provider::Gateway>>,
    pub registry: RwLock<neu_tools::Registry>,
    pub confirms: ConfirmHub,
}

#[derive(Default)]
pub struct ConfirmHub {
    next: AtomicU64,
    pending: Mutex<HashMap<u64, tokio::sync::oneshot::Sender<bool>>>,
}

impl ConfirmHub {
    pub fn create(&self) -> (u64, tokio::sync::oneshot::Receiver<bool>) {
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
        (id, rx)
    }

    pub fn resolve(&self, id: u64, approve: bool) -> bool {
        if let Some(tx) = self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id) {
            tx.send(approve).is_ok()
        } else {
            false
        }
    }

    /// Ask via UI event; resolve when the user answers (60s timeout = deny).
    pub async fn ask(
        &self,
        app: &tauri::AppHandle,
        tool: &str,
        summary: &str,
    ) -> bool {
        use tauri::Emitter;
        let (id, rx) = self.create();
        let _ = app.emit(
            "agent-confirm",
            serde_json::json!({ "id": id, "tool": tool, "summary": summary }),
        );
        match tokio::time::timeout(std::time::Duration::from_secs(60), rx).await {
            Ok(Ok(true)) => true,
            _ => false,
        }
    }
}
