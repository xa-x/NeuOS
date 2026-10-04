//! neu-router — intent routing and shared result types.
//!
//! Classifies what the user meant by their query (files, apps, web, AI,
//! shell, slash command) and defines the `SearchItem`/`ResultGroup` shapes
//! that every backend (index, web, AI, tools) produces.

use serde::{Deserialize, Serialize};

/// A single actionable result shown in the launcher list.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Action {
    /// Open a file/directory with the OS default handler.
    OpenPath { path: String },
    /// Launch an application by parsed .desktop exec line.
    LaunchApp { exec: String, app_name: String },
    /// Open a URL (default browser for now; reader window later).
    OpenUrl { url: String },
    /// Open a URL in NeuOS's clean reader window.
    OpenReader { url: String },
    /// Ask the AI gateway a question.
    AskAi { prompt: String },
    /// Run a shell command (M2; full access with confirm gate).
    RunShell { command: String },
    /// A slash command or plugin tool invocation.
    Command { name: String, args: String },
    /// Nothing wired yet (milestone placeholder surfaced honestly in UI).
    NotImplemented { what: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchItem {
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    /// Two-letter monogram for the icon slot until real icon lookup lands.
    pub badge: String,
    pub score: f32,
    pub action: Action,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GroupKind {
    Commands,
    Apps,
    Files,
    Content,
    Web,
    Ai,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultGroup {
    pub kind: GroupKind,
    pub items: Vec<SearchItem>,
}

/// What the query most likely means. Drives which backends we query.
#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    Empty,
    /// `/...` — slash command palette.
    SlashCommand { name: String, args: String },
    /// `> ...` or `! ...` — explicit shell intent.
    Shell { command: String },
    /// `? ...` — explicit AI intent.
    Ai { prompt: String },
    /// Looks like a URL or domain.
    Url { url: String },
    /// Contains path separators or a leading `~`/`/` — lean files.
    PathLike,
    /// Anything else: query everything and group results.
    Mixed,
}

pub fn route(query: &str) -> Intent {
    let q = query.trim();
    if q.is_empty() {
        return Intent::Empty;
    }
    if let Some(rest) = q.strip_prefix('/') {
        let mut parts = rest.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or("").to_string();
        let args = parts.next().unwrap_or("").trim().to_string();
        return Intent::SlashCommand { name, args };
    }
    if let Some(rest) = q.strip_prefix('>') {
        return Intent::Shell { command: rest.trim().to_string() };
    }
    if let Some(rest) = q.strip_prefix('?') {
        return Intent::Ai { prompt: rest.trim().to_string() };
    }
    if is_url(q) {
        return Intent::Url { url: normalize_url(q) };
    }
    if q.starts_with('~') || q.starts_with("./") || q.starts_with("../") || q.contains('/') {
        return Intent::PathLike;
    }
    Intent::Mixed
}

fn is_url(s: &str) -> bool {
    let s = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://")).unwrap_or(s);
    if s.contains(' ') || s.is_empty() {
        return false;
    }
    if s.starts_with("www.") {
        return true;
    }
    // bare domain like example.com or example.com/path
    match s.split_once('.') {
        Some((head, tail)) => {
            !head.is_empty()
                && !tail.is_empty()
                && (tail.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
                    || tail.contains('/'))
        }
        None => false,
    }
}

fn normalize_url(s: &str) -> String {
    if s.starts_with("http://") || s.starts_with("https://") {
        s.to_string()
    } else {
        format!("https://{s}")
    }
}

/// Simple fuzzy scorer: case-insensitive; rewards prefix matches and
/// consecutive-character runs. Higher is better; 0.0 means no match.
pub fn fuzzy_score(haystack: &str, needle: &str) -> f32 {
    if needle.is_empty() {
        return 0.1;
    }
    let h: Vec<char> = haystack.to_lowercase().chars().collect();
    let n: Vec<char> = needle.to_lowercase().chars().collect();
    let mut score = 0.0f32;
    let mut hi = 0usize;
    let mut run = 0usize;
    let mut first = true;
    for &nc in &n {
        let mut found = None;
        while hi < h.len() {
            if h[hi] == nc {
                found = Some(hi);
                break;
            }
            hi += 1;
        }
        let Some(pos) = found else { return 0.0 };
        let mut s = 1.0;
        if first && pos == 0 {
            s += 3.0; // prefix
        }
        if run > 0 {
            s += 1.5 * run as f32; // consecutive
        }
        // word-boundary bonus (after space, slash, dot, dash, underscore)
        if pos > 0 {
            let prev = h[pos - 1];
            if prev == ' ' || prev == '/' || prev == '.' || prev == '-' || prev == '_' {
                s += 1.0;
            }
        }
        score += s;
        run += 1;
        hi = pos + 1;
        first = false;
    }
    // prefer shorter haystacks for the same match
    score * (1.0 - (h.len() as f32 * 0.002).min(0.4))
}

pub fn matches(haystack: &str, needle: &str) -> bool {
    fuzzy_score(haystack, needle) > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_intents() {
        assert!(matches!(route(""), Intent::Empty));
        assert!(matches!(route("/run ls"), Intent::SlashCommand { .. }));
        assert!(matches!(route("> ls -la"), Intent::Shell { .. }));
        assert!(matches!(route("? what is rust"), Intent::Ai { .. }));
        assert!(matches!(route("example.com"), Intent::Url { .. }));
        assert!(matches!(route("~/code"), Intent::PathLike));
        assert!(matches!(route("readme"), Intent::Mixed));
    }

    #[test]
    fn url_detection() {
        assert!(matches!(route("github.com/tauri-apps/tauri"), Intent::Url { .. }));
        assert!(!matches!(route("todo.md notes"), Intent::Url { .. }));
    }

    #[test]
    fn fuzzy_ranking() {
        let exact = fuzzy_score("Firefox", "firefox");
        let partial = fuzzy_score("Firefox Web Browser", "firefox");
        assert!(exact > partial);
        assert_eq!(fuzzy_score("Notes", "zzz"), 0.0);
    }
}
