//! E2E against a real local server (Ollama/LM Studio/llama.cpp on :11434 etc).
//! Run with: NEUOS_E2E=1 cargo test -p neu-agent --test e2e_local -- --nocapture

use neu_agent::{AgentEvent, ToolRunner};
use neu_provider::{Gateway, Msg};
use serde_json::json;

struct StubRunner;

impl ToolRunner for StubRunner {
    fn defs(&self) -> Vec<neu_provider::ToolDef> {
        vec![neu_provider::ToolDef {
            name: "echo_tool".into(),
            description: "Echoes its input back.".into(),
            parameters: json!({ "type": "object", "properties": { "text": { "type": "string" } } }),
        }]
    }

    fn run(&self, name: &str, args: serde_json::Value) -> neu_agent::BoxedToolFuture {
        let name = name.to_string();
        Box::pin(async move {
            if name == "echo_tool" {
                Ok(json!({ "echoed": args.get("text").cloned().unwrap_or(json!(null)) }))
            } else {
                Err(format!("unknown tool {name}"))
            }
        })
    }
}

#[tokio::test]
async fn agent_streams_answer_from_local_model() {
    if std::env::var("NEUOS_E2E").is_err() {
        eprintln!("skipping (set NEUOS_E2E=1 to run)");
        return;
    }
    let gw = std::sync::Arc::new(Gateway::discover().await);
    let provider = gw
        .default_provider()
        .cloned()
        .expect("no provider found (is a local server running?)");
    eprintln!("using provider: {} ({})", provider.id, provider.model);

    // plain streaming first
    let (_p, mut rx) = gw
        .chat(&[Msg::user("Reply with exactly: LOCAL-OK")], &[])
        .await
        .expect("chat start");
    let mut text = String::new();
    let mut errored = false;
    while let Some(ev) = rx.recv().await {
        match ev {
            neu_provider::ChatEvent::Delta { text: d } => text.push_str(&d),
            neu_provider::ChatEvent::Done { .. } => break,
            neu_provider::ChatEvent::Error { message } => {
                errored = true;
                eprintln!("stream error: {message}");
                break;
            }
            _ => {}
        }
    }
    assert!(!errored, "stream errored");
    assert!(!text.trim().is_empty(), "no text streamed back");
    eprintln!("plain stream replied: {text:?}");

    // agent loop with a tool
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let prompt = "Use the echo_tool with text set to \"agent-e2e\" and then tell me what it returned.";
    let gw_task = gw.clone();
    tokio::spawn(async move {
        neu_agent::run(&gw_task, prompt, std::sync::Arc::new(StubRunner), tx).await;
    });
    let mut saw_tool = false;
    let mut answer = String::new();
    while let Some(ev) = rx.recv().await {
        match ev {
            AgentEvent::ToolDone { name, ok, .. } if name == "echo_tool" => {
                saw_tool = true;
                assert!(ok);
            }
            AgentEvent::Delta { text } => answer.push_str(&text),
            AgentEvent::Done { steps, .. } => {
                eprintln!("agent finished in {steps} steps");
                break;
            }
            AgentEvent::Error { message } => panic!("agent error: {message}"),
            _ => {}
        }
    }
    assert!(saw_tool, "model never called the tool (tool-calling may be unsupported by this model)");
    assert!(!answer.trim().is_empty(), "no final answer");
    eprintln!("agent answer: {answer:?}");
}
