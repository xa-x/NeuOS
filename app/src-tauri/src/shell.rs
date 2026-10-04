//! Shell execution with streaming output and a destructive-action gate.

use tauri::Emitter;
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum ShellStart {
    Started,
    NeedConfirm { reason: String },
}

const DESTRUCTIVE_PATTERNS: &[&str] = &[
    "rm ", "rm\n", "rmdir", "mkfs", "dd ", "sudo", "shutdown", "reboot", "halt",
    "chmod -R", "chown -R", "mv /", "> /", "truncate", "shred", "wipefs",
    "git push --force", "git reset --hard", ":(){", "| sh", "| bash", "|sh",
    "|bash", "curl", "wget", "kill ", "pkill", "killall",
];

/// Structural red flags: indirection that pattern matching cannot see
/// through. A command using these may not be what its visible text says —
/// `bash -c` hides the payload in a nested string, `$()`/backticks run it
/// before the outer command is even judged, base64 smuggles it, and piping
/// into an interpreter executes unvetted text. All of these confirm even
/// when they look harmless.
const OBFUSCATION_PATTERNS: &[&str] = &[
    "bash -c", "sh -c", "zsh -c", "bash -lc", "sh -lc", "eval ", "base64",
    "$(", "`", "| python", "|python", "| perl", "|perl", "| ruby", "|ruby",
    "| node", "|node",
];

pub fn destructive_reason(cmd: &str) -> Option<String> {
    let lc = format!("{}\n", cmd.trim().to_lowercase());
    for p in OBFUSCATION_PATTERNS {
        if lc.contains(p) {
            return Some(format!(
                "uses indirection “{}” — the visible text may not be what runs",
                p.trim()
            ));
        }
    }
    for p in DESTRUCTIVE_PATTERNS {
        if lc.contains(p) {
            return Some(format!("matches destructive pattern “{}”", p.trim()));
        }
    }
    None
}

/// Run a shell command, streaming output lines as `shell-out` events and a
/// final `shell-done`. The command itself is invoked through the user's
/// login shell with their full permissions.
#[tauri::command]
pub async fn run_shell(
    app: tauri::AppHandle,
    command: String,
    confirmed: bool,
) -> Result<ShellStart, String> {
    if !confirmed {
        if let Some(reason) = destructive_reason(&command) {
            crate::audit::record("run_shell", &command, false);
            return Ok(ShellStart::NeedConfirm { reason });
        }
    }

    crate::audit::record("run_shell", &command, confirmed);

    let mut child = tokio::process::Command::new("bash")
        .arg("-lc")
        .arg(&command)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to spawn: {e}"))?;

    let pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let emit = {
        let app = app.clone();
        move |stream: &'static str, line: String| {
            use tauri::Emitter;
            let _ = app.emit("shell-out", serde_json::json!({ "stream": stream, "line": line }));
        }
    };

    let out_task = tokio::spawn(async move {
        if let Some(out) = stdout {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                emit("out", line);
            }
        }
    });
    let err_task = {
        let app = app.clone();
        let stderr = stderr;
        tokio::spawn(async move {
            if let Some(err) = stderr {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let _ = app.emit("shell-out", serde_json::json!({ "stream": "err", "line": line }));
                }
            }
        })
    };

    let _ = app.emit("shell-started", serde_json::json!({ "pid": pid }));

    let code = tokio::time::timeout(std::time::Duration::from_secs(120), child.wait())
        .await
        .ok()
        .and_then(|r| r.ok())
        .and_then(|s| s.code());

    let _ = out_task.await;
    let _ = err_task.await;
    let _ = app.emit("shell-done", serde_json::json!({ "code": code }));
    Ok(ShellStart::Started)
}
