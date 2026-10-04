//! neu-index — local search sources: installed apps, deep file-name index
//! (in-memory walk, live-updated) and full-text content index (Tantivy).

use neu_router::{Action, SearchItem};
use notify::{Event, RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Schema, Value, STRING, TEXT};
use tantivy::{doc, Index, IndexReader, IndexWriter, TantivyDocument};

/// An application discovered on the system (.desktop entries on Linux).
#[derive(Debug, Clone)]
pub struct AppEntry {
    pub name: String,
    pub exec: String,
    pub desktop_file: String,
}

#[derive(Debug, Clone)]
struct FileEntry {
    path: PathBuf,
    name_lc: String,
    is_dir: bool,
    mtime: u64,
    size: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStats {
    pub files: u64,
    pub dirs: u64,
    pub content_docs: u64,
    pub walk_done: bool,
    pub watcher_alive: bool,
}

/// Progress callback: (files seen, content docs, walk done)
pub type ProgressFn = Arc<dyn Fn(u64, u64, bool) + Send + Sync + 'static>;

const MAX_FILE_ENTRIES: usize = 400_000;
const CONTENT_MAX_BYTES: u64 = 2 * 1024 * 1024;
/// Beyond this many entries, name matching falls back to substring-only.
const FUZZY_ENTRY_LIMIT: usize = 200_000;

const EXCLUDED_DIRS: &[&str] = &[
    ".git", "node_modules", "target", ".cache", "__pycache__", ".venv", "venv",
    "dist", "build", ".npm", ".cargo", ".rustup", "go", "snap", ".local",
    ".docker", "VirtualBox VMs", ".terraform", ".next", ".nuxt", "vendor",
];

const TEXT_EXTENSIONS: &[&str] = &[
    "txt", "md", "rst", "rs", "py", "js", "mjs", "ts", "tsx", "jsx", "go",
    "java", "c", "h", "cpp", "hpp", "cc", "cs", "rb", "php", "sh", "bash",
    "zsh", "fish", "json", "toml", "yaml", "yml", "xml", "html", "htm", "css",
    "scss", "sql", "csv", "tsv", "ini", "conf", "cfg", "env", "service",
    "desktop", "lock", "log", "makefile", "mk", "dockerfile", "patch", "diff",
];

pub struct FileIndex {
    /// path → entry; single source of truth (no parallel Vec to avoid
    /// duplicating every path in memory).
    entries: RwLock<HashMap<PathBuf, FileEntry>>,
    apps: Vec<AppEntry>,
    stats_files: AtomicU64,
    stats_dirs: AtomicU64,
    walk_done: AtomicBool,
    watcher_alive: AtomicBool,
    tantivy: RwLock<Option<TantivyState>>,
    /// serializes content-index mutations (writer is !Sync)
    content_lock: Mutex<()>,
    progress: ProgressFn,
}

struct TantivyState {
    index: Index,
    reader: IndexReader,
    writer: IndexWriter,
    path_field: tantivy::schema::Field,
    name_field: tantivy::schema::Field,
    content_field: tantivy::schema::Field,
    docs: AtomicU64,
}

impl FileIndex {
    /// Load apps immediately; spawn background walk + watcher for files.
    pub fn start(progress: ProgressFn) -> Arc<Self> {
        let idx = Arc::new(Self {
            entries: RwLock::new(HashMap::new()),
            apps: load_apps(),
            stats_files: AtomicU64::new(0),
            stats_dirs: AtomicU64::new(0),
            walk_done: AtomicBool::new(false),
            watcher_alive: AtomicBool::new(false),
            tantivy: RwLock::new(None),
            content_lock: Mutex::new(()),
            progress,
        });

        std::thread::spawn({
            let idx = idx.clone();
            move || idx.run_walk()
        });
        std::thread::spawn({
            let idx = idx.clone();
            move || idx.run_watcher()
        });
        idx
    }

    pub fn apps(&self) -> &[AppEntry] {
        &self.apps
    }

    pub fn stats(&self) -> IndexStats {
        IndexStats {
            files: self.stats_files.load(Ordering::Relaxed),
            dirs: self.stats_dirs.load(Ordering::Relaxed),
            content_docs: self
                .tantivy
                .read()
                .map(|t| t.as_ref().map(|t| t.docs.load(Ordering::Relaxed)).unwrap_or(0))
                .unwrap_or(0),
            walk_done: self.walk_done.load(Ordering::Relaxed),
            watcher_alive: self.watcher_alive.load(Ordering::Relaxed),
        }
    }

    pub fn search_apps(&self, query: &str, limit: usize) -> Vec<SearchItem> {
        if query.is_empty() {
            return self
                .apps
                .iter()
                .take(limit)
                .map(|a| app_item(a, 1.0))
                .collect();
        }
        let mut scored: Vec<(f32, &AppEntry)> = self
            .apps
            .iter()
            .filter_map(|a| {
                let s = neu_router::fuzzy_score(&a.name, query);
                (s > 0.0).then_some((s, a))
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().take(limit).map(|(s, a)| app_item(a, s)).collect()
    }

    /// File-name search over the in-memory walk. Instant at any size:
    /// substring scoring always, fuzzy subsequence under the entry cap.
    pub fn search_names(&self, query: &str, limit: usize) -> Vec<SearchItem> {
        let entries = self.entries.read().unwrap_or_else(|e| e.into_inner());
        let needle = query.to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        let fuzzy_ok = entries.len() < FUZZY_ENTRY_LIMIT && needle.len() >= 2;
        let mut out: Vec<(f32, &FileEntry)> = Vec::new();
        for e in entries.values() {
            let Some(pos) = e.name_lc.find(&needle) else {
                if fuzzy_ok {
                    let s = fuzzy_score_str(&e.name_lc, &needle);
                    if s > 0.0 && out.len() < limit * 4 {
                        out.push((s * 0.7, e));
                    }
                }
                continue;
            };
            let mut s = 2.0;
            if pos == 0 {
                s += 3.0;
            }
            if e.name_lc.len() == needle.len() {
                s += 2.0; // exact name
            }
            out.push((s, e));
        }
        out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        out.truncate(limit);
        out.into_iter()
            .map(|(score, e)| SearchItem {
                id: e.path.to_string_lossy().to_string(),
                title: e
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| e.path.to_string_lossy().to_string()),
                subtitle: Some(
                    e.path
                        .parent()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default(),
                ),
                badge: if e.is_dir { "DIR".into() } else { "TXT".into() },
                score,
                action: Action::OpenPath { path: e.path.to_string_lossy().to_string() },
            })
            .collect()
    }

    /// Full-text content search via Tantivy (empty until the walk commits).
    pub fn search_content(&self, query: &str, limit: usize) -> Vec<SearchItem> {
        if query.len() < 3 {
            return Vec::new();
        }
        let guard = self.tantivy.read().unwrap_or_else(|e| e.into_inner());
        let Some(t) = guard.as_ref() else { return Vec::new() };
        let _ = t.reader.reload();
        let parser = QueryParser::for_index(&t.index, vec![t.content_field, t.name_field]);
        let Ok(q) = parser.parse_query(query) else { return Vec::new() };
        let searcher = t.reader.searcher();
        let Ok(top) = searcher.search(&q, &TopDocs::with_limit(limit)) else {
            return Vec::new();
        };
        top.into_iter()
            .filter_map(|(_, addr)| {
                let doc: TantivyDocument = searcher.doc(addr).ok()?;
                let path = doc.get_first(t.path_field)?.as_str()?.to_string();
                let name = Path::new(&path)
                    .file_name()?
                    .to_string_lossy()
                    .to_string();
                Some(SearchItem {
                    id: format!("content:{path}"),
                    title: name,
                    subtitle: Some(path.clone()),
                    badge: "≈".into(),
                    score: 0.05,
                    action: Action::OpenPath { path },
                })
            })
            .collect()
    }

    // ---- background walk -------------------------------------------------

    fn run_walk(&self) {
        let Some(home) = home_dir() else { return };
        let mut content_files: Vec<PathBuf> = Vec::new();
        let mut at_cap = false;

        let mut builder = ignore::WalkBuilder::new(&home);
        builder.hidden(true).git_ignore(true).git_global(true).git_exclude(true);
        builder.filter_entry(move |e| {
            let name = e.file_name().to_string_lossy();
            !EXCLUDED_DIRS.contains(&name.as_ref())
        });

        for entry in builder.build().flatten() {
            let Some(ft) = entry.file_type() else { continue };
            let path = entry.path().to_path_buf();
            let Ok(meta) = entry.metadata() else { continue };
            if ft.is_dir() {
                self.stats_dirs.fetch_add(1, Ordering::Relaxed);
                self.upsert_entry(&path, true, 0, 0);
                continue;
            }
            if self.entries.read().map(|e| e.len()).unwrap_or(0) >= MAX_FILE_ENTRIES {
                at_cap = true;
                break;
            }
            let size = meta.len();
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            self.stats_files.fetch_add(1, Ordering::Relaxed);
            self.upsert_entry(&path, false, mtime, size);
            if is_text_file(&path, size) {
                content_files.push(path);
            }
            if self.stats_files.load(Ordering::Relaxed) % 2000 == 0 {
                self.report(false);
            }
        }
        let _ = at_cap;

        self.build_content_index(content_files);
        self.walk_done.store(true, Ordering::Relaxed);
        self.report(true);
    }

    fn build_content_index(&self, files: Vec<PathBuf>) {
        let mut sb = Schema::builder();
        let path_field = sb.add_text_field("path", STRING | tantivy::schema::STORED);
        let name_field = sb.add_text_field("name", TEXT);
        // content is indexed but NOT stored — we only ever retrieve `path`,
        // and storing raw text would keep the whole corpus in RAM.
        let content_field = sb.add_text_field("content", TEXT);
        let schema = sb.build();

        // disk-backed: mmap segments keep anonymous memory low; the kernel
        // reclaims pages under pressure. ~/.cache/neuos/index
        let dir = std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(".cache/neuos/index"))
            .unwrap_or_else(|| PathBuf::from("/tmp/neuos-index"));
        let index = match Index::open_in_dir(&dir) {
            Ok(ix) => ix,
            Err(_) => {
                let _ = std::fs::remove_dir_all(&dir);
                let _ = std::fs::create_dir_all(&dir);
                match Index::create_in_dir(&dir, schema) { Ok(ix) => ix, Err(_) => return }
            }
        };
        let Ok(writer) = index.writer(32 * 1024 * 1024) else { return };
        let Ok(reader) = index.reader() else { return };

        let mut state = TantivyState {
            index: index.clone(),
            reader,
            writer,
            path_field,
            name_field,
            content_field,
            docs: AtomicU64::new(0),
        };

        // delete docs for files that no longer exist (previous run's leftovers)
        self.cleanup_ghosts(&mut state, &files);

        for (i, path) in files.iter().enumerate() {
            if let Some(text) = read_text_head(path) {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let d = doc!(
                    path_field => path.to_string_lossy().as_ref(),
                    name_field => name,
                    content_field => text
                );
                if state.writer.add_document(d).is_ok() {
                    state.docs.fetch_add(1, Ordering::Relaxed);
                }
            }
            if i % 4000 == 3999 {
                let _ = state.writer.commit();
                let _ = state.reader.reload();
                self.report(false);
            }
        }
        let _ = state.writer.commit();
        let _ = state.reader.reload();

        *self.tantivy.write().unwrap_or_else(|e| e.into_inner()) = Some(state);
    }

    /// Remove content docs whose file wasn't seen in this walk.
    fn cleanup_ghosts(&self, state: &mut TantivyState, seen: &[PathBuf]) {
        use std::collections::HashSet;
        let seen_set: HashSet<&Path> = seen.iter().map(|p| p.as_path()).collect();
        let _ = state.reader.reload();
        let searcher = state.reader.searcher();
        let mut stale: Vec<String> = Vec::new();
        for (ord, seg) in searcher.segment_readers().iter().enumerate() {
            for doc_id in 0..seg.max_doc() {
                let addr = tantivy::DocAddress::new(ord as u32, doc_id);
                if let Ok(doc) = searcher.doc::<TantivyDocument>(addr) {
                    if let Some(p) = doc.get_first(state.path_field).and_then(|v| v.as_str()) {
                        if !seen_set.contains(Path::new(p)) {
                            stale.push(p.to_string());
                        }
                    }
                }
            }
        }
        if stale.is_empty() {
            return;
        }
        eprintln!("neuos: pruning {} stale content docs", stale.len());
        for p in &stale {
            let term = tantivy::Term::from_field_text(state.path_field, p);
            let _ = state.writer.delete_term(term);
        }
        let _ = state.writer.commit();
        let _ = state.reader.reload();
    }

    fn upsert_entry(&self, path: &Path, is_dir: bool, mtime: u64, size: u64) {
        let name_lc = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if name_lc.is_empty() {
            return;
        }
        self.entries.write().unwrap_or_else(|e| e.into_inner()).insert(
            path.to_path_buf(),
            FileEntry { path: path.to_path_buf(), name_lc, is_dir, mtime, size },
        );
    }

    fn remove_entry(&self, path: &Path) {
        self.entries.write().unwrap_or_else(|e| e.into_inner()).remove(path);
    }

    fn report(&self, done: bool) {
        let docs = self
            .tantivy
            .read()
            .map(|t| t.as_ref().map(|t| t.docs.load(Ordering::Relaxed)).unwrap_or(0))
            .unwrap_or(0);
        (self.progress)(self.stats_files.load(Ordering::Relaxed), docs, done);
    }

    // ---- live watcher ----------------------------------------------------

    fn run_watcher(self: &Arc<Self>) {
        let Some(home) = home_dir() else { return };
        let (tx, rx) = std::sync::mpsc::channel::<Event>();
        let Ok(mut watcher) = notify::recommended_watcher(move |res: Result<Event, _>| {
            if let Ok(ev) = res {
                let _ = tx.send(ev);
            }
        }) else { return };
        if watcher.watch(&home, RecursiveMode::Recursive).is_err() {
            return;
        }
        self.watcher_alive.store(true, Ordering::Relaxed);

        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut last_flush = std::time::Instant::now();
        loop {
            // batch events for 1s after the last one, then apply
            let Ok(ev) = rx.recv_timeout(Duration::from_millis(200)) else {
                if !seen.is_empty() && last_flush.elapsed() > Duration::from_millis(800) {
                    self.apply_changes(std::mem::take(&mut seen));
                }
                continue;
            };
            use notify::EventKind as K;
            let relevant = matches!(
                ev.kind,
                K::Create(_) | K::Modify(_) | K::Remove(_)
            );
            if !relevant {
                continue;
            }
            for p in ev.paths {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if name.starts_with('.') || EXCLUDED_DIRS.contains(&name.as_str()) {
                    // still allow direct dotfiles at root? keep it simple: skip hidden
                    continue;
                }
                seen.insert(p);
            }
            last_flush = std::time::Instant::now();
        }
    }

    fn apply_changes(&self, paths: HashSet<PathBuf>) {
        let _c = self.content_lock.lock().unwrap_or_else(|e| e.into_inner());
        for path in paths {
            let Ok(meta) = std::fs::metadata(&path) else {
                self.remove_entry(&path);
                self.delete_content_doc(&path);
                continue;
            };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if meta.is_dir() {
                self.stats_dirs.fetch_add(1, Ordering::Relaxed);
                self.upsert_entry(&path, true, 0, 0);
                continue;
            }
            self.stats_files.fetch_add(1, Ordering::Relaxed);
            self.upsert_entry(&path, false, mtime, meta.len());
            if is_text_file(&path, meta.len()) {
                if let Some(text) = read_text_head(&path) {
                    self.add_content_doc(&path, text);
                }
            }
        }
    }

    fn add_content_doc(&self, path: &Path, text: String) {
        let mut guard = self.tantivy.write().unwrap_or_else(|e| e.into_inner());
        let Some(t) = guard.as_mut() else { return };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let d = doc!(
            t.path_field => path.to_string_lossy().as_ref(),
            t.name_field => name,
            t.content_field => text
        );
        if t.writer.add_document(d).is_ok() {
            t.docs.fetch_add(1, Ordering::Relaxed);
            let _ = t.writer.commit();
            let _ = t.reader.reload();
        }
    }

    fn delete_content_doc(&self, path: &Path) {
        let mut guard = self.tantivy.write().unwrap_or_else(|e| e.into_inner());
        let Some(t) = guard.as_mut() else { return };
        let term = tantivy::Term::from_field_text(t.path_field, &path.to_string_lossy());
        if t.writer.delete_term(term) > 0 {
            t.docs.fetch_sub(1, Ordering::Relaxed);
            let _ = t.writer.commit();
            let _ = t.reader.reload();
        }
    }
}

// helper conversions -------------------------------------------------------

fn app_item(a: &AppEntry, score: f32) -> SearchItem {
    SearchItem {
        id: a.desktop_file.clone(),
        title: a.name.clone(),
        subtitle: Some("Application".into()),
        badge: monogram(&a.name),
        score,
        action: Action::LaunchApp { exec: a.exec.clone(), app_name: a.name.clone() },
    }
}

pub fn monogram(name: &str) -> String {
    let mut chars = name.chars().filter(|c| c.is_alphanumeric());
    match (chars.next(), chars.next()) {
        (Some(a), Some(b)) => format!("{}{}", a, b).to_uppercase(),
        (Some(a), None) => a.to_uppercase().to_string(),
        _ => "?".into(),
    }
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).filter(|p| !p.as_os_str().is_empty())
}

fn is_text_file(path: &Path, size: u64) -> bool {
    if size == 0 || size > CONTENT_MAX_BYTES {
        return false;
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if TEXT_EXTENSIONS.contains(&ext.as_str()) {
        return true;
    }
    // extensionless common files
    matches!(
        path.file_name().map(|n| n.to_string_lossy().to_lowercase()).as_deref(),
        Some("makefile" | "dockerfile" | "license" | "readme")
    )
}

fn read_text_head(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 48 * 1024];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    if buf.contains(&0) {
        return None; // binary
    }
    String::from_utf8(buf).ok()
}

/// Fuzzy subsequence score on pre-lowercased strings.
fn fuzzy_score_str(haystack_lc: &str, needle_lc: &str) -> f32 {
    let h: Vec<char> = haystack_lc.chars().collect();
    let n: Vec<char> = needle_lc.chars().collect();
    let mut hi = 0usize;
    let mut run = 0usize;
    let mut score = 0.0f32;
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
            s += 2.0;
        }
        if run > 0 {
            s += 1.0 * run as f32;
        }
        score += s;
        run += 1;
        hi = pos + 1;
        first = false;
    }
    score
}

// ---- .desktop app discovery ---------------------------------------------

/// Parse `Exec=` lines: strip desktop field codes (%f %F %u %U %i %c %k).
pub fn clean_exec(exec: &str) -> String {
    let mut cleaned = String::with_capacity(exec.len());
    let mut in_quote = false;
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_quote = !in_quote;
                cleaned.push(c);
            }
            '%' if !in_quote => {
                chars.next();
            }
            _ => cleaned.push(c),
        }
    }
    cleaned.trim().trim_end_matches(':').trim_end().to_string()
}

fn load_apps() -> Vec<AppEntry> {
    let mut dirs: Vec<PathBuf> = vec![
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/usr/local/share/applications"),
    ];
    if let Some(home) = home_dir() {
        dirs.push(home.join(".local/share/applications"));
    }
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));

    let mut apps = Vec::new();
    let mut seen = HashSet::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            if let Some(app) = parse_desktop(&path) {
                if seen.insert(app.name.to_lowercase()) {
                    apps.push(app);
                }
            }
        }
    }
    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps
}

fn parse_desktop(path: &Path) -> Option<AppEntry> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut in_entry = false;
    let mut name = None;
    let mut exec = None;
    let mut nodisplay = false;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        match key {
            "Name" if name.is_none() => name = Some(value.trim().trim_matches('"').to_string()),
            "Exec" if exec.is_none() => exec = Some(clean_exec(value)),
            "NoDisplay" | "Hidden" if value.trim() == "true" => nodisplay = true,
            _ => {}
        }
    }
    if nodisplay {
        return None;
    }
    Some(AppEntry {
        name: name?,
        exec: exec.unwrap_or_default(),
        desktop_file: path.to_string_lossy().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_field_codes() {
        assert_eq!(clean_exec("firefox %u"), "firefox");
        assert_eq!(clean_exec("code --goto %f:%l"), "code --goto");
        assert_eq!(clean_exec("sh -c \"thing $@\""), "sh -c \"thing $@\"");
    }

    #[test]
    fn text_file_detection() {
        assert!(is_text_file(Path::new("/a/b/main.rs"), 10));
        assert!(!is_text_file(Path::new("/a/b/image.png"), 10));
        assert!(!is_text_file(Path::new("/a/b/main.rs"), 9 * 1024 * 1024));
        assert!(is_text_file(Path::new("/a/Makefile"), 10));
    }

    #[test]
    fn fuzzy() {
        assert!(fuzzy_score_str("firefox", "ffx") > 0.0);
        assert_eq!(fuzzy_score_str("zed", "xyz"), 0.0);
    }
}
