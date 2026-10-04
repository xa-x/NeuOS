//! neu-tools — the tool registry.
//!
//! Every agent capability is a tool: built-ins implemented in the app and
//! plugin tools loaded from `~/.neuos/tools/<name>/manifest.toml` plus a
//! command (JSON args on stdin, JSON on stdout). The registry can be
//! re-scanned at runtime, which is how the agent's own authored tools
//! (`/tools new …`) become live immediately.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum ToolExecutor {
    Builtin,
    Script { command: String, dir: PathBuf },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the `arguments` object.
    pub parameters: serde_json::Value,
    pub executor: ToolExecutor,
    pub destructive: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolListing {
    pub name: String,
    pub description: String,
    pub source: &'static str, // "builtin" | "plugin"
    pub destructive: bool,
}

#[derive(Default)]
pub struct Registry {
    plugins: Vec<ToolSpec>,
}

impl Registry {
    pub fn tools_dir() -> Option<PathBuf> {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".neuos/tools"))
    }

    /// Scan the plugin dir (missing dir is fine — zero plugins).
    pub fn load() -> Self {
        let mut plugins = Vec::new();
        let Some(dir) = Self::tools_dir() else { return Self { plugins } };
        let Ok(entries) = std::fs::read_dir(&dir) else { return Self { plugins } };
        for entry in entries.flatten() {
            if let Some(spec) = load_plugin(entry.path()) {
                plugins.push(spec);
            }
        }
        plugins.sort_by(|a, b| a.name.cmp(&b.name));
        Self { plugins }
    }

    pub fn plugins(&self) -> &[ToolSpec] {
        &self.plugins
    }

    pub fn all(&self) -> Vec<ToolSpec> {
        let mut all = builtin_specs();
        all.extend(self.plugins.iter().cloned());
        all
    }

    pub fn find(&self, name: &str) -> Option<ToolSpec> {
        self.plugins
            .iter()
            .find(|t| t.name == name)
            .cloned()
            .or_else(|| builtin_specs().into_iter().find(|t| t.name == name))
    }

    pub fn listings(&self) -> Vec<ToolListing> {
        let mut out: Vec<ToolListing> = builtin_specs()
            .into_iter()
            .map(|t| ToolListing {
                name: t.name,
                description: t.description,
                source: "builtin",
                destructive: t.destructive,
            })
            .collect();
        out.extend(self.plugins.iter().map(|t| ToolListing {
            name: t.name.clone(),
            description: t.description.clone(),
            source: "plugin",
            destructive: t.destructive,
        }));
        out
    }
}

fn load_plugin(dir: PathBuf) -> Option<ToolSpec> {
    if !dir.is_dir() {
        return None;
    }
    let manifest = dir.join("manifest.toml");
    let text = std::fs::read_to_string(manifest).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    let name = value.get("name")?.as_str()?.to_string();
    let description = value.get("description").and_then(|d| d.as_str()).unwrap_or("").to_string();
    let command = value
        .get("command")
        .and_then(|c| c.as_str())
        .unwrap_or("bash run.sh")
        .to_string();
    let parameters = value
        .get("parameters")
        .and_then(|p| serde_json::to_value(p).ok())
        .unwrap_or(serde_json::json!({"type":"object","properties":{}}));
    let destructive = value
        .get("destructive")
        .and_then(|d| d.as_bool())
        .unwrap_or(false);
    Some(ToolSpec {
        name,
        description,
        parameters,
        executor: ToolExecutor::Script { command, dir },
        destructive,
    })
}

/// Write a plugin tool to the tools dir (used by `/tools new`).
pub fn write_plugin(
    name: &str,
    description: &str,
    parameters: &serde_json::Value,
    script: &str,
    destructive: bool,
) -> Result<PathBuf, String> {
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err("tool name must be lowercase-with-dashes".into());
    }
    let root = Registry::tools_dir().ok_or("no HOME")?;
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    // serialize the whole manifest as one table so nested [parameters.*]
    // headers come out correctly nested
    let manifest_value = toml::Value::Table({
        let mut t = toml::map::Map::new();
        t.insert("name".into(), toml::Value::String(name.to_string()));
        t.insert("description".into(), toml::Value::String(description.to_string()));
        t.insert("command".into(), toml::Value::String("bash run.sh".into()));
        t.insert("destructive".into(), toml::Value::Boolean(destructive));
        if let Ok(params) = toml::Value::try_from(parameters) {
            t.insert("parameters".into(), params);
        }
        t
    });
    let manifest = format!(
        "# authored by the NeuOS agent\n{}\n",
        toml::to_string_pretty(&manifest_value).map_err(|e| e.to_string())?
    );
    std::fs::write(dir.join("manifest.toml"), manifest).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("run.sh"), script).map_err(|e| e.to_string())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir.join("run.sh"), std::fs::Permissions::from_mode(0o755));
    }
    Ok(dir)
}

/// The built-in tool set the agent starts with.
pub fn builtin_specs() -> Vec<ToolSpec> {
    let arg = |desc: &str| {
        serde_json::json!({ "type": "object", "properties": { "path": { "type": "string", "description": desc } }, "required": ["path"] })
    };
    vec![
        ToolSpec { name: "open_path".into(), description: "Open a file or directory with the OS default handler.".into(), parameters: arg("path to open"), executor: ToolExecutor::Builtin, destructive: false },
        ToolSpec { name: "search_files".into(), description: "Search the local file index by name.".into(), parameters: serde_json::json!({ "type": "object", "properties": { "query": { "type": "string" } }, "required": ["query"] }), executor: ToolExecutor::Builtin, destructive: false },
        ToolSpec { name: "list_dir".into(), description: "List a directory's entries.".into(), parameters: arg("directory path"), executor: ToolExecutor::Builtin, destructive: false },
        ToolSpec { name: "read_file".into(), description: "Read a text file (first 64 KB).".into(), parameters: arg("path to read"), executor: ToolExecutor::Builtin, destructive: false },
        ToolSpec { name: "run_shell".into(), description: "Execute a shell command with the user's full permissions.".into(), parameters: serde_json::json!({ "type": "object", "properties": { "command": { "type": "string" } }, "required": ["command"] }), executor: ToolExecutor::Builtin, destructive: true },
        ToolSpec { name: "write_file".into(), description: "Create or overwrite a file.".into(), parameters: serde_json::json!({ "type": "object", "properties": { "path": { "type": "string" }, "content": { "type": "string" } }, "required": ["path", "content"] }), executor: ToolExecutor::Builtin, destructive: true },
        ToolSpec { name: "web_fetch".into(), description: "Fetch a URL and return its readable text.".into(), parameters: arg("url to fetch"), executor: ToolExecutor::Builtin, destructive: false },
        ToolSpec { name: "web_search".into(), description: "Search the web (DuckDuckGo) and return top results.".into(), parameters: serde_json::json!({ "type": "object", "properties": { "query": { "type": "string" } }, "required": ["query"] }), executor: ToolExecutor::Builtin, destructive: false },
        ToolSpec { name: "launch_app".into(), description: "Launch an installed application by name.".into(), parameters: serde_json::json!({ "type": "object", "properties": { "name": { "type": "string" } }, "required": ["name"] }), executor: ToolExecutor::Builtin, destructive: false },
    ]
}

#[allow(unused)]
fn _unused(p: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_roundtrip() {
        let written = write_plugin(
            "git-summary-test",
            "Summarize recent git activity",
            &serde_json::json!({"type":"object","properties":{"days":{"type":"number"}}}),
            "#!/usr/bin/env bash\necho '{}'",
            false,
        )
        .unwrap();
        let spec = load_plugin(written.clone()).expect("plugin loads");
        assert_eq!(spec.name, "git-summary-test");
        assert!(matches!(spec.executor, ToolExecutor::Script { .. }));
        let _ = std::fs::remove_dir_all(&written);
    }

    #[test]
    fn name_validation() {
        assert!(write_plugin("Bad Name", "", &serde_json::json!({}), "", false).is_err());
    }
}
