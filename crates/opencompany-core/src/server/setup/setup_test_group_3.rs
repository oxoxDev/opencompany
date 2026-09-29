#[cfg(feature = "openhuman")]
use crate::app::config::MapEnv;
use crate::ports::types::CompanyId;
use axum::http::StatusCode;
#[cfg(feature = "openhuman")]
use std::sync::Arc;

use super::setup_test_support_1::*;

/// The merge, end to end over HTTP: three answers and a reviewed roster become
/// a registered company, with no template involved.
#[tokio::test]
async fn an_apply_seeds_the_company_the_wizard_designed() {
    let home = home();
    let state = fresh_state(home.path());
    assert!(state.registry().is_empty(), "the premise: nothing yet");

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({ "company": designed_company(Some("ada@example.com")) }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let seeded = body["seeded_company"]
        .as_str()
        .expect("a company was seeded");
    assert!(
        state.registry().get(&CompanyId::new(seeded)).is_some(),
        "the seeded company is registered"
    );
    let manifest = seeded_manifest(home.path(), seeded).await;
    // Every designed teammate is on the roster. NOT an exact count: a company
    // also receives the global baseline agents (`src/globals/`), so asserting a
    // total here would pin this test to how many globals ship rather than to
    // anything this flow decides.
    let roles: Vec<&str> = manifest.agents.iter().map(|a| a.role.as_str()).collect();
    for designed in [
        "Meta Ads Specialist",
        "Order Dispatch Coordinator",
        "Accountant",
        "Operations Lead",
    ] {
        assert!(
            roles.contains(&designed),
            "{designed} is missing from {roles:?}"
        );
    }
    // The dead end this closes: without the address, email sign-in completes
    // and nobody can log in.
    assert_eq!(manifest.users.admins, vec!["ada@example.com".to_string()]);
    // Indistinguishable from a provisioned company.
    assert_eq!(
        manifest.policy.mode,
        crate::company::PROVISIONED_POLICY_MODE
    );
}

#[tokio::test]
async fn onboarding_persists_the_local_model_it_tested() {
    let home = home();
    let state = fresh_state(home.path());
    let mut company = designed_company(None);
    company["inference"] = serde_json::json!({
        "provider": "ollama",
        "baseUrl": "localhost:6969",
        "model": "qwen3:8b"
    });

    let (status, body) = post_setup(state.clone(), serde_json::json!({ "company": company })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let seeded = body["seeded_company"].as_str().expect("seeded");
    let manifest = seeded_manifest(home.path(), seeded).await;
    assert_eq!(manifest.inference.provider.as_deref(), Some("ollama"));
    assert_eq!(
        manifest.inference.base_url.as_deref(),
        Some("http://localhost:6969/v1")
    );
    for tier in crate::company::INFERENCE_TIERS {
        assert_eq!(
            manifest.inference.models.get(*tier).map(String::as_str),
            Some("qwen3:8b")
        );
    }
}

#[cfg(feature = "openhuman")]
#[tokio::test]
async fn local_model_probe_normalizes_the_address_and_detects_its_model() {
    let app = axum::Router::new()
        .route(
            "/v1/models",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({ "data": [{ "id": "qwen3:8b" }] }))
            }),
        )
        .route(
            "/v1/chat/completions",
            axum::routing::post(|| async {
                axum::Json(serde_json::json!({
                    "choices": [{ "message": { "content": "pong" } }]
                }))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let result = super::probe_inference(
        &super::InferenceTestRequest {
            provider: "ollama".to_string(),
            base_url: Some(address.to_string()),
            ..Default::default()
        },
        &MapEnv::default(),
        crate::app::config::DEFAULT_API_URL,
    )
    .await;
    server.abort();

    assert!(result.ok, "{:?}", result.error);
    assert_eq!(result.base_url, format!("http://{address}/v1"));
    assert_eq!(result.model.as_deref(), Some("qwen3:8b"));
}

#[cfg(feature = "openhuman")]
#[tokio::test]
async fn managed_probe_accepts_an_authenticated_catalog_without_spending_credit() {
    let chat_requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let chat_requests_for_route = chat_requests.clone();
    let app = axum::Router::new()
        .route(
            "/agent-integrations/openrouter/models",
            axum::routing::get(|headers: axum::http::HeaderMap| async move {
                assert_eq!(
                    headers
                        .get("authorization")
                        .and_then(|value| value.to_str().ok()),
                    Some("Bearer th-not-a-real-key")
                );
                axum::Json(serde_json::json!({
                    "success": true,
                    "data": {
                        "data": [{ "id": "openai/gpt-test" }],
                        "total": 1,
                        "limit": 500,
                        "offset": 0
                    }
                }))
            }),
        )
        .route(
            "/agent-integrations/openrouter/chat/completions",
            axum::routing::post(move || {
                let chat_requests = chat_requests_for_route.clone();
                async move {
                    chat_requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    (
                        axum::http::StatusCode::TOO_MANY_REQUESTS,
                        axum::Json(serde_json::json!({
                            "error": { "message": "rate limited" }
                        })),
                    )
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let endpoint = format!("http://{address}/agent-integrations/openrouter");
    let env = MapEnv::new([
        ("OPENCOMPANY_INFERENCE_URL", endpoint.as_str()),
        ("TINYHUMANS_API_KEY", "th-not-a-real-key"),
    ]);

    let result = super::probe_inference(
        &super::InferenceTestRequest {
            provider: "managed".to_string(),
            ..Default::default()
        },
        &env,
        crate::app::config::DEFAULT_API_URL,
    )
    .await;
    server.abort();

    assert!(result.ok, "{:?}", result.error);
    assert_eq!(result.model.as_deref(), Some("openai/gpt-test"));
    assert_eq!(chat_requests.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[cfg(feature = "openhuman")]
#[tokio::test]
async fn cloud_provider_probe_discovers_a_model_before_chat() {
    let app = axum::Router::new()
        .route(
            "/v1/models",
            axum::routing::get(|headers: axum::http::HeaderMap| async move {
                assert_eq!(
                    headers
                        .get("x-api-key")
                        .and_then(|value| value.to_str().ok()),
                    Some("sk-ant-not-a-real-key")
                );
                axum::Json(serde_json::json!({
                    "data": [{ "id": "claude-test" }]
                }))
            }),
        )
        .route(
            "/v1/chat/completions",
            axum::routing::post(|headers: axum::http::HeaderMap| async move {
                assert_eq!(
                    headers
                        .get("authorization")
                        .and_then(|value| value.to_str().ok()),
                    Some("Bearer sk-ant-not-a-real-key")
                );
                axum::Json(serde_json::json!({
                    "choices": [{ "message": { "content": "pong" } }]
                }))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let result = super::probe_inference(
        &super::InferenceTestRequest {
            provider: "anthropic".to_string(),
            key: Some("sk-ant-not-a-real-key".to_string()),
            base_url: Some(format!("http://{address}/v1")),
        },
        &MapEnv::default(),
        crate::app::config::DEFAULT_API_URL,
    )
    .await;
    server.abort();

    assert!(result.ok, "{:?}", result.error);
    assert_eq!(result.model.as_deref(), Some("claude-test"));
}

#[cfg(feature = "openhuman")]
#[tokio::test]
async fn probe_prioritises_a_chat_model_after_five_non_chat_entries() {
    let attempted = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let attempted_for_route = attempted.clone();
    let app = axum::Router::new()
        .route(
            "/v1/models",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({
                    "data": [
                        { "id": "embedding-test-0" },
                        { "id": "embedding-test-1" },
                        { "id": "embedding-test-2" },
                        { "id": "embedding-test-3" },
                        { "id": "embedding-test-4" },
                        { "id": "chat-test" }
                    ]
                }))
            }),
        )
        .route(
            "/v1/chat/completions",
            axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let attempted = attempted_for_route.clone();
                async move {
                    let model = body["model"].as_str().unwrap().to_string();
                    attempted.lock().unwrap().push(model.clone());
                    if model.starts_with("embedding-") {
                        return (
                            axum::http::StatusCode::BAD_REQUEST,
                            axum::Json(serde_json::json!({
                                "error": { "message": "model does not support chat" }
                            })),
                        );
                    }
                    (
                        axum::http::StatusCode::OK,
                        axum::Json(serde_json::json!({
                            "choices": [{ "message": { "content": "pong" } }]
                        })),
                    )
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let result = super::probe_inference(
        &super::InferenceTestRequest {
            provider: "openai_compatible".to_string(),
            base_url: Some(format!("http://{address}/v1")),
            ..Default::default()
        },
        &MapEnv::default(),
        crate::app::config::DEFAULT_API_URL,
    )
    .await;
    server.abort();

    assert!(result.ok, "{:?}", result.error);
    assert_eq!(result.model.as_deref(), Some("chat-test"));
    assert_eq!(attempted.lock().unwrap().as_slice(), ["chat-test"]);
}

#[cfg(feature = "openhuman")]
#[tokio::test]
async fn probe_bounds_model_specific_catalog_rejections() {
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let attempts_for_route = attempts.clone();
    let models = (0..super::MODEL_PROBE_CANDIDATE_LIMIT + 2)
        .map(|index| serde_json::json!({ "id": format!("model-{index}") }))
        .collect::<Vec<_>>();
    let app = axum::Router::new()
        .route(
            "/v1/models",
            axum::routing::get(move || {
                let models = models.clone();
                async move { axum::Json(serde_json::json!({ "data": models })) }
            }),
        )
        .route(
            "/v1/chat/completions",
            axum::routing::post(move || {
                attempts_for_route.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async {
                    (
                        axum::http::StatusCode::BAD_REQUEST,
                        axum::Json(serde_json::json!({
                            "error": { "message": "model does not support chat" }
                        })),
                    )
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let result = super::probe_inference(
        &super::InferenceTestRequest {
            provider: "openai_compatible".to_string(),
            base_url: Some(format!("http://{address}/v1")),
            ..Default::default()
        },
        &MapEnv::default(),
        crate::app::config::DEFAULT_API_URL,
    )
    .await;
    server.abort();

    assert!(!result.ok);
    assert_eq!(
        attempts.load(std::sync::atomic::Ordering::SeqCst),
        super::MODEL_PROBE_CANDIDATE_LIMIT
    );
}

#[cfg(feature = "openhuman")]
#[tokio::test]
async fn an_empty_catalog_has_its_own_failure_and_never_sends_chat() {
    let chat_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hits_for_route = chat_hits.clone();
    let app = axum::Router::new()
        .route(
            "/v1/models",
            axum::routing::get(|| async { axum::Json(serde_json::json!({ "data": [] })) }),
        )
        .route(
            "/v1/chat/completions",
            axum::routing::post(move || {
                hits_for_route.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { axum::http::StatusCode::NO_CONTENT }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let result = super::probe_inference(
        &super::InferenceTestRequest {
            provider: "openai_compatible".to_string(),
            base_url: Some(format!("http://{address}/v1")),
            ..Default::default()
        },
        &MapEnv::default(),
        crate::app::config::DEFAULT_API_URL,
    )
    .await;
    server.abort();

    assert!(!result.ok);
    assert_eq!(
        result.error.as_deref(),
        Some(super::MODEL_DISCOVERY_FAILURE)
    );
    assert_eq!(result.model, None);
    assert_eq!(chat_hits.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[cfg(feature = "openhuman")]
#[tokio::test]
async fn catalog_auth_rejections_keep_their_credential_message() {
    for (status, expected) in [
        (
            axum::http::StatusCode::UNAUTHORIZED,
            "That key was rejected by the provider.",
        ),
        (
            axum::http::StatusCode::FORBIDDEN,
            "That key was accepted but is not allowed to list models.",
        ),
    ] {
        let app = axum::Router::new().route(
            "/v1/models",
            axum::routing::get(move || async move { status }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let result = super::probe_inference(
            &super::InferenceTestRequest {
                provider: "openai_compatible".to_string(),
                key: Some("not-a-real-key".to_string()),
                base_url: Some(format!("http://{address}/v1")),
            },
            &MapEnv::default(),
            crate::app::config::DEFAULT_API_URL,
        )
        .await;
        server.abort();

        assert!(!result.ok);
        assert_eq!(result.error.as_deref(), Some(expected));
        assert_eq!(result.model, None);
    }
}

#[cfg(feature = "openhuman")]
#[test]
fn typed_probe_failures_choose_copy_from_the_error_type() {
    let provider = |status| {
        anyhow::Error::new(tinyinference::Error::Provider(Box::new(
            tinyinference::model::ProviderError {
                provider: "test".to_string(),
                status: Some(status),
                message: "wording may change".to_string(),
                ..Default::default()
            },
        )))
    };

    for (status, expected) in [
        (401, "That key was rejected by the provider."),
        (
            403,
            "That key was accepted but is not allowed to use this model.",
        ),
        (
            404,
            "Reached the host, but there is no chat endpoint at that URL.",
        ),
        (429, "The provider is rate-limiting this key right now."),
    ] {
        assert_eq!(super::summarise_probe_failure(&provider(status)), expected);
    }

    let unavailable_model = anyhow::Error::new(tinyinference::Error::Provider(Box::new(
        tinyinference::model::ProviderError {
            provider: "test".to_string(),
            status: Some(404),
            message: concat!(
                "the configured inference model is not available from the provider",
                " — choose another model"
            )
            .to_string(),
            ..Default::default()
        },
    )));
    assert_eq!(
        super::summarise_probe_failure(&unavailable_model),
        "That model is not available from this provider for your account."
    );
}

#[cfg(feature = "openhuman")]
#[test]
fn typed_configuration_failures_cannot_match_transport_words() {
    for error in [
        tinyinference::Error::Model("Choose a model in Connections before continuing.".to_string()),
        tinyinference::Error::Validation("connection model is invalid".to_string()),
    ] {
        assert_eq!(
            super::summarise_probe_failure(&anyhow::Error::new(error)),
            "The model configuration for this connection is invalid."
        );
    }
    assert_eq!(
        super::summarise_probe_failure(&anyhow::anyhow!(
            "dns failed while connecting to the endpoint"
        )),
        "Could not reach that address."
    );
}

/// The first-run probe refuses an endpoint carrying a credential **before it
/// sends anything**, and never echoes the credential back.
///
/// The server counts every request it receives: the catalogue read and the
/// probe would each have presented the userinfo as basic auth, so a refusal
/// that came after either of them would already have leaked it.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn setup_probe_refuses_a_credentialed_endpoint_before_sending_anything() {
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let app = axum::Router::new().fallback({
        let hits = hits.clone();
        move || {
            hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { axum::http::StatusCode::NOT_FOUND }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let result = super::probe_inference(
        &super::InferenceTestRequest {
            provider: "openai_compatible".to_string(),
            base_url: Some(format!("http://alice:hunter2@{address}/v1")),
            ..Default::default()
        },
        &MapEnv::default(),
        crate::app::config::DEFAULT_API_URL,
    )
    .await;
    server.abort();

    assert!(!result.ok, "a credentialed endpoint must not test green");
    assert_eq!(
        hits.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "nothing may be sent to an endpoint carrying a credential"
    );
    assert!(
        !result.base_url.contains("hunter2") && !result.base_url.contains("alice"),
        "the echoed endpoint must be redacted: {}",
        result.base_url
    );
    let error = result.error.as_deref().unwrap_or_default();
    assert!(
        error.contains("username or password"),
        "expected the refusal sentence, got: {error}"
    );
    assert!(!error.contains("hunter2"), "{error}");
}

/// A designed company beats a template slug. An operator who answered three
/// questions and edited a roster has expressed a preference a preset cannot
/// override — and sending both must never produce two companies.
#[tokio::test]
async fn a_designed_company_wins_over_a_template() {
    let home = home();
    let state = fresh_state(home.path());

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({
            "template": "marketing_agency",
            "company": designed_company(None),
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(state.registry().list().len(), 1, "exactly one company");
    let seeded = body["seeded_company"].as_str().expect("seeded");
    let roles: Vec<String> = seeded_manifest(home.path(), seeded)
        .await
        .agents
        .into_iter()
        .map(|a| a.role)
        .collect();
    // The designed roster landed; the template's did not. Named rather than
    // counted, because the global baseline agents are on here too.
    assert!(
        roles.iter().any(|r| r == "Order Dispatch Coordinator"),
        "the designed roster is missing: {roles:?}"
    );
    assert!(
        !roles.iter().any(|r| r == "Creative Director"),
        "the marketing template's roster leaked in: {roles:?}"
    );
}

/// The re-run guard applies to a designed company exactly as it does to a
/// template: setup must never hand an operator a second starter company.
#[tokio::test]
async fn a_second_apply_does_not_seed_another_company() {
    let home = home();
    let state = fresh_state(home.path());
    with_company(&state, home.path()).await;

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({ "company": designed_company(None) }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["seeded_company"].is_null(),
        "a host with a company must not be handed a second: {body}"
    );
    assert_eq!(state.registry().list().len(), 1);
}

/// CONSOLE-ADMIN-056: the re-run guard above holds against a second,
/// *sequential* apply. It must hold just as well when two first-run applies
/// land concurrently — the case the guard's own doc comment is really about
/// ("a re-run must never hand the operator a second starter company"), just
/// reached by two racing callers instead of one later one.
#[tokio::test]
async fn concurrent_first_run_applies_seed_at_most_one_company() {
    let home = home();
    let state = fresh_state(home.path());
    assert!(state.registry().is_empty(), "the premise: nothing yet");

    let body = serde_json::json!({ "company": designed_company(None) });
    let (first, second) = tokio::join!(
        post_setup(state.clone(), body.clone()),
        post_setup(state.clone(), body),
    );

    assert_eq!(first.0, StatusCode::OK, "{:?}", first.1);
    assert_eq!(second.0, StatusCode::OK, "{:?}", second.1);
    assert_eq!(
        state.registry().list().len(),
        1,
        "two concurrent first-run applies must seed exactly one company, not two: \
         {first:?} {second:?}"
    );

    // Exactly one of the two responses may report a seed; the other must
    // accurately report it found the registry already occupied by the time it
    // ran, not silently invent (or omit) a second one.
    let seeded = [&first.1, &second.1]
        .iter()
        .filter(|body| !body["seeded_company"].is_null())
        .count();
    assert_eq!(
        seeded, 1,
        "exactly one of the two concurrent calls may report a seed: {first:?} {second:?}"
    );
}
