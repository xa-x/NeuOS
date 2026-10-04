//! neu-agent — the harness loop.
//!
//! Runs the model ↔ tool loop with visible steps. Tool execution (including
//! any user-confirmation gating for destructive actions) is delegated to the
//! host app through the `ToolRunner` trait, so this crate stays pure policy:
//! how many steps, what the system prompt says, how results are fed back.

use neu_provider::{ChatEvent, Gateway, Msg, StopReason, ToolDef, ToolCall};
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum AgentEvent {
    Provider { id: String, model: String },
    Delta { text: String },
    ToolStart { name: String, args: serde_json::Value },
    ToolDone { name: String, ok: bool, summary: String },
    Done { answer: String, steps: usize },
    Error { message: String },
}

/// Host-implemented tool access. `run` may block on user confirmation —
/// the app decides how gating works.
pub type BoxedToolFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value, String>> + Send>>;

pub trait ToolRunner: Send + Sync {
    fn defs(&self) -> Vec<ToolDef>;
    fn run(&self, name: &str, args: serde_json::Value) -> BoxedToolFuture;
}

/// Step budget for the agent loop. `DEFAULT` suits chat; `EXTENDED` suits
/// multi-file coding and Creator-mode plugin installation (search → write →
/// test → reload chains regularly exceed 30 steps). Long-horizon work should
/// request `EXTENDED` explicitly rather than unbounded loops — the budget is
/// a runaway guard, surfaced to the user when exhausted.
pub const DEFAULT_MAX_STEPS: usize = 25;
pub const EXTENDED_MAX_STEPS: usize = 60;
pub const MAX_STEPS: usize = DEFAULT_MAX_STEPS;

const SYSTEM_PROMPT: &str = "You are NeuOS, the agent built into the user's system launcher. \
You have tools that give you full access to this machine. Use them to answer and to act. \
Prefer the smallest action that satisfies the request. Read files before claiming their contents. \
Destructive operations will be confirmed by the user. Final answers: concise, plain text, no markdown headers.";

pub async fn run(
    gateway: &Gateway,
    prompt: &str,
    runner: Arc<dyn ToolRunner>,
    tx: tokio::sync::mpsc::Sender<AgentEvent>,
) {
    if let Err(e) = run_inner(gateway, prompt, runner, &tx).await {
        let _ = tx
            .send(AgentEvent::Error { message: e })
            .await;
    }
}

async fn run_inner(
    gateway: &Gateway,
    prompt: &str,
    runner: Arc<dyn ToolRunner>,
    tx: &tokio::sync::mpsc::Sender<AgentEvent>,
) -> Result<(), String> {
    let defs = runner.defs();
    let mut messages = vec![Msg::system(SYSTEM_PROMPT), Msg::user(prompt)];

    for step in 0..MAX_STEPS {
        let (provider, mut rx) = gateway.chat(&messages, &defs).await?;
        tx.send(AgentEvent::Provider { id: provider.id.clone(), model: provider.model.clone() })
            .await
            .map_err(|_| "ui closed".to_string())?;

        let mut text = String::new();
        let mut calls: Vec<ToolCall> = Vec::new();
        let mut stop = StopReason::EndTurn;

        while let Some(ev) = rx.recv().await {
            match ev {
                ChatEvent::Delta { text: d } => {
                    let _ = tx.send(AgentEvent::Delta { text: d.clone() }).await;
                    text.push_str(&d);
                }
                ChatEvent::ToolCall { call } => calls.push(call),
                ChatEvent::Done { stop: s } => {
                    stop = s;
                    break;
                }
                ChatEvent::Error { message } => return Err(message),
            }
        }

        if stop != StopReason::ToolUse || calls.is_empty() {
            let _ = tx
                .send(AgentEvent::Done { answer: text, steps: step + 1 })
                .await;
            return Ok(());
        }

        // record the assistant turn with its tool calls, then execute
        let call_ids: Vec<String> = calls.iter().map(|c| c.id.clone()).collect();
        let call_names: Vec<String> = calls.iter().map(|c| c.name.clone()).collect();
        let call_args: Vec<serde_json::Value> = calls.iter().map(|c| c.arguments.clone()).collect();
        messages.push(Msg {
            role: neu_provider::Role::Assistant,
            content: text.clone(),
            tool_calls: calls,
            tool_call_id: None,
        });

        for (i, id) in call_ids.iter().enumerate() {
            let name = &call_names[i];
            let args = call_args[i].clone();
            tx.send(AgentEvent::ToolStart { name: name.clone(), args: args.clone() })
                .await
                .map_err(|_| "ui closed".to_string())?;
            let result = match runner.run(name, args).await {
                Ok(v) => {
                    let summary = summarize(&v);
                    let _ = tx
                        .send(AgentEvent::ToolDone { name: name.clone(), ok: true, summary })
                        .await;
                    serde_json::to_string(&v).unwrap_or_else(|_| "{}".into())
                }
                Err(e) => {
                    let _ = tx
                        .send(AgentEvent::ToolDone {
                            name: name.clone(),
                            ok: false,
                            summary: truncate(&e, 200),
                        })
                        .await;
                    format!("{{\"error\":{e:?}}}")
                }
            };
            messages.push(Msg::tool_result(id.clone(), result));
        }
    }

    tx.send(AgentEvent::Done {
        answer: format!(
            "Stopped after the step limit ({MAX_STEPS}). Say \"continue\" to resume from where it left off, or ask for the remaining work in smaller pieces."
        ),
        steps: MAX_STEPS,
    })
    .await
    .map_err(|_| "ui closed".to_string())?;
    Ok(())
}

fn summarize(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => truncate(s, 160),
        other => truncate(&other.to_string(), 160),
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn truncates_unicode() {
        let s = "héllo wörld".repeat(30);
        assert!(super::truncate(&s, 20).chars().count() <= 21);
    }
}
