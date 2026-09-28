use super::provider_test_helpers_tests::*;
use super::*;

/// Legacy sibling of the above: `finish_reason: "function_call"` with no
/// `message.function_call` field at all, beside a nonempty array-shaped
/// `content` preamble.
#[test]
fn function_call_finish_reason_with_missing_call_body_beside_content_preamble_errors_instead_of_dropping_action()
 {
    let payload = serde_json::json!({
        "choices": [{
            "finish_reason": "function_call",
            "message": {
                "role": "assistant",
                "content": [
                    { "type": "text", "text": "Let me check that for you." }
                ]
            }
        }]
    });
    let err = model_response_from_payload(payload).expect_err(
        "a function_call finish reason with no call body must not let a content preamble \
         stand in for the requested action",
    );
    let msg = err.to_string();
    assert!(
        msg.contains("function_call"),
        "error must name finish_reason for diagnosis, got: {msg}"
    );
}

/// The legacy sibling of the above: `finish_reason: "function_call"` with
/// no `message.function_call` field present at all.
#[test]
fn function_call_finish_reason_with_missing_call_body_errors_instead_of_promoting() {
    let payload = serde_json::json!({
        "choices": [{
            "finish_reason": "function_call",
            "message": {
                "role": "assistant",
                "content": null,
                "reasoning": "I should call the weather function"
            }
        }]
    });
    let err = model_response_from_payload(payload)
        .expect_err("a function_call finish reason with no call body must not promote reasoning");
    let msg = err.to_string();
    assert!(
        msg.contains("function_call"),
        "error must name finish_reason for diagnosis, got: {msg}"
    );
}

/// Any other non-success finish reason — including ones this module does
/// not name explicitly — must fail closed rather than be assumed safe to
/// promote. The guard is an allow-list of genuine textual completions
/// (`stop` only — see [`model_response_from_payload`] for why
/// `tool_calls`/`function_call` are excluded), not a blocklist of known
/// failures, so an unrecognized value never silently promotes reasoning.
#[test]
fn unrecognized_finish_reason_reasoning_only_turn_errors() {
    let payload = serde_json::json!({
        "choices": [{
            "finish_reason": "error",
            "message": {
                "role": "assistant",
                "content": null,
                "reasoning": "Working through it"
            }
        }]
    });
    let err = model_response_from_payload(payload)
        .expect_err("unrecognized finish_reason must not promote reasoning to an answer");
    let msg = err.to_string();
    assert!(
        msg.contains("error"),
        "error must name finish_reason for diagnosis, got: {msg}"
    );
}

/// The diagnostic issue #2016 exists for: an empty turn must say enough to
/// separate its causes. Zero usage beside a 200 is the signature of the
/// silent provider failure this file documents; nonzero prompt tokens would
/// mean the request was read and only the answer was missing.
#[test]
fn an_empty_turn_reports_the_facts_that_separate_its_causes() {
    let payload = serde_json::json!({
        "choices": [{
            "finish_reason": "failed",
            "message": { "role": "assistant", "content": "" }
        }],
        "usage": { "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 }
    });
    let msg = model_response_from_payload(payload)
        .expect_err("an empty turn is an error")
        .to_string();

    assert!(msg.contains("finish_reason: failed"), "{msg}");
    assert!(msg.contains("choices: 1"), "{msg}");
    assert!(msg.contains("in=0 out=0 total=0"), "{msg}");
    assert!(msg.contains("refusal_present: false"), "{msg}");
}

/// The three provider bugs that reach the same line identically: no
/// `choices` key at all, an empty array, and a present choice with an empty
/// message. Only the reported shape tells them apart.
#[test]
fn an_empty_turn_distinguishes_the_choices_shapes() {
    let absent = serde_json::json!({ "usage": { "prompt_tokens": 7 } });
    let empty = serde_json::json!({ "choices": [] });
    for (payload, expected) in [(absent, "choices: absent"), (empty, "choices: empty")] {
        let msg = model_response_from_payload(payload)
            .expect_err("an empty turn is an error")
            .to_string();
        assert!(msg.contains(expected), "expected {expected:?} in: {msg}");
    }
}

/// A missing `finish_reason` altogether is unproven, not proven-complete —
/// the allow-list requires an explicit good status, so this must also fail
/// closed rather than assume the omission means success.
///
/// The *message* assertion changed with issue #2016. It used to require
/// that nothing be appended when no finish_reason was present; an empty turn
/// now always states the facts it observed, and "absent" is one of them — a
/// provider that sends no finish_reason is a different cause from one that
/// sends `failed`, and saying nothing made the two indistinguishable in a
/// report. The invariant this test exists for, that the turn fails rather
/// than promoting reasoning, is unchanged.
#[test]
fn missing_finish_reason_reasoning_only_turn_errors() {
    let payload = serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "reasoning": "Working through it"
            }
        }]
    });
    let err = model_response_from_payload(payload)
        .expect_err("missing finish_reason must not promote reasoning to an answer");
    let msg = err.to_string();
    assert!(
        msg.contains("finish_reason: absent"),
        "an absent finish_reason must be reported as absent, got: {msg}"
    );
}

/// A tool-call-only turn carries `content: null` and a `tool_calls` array.
/// It must parse into a response whose message has no text block but the
/// tool call intact (id, name, arguments parsed from the JSON string), so the
/// harness's native tool loop can dispatch it. This is the core of bug #1:
/// previously the null content hard-errored and the tool call was dropped.
#[test]
fn parses_tool_call_only_response_with_null_content() {
    let payload = serde_json::json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_abc",
                    "type": "function",
                    "function": {
                        "name": "check_inventory",
                        "arguments": "{\"sku\":\"A-1\"}"
                    }
                }]
            }
        }]
    });
    let resp = model_response_from_payload(payload).expect("parses tool-call-only turn");
    assert_eq!(resp.text(), "", "no visible text on a tool-call-only turn");
    let calls = resp.tool_calls();
    assert_eq!(calls.len(), 1, "the tool call survives parsing");
    assert_eq!(calls[0].id, "call_abc");
    assert_eq!(calls[0].name, "check_inventory");
    assert_eq!(calls[0].arguments, serde_json::json!({ "sku": "A-1" }));
    assert!(calls[0].invalid.is_none());
    assert_eq!(resp.finish_reason.as_deref(), Some("tool_calls"));
}

#[test]
fn request_approval_with_siblings_refuses_the_whole_model_response() {
    let payload = serde_json::json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [
                    {"id":"c1","type":"function","function":{"name":"shell","arguments":"{}"}},
                    {"id":"c2","type":"function","function":{"name":"request_approval","arguments":"{\"title\":\"Run\",\"question\":\"Proceed?\"}"}}
                ]
            }
        }]
    });
    let error = model_response_from_payload(payload)
        .expect_err("an approval boundary cannot share one tool-call batch");
    assert!(error.to_string().contains("sibling tool calls"));
}

/// A missing/empty tool-call `id` is back-filled with a stable `tool-{index}`
/// slot id so the tool result can still correlate, and unparseable arguments
/// are preserved + flagged `invalid` rather than dropping the call.
#[test]
fn tool_call_id_backfill_and_invalid_arguments_are_tolerated() {
    let payload = serde_json::json!({
        "choices": [{
            "message": {
                "tool_calls": [{
                    "function": { "name": "do_thing", "arguments": "{not json" }
                }]
            }
        }]
    });
    let resp = model_response_from_payload(payload).expect("parses");
    let calls = resp.tool_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "tool-0", "missing id back-fills to slot id");
    assert!(
        calls[0].invalid.is_some(),
        "unparseable arguments flag the call instead of dropping it"
    );
    assert_eq!(
        calls[0].arguments,
        serde_json::Value::String("{not json".to_string()),
        "raw arguments preserved for model retry"
    );
}

/// Exposed tools serialize into the OpenAI `tools[]`/`tool_choice` wire
/// shape, and a multi-turn tool history (assistant `tool_calls` + a `tool`
/// result) round-trips through the outbound message mapping. Together these
/// are the outbound half of native tool calling.
#[test]
fn tools_and_tool_history_serialize_to_openai_wire() {
    use tinyinference::message::Message;

    let tools = wire_tools(&[ToolSchema {
        name: "check_inventory".to_string(),
        description: "look up stock".to_string(),
        parameters: serde_json::json!({ "type": "object" }),
        format: tinyinference::tool::ToolFormat::default(),
    }]);
    let mut body = serde_json::json!({ "model": "chat-v1" });
    attach_tools(&mut body, tools, &ToolChoice::Required, true);
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["name"], "check_inventory");
    assert_eq!(body["tool_choice"], "required");
    assert_eq!(body["parallel_tool_calls"], false);
    let mut unsupported = serde_json::json!({ "model": "local" });
    attach_tools(
        &mut unsupported,
        wire_tools(&[ToolSchema {
            name: "check_inventory".to_string(),
            description: "look up stock".to_string(),
            parameters: serde_json::json!({ "type": "object" }),
            format: tinyinference::tool::ToolFormat::default(),
        }]),
        &ToolChoice::Required,
        false,
    );
    assert!(unsupported.get("parallel_tool_calls").is_none());

    // An assistant tool-call turn → null content + wire tool_calls; the tool
    // result → a `tool` role message carrying its `tool_call_id`.
    let assistant = Message::Assistant(AssistantMessage {
        id: None,
        content: Vec::new(),
        tool_calls: vec![ToolCall {
            id: "call_1".to_string(),
            name: "check_inventory".to_string(),
            arguments: serde_json::json!({ "sku": "A-1" }),
            invalid: None,
        }],
        usage: None,
        origin: None,
    });
    let tool_result = Message::tool("call_1", "3 in stock");
    let wire = wire_messages(&[assistant, tool_result]);
    assert_eq!(wire[0]["role"], "assistant");
    assert!(
        wire[0]["content"].is_null(),
        "tool-call-only turn has null content"
    );
    assert_eq!(wire[0]["tool_calls"][0]["id"], "call_1");
    // OpenAI requires arguments as a JSON string, not an object.
    assert_eq!(
        wire[0]["tool_calls"][0]["function"]["arguments"],
        "{\"sku\":\"A-1\"}"
    );
    assert_eq!(wire[1]["role"], "tool");
    assert_eq!(wire[1]["tool_call_id"], "call_1");
    assert_eq!(wire[1]["content"], "3 in stock");
}

/// The hosted provider must advertise native tool calling, since openhuman's
/// turn loop derives `native_tools` from `profile().tool_calling` — without it
/// the harness silently falls back to prompt-guided XML (bug #1's mechanism).
#[test]
fn hosted_provider_advertises_native_tool_calling() {
    let provider = HostedProvider::new(HostedProviderConfig {
        base_url: "https://example.test/v1".to_string(),
        credential: Credential::None,
        extra_headers: Vec::new(),
    });
    let profile = provider.profile().expect("hosted profile is advertised");
    assert!(
        profile.tool_calling,
        "native tool calling must be advertised"
    );
    assert!(
        !profile.parallel_tool_calls,
        "one native call per assistant message keeps request_approval a turn boundary"
    );
}

/// The profile must advertise a context window because it activates
/// `ContextCompressionMiddleware` and `ImageAwareMessageTrimMiddleware`.
/// A missing value leaves intra-turn history unbounded and can end in the
/// observed silent provider failure: HTTP 200, `finish_reason: "failed"`, an
/// empty response, and zero usage.
#[test]
fn both_providers_advertise_the_same_context_window() {
    let expected = super::context_window();
    // `OPENCOMPANY_CONTEXT_WINDOW=off|0` is the documented escape hatch that
    // restores unbounded history, so `None` is legitimate only there; every
    // other environment must still advertise a window.
    let explicitly_disabled = std::env::var("OPENCOMPANY_CONTEXT_WINDOW")
        .map(|raw| {
            let raw = raw.trim();
            raw.eq_ignore_ascii_case("off") || raw == "0"
        })
        .unwrap_or(false);
    if explicitly_disabled {
        assert_eq!(
            expected, None,
            "OPENCOMPANY_CONTEXT_WINDOW=off|0 must disable the window"
        );
    } else {
        assert!(
            expected.is_some(),
            "the default profile must advertise a context window"
        );
    }
    let hosted = HostedProvider::new(HostedProviderConfig {
        base_url: "https://example.test/v1".to_string(),
        credential: Credential::None,
        extra_headers: Vec::new(),
    });
    assert_eq!(
        hosted
            .profile()
            .expect("hosted profile is advertised")
            .max_input_tokens,
        expected
    );
    // TenantProvider returns the same `MANAGED_PROFILE`, so tenant-provided
    // credentials receive the same history protection as the hosted route.
    assert_eq!(*MANAGED_PROFILE_WINDOW, expected);
}

/// Read the static profile directly to verify that both `profile()`
/// implementations draw from the same source.
static MANAGED_PROFILE_WINDOW: std::sync::LazyLock<Option<u64>> =
    std::sync::LazyLock::new(|| super::MANAGED_PROFILE.max_input_tokens);

/// The rotation contract at the transport: the SAME provider instance must
/// present the token the projected file holds **now**, not the one it held
/// when the provider was built. Without this, a hosted pod keeps sending a
/// bearer the cluster rotated away from and every turn 401s.
#[tokio::test]
async fn hosted_provider_resolves_the_bearer_per_request() {
    let (url, seen) = spawn_auth_recorder().await;
    let dir = tempfile::Builder::new()
        .prefix("oc-prov-rot-")
        .tempdir()
        .expect("tempdir");
    let path = dir.path().join("token");
    // No `exp` to read ⇒ never cached ⇒ every request re-reads the file.
    std::fs::write(&path, "token-before-rotation").unwrap();

    let provider = HostedProvider::new(HostedProviderConfig {
        base_url: url,
        credential: Credential::from_source(Arc::new(TinyhumansTokenSource::projected_file(&path))),
        extra_headers: Vec::new(),
    });

    provider
        .invoke(
            &(),
            ModelRequest {
                model: Some("stub-model".into()),
                ..user_request("one")
            },
        )
        .await
        .expect("t1");
    std::fs::write(&path, "token-after-rotation").unwrap();
    provider
        .invoke(
            &(),
            ModelRequest {
                model: Some("stub-model".into()),
                ..user_request("two")
            },
        )
        .await
        .expect("t2");

    let headers = seen.lock().unwrap().clone();
    assert_eq!(
        headers,
        vec![
            "Bearer token-before-rotation".to_string(),
            "Bearer token-after-rotation".to_string()
        ],
        "the bearer must be resolved per request, not captured at build time"
    );
}

/// With no credential at all the header is omitted rather than sent empty.
#[tokio::test]
async fn hosted_provider_omits_the_bearer_without_a_credential() {
    let (url, seen) = spawn_auth_recorder().await;
    let provider = HostedProvider::new(HostedProviderConfig {
        base_url: url,
        credential: Credential::None,
        extra_headers: Vec::new(),
    });
    provider
        .invoke(
            &(),
            ModelRequest {
                model: Some("stub-model".into()),
                ..user_request("hi")
            },
        )
        .await
        .expect("turn");
    assert_eq!(seen.lock().unwrap().clone(), vec![String::new()]);
}

/// A 401 invalidates the cached read, so the next turn presents whatever the
/// file holds now instead of re-sending a bearer the backend just refused —
/// the recovery path for a token the platform rotated early.
#[tokio::test]
async fn a_rejected_bearer_forces_a_re_read_on_the_next_turn() {
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::{Json, Router};

    let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let app = Router::new().route(
        "/chat/completions",
        post(move |headers: HeaderMap| {
            let log = Arc::clone(&log);
            async move {
                let auth = headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                let first = {
                    let mut guard = log.lock().unwrap();
                    guard.push(auth);
                    guard.len() == 1
                };
                // Refuse the first bearer, accept the second.
                if first {
                    (StatusCode::UNAUTHORIZED, Json(serde_json::json!({}))).into_response()
                } else {
                    Json(serde_json::json!({
                        "choices": [{ "message": { "role": "assistant", "content": "ok" } }]
                    }))
                    .into_response()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let dir = tempfile::Builder::new()
        .prefix("oc-prov-401-")
        .tempdir()
        .expect("tempdir");
    let path = dir.path().join("token");
    // A long-lived `exp` ⇒ the window would normally hold this read for the
    // full cap, so only invalidation can explain the second value going out.
    let long_lived = jwt_with_exp(60 * 60 * 24 * 365 * 100);
    std::fs::write(&path, format!("stale-{long_lived}")).unwrap();

    let provider = HostedProvider::new(HostedProviderConfig {
        base_url: format!("http://{addr}"),
        credential: Credential::from_source(Arc::new(TinyhumansTokenSource::projected_file(&path))),
        extra_headers: Vec::new(),
    });

    provider
        .invoke(
            &(),
            ModelRequest {
                model: Some("stub-model".into()),
                ..user_request("one")
            },
        )
        .await
        .expect_err("first turn is refused");
    std::fs::write(&path, format!("rotated-{long_lived}")).unwrap();
    provider
        .invoke(
            &(),
            ModelRequest {
                model: Some("stub-model".into()),
                ..user_request("two")
            },
        )
        .await
        .expect("t2");

    let headers = seen.lock().unwrap().clone();
    assert_eq!(headers.len(), 2, "{headers:?}");
    assert!(headers[0].starts_with("Bearer stale-"), "{headers:?}");
    assert!(
        headers[1].starts_with("Bearer rotated-"),
        "a 401 must send the next turn back to the file: {headers:?}"
    );
}

/// An unreadable projected file fails the turn with a model error that names
/// the problem — it must never silently send no bearer and get a confusing
/// 401 from the backend instead.
#[tokio::test]
async fn hosted_provider_surfaces_an_unreadable_token_file() {
    let provider = HostedProvider::new(HostedProviderConfig {
        base_url: "http://127.0.0.1:1/v1".to_string(),
        credential: Credential::from_source(Arc::new(TinyhumansTokenSource::projected_file(
            "/nonexistent/oc/token",
        ))),
        extra_headers: Vec::new(),
    });
    let err = provider
        .invoke(
            &(),
            ModelRequest {
                model: Some("stub-model".into()),
                ..user_request("hi")
            },
        )
        .await
        .expect_err("unreadable credential");
    assert!(err.to_string().contains("credential"), "{err}");
}
