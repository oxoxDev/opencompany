//! Tests for the TinyHumans System One transport: the wire it puts a request
//! on, the one retry it allows, and the configuration it refuses.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::json;
use tinyhivemind_typesafe::{
    Error, Question, SystemOneAnswer, SystemOneRequest, SystemOneTransport,
};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::app::config::MapEnv;

fn request() -> SystemOneRequest {
    let mut questions = BTreeMap::new();
    questions.insert(
        "primary".to_string(),
        Question::Choice {
            instructions: json!("Who answers?"),
            criteria: BTreeMap::from([("engineer".to_string(), None), ("none".to_string(), None)]),
        },
    );
    SystemOneRequest {
        state: json!({"desk": "engineering"}),
        model: "jev-latest".to_string(),
        questions,
    }
}

fn answer_body() -> serde_json::Value {
    json!({
        // The proxy resolves the alias; the concrete id comes back.
        "model": "typesafe/jev-2026-08",
        "answers": {
            "primary": {
                "type": "choice",
                "choice": "engineer",
                "probabilities": {"engineer": 0.9, "none": 0.1},
                "confidence": 0.8
            }
        },
        "usage": {"input_tokens": 12, "output_tokens": 3}
    })
}

#[tokio::test]
async fn round_trip_decodes_a_system_one_response_with_bearer_and_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/agent-integrations/openrouter/systemone"))
        .and(header("authorization", "Bearer th_test_key"))
        .and(header("content-type", "application/json"))
        .and(body_partial_json(json!({"model": "jev-latest"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer_body()))
        .expect(1)
        .mount(&server)
        .await;

    let transport = TinyHumansSystemOne::with_key(
        format!("{}/agent-integrations/openrouter/systemone", server.uri()),
        "th_test_key",
    )
    .unwrap();
    let response = transport.evaluate(&request()).await.unwrap();

    assert_eq!(response.model, "typesafe/jev-2026-08");
    assert_eq!(response.usage.input_tokens, 12);
    match &response.answers["primary"] {
        SystemOneAnswer::Choice(choice) => assert_eq!(choice.choice, "engineer"),
        other => panic!("expected a choice answer, got {other:?}"),
    }
}

#[tokio::test]
async fn a_5xx_is_retried_exactly_once_then_surfaces_as_a_transport_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503).set_body_string("upstream unavailable"))
        .expect(2)
        .mount(&server)
        .await;

    let transport = TinyHumansSystemOne::with_key(server.uri(), "th_test_key").unwrap();
    let error = transport.evaluate(&request()).await.unwrap_err();

    match error {
        Error::Transport { status, message } => {
            assert_eq!(status, Some(503));
            assert_eq!(message, "upstream unavailable");
        }
        other => panic!("expected a transport error, got {other:?}"),
    }
    // `.expect(2)` on the mock is verified when `server` drops.
}

#[tokio::test]
async fn a_retryable_status_that_recovers_returns_the_second_answer() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer_body()))
        .expect(1)
        .mount(&server)
        .await;

    let transport = TinyHumansSystemOne::with_key(server.uri(), "th_test_key").unwrap();
    let response = transport.evaluate(&request()).await.unwrap();
    assert_eq!(response.model, "typesafe/jev-2026-08");
}

#[tokio::test]
async fn a_4xx_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
        .expect(1)
        .mount(&server)
        .await;

    let transport = TinyHumansSystemOne::with_key(server.uri(), "th_test_key").unwrap();
    let error = transport.evaluate(&request()).await.unwrap_err();
    assert!(
        matches!(
            error,
            Error::Transport {
                status: Some(401),
                ..
            }
        ),
        "got {error:?}"
    );
}

#[tokio::test]
async fn an_undecodable_2xx_is_a_transport_error_with_the_status() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"not": "system one"})))
        .expect(1)
        .mount(&server)
        .await;

    let transport = TinyHumansSystemOne::with_key(server.uri(), "th_test_key").unwrap();
    let error = transport.evaluate(&request()).await.unwrap_err();
    match error {
        Error::Transport { status, message } => {
            assert_eq!(status, Some(200));
            assert!(message.contains("undecodable"), "{message}");
        }
        other => panic!("expected a transport error, got {other:?}"),
    }
}

/// A transport whose ceiling has been pulled in to something a test can wait
/// for. The public builder pins [`REQUEST_TIMEOUT`]; the test reaches past it.
fn transport_with_timeout(url: String, timeout: Duration) -> TinyHumansSystemOne {
    let mut transport = TinyHumansSystemOne::with_key(url, "th_test_key").unwrap();
    transport.client = reqwest::Client::builder().timeout(timeout).build().unwrap();
    transport
}

#[tokio::test]
async fn a_timeout_is_a_transport_error_without_a_status_and_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(answer_body())
                .set_delay(Duration::from_secs(5)),
        )
        .expect(1)
        .mount(&server)
        .await;

    let transport = transport_with_timeout(server.uri(), Duration::from_millis(200));
    let error = transport.evaluate(&request()).await.unwrap_err();
    match error {
        Error::Transport { status, message } => {
            assert_eq!(status, None);
            assert!(message.contains("timed out"), "{message}");
        }
        other => panic!("expected a transport error, got {other:?}"),
    }
}

#[test]
fn retry_class_covers_typesafe_transients_and_proxy_5xx_only() {
    for status in [429, 529, 500, 502, 503, 504] {
        assert!(should_retry(status), "{status} should be retried once");
    }
    for status in [400, 401, 403, 404, 422] {
        assert!(!should_retry(status), "{status} must not be retried");
    }
}

#[test]
fn url_rule_allows_https_and_loopback_http_only() {
    assert!(validate_url("https://api.tinyhumans.ai/x".into()).is_ok());
    assert!(validate_url("http://127.0.0.1:9999/systemone".into()).is_ok());
    assert!(validate_url("http://localhost:9999/systemone".into()).is_ok());
    assert!(validate_url("http://[::1]/systemone".into()).is_ok());
    for bad in [
        "http://example.com/systemone",
        "http://127.0.0.1.evil.example/systemone",
        "ftp://127.0.0.1/systemone",
        "not a url",
        "https://",
        "mailto:ops@example.com",
    ] {
        let error = validate_url(bad.into()).unwrap_err();
        assert!(
            matches!(error, OpenCompanyError::Config(_)),
            "{bad}: {error:?}"
        );
    }
}

#[test]
fn jev_router_is_none_without_a_credential() {
    let env = MapEnv::new(Vec::<(String, String)>::new());
    assert!(jev_router(&env, None).unwrap().is_none());
    assert!(jev_router(&env, Some("   ")).unwrap().is_none());
}

#[test]
fn jev_router_takes_the_default_url_and_the_inference_key_ladder() {
    let env = MapEnv::new([("TINYHUMANS_API_KEY", "th_from_env")]);
    let router = jev_router(&env, None).unwrap().expect("a router");
    assert_eq!(router.transport().url(), DEFAULT_JEV_URL);

    let env = MapEnv::new([
        ("OPENCOMPANY_INFERENCE_KEY", "th_inference"),
        ("TINYHUMANS_API_KEY", "th_account"),
        (
            JEV_URL_ENV,
            "https://staging.tinyhumans.ai/agent-integrations/openrouter/systemone",
        ),
    ]);
    let router = jev_router(&env, None).unwrap().expect("a router");
    assert_eq!(
        router.transport().url(),
        "https://staging.tinyhumans.ai/agent-integrations/openrouter/systemone"
    );
    // The transport's Debug never prints the key, whichever tier it came from.
    let shown = format!("{:?}", router.transport());
    assert!(
        !shown.contains("th_inference") && !shown.contains("th_account"),
        "{shown}"
    );
}

#[test]
fn jev_router_prefers_a_caller_supplied_key_over_the_environment() {
    let env = MapEnv::new([("TINYHUMANS_API_KEY", "th_account")]);
    let router = jev_router(&env, Some("th_company"))
        .unwrap()
        .expect("a router");
    assert_eq!(
        router.transport().bearer.source(),
        crate::company::credentials::CredentialSource::Static
    );
}

#[test]
fn jev_router_refuses_a_plain_http_proxy_on_a_routable_host() {
    let env = MapEnv::new([
        ("TINYHUMANS_API_KEY", "th_account"),
        (JEV_URL_ENV, "http://example.com/systemone"),
    ]);
    let error = jev_router(&env, None).unwrap_err();
    let text = error.to_string();
    assert!(
        text.contains(JEV_URL_ENV) && text.contains("loopback"),
        "{text}"
    );
}

#[tokio::test]
async fn jev_router_reaches_a_loopback_http_proxy_with_the_env_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer th_account"))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer_body()))
        .expect(1)
        .mount(&server)
        .await;
    let env = MapEnv::new([
        ("TINYHUMANS_API_KEY", "th_account"),
        (JEV_URL_ENV, server.uri().as_str()),
    ]);
    let router = jev_router(&env, None).unwrap().expect("a router");
    let response = router.transport().evaluate(&request()).await.unwrap();
    assert_eq!(response.usage.output_tokens, 3);
}
