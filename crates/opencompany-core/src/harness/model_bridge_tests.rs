//! Tests for the loopback model bridge: wire translation both ways, the
//! per-token registry, and the usage tap a turn is metered from.

use super::*;
use async_trait::async_trait;
use tinyinference::model::ModelProfile;

/// A model that answers with a fixed tool call the first time and text after,
/// reporting usage with a charged amount the way the hosted provider does.
struct Scripted {
    calls: Mutex<u32>,
}

#[async_trait]
impl ChatModel<()> for Scripted {
    fn profile(&self) -> Option<&ModelProfile> {
        None
    }

    async fn invoke(
        &self,
        _state: &(),
        request: ModelRequest,
    ) -> tinyinference::Result<ModelResponse> {
        let mut calls = self.calls.lock().unwrap();
        *calls += 1;
        assert_eq!(request.model.as_deref(), Some("chat-v1"), "prefix stripped");
        let mut response = if *calls == 1 {
            assert_eq!(request.tools.len(), 1);
            let mut response = ModelResponse::assistant("");
            response.message.tool_calls.push(ToolCall {
                id: "call_1".to_string(),
                name: "shell".to_string(),
                arguments: json!({"command": "ls"}),
                invalid: None,
            });
            response
        } else {
            let last = request.messages.last().expect("a message");
            assert!(matches!(last, Message::Tool(_)), "tool result round-trips");
            ModelResponse::assistant("done")
        };
        response = response.with_usage(Usage {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
            ..Usage::default()
        });
        response.raw = Some(json!({"openhuman": {"billing": {"charged_amount_usd": 0.25}}}));
        Ok(response)
    }
}

fn wire_request(model: &str, with_tool_result: bool) -> Value {
    let mut messages = vec![
        json!({"role": "system", "content": "be brief"}),
        json!({"role": "user", "content": [{"type": "text", "text": "hi"}]}),
    ];
    if with_tool_result {
        messages.push(json!({"role": "assistant", "content": null, "tool_calls": [
            {"id": "call_1", "type": "function", "function": {"name": "shell", "arguments": "{\"command\":\"ls\"}"}}
        ]}));
        messages.push(json!({"role": "tool", "tool_call_id": "call_1", "content": "a.txt"}));
    }
    json!({
        "model": model,
        "messages": messages,
        "tools": [{"type": "function", "function": {"name": "shell", "description": "run", "parameters": {"type": "object"}}}],
        "stream": true,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_registered_model_is_served_over_loopback_with_its_usage_tapped() {
    let model: Arc<dyn ChatModel<()>> = Arc::new(Scripted {
        calls: Mutex::new(0),
    });
    let handle = register(model, "chat-v1").expect("register");
    let provider = handle.provider();
    assert_eq!(provider.model_id(), Some("oc/chat-v1"));
    let route = provider.route().expect("route");
    let client = reqwest::Client::new();
    let url = format!("{}/chat/completions", route.base_url);

    let first: Value = client
        .post(&url)
        .bearer_auth(&route.api_key)
        .json(&wire_request("oc/chat-v1", false))
        .send()
        .await
        .expect("send")
        .json()
        .await
        .expect("json");
    assert_eq!(first["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        first["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "shell"
    );
    assert_eq!(first["usage"]["prompt_tokens"], 10);

    let second: Value = client
        .post(&url)
        .bearer_auth(&route.api_key)
        .json(&wire_request("oc/chat-v1", true))
        .send()
        .await
        .expect("send")
        .json()
        .await
        .expect("json");
    assert_eq!(second["choices"][0]["message"]["content"], "done");
    assert_eq!(second["choices"][0]["finish_reason"], "stop");

    let usage = handle.take_usage();
    assert_eq!(usage.len(), 2);
    assert_eq!(usage[0].input_tokens, 10);
    assert_eq!(usage[0].output_tokens, 5);
    assert!((usage[0].cost_usd - 0.25).abs() < f64::EPSILON);
    assert!(handle.take_usage().is_empty(), "drained");

    let unauthorized = client
        .post(&url)
        .bearer_auth("ocb_nope")
        .json(&wire_request("oc/chat-v1", false))
        .send()
        .await
        .expect("send");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let token = route.api_key.clone();
    drop(handle);
    let gone = client
        .post(&url)
        .bearer_auth(&token)
        .json(&wire_request("oc/chat-v1", false))
        .send()
        .await
        .expect("send");
    assert_eq!(
        gone.status(),
        StatusCode::UNAUTHORIZED,
        "unregistered on drop"
    );
}

#[test]
fn a_provider_error_maps_to_a_gateway_status() {
    assert!(is_budget_or_auth("insufficient credits"));
    assert!(!is_budget_or_auth("timed out"));
}
