use super::*;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::get;
use serde_json::{Value, json};
use tinytools::Tool;

/// Shared recorder for every `Authorization` header the mock backend saw.
type AuthLog = Arc<Mutex<Vec<String>>>;

/// The mock `/agent-integrations/composio/connections` handler: records the
/// bearer it received and returns a `{success,data}` envelope whose single
/// connection's `account_email` is derived from that bearer — so a caller can
/// prove it only ever sees *its own* tenant's data.
async fn connections(State(log): State<AuthLog>, headers: HeaderMap) -> axum::Json<Value> {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    log.lock().unwrap().push(auth.clone());
    // Derive tenant identity purely from the presented bearer.
    let email = if auth.contains("token-a") {
        "a@example.com"
    } else if auth.contains("token-b") {
        "b@example.com"
    } else {
        "unknown@example.com"
    };
    axum::Json(json!({
        "success": true,
        "data": {
            "connections": [
                { "id": "conn-1", "toolkit": "gmail", "status": "ACTIVE", "accountEmail": email }
            ]
        }
    }))
}

/// Spawn the mock backend on an ephemeral port; returns its base URL + the
/// auth-header recorder.
async fn spawn_backend() -> (String, AuthLog) {
    let log: AuthLog = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/agent-integrations/composio/connections", get(connections))
        .with_state(log.clone());
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), log)
}

fn config(url: &str, token: &str) -> TenantComposio {
    TenantComposio::new(url, Credential::from_value(token), Vec::new())
}

fn list_connections_tool(config: &TenantComposio) -> Box<dyn Tool> {
    let metering = ComposioMetering {
        company: CompanyId::new("acme"),
        agent: "ceo".to_string(),
        meter: None,
    };
    composio_tools(config, metering)
        .into_iter()
        .find(|t| t.name() == "composio_list_connections")
        .expect("composio_list_connections tool present")
}

#[tokio::test]
async fn each_tenant_only_ever_carries_its_own_token_and_sees_its_own_accounts() {
    let (url, log) = spawn_backend().await;

    let tool_a = list_connections_tool(&config(&url, "token-a"));
    let tool_b = list_connections_tool(&config(&url, "token-b"));

    let out_a = tool_a.execute(json!({})).await.unwrap();
    let text_a = out_a.output();
    let out_b = tool_b.execute(json!({})).await.unwrap();
    let text_b = out_b.output();

    // A saw only A's account; never B's account nor B's token.
    assert!(
        text_a.contains("a@example.com"),
        "A missing its account: {text_a}"
    );
    assert!(
        !text_a.contains("b@example.com"),
        "A leaked B's account: {text_a}"
    );
    assert!(!text_a.contains("token-b"), "A leaked B's token: {text_a}");
    // Symmetrically for B.
    assert!(
        text_b.contains("b@example.com"),
        "B missing its account: {text_b}"
    );
    assert!(
        !text_b.contains("a@example.com"),
        "B leaked A's account: {text_b}"
    );

    // A's own token is scrubbed out of its own successful output.
    assert!(
        !text_a.contains("token-a"),
        "A leaked its own token: {text_a}"
    );

    // The backend received exactly the two distinct bearers — each request
    // carried its own tenant's token, never the other's.
    let seen = log.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "expected one request per tenant: {seen:?}");
    assert!(
        seen.iter().any(|a| a == "Bearer token-a"),
        "missing A bearer: {seen:?}"
    );
    assert!(
        seen.iter().any(|a| a == "Bearer token-b"),
        "missing B bearer: {seen:?}"
    );
    assert!(
        !seen
            .iter()
            .any(|a| a.contains("token-a") && a.contains("token-b")),
        "a single request must never carry both tokens: {seen:?}"
    );
}

/// The rotation contract at the tool boundary: a projected platform token the
/// cluster rewrites in place must reach the backend on the **next** call, with
/// no roster rebuild — and the freshly-resolved value must be the one the
/// scrub vector protects, so a backend that reflects it still cannot leak it.
#[tokio::test]
async fn a_rotated_projected_token_is_presented_and_scrubbed_per_call() {
    use crate::company::credentials::TinyhumansTokenSource;

    // Reflect the bearer back inside an envelope failure, and record it.
    async fn reflect(State(log): State<AuthLog>, headers: HeaderMap) -> axum::Json<Value> {
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        log.lock().unwrap().push(auth.clone());
        axum::Json(json!({ "success": false, "error": format!("upstream said: {auth}") }))
    }
    let log: AuthLog = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/agent-integrations/composio/connections", get(reflect))
        .with_state(log.clone());
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = tempfile::Builder::new()
        .prefix("oc-composio-rot-")
        .tempdir()
        .expect("tempdir");
    let path = dir.path().join("token");
    std::fs::write(&path, "projected-secret-before").unwrap();

    // ONE config, built once — exactly what a roster holds across turns.
    let config = TenantComposio::new(
        format!("http://{addr}"),
        Credential::from_source(Arc::new(TinyhumansTokenSource::projected_file(&path))),
        Vec::new(),
    );
    let tool = list_connections_tool(&config);

    let first = tool.execute(json!({})).await.unwrap();
    assert!(
        !first.output().contains("projected-secret-before"),
        "the resolved token leaked into agent-visible output: {}",
        first.output()
    );

    // The kubelet rewrites the file in place; the SAME tool must present the
    // new token and scrub that one.
    std::fs::write(&path, "projected-secret-after").unwrap();
    let second = tool.execute(json!({})).await.unwrap();
    assert!(
        !second.output().contains("projected-secret-after"),
        "the rotated token leaked into agent-visible output: {}",
        second.output()
    );

    let seen = log.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec![
            "Bearer projected-secret-before".to_string(),
            "Bearer projected-secret-after".to_string()
        ],
        "each call must carry the token the file held at that moment: {seen:?}"
    );
}

/// A mock backend that echoes the caller's bearer inside an error body; the
/// tool's scrub must strip it before the agent ever sees it.
#[tokio::test]
async fn error_body_reflecting_the_token_is_scrubbed() {
    async fn reflect(headers: HeaderMap) -> axum::Json<Value> {
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        // A 2xx envelope failure whose message reflects the raw bearer.
        axum::Json(json!({ "success": false, "error": format!("upstream said: {auth}") }))
    }
    let app = Router::new().route("/agent-integrations/composio/connections", get(reflect));
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let url = format!("http://{addr}");

    let tool = list_connections_tool(&config(&url, "reflected-secret-token"));
    let out = tool.execute(json!({})).await.unwrap();
    let text = out.output();
    assert!(
        !text.contains("reflected-secret-token"),
        "the reflected token leaked into agent-visible output: {text}"
    );
}

// --- FAIL-axis: what each Composio tool does when the backend fails ------

use axum::http::StatusCode;
use axum::routing::post;

/// A handler that always 5xxs, recording each hit so a caller can count the
/// requests a single tool call actually made.
async fn always_500(State(log): State<AuthLog>) -> (StatusCode, axum::Json<Value>) {
    log.lock().unwrap().push("hit".to_string());
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(json!({ "success": false, "error": "upstream exploded" })),
    )
}

/// Spawn a backend whose every Composio route 5xxs.
async fn spawn_failing_backend() -> (String, AuthLog) {
    let log: AuthLog = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/agent-integrations/composio/toolkits", get(always_500))
        .route("/agent-integrations/composio/tools", get(always_500))
        .route("/agent-integrations/composio/connections", get(always_500))
        .with_state(log.clone());
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), log)
}

fn tool_named(config: &TenantComposio, name: &str) -> Box<dyn Tool> {
    let metering = ComposioMetering {
        company: CompanyId::new("acme"),
        agent: "ceo".to_string(),
        meter: None,
    };
    composio_tools(config, metering)
        .into_iter()
        .find(|t| t.name() == name)
        .unwrap_or_else(|| panic!("`{name}` tool present"))
}

/// A hard backend failure on the toolkit catalogue must surface as an error,
/// never as an empty-but-successful listing. The distinction is the whole
/// point: an agent told "no toolkits" concludes the company has connected
/// nothing and stops asking, while an agent told the catalogue could not be
/// read can say so and retry later.
#[tokio::test]
async fn list_toolkits_reports_a_backend_failure_rather_than_an_empty_catalogue() {
    let (url, _log) = spawn_failing_backend().await;
    let tool = tool_named(&config(&url, "token-a"), "composio_list_toolkits");

    let out = tool.execute(json!({})).await.unwrap();
    let text = out.output();
    assert!(
        out.is_error,
        "a 500 from the catalogue must be an error, not a listing: {text}"
    );
    assert!(
        !text.contains("token-a"),
        "the tenant token leaked into the failure text: {text}"
    );
}

/// The same contract on the action catalogue. `composio_list_tools` is what
/// an agent reads before it picks a slug, so an empty success here sends it
/// on to guess a slug that was never listed.
#[tokio::test]
async fn list_tools_reports_a_backend_failure_rather_than_an_empty_listing() {
    let (url, _log) = spawn_failing_backend().await;
    let tool = tool_named(&config(&url, "token-a"), "composio_list_tools");

    let out = tool
        .execute(json!({ "search": "send email" }))
        .await
        .unwrap();
    let text = out.output();
    assert!(
        out.is_error,
        "a 500 from the action catalogue must be an error, not a listing: {text}"
    );
    assert!(
        !text.contains("token-a"),
        "the tenant token leaked into the failure text: {text}"
    );
}

/// Cross-tenant isolation must hold on the failure path too. A backend that
/// 5xxs gives the tool nothing to render, and the one thing it must not do
/// is fall back to any other source of connections — the output carries no
/// account at all, and no other tenant's bearer was ever presented.
#[tokio::test]
async fn a_backend_failure_on_connections_yields_no_accounts_and_no_other_tenants_token() {
    let (url, log) = spawn_failing_backend().await;
    let tool = tool_named(&config(&url, "token-a"), "composio_list_connections");

    let out = tool.execute(json!({})).await.unwrap();
    let text = out.output();
    assert!(
        out.is_error,
        "a 500 on connections must be an error: {text}"
    );
    assert!(
        !text.contains("@example.com"),
        "a failed listing must render no account whatsoever: {text}"
    );
    assert!(
        !text.contains("token-a") && !text.contains("token-b"),
        "no bearer may appear in the failure text: {text}"
    );
    let seen = log.lock().unwrap().len();
    assert!(seen >= 1, "the call must actually have reached the backend");
}

/// A company that has configured no Composio credential at all must have
/// every tool refuse before the network, rather than calling the backend
/// unauthenticated and rendering whatever it returns.
#[tokio::test]
async fn an_absent_credential_refuses_every_tool_before_the_network() {
    let (url, log) = spawn_failing_backend().await;
    let config = TenantComposio::new(url, Credential::None, Vec::new());

    for name in [
        "composio_list_toolkits",
        "composio_list_connections",
        "composio_list_tools",
    ] {
        let out = tool_named(&config, name).execute(json!({})).await.unwrap();
        assert!(
            out.is_error,
            "`{name}` must refuse without a credential: {}",
            out.output()
        );
    }
    assert!(
        log.lock().unwrap().is_empty(),
        "no request may leave for a company that configured no credential"
    );
}

#[tokio::test]
async fn a_repeated_authorize_for_one_toolkit_is_deduped() {
    let log: AuthLog = Arc::new(Mutex::new(Vec::new()));
    async fn authorize(State(log): State<AuthLog>) -> axum::Json<Value> {
        log.lock().unwrap().push("authorize".to_string());
        axum::Json(json!({
            "success": true,
            "data": { "connectUrl": "https://connect.composio.dev/abc", "connectionId": "conn-1" }
        }))
    }
    let app = Router::new()
        .route("/agent-integrations/composio/authorize", post(authorize))
        .route(
            "/agent-integrations/composio/connections",
            get(async || {
                axum::Json(json!({
                    "success": true,
                    "data": { "connections": [
                        { "id": "conn-1", "toolkit": "gmail", "status": "INITIATED" }
                    ] }
                }))
            }),
        )
        .with_state(log.clone());
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let config = TenantComposio::new(
        format!("http://{addr}"),
        Credential::from_value("token-a"),
        vec!["gmail".to_string()],
    );
    let tool = tool_named(&config, "composio_authorize");

    let first = tool.execute(json!({ "toolkit": "gmail" })).await.unwrap();
    assert!(!first.is_error, "{}", first.output());
    let second = tool.execute(json!({ "toolkit": "GMAIL" })).await.unwrap();
    assert!(!second.is_error, "{}", second.output());

    assert_eq!(
        log.lock().unwrap().len(),
        1,
        "a repeat authorize for the same toolkit must not open a second handoff"
    );
}

/// Repeated managed executes carry the same backend idempotency key.
#[tokio::test]
async fn a_repeated_execute_carries_an_idempotency_key_the_backend_can_dedupe_on() {
    type BodyLog = Arc<Mutex<Vec<(Value, Option<String>)>>>;
    async fn execute(
        State(log): State<BodyLog>,
        headers: HeaderMap,
        axum::Json(body): axum::Json<Value>,
    ) -> axum::Json<Value> {
        let key = headers
            .get("idempotency-key")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        log.lock().unwrap().push((body, key));
        axum::Json(json!({
            "success": true,
            "data": { "successful": true, "data": { "id": "msg-1" } }
        }))
    }
    let log: BodyLog = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/agent-integrations/composio/execute", post(execute))
        .with_state(log.clone());
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let config = TenantComposio::new(
        format!("http://{addr}"),
        Credential::from_value("token-a"),
        vec!["gmail".to_string()],
    );
    let tool = tool_named(&config, "composio_execute");

    let args = json!({
        "tool": "GMAIL_SEND_EMAIL",
        "arguments": { "to": "ops@acme.test", "subject": "hi", "body": "hello" }
    });
    let _ = tool.execute(args.clone()).await.unwrap();
    let _ = tool.execute(args).await.unwrap();

    let seen = log.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "both calls must have reached the backend");
    let keys: Vec<Option<String>> = seen.iter().map(|(_, k)| k.clone()).collect();
    assert!(
        keys.iter().all(Option::is_some),
        "a side-effecting execute must carry an idempotency key: {keys:?}"
    );
    assert_eq!(
        keys[0], keys[1],
        "two identical executes must present the SAME key so the backend can dedupe"
    );
}
