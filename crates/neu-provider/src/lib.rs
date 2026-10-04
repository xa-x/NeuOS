//! neu-provider — provider-agnostic model gateway.
//!
//! One interface over local OpenAI-compatible servers (Ollama, LM Studio,
//! llama.cpp, vLLM — auto-detected), cloud OpenAI-compatible APIs
//! (OpenAI, DeepSeek) and Anthropic's native API. Streams deltas and
//! tool calls in a normalized shape.

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::time::Duration;

// ---------------------------------------------------------------------------
// types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Msg {
    pub role: Role,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Msg {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: Role::System, content: content.into(), tool_calls: vec![], tool_call_id: None }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: Role::User, content: content.into(), tool_calls: vec![], tool_call_id: None }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: Role::Assistant, content: content.into(), tool_calls: vec![], tool_call_id: None }
    }
    pub fn tool_result(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: vec![],
            tool_call_id: Some(id.into()),
        }
    }
}

/// Neutral tool definition passed to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum ChatEvent {
    Delta { text: String },
    ToolCall { call: ToolCall },
    Done { stop: StopReason },
    Error { message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StopReason {
    EndTurn,
    ToolUse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    OpenAiCompat,
    Anthropic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: String,
    pub kind: Kind,
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub local: bool,
}

// ---------------------------------------------------------------------------
// gateway

#[derive(Default, Clone)]
pub struct Gateway {
    pub providers: Vec<Provider>,
    pub default_id: Option<String>,
}

const LOCAL_PROBES: &[(&str, u16, &str)] = &[
    ("ollama", 11434, "Ollama"),
    ("lmstudio", 1234, "LM Studio"),
    ("llamacpp", 8080, "llama.cpp"),
    ("vllm", 8000, "vLLM"),
];

const BAD_MODEL_HINTS: &[&str] = &["embed", "rerank", "clip", "whisper", "tts", "guard", "flux", "sd-", "bge", "nomic"];
const GOOD_MODEL_HINTS: &[&str] = &["qwen", "llama", "deepseek", "gpt", "claude", "mistral", "kimi", "glm"];

impl Gateway {
    /// Auto-detect local servers + env-keyed cloud providers, then overlay
    /// the user's config file (~/.config/neuos/config.toml) if present.
    pub async fn discover() -> Self {
        let mut providers = Vec::new();

        for (id, port, label) in LOCAL_PROBES {
            let base = format!("http://127.0.0.1:{port}/v1");
            if let Some(model) = probe_local(&base).await {
                providers.push(Provider {
                    id: id.to_string(),
                    kind: Kind::OpenAiCompat,
                    base_url: base,
                    api_key: None,
                    model,
                    local: true,
                });
                eprintln!("neuos: found local model server {label} on :{port}");
            }
        }

        if let Ok(key) = std::env::var("OPENAI_API_KEY") {
            providers.push(Provider {
                id: "openai".into(),
                kind: Kind::OpenAiCompat,
                base_url: env_or("NEUOS_OPENAI_BASE", "https://api.openai.com/v1"),
                api_key: Some(key),
                model: env_or("NEUOS_OPENAI_MODEL", "gpt-4o-mini"),
                local: false,
            });
        }
        if let Ok(key) = std::env::var("DEEPSEEK_API_KEY") {
            providers.push(Provider {
                id: "deepseek".into(),
                kind: Kind::OpenAiCompat,
                base_url: env_or("NEUOS_DEEPSEEK_BASE", "https://api.deepseek.com/v1"),
                api_key: Some(key),
                model: env_or("NEUOS_DEEPSEEK_MODEL", "deepseek-chat"),
                local: false,
            });
        }
        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
            providers.push(Provider {
                id: "anthropic".into(),
                kind: Kind::Anthropic,
                base_url: env_or("NEUOS_ANTHROPIC_BASE", "https://api.anthropic.com"),
                api_key: Some(key),
                model: env_or("NEUOS_ANTHROPIC_MODEL", "claude-sonnet-4-5-20250929"),
                local: false,
            });
        }

        let mut default_id = None;
        if let Some(cfg) = load_config() {
            for p in cfg.provider {
                providers.retain(|existing| existing.id != p.id);
                let local = p.base_url.contains("127.0.0.1") || p.base_url.contains("localhost");
                providers.push(Provider {
                    id: p.id,
                    kind: match p.kind.as_deref() {
                        Some("anthropic") => Kind::Anthropic,
                        _ => Kind::OpenAiCompat,
                    },
                    base_url: p.base_url,
                    api_key: p.api_key,
                    model: p.model,
                    local,
                });
            }
            default_id = cfg.ai.and_then(|a| a.default_provider);
        }

        if default_id.is_none() {
            default_id = providers
                .iter()
                .find(|p| p.local)
                .or_else(|| providers.first())
                .map(|p| p.id.clone());
        }

        Self { providers, default_id }
    }

    pub fn default_provider(&self) -> Option<&Provider> {
        let id = self.default_id.as_deref()?;
        self.providers.iter().find(|p| p.id == id)
    }

    /// Start a streaming chat. Returns the provider used plus an event receiver.
    pub async fn chat(
        &self,
        messages: &[Msg],
        tools: &[ToolDef],
    ) -> Result<(Provider, tokio::sync::mpsc::Receiver<ChatEvent>), String> {
        let provider = self
            .default_provider()
            .or_else(|| self.providers.first())
            .cloned()
            .ok_or_else(|| "no AI provider available — set OPENAI_API_KEY / ANTHROPIC_API_KEY / DEEPSEEK_API_KEY or run a local server (Ollama, LM Studio…)".to_string())?;
        let rx = spawn_stream(provider.clone(), messages.to_vec(), tools.to_vec());
        Ok((provider, rx))
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

async fn probe_local(base: &str) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(800))
        .build()
        .ok()?;
    let resp = client.get(format!("{base}/models")).send().await.ok()?;
    let body: serde_json::Value = resp.json().await.ok()?;
    let mut models: Vec<String> = body["data"]
        .as_array()?
        .iter()
        .filter_map(|m| m["id"].as_str().map(String::from))
        .collect();
    models.retain(|m| {
        let lc = m.to_lowercase();
        !BAD_MODEL_HINTS.iter().any(|h| lc.contains(h))
    });
    models.sort_by_key(|m| {
        let lc = m.to_lowercase();
        let good = GOOD_MODEL_HINTS.iter().any(|h| lc.contains(h));
        (!good, m.clone())
    });
    models.into_iter().next()
}

// ---------------------------------------------------------------------------
// config file

#[derive(Debug, Deserialize, Default)]
struct Config {
    ai: Option<ConfigAi>,
    #[serde(default)]
    provider: Vec<ConfigProvider>,
}

#[derive(Debug, Deserialize)]
struct ConfigAi {
    #[serde(default)]
    default_provider: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConfigProvider {
    id: String,
    #[serde(default)]
    kind: Option<String>,
    base_url: String,
    #[serde(default)]
    api_key: Option<String>,
    model: String,
}

fn load_config() -> Option<Config> {
    let path = config_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    toml::from_str(&text).ok()
}

pub fn config_path() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| {
        std::path::PathBuf::from(h).join(".config/neuos/config.toml")
    })
}

// ---------------------------------------------------------------------------
// streaming

fn spawn_stream(
    provider: Provider,
    messages: Vec<Msg>,
    tools: Vec<ToolDef>,
) -> tokio::sync::mpsc::Receiver<ChatEvent> {
    let (tx, rx) = tokio::sync::mpsc::channel(128);
    tokio::spawn(async move {
        let result = match provider.kind {
            Kind::OpenAiCompat => stream_openai(&provider, &messages, &tools, &tx).await,
            Kind::Anthropic => stream_anthropic(&provider, &messages, &tools, &tx).await,
        };
        match result {
            Ok(stop) => {
                let _ = tx.send(ChatEvent::Done { stop }).await;
            }
            Err(e) => {
                let _ = tx.send(ChatEvent::Error { message: e }).await;
            }
        }
    });
    rx
}

async fn sse_channel(
    provider: &Provider,
    url: &str,
    body: serde_json::Value,
    extra_headers: &[(&str, &str)],
) -> Result<impl futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>>, String> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(300));
    // local servers: plain http; cloud: https via rustls (client default)
    let _ = &mut builder;
    let client = builder.build().map_err(|e| e.to_string())?;
    let mut req = client.post(url).json(&body);
    if let Some(key) = &provider.api_key {
        req = req.header("authorization", format!("Bearer {key}"));
    }
    for (k, v) in extra_headers {
        req = req.header(*k, *v);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("{status}: {}", truncate(&text, 300)));
    }
    Ok(resp.bytes_stream())
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n.max(3) - 3])
    }
}

/// Pull the next complete SSE `data:` payload from the byte buffer.
/// Returns (payloads, remaining buffer).
fn parse_sse_chunk(buffer: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = buffer;
    while let Some(pos) = rest.find("\n\n") {
        let (block, remainder) = rest.split_at(pos);
        rest = &remainder[2..];
        let data: String = block
            .lines()
            .filter_map(|l| l.strip_prefix("data:").map(|d| d.trim().to_string()))
            .collect::<Vec<_>>()
            .join("\n");
        if !data.is_empty() {
            out.push(data);
        }
    }
    out
}

#[allow(unused_mut)]
async fn stream_openai(
    provider: &Provider,
    messages: &[Msg],
    tools: &[ToolDef],
    tx: &tokio::sync::mpsc::Sender<ChatEvent>,
) -> Result<StopReason, String> {
    let url = format!("{}/chat/completions", provider.base_url.trim_end_matches('/'));
    let mut body = serde_json::json!({
        "model": provider.model,
        "stream": true,
        "messages": messages.iter().map(openai_msg).collect::<Vec<_>>(),
    });
    if !tools.is_empty() {
        body["tools"] = serde_json::Value::Array(
            tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": { "name": t.name, "description": t.description, "parameters": t.parameters }
                    })
                })
                .collect(),
        );
    }

    let mut stream = sse_channel(provider, &url, body, &[]).await?;
    let mut buffer = String::new();
    let mut tool_acc: Vec<(usize, String, String, String)> = Vec::new(); // index, id, name, args
    let mut stop = StopReason::EndTurn;

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| e.to_string())?;
        buffer.push_str(&String::from_utf8_lossy(&bytes));
        let payloads = parse_sse_chunk(&buffer);
        let remainder_start = buffer.rfind("\n\n").map(|p| p + 2).unwrap_or(0);
        for data in payloads {
            if data == "[DONE]" {
                break;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) else { continue };
            let delta = &v["choices"][0]["delta"];
            if let Some(text) = delta["content"].as_str() {
                if !text.is_empty() {
                    tx.send(ChatEvent::Delta { text: text.into() }).await.map_err(|_| "ui closed".to_string())?;
                }
            }
            if let Some(tcs) = delta["tool_calls"].as_array() {
                stop = StopReason::ToolUse;
                for tc in tcs {
                    let idx = tc["index"].as_u64().unwrap_or(0) as usize;
                    let id = tc["id"].as_str().unwrap_or("").to_string();
                    let name = tc["function"]["name"].as_str().unwrap_or("").to_string();
                    let args = tc["function"]["arguments"].as_str().unwrap_or("").to_string();
                    match tool_acc.iter_mut().find(|(i, ..)| *i == idx) {
                        Some(slot) => {
                            if !id.is_empty() {
                                slot.1 = id.clone();
                            }
                            if !name.is_empty() {
                                slot.2 = name.clone();
                            }
                            slot.3.push_str(&args);
                        }
                        None => tool_acc.push((idx, id, name, args)),
                    }
                }
            }
        }
        buffer = buffer[remainder_start..].to_string();
    }

    for (_, id, name, args) in tool_acc {
        let arguments = serde_json::from_str(&args).unwrap_or(serde_json::Value::String(args));
        tx.send(ChatEvent::ToolCall {
            call: ToolCall { id, name, arguments },
        })
        .await
        .map_err(|_| "ui closed".to_string())?;
    }
    Ok(stop)
}

fn openai_msg(m: &Msg) -> serde_json::Value {
    match m.role {
        Role::Tool => serde_json::json!({
            "role": "tool",
            "tool_call_id": m.tool_call_id.clone().unwrap_or_default(),
            "content": m.content,
        }),
        Role::Assistant if !m.tool_calls.is_empty() => {
            serde_json::json!({
                "role": "assistant",
                "content": m.content,
                "tool_calls": m.tool_calls.iter().map(|c| serde_json::json!({
                    "id": c.id,
                    "type": "function",
                    "function": { "name": c.name, "arguments": c.arguments.to_string() }
                })).collect::<Vec<_>>(),
            })
        }
        _ => serde_json::json!({ "role": m.role, "content": m.content }),
    }
}

async fn stream_anthropic(
    provider: &Provider,
    messages: &[Msg],
    tools: &[ToolDef],
    tx: &tokio::sync::mpsc::Sender<ChatEvent>,
) -> Result<StopReason, String> {
    let url = format!("{}/v1/messages", provider.base_url.trim_end_matches('/'));
    let system: Vec<&str> = messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.as_str())
        .collect();
    let mut convo: Vec<serde_json::Value> = Vec::new();
    for m in messages.iter().filter(|m| m.role != Role::System) {
        match m.role {
            Role::Tool => convo.push(serde_json::json!({
                "role": "user",
                "content": [{ "type": "tool_result", "tool_use_id": m.tool_call_id.clone().unwrap_or_default(), "content": m.content }]
            })),
            Role::Assistant if !m.tool_calls.is_empty() => {
                let mut blocks = vec![];
                if !m.content.is_empty() {
                    blocks.push(serde_json::json!({"type":"text","text":m.content}));
                }
                for c in &m.tool_calls {
                    blocks.push(serde_json::json!({
                        "type": "tool_use", "id": c.id, "name": c.name, "input": c.arguments
                    }));
                }
                convo.push(serde_json::json!({ "role": "assistant", "content": blocks }));
            }
            _ => convo.push(serde_json::json!({
                "role": if m.role == Role::User { "user" } else { "assistant" },
                "content": [{ "type": "text", "text": m.content }]
            })),
        }
    }
    let mut body = serde_json::json!({
        "model": provider.model,
        "max_tokens": 4096,
        "stream": true,
        "messages": convo,
    });
    if !system.is_empty() {
        body["system"] = serde_json::Value::String(system.join("\n\n"));
    }
    if !tools.is_empty() {
        body["tools"] = serde_json::Value::Array(
            tools
                .iter()
                .map(|t| serde_json::json!({ "name": t.name, "description": t.description, "input_schema": t.parameters }))
                .collect(),
        );
    }

    let mut stream = sse_channel(
        provider,
        &url,
        body,
        &[("x-api-key", provider.api_key.as_deref().unwrap_or("")), ("anthropic-version", "2023-06-01")],
    )
    .await?;
    let mut buffer = String::new();
    let mut current_tool: Option<(String, String, String)> = None; // id, name, json acc
    let mut stop = StopReason::EndTurn;

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| e.to_string())?;
        buffer.push_str(&String::from_utf8_lossy(&bytes));
        let payloads = parse_sse_chunk(&buffer);
        // compute remainder after last complete block
        let remainder_start = buffer.rfind("\n\n").map(|p| p + 2).unwrap_or(0);
        for data in payloads {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) else { continue };
            let ev_type = v["type"].as_str().unwrap_or("");
            match ev_type {
                "content_block_start" => {
                    let block = &v["content_block"];
                    if block["type"] == "tool_use" {
                        current_tool = Some((
                            block["id"].as_str().unwrap_or("").to_string(),
                            block["name"].as_str().unwrap_or("").to_string(),
                            String::new(),
                        ));
                        stop = StopReason::ToolUse;
                    }
                }
                "content_block_delta" => {
                    let delta = &v["delta"];
                    if delta["type"] == "text_delta" {
                        if let Some(t) = delta["text"].as_str() {
                            tx.send(ChatEvent::Delta { text: t.into() }).await.map_err(|_| "ui closed".to_string())?;
                        }
                    } else if delta["type"] == "input_json_delta" {
                        if let Some(p) = delta["partial_json"].as_str() {
                            if let Some((_, _, acc)) = current_tool.as_mut() {
                                acc.push_str(p);
                            }
                        }
                    }
                }
                "content_block_stop" => {
                    if let Some((id, name, acc)) = current_tool.take() {
                        let arguments = serde_json::from_str(&acc).unwrap_or(serde_json::Value::String(acc));
                        tx.send(ChatEvent::ToolCall { call: ToolCall { id, name, arguments } })
                            .await
                            .map_err(|_| "ui closed".to_string())?;
                    }
                }
                "message_stop" => break,
                "error" => {
                    return Err(v["error"]["message"].as_str().unwrap_or("stream error").to_string());
                }
                _ => {}
            }
        }
        buffer = buffer[remainder_start..].to_string();
    }
    Ok(stop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_parsing() {
        let buf = "data: {\"a\":1}\n\ndata: {\"b\":2}\n\ndata: [DONE]\n\n";
        let out = parse_sse_chunk(buf);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], "{\"a\":1}");
        assert_eq!(out[2], "[DONE]");
        let partial = parse_sse_chunk("data: {\"a\":");
        assert!(partial.is_empty());
    }

    #[test]
    fn openai_msg_shapes() {
        let tool = Msg::tool_result("t1", "result text");
        assert_eq!(tool.role, Role::Tool);
        let v = openai_msg(&tool);
        assert_eq!(v["tool_call_id"], "t1");
        let assistant = Msg { role: Role::Assistant, content: String::new(), tool_calls: vec![ToolCall { id: "t2".into(), name: "run_shell".into(), arguments: serde_json::json!({}) }], tool_call_id: None };
        let v = openai_msg(&assistant);
        assert!(v["tool_calls"].is_array());
    }

    #[test]
    fn config_parses() {
        let cfg: Config = toml::from_str(
            r#"
[ai]
default_provider = "my-ollama"

[[provider]]
id = "my-ollama"
base_url = "http://127.0.0.1:11434/v1"
model = "qwen3:14b"
"#,
        )
        .unwrap();
        assert_eq!(cfg.provider.len(), 1);
        assert_eq!(cfg.ai.unwrap().default_provider.as_deref(), Some("my-ollama"));
    }
}
