//! A loopback OpenAI-compatible endpoint over this crate's own [`ChatModel`]s.
//!
//! An [`openhuman_embed::Agent`] reaches its model through OpenHuman's own
//! inference client, which speaks HTTP to an OpenAI-shaped endpoint named by
//! [`Provider::openai_compatible`](openhuman_embed::Provider). This crate's
//! inference layer is a set of in-process [`ChatModel`] implementations —
//! [`HostedProvider`](crate::harness::provider::HostedProvider) with its
//! vendor dialect rules and Medulla billing meta,
//! [`TenantProvider`](crate::harness::provider::TenantProvider) with a
//! company's BYOK pins and per-agent telemetry cells, and every scripted
//! double the test corpus drives a turn with. The bridge is what lets the
//! former talk to the latter without rewriting either: one loopback listener
//! per process, one bearer token per registered model, and a request/response
//! translation between the OpenAI wire and [`ModelRequest`]/[`ModelResponse`].
//!
//! Why a bridge rather than handing OpenHuman the upstream URL directly (plan
//! hive-desks, Phase 2 — a recorded deviation):
//!
//! * The provider behaviours a turn depends on — dialect translation
//!   (`max_tokens` → `max_completion_tokens`), model-unavailable advice, the
//!   `charged_amount_usd` billing meta the usage meter reads, the tenant's
//!   own key resolution — all live in `provider.rs`. Routing the turn through
//!   them keeps every one byte-for-byte, and keeps metering exact: the bridge
//!   taps each call's [`Usage`] (with the charged amount off the raw payload)
//!   so a turn is metered from what the provider actually reported, not from a
//!   catalogue estimate for a model OpenHuman has never heard of.
//! * A scripted test model has no URL. Serving it here is precisely the
//!   "scripted OpenAI-compatible HTTP model" the plan asks for, generalised so
//!   the sixty-odd existing doubles need no rewrite.
//!
//! The listener binds `127.0.0.1:0` on first use; a loopback host is one of
//! the endpoints OpenHuman accepts a bearer for over plain `http`. The bearer
//! is a random token minted per registration, and an unregistered token is a
//! `401`, so a stray client on the same box cannot drive somebody else's
//! model.
//!
//! Model names: OpenHuman refuses its abstract tier names (`chat-v1`,
//! `agentic-v1`, …) on a cloud route, and those are exactly the names this
//! crate's tier ladder produces. The route therefore advertises
//! `oc/<model>` and the bridge strips the prefix on the way in, so the
//! provider sees the model it always did.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};
use tinyinference::message::{AssistantMessage, ContentBlock, Message, ToolMessage};
use tinyinference::model::{ChatModel, ModelRequest, ModelResponse, ToolChoice};
use tinyinference::tool::{ToolCall, ToolFormat, ToolSchema};
use tinyinference::usage::Usage;

use crate::harness::cost::TurnUsage;

/// The prefix the advertised model name carries so OpenHuman never sees one of
/// its own abstract tier names on the route.
const MODEL_PREFIX: &str = "oc/";

/// One registered model: what serves the calls and where their usage lands.
struct Registration {
    model: Arc<dyn ChatModel<()>>,
    tap: Arc<Mutex<Vec<TurnUsage>>>,
    errors: Arc<Mutex<Vec<String>>>,
}

#[derive(Clone)]
struct BridgeState {
    models: Arc<Mutex<HashMap<String, Registration>>>,
}

struct Bridge {
    addr: SocketAddr,
    state: BridgeState,
}

static BRIDGE: OnceLock<Bridge> = OnceLock::new();

/// A registered model's route and the usage tap for the turns it serves.
///
/// Dropping it unregisters the token, so a roster rebuild leaves nothing
/// behind on the listener.
pub struct BridgeHandle {
    token: String,
    base_url: String,
    model_name: String,
    tap: Arc<Mutex<Vec<TurnUsage>>>,
    errors: Arc<Mutex<Vec<String>>>,
}

impl std::fmt::Debug for BridgeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BridgeHandle")
            .field("base_url", &self.base_url)
            .field("model_name", &self.model_name)
            .finish_non_exhaustive()
    }
}

impl BridgeHandle {
    /// The provider an [`openhuman_embed::AgentSpec`] is built with.
    pub fn provider(&self) -> openhuman_embed::Provider {
        openhuman_embed::Provider::openai_compatible(&self.base_url, &self.token)
            .model(&self.model_name)
    }

    /// The OpenAI-compatible base URL the route points at.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Drains the usage recorded since the previous drain.
    ///
    /// One agent runs at most one turn at a time (its `turn_lock`), so the
    /// calls recorded between two drains are exactly one attempt's.
    pub fn take_usage(&self) -> Vec<TurnUsage> {
        self.tap
            .lock()
            .map(|mut tap| std::mem::take(&mut *tap))
            .unwrap_or_default()
    }

    /// Drains the provider errors recorded since the previous drain, oldest
    /// first, verbatim as the provider raised them.
    ///
    /// OpenHuman collapses a failed hosted invocation to a caller-safe
    /// sentence before it reaches the turn's caller, so the wire-shape checks
    /// this host classifies a failure with — budget exhausted (issue #1846),
    /// a wall-clock ceiling — cannot read it off the error. The bridge saw
    /// the provider's own error first; this is where it is kept.
    pub fn take_errors(&self) -> Vec<String> {
        self.errors
            .lock()
            .map(|mut errors| std::mem::take(&mut *errors))
            .unwrap_or_default()
    }
}

impl Drop for BridgeHandle {
    fn drop(&mut self) {
        if let Some(bridge) = BRIDGE.get()
            && let Ok(mut models) = bridge.state.models.lock()
        {
            models.remove(&self.token);
        }
    }
}

/// Registers `model` on the loopback bridge and returns its route.
///
/// `model_name` is the name the provider will see on every request
/// (`ModelRequest::model`), which is the same name the previous agent
/// builder stamped onto its turns. The listener is spawned on the OpenHuman
/// executor the first time, so this may be called from anywhere.
pub fn register(model: Arc<dyn ChatModel<()>>, model_name: &str) -> crate::Result<BridgeHandle> {
    let bridge = bridge()?;
    let token = format!("ocb_{}", uuid::Uuid::new_v4().simple());
    let tap = Arc::new(Mutex::new(Vec::new()));
    let errors = Arc::new(Mutex::new(Vec::new()));
    bridge.state.models.lock().map_err(|_| poisoned())?.insert(
        token.clone(),
        Registration {
            model,
            tap: tap.clone(),
            errors: errors.clone(),
        },
    );
    Ok(BridgeHandle {
        token,
        base_url: format!("http://{}/v1", bridge.addr),
        model_name: format!("{MODEL_PREFIX}{}", model_name.trim()),
        tap,
        errors,
    })
}

fn poisoned() -> crate::error::OpenCompanyError {
    crate::error::OpenCompanyError::Harness("model bridge registry poisoned".to_string())
}

fn bridge() -> crate::Result<&'static Bridge> {
    if let Some(bridge) = BRIDGE.get() {
        return Ok(bridge);
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|err| {
        crate::error::OpenCompanyError::Harness(format!("bind the model bridge: {err}"))
    })?;
    listener.set_nonblocking(true).map_err(|err| {
        crate::error::OpenCompanyError::Harness(format!("model bridge listener: {err}"))
    })?;
    let addr = listener.local_addr().map_err(|err| {
        crate::error::OpenCompanyError::Harness(format!("model bridge address: {err}"))
    })?;
    let state = BridgeState {
        models: Arc::new(Mutex::new(HashMap::new())),
    };
    let router = Router::new()
        .route("/v1/chat/completions", post(complete))
        .route("/chat/completions", post(complete))
        .with_state(state.clone());
    // On the OpenHuman executor, not the caller's runtime: the listener has
    // to outlive whichever test or request registered the first model.
    let handle = crate::harness::openhuman_runtime::executor();
    let _enter = handle.enter();
    let listener = tokio::net::TcpListener::from_std(listener).map_err(|err| {
        crate::error::OpenCompanyError::Harness(format!("model bridge listener: {err}"))
    })?;
    handle.spawn(async move {
        if let Err(err) = axum::serve(listener, router).await {
            tracing::error!(%err, "[model-bridge] listener exited");
        }
    });
    let bridge = Bridge { addr, state };
    // Two callers can race here; the loser's listener is dropped and its task
    // ends with the listener, so nothing leaks past the first registration.
    let _ = BRIDGE.set(bridge);
    BRIDGE.get().ok_or_else(poisoned)
}

async fn complete(
    State(state): State<BridgeState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    let registration = state.models.lock().ok().and_then(|models| {
        models.get(&token).map(|reg| Registration {
            model: reg.model.clone(),
            tap: reg.tap.clone(),
            errors: reg.errors.clone(),
        })
    });
    let Some(registration) = registration else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": {"message": "unknown model bridge token"}})),
        );
    };
    let request = match request_from_wire(&body) {
        Ok(request) => request,
        Err(message) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": {"message": message}})),
            );
        }
    };
    let model_name = request.model.clone().unwrap_or_default();
    match registration.model.invoke(&(), request).await {
        Ok(response) => {
            if let Ok(mut tap) = registration.tap.lock() {
                tap.push(usage_of(&response));
            }
            (
                StatusCode::OK,
                Json(response_to_wire(&response, &model_name)),
            )
        }
        Err(err) => {
            let message = format!("{err:#}");
            if let Ok(mut errors) = registration.errors.lock() {
                errors.push(message.clone());
            }
            let status = if is_budget_or_auth(&message) {
                StatusCode::PAYMENT_REQUIRED
            } else {
                StatusCode::BAD_GATEWAY
            };
            (
                status,
                Json(json!({"error": {"message": message, "type": "provider_error"}})),
            )
        }
    }
}

/// Auth/billing failures keep their status class so OpenHuman classifies
/// them as non-retryable rather than hammering the provider three times.
fn is_budget_or_auth(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("budget") || lower.contains("credit") || lower.contains("401")
}

/// The usage one call reported, with the charged amount the hosted provider
/// stamps onto `raw.openhuman.billing.charged_amount_usd`.
fn usage_of(response: &ModelResponse) -> TurnUsage {
    let usage = response
        .usage
        .as_ref()
        .or(response.message.usage.as_ref())
        .cloned()
        .unwrap_or_default();
    let charged = response
        .raw
        .as_ref()
        .and_then(|raw| {
            raw.pointer("/openhuman/billing/charged_amount_usd")
                .or_else(|| raw.pointer("/openhuman_usage_meta/charged_amount_usd"))
        })
        .and_then(Value::as_f64)
        .or_else(|| {
            usage
                .charged_amount
                .as_ref()
                .map(|amount| amount.micros as f64 / 1_000_000.0)
        })
        .unwrap_or(0.0);
    TurnUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cached_input_tokens: usage.cache_read_tokens,
        cost_usd: charged,
    }
}

/// OpenAI wire request → [`ModelRequest`].
fn request_from_wire(body: &Value) -> Result<ModelRequest, String> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .map(|name| name.strip_prefix(MODEL_PREFIX).unwrap_or(name).to_string())
        .filter(|name| !name.is_empty());
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| "`messages` must be an array".to_string())?
        .iter()
        .map(message_from_wire)
        .collect::<Result<Vec<_>, _>>()?;
    let tools = body
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| tools.iter().filter_map(tool_from_wire).collect())
        .unwrap_or_default();
    let tool_choice = match body.get("tool_choice") {
        Some(Value::String(choice)) if choice == "none" => ToolChoice::None,
        Some(Value::String(choice)) if choice == "required" => ToolChoice::Required,
        Some(Value::Object(object)) => object
            .get("function")
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            .map(|name| ToolChoice::Tool(name.to_string()))
            .unwrap_or_default(),
        _ => ToolChoice::Auto,
    };
    Ok(ModelRequest {
        messages,
        tools,
        tool_choice,
        model,
        temperature: body.get("temperature").and_then(Value::as_f64),
        top_p: body.get("top_p").and_then(Value::as_f64),
        max_tokens: body
            .get("max_tokens")
            .or_else(|| body.get("max_completion_tokens"))
            .and_then(Value::as_u64)
            .map(|cap| cap.min(u64::from(u32::MAX)) as u32),
        stop_sequences: body
            .get("stop")
            .and_then(Value::as_array)
            .map(|stops| {
                stops
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        ..ModelRequest::default()
    })
}

fn text_of(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| part.as_str())
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn message_from_wire(message: &Value) -> Result<Message, String> {
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("user");
    let text = text_of(message.get("content"));
    Ok(match role {
        "system" | "developer" => Message::system(text),
        "assistant" => {
            let tool_calls = message
                .get("tool_calls")
                .and_then(Value::as_array)
                .map(|calls| calls.iter().filter_map(tool_call_from_wire).collect())
                .unwrap_or_default();
            let content = if text.is_empty() {
                Vec::new()
            } else {
                vec![ContentBlock::Text(text)]
            };
            Message::Assistant(AssistantMessage {
                id: None,
                content,
                tool_calls,
                usage: None,
                origin: None,
            })
        }
        "tool" => Message::Tool(ToolMessage {
            tool_call_id: message
                .get("tool_call_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            content: vec![ContentBlock::Text(text)],
            trusted_verbatim: false,
            artifact: None,
        }),
        _ => Message::user(text),
    })
}

fn tool_call_from_wire(call: &Value) -> Option<ToolCall> {
    let function = call.get("function")?;
    let name = function.get("name")?.as_str()?.to_string();
    let arguments = match function.get("arguments") {
        Some(Value::String(raw)) => serde_json::from_str(raw).unwrap_or(Value::Null),
        Some(other) => other.clone(),
        None => Value::Null,
    };
    Some(ToolCall {
        id: call
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        name,
        arguments,
        invalid: None,
    })
}

fn tool_from_wire(tool: &Value) -> Option<ToolSchema> {
    let function = tool.get("function").unwrap_or(tool);
    Some(ToolSchema {
        name: function.get("name")?.as_str()?.to_string(),
        description: function
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        parameters: function
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
        format: ToolFormat::default(),
    })
}

/// [`ModelResponse`] → OpenAI wire response.
fn response_to_wire(response: &ModelResponse, model_name: &str) -> Value {
    let content = response
        .message
        .content
        .iter()
        .filter_map(ContentBlock::as_text)
        .collect::<Vec<_>>()
        .join("");
    let tool_calls: Vec<Value> = response
        .message
        .tool_calls
        .iter()
        .enumerate()
        .map(|(index, call)| {
            json!({
                "id": if call.id.is_empty() { format!("call_{index}") } else { call.id.clone() },
                "type": "function",
                "function": {
                    "name": call.name,
                    "arguments": serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".to_string()),
                }
            })
        })
        .collect();
    let finish_reason = response.finish_reason.clone().unwrap_or_else(|| {
        if tool_calls.is_empty() {
            "stop".to_string()
        } else {
            "tool_calls".to_string()
        }
    });
    let mut message = json!({
        "role": "assistant",
        "content": if content.is_empty() && !tool_calls.is_empty() { Value::Null } else { Value::String(content) },
    });
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }
    let usage: Usage = response
        .usage
        .or(response.message.usage)
        .unwrap_or_default();
    json!({
        "id": format!("ocb-{}", uuid::Uuid::new_v4().simple()),
        "object": "chat.completion",
        "model": if model_name.is_empty() { "oc/bridge".to_string() } else { format!("{MODEL_PREFIX}{model_name}") },
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason,
        }],
        "usage": {
            "prompt_tokens": usage.input_tokens,
            "completion_tokens": usage.output_tokens,
            "total_tokens": if usage.total_tokens == 0 { usage.input_tokens + usage.output_tokens } else { usage.total_tokens },
            "prompt_tokens_details": { "cached_tokens": usage.cache_read_tokens },
        },
    })
}

#[cfg(test)]
#[path = "model_bridge_tests.rs"]
mod tests;
