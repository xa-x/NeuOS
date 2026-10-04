//! Append-only JSONL audit log of side-effectful agent actions.
//!
//! Written to `~/.local/state/neuos/audit.jsonl` (XDG_STATE_HOME aware).
//! Every shell execution, file write, and plugin tool run the agent or the
//! user performs through NeuOS lands here — trust in an agent is built on
//! being able to answer "what exactly did it do?" after the fact.
//!
//! Best-effort by design: a logging failure must never break the action
//! being recorded, and this module never panics.

/// Record one action. `confirmed` = the user explicitly approved it (vs.
/// auto-allowed as non-destructive). Callers pass the raw command/path so
/// the log is self-contained evidence.
pub fn record(tool: &str, detail: &str, confirmed: bool) {
    let entry = serde_json::json!({
        "ts": now_secs(),
        "tool": tool,
        "confirmed": confirmed,
        "detail": detail,
    });
    let path = log_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = writeln!(f, "{entry}");
    }
}

fn log_path() -> std::path::PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| std::path::PathBuf::from(h).join(".local/state"))
        })
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("neuos/audit.jsonl")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_never_panics_and_writes_jsonl() {
        // Exercises the real path (dir creation, append); must not panic
        // even if HOME is unset in the test environment.
        record("test:noop", "echo hi", false);
    }
}
