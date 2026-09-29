//! A scripted OpenAI-compatible model on loopback, for integration tests
//! that boot a real company and need the model — and only the model — to
//! answer on cue.
//!
//! Extracted from `tests/hivemind_e2e.rs` (plan hive-desks, Phase 2) so the
//! hive suite that replaces it (Phase 8, `tests/hive_e2e.rs`) and any other
//! integration target boot against the same endpoint. The endpoint is
//! **content-aware**: every request body is handed to the test's responder as
//! an [`Ask`], which has already pulled out the message array, the tool
//! results so far and the tool result this request continues, so a script can
//! answer according to what the agent was actually shown rather than off a
//! fixed queue.
//!
//! `/embeddings` is served alongside `/chat/completions` for the same reason
//! `offline_e2e` serves it: the host's embeddings client shares the
//! `base_url`, and a 404 there reads as an inference failure and is not one.
//!
//! With the company agents on the embedded OpenHuman runtime, the request
//! this endpoint sees is the one OpenHuman's inference client sends — through
//! the harness's loopback model bridge, which forwards to the provider the
//! company manifest names, which is this endpoint. The `model` field therefore
//! carries the manifest's mapped model name, not an OpenHuman tier.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::routing::post;
use serde_json::{Value, json};

/// One request as the script sees it.
#[derive(Clone, Debug)]
pub struct Ask {
    /// The whole message array, for assertions that need the roles.
    pub messages: Vec<Value>,
    /// Every `tool` message already in this conversation, oldest first.
    pub tool_outputs: Vec<String>,
    /// The tool result this request is the continuation of, when it is one.
    pub pending_tool: Option<String>,
    /// The tool schemas offered on this request, by name.
    pub tools: Vec<String>,
    /// The `model` the request named.
    pub model: String,
}

impl Ask {
    /// Reads a request body.
    pub fn read(body: &Value) -> Self {
        let messages: Vec<Value> = body
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let tool_outputs = messages
            .iter()
            .filter(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
            .filter_map(|message| message.get("content").and_then(Value::as_str))
            .map(str::to_owned)
            .collect();
        let pending_tool = messages
            .last()
            .filter(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
            .and_then(|message| message.get("content").and_then(Value::as_str))
            .map(str::to_owned);
        let tools = body
            .get("tools")
            .and_then(Value::as_array)
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|tool| {
                        tool.get("function")
                            .and_then(|function| function.get("name"))
                            .or_else(|| tool.get("name"))
                            .and_then(Value::as_str)
                    })
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            messages,
            tool_outputs,
            pending_tool,
            tools,
            model: body
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        }
    }

    /// The text of the newest `user` message.
    pub fn last_user_text(&self) -> &str {
        self.messages
            .iter()
            .rev()
            .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
            .and_then(|message| message.get("content").and_then(Value::as_str))
            .unwrap_or_default()
    }

    /// Every `user` message's text, oldest first.
    pub fn user_texts(&self) -> Vec<&str> {
        self.messages
            .iter()
            .filter(|message| message.get("role").and_then(Value::as_str) == Some("user"))
            .filter_map(|message| message.get("content").and_then(Value::as_str))
            .collect()
    }

    /// The system prompt, when the request carries one.
    pub fn system_text(&self) -> &str {
        self.messages
            .iter()
            .find(|message| message.get("role").and_then(Value::as_str) == Some("system"))
            .and_then(|message| message.get("content").and_then(Value::as_str))
            .unwrap_or_default()
    }

    /// Whether this request opens a turn: its last message is a `user` line
    /// rather than a tool result the turn loop is continuing from.
    pub fn opens_turn(&self) -> bool {
        self.messages
            .last()
            .is_some_and(|last| last.get("role").and_then(Value::as_str) == Some("user"))
    }
}

/// What the script answers with.
#[derive(Clone, Debug)]
pub enum Reply {
    /// A plain assistant reply.
    Say(String),
    /// One tool call.
    Call {
        /// The tool name.
        tool: &'static str,
        /// Its JSON arguments.
        args: Value,
    },
}

/// A test's script: what to answer, given what was asked.
pub type Responder = Arc<dyn Fn(&Ask) -> Reply + Send + Sync>;

/// A scripted OpenAI-compatible endpoint, served on loopback.
pub struct Script {
    responder: Responder,
    /// Every request body the harness sent, in order.
    seen: Mutex<Vec<Value>>,
    /// How long each completion takes to answer — zero unless a test needs
    /// its turns to be long enough to overlap.
    latency: std::time::Duration,
    /// Requests being answered right now, and the most ever at once: the
    /// model's own view of concurrency, independent of the journal.
    in_flight: Mutex<(usize, usize)>,
}

impl Script {
    /// Every request body the harness sent, in order.
    pub fn bodies(&self) -> Vec<Value> {
        self.seen.lock().expect("script poisoned").clone()
    }

    /// Every request, read.
    pub fn asks(&self) -> Vec<Ask> {
        self.bodies().iter().map(Ask::read).collect()
    }

    /// The most completions the endpoint was answering at once.
    pub fn peak_in_flight(&self) -> usize {
        self.in_flight.lock().expect("script poisoned").1
    }
}

/// Serves `responder` on a fresh loopback port and returns the base URL a
/// company manifest points its `[inference] base_url` at.
pub async fn spawn_script(responder: Responder) -> (String, Arc<Script>) {
    spawn_script_with_latency(responder, std::time::Duration::ZERO).await
}

/// [`spawn_script`], with every completion held for `latency` before it
/// answers — so two turns proposed together are still both running when the
/// second one starts, and a test can assert they overlapped.
pub async fn spawn_script_with_latency(
    responder: Responder,
    latency: std::time::Duration,
) -> (String, Arc<Script>) {
    let script = Arc::new(Script {
        responder,
        seen: Mutex::new(Vec::new()),
        latency,
        in_flight: Mutex::new((0, 0)),
    });
    let chat = Arc::clone(&script);
    let app = axum::Router::new()
        .route(
            "/chat/completions",
            post(move |Json(body): Json<Value>| {
                let script = Arc::clone(&chat);
                async move {
                    script
                        .seen
                        .lock()
                        .expect("script poisoned")
                        .push(body.clone());
                    {
                        let mut live = script.in_flight.lock().expect("script poisoned");
                        live.0 += 1;
                        live.1 = live.1.max(live.0);
                    }
                    if !script.latency.is_zero() {
                        tokio::time::sleep(script.latency).await;
                    }
                    let ask = Ask::read(&body);
                    let (message, finish) = match (script.responder)(&ask) {
                        Reply::Say(text) => {
                            (json!({ "role": "assistant", "content": text }), "stop")
                        }
                        Reply::Call { tool, args } => (
                            json!({
                                "role": "assistant",
                                "content": null,
                                "tool_calls": [{
                                    "id": format!("call-{tool}"),
                                    "type": "function",
                                    "function": { "name": tool, "arguments": args.to_string() }
                                }]
                            }),
                            "tool_calls",
                        ),
                    };
                    script.in_flight.lock().expect("script poisoned").0 -= 1;
                    Json(json!({
                        "id": "scripted",
                        "object": "chat.completion",
                        "model": ask.model,
                        "choices": [{ "index": 0, "message": message, "finish_reason": finish }],
                        "usage": { "prompt_tokens": 12, "completion_tokens": 4, "total_tokens": 16 }
                    }))
                }
            }),
        )
        .route(
            "/embeddings",
            post(|Json(_body): Json<Value>| async move {
                Json(json!({
                    "data": [{ "index": 0, "embedding": vec![0.0_f32; 1536] }],
                    "usage": { "prompt_tokens": 1, "total_tokens": 1 }
                }))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), script)
}
