use super::*;

use axum::body::{Body, to_bytes};
use axum::http::Request;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::ports::types::CompanyId;

async fn state_with_company(home: &std::path::Path) -> AppState {
    use crate::ports::CompanyStore;
    use crate::ports::types::CompanyRecord;

    let id = CompanyId::new("acme");
    let manifest: crate::company::CompanyManifest = ::toml::from_str(
        "[company]\nname = \"Acme\"\n[[agent]]\nid = \"ceo\"\nrole = \"Chief\"\n[policy]\nmode = \"full\"\n",
    )
    .expect("manifest");
    crate::store::FsCompanyStore::new(home.to_path_buf())
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_desk_hive: Vec::new(),
            id: id.clone(),
            manifest: manifest.clone(),
            ledger: Vec::new(),
            lifecycle: "running".to_string(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            overlay_agents: Vec::new(),
            overlay_desk_members: Vec::new(),
            overlay_desk_order: Vec::new(),
            overlay_tool_grants: None,
            overlay_desk_tools: Default::default(),
            overlay_desks: Vec::new(),
            overlay_workflows: Vec::new(),
            overlay_budgets: Vec::new(),
            overlay_policy: None,
            disabled_workflows: Vec::new(),
            template_provenance: None,
            setup: None,
            name_confirmed: false,
            activation_completed_at: None,
            created_at_millis: None,
        })
        .await
        .expect("save");

    let runtime = crate::runtime::RuntimeBuilder::new(home.to_path_buf(), manifest)
        .with_id(id.clone())
        .build()
        .await
        .expect("runtime");
    let state = AppState::new(crate::AppConfig::default());
    state.registry().insert(id, std::sync::Arc::new(runtime));
    state
}

async fn call(
    state: &AppState,
    method: &str,
    uri: &str,
    cookie: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", cookie);
    let request = match body {
        Some(body) => request
            .header("content-type", "application/json")
            .body(Body::from(body.to_string())),
        None => request.body(Body::empty()),
    }
    .expect("request");
    let response = crate::server::router(state.clone())
        .oneshot(request)
        .await
        .expect("routed");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Every read route, so a new one cannot be added without deciding what it
/// says on an unconfigured company.
const READS: [&str; 4] = [
    "/api/v1/companies/acme/finance/chargebee/invoices",
    "/api/v1/companies/acme/finance/chargebee/customers?email=alan@example.com",
    "/api/v1/companies/acme/finance/paypal/balance",
    "/api/v1/companies/acme/finance/paypal/transactions?since=2026-01-01T00:00:00Z&until=2026-01-08T00:00:00Z",
];

// --- The three failures stay three failures ---------------------------

#[tokio::test]
async fn an_unconfigured_company_is_a_409_naming_the_provider_not_a_500() {
    // The remedy is the connection panel on the page the operator is
    // already looking at, and a 500 sends them to the host logs instead.
    // Skipped on a build without the feature, where 501 is the right answer
    // and is asserted by its own test below.
    if !cfg!(feature = "chargebee") && !cfg!(feature = "paypal") {
        return;
    }
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path()).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    for uri in READS {
        let provider = if uri.contains("paypal") {
            "paypal"
        } else {
            "chargebee"
        };
        if (provider == "chargebee" && !cfg!(feature = "chargebee"))
            || (provider == "paypal" && !cfg!(feature = "paypal"))
        {
            continue;
        }
        let (status, answer) = call(&state, "GET", uri, &admin, None).await;
        assert_eq!(status, StatusCode::CONFLICT, "GET {uri}: {answer}");
        assert_eq!(answer["code"], "not_configured", "{uri}: {answer}");
        assert_eq!(answer["provider"], provider, "{uri}: {answer}");
    }
}

#[cfg(not(any(feature = "chargebee", feature = "paypal")))]
#[tokio::test]
async fn a_build_without_the_feature_answers_501_rather_than_404() {
    // A 404 reads as "wrong URL" and sends an operator looking for a typo.
    // 501 with `not_in_build` is what lets the console say "this host has no
    // PayPal support" — the reason every route is registered on every build.
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path()).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    for uri in READS {
        let (status, answer) = call(&state, "GET", uri, &admin, None).await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "GET {uri}: {answer}");
        assert_eq!(answer["code"], "not_in_build", "{uri}: {answer}");
    }
}

#[tokio::test]
async fn a_member_may_read_every_invoice_and_raise_none() {
    // The asymmetry this surface is built around: reading is not more
    // sensitive than the ledger Finance already shows a member, and raising
    // an invoice bills a real customer with no route here that undoes it.
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path()).await;
    let member = crate::server::test_support::seed_session(
        &state,
        "acme",
        crate::ports::users::UserRole::Member,
    )
    .await;

    // A member reaching the handler is what matters, not what the handler
    // then says: unconfigured (409) or not-in-build (501) both mean the
    // scope let them through. A 401/403 would mean it did not.
    for uri in READS {
        let (status, answer) = call(&state, "GET", uri, &member, None).await;
        assert!(
            status != StatusCode::FORBIDDEN && status != StatusCode::UNAUTHORIZED,
            "GET {uri} refused a member: {status} {answer}"
        );
    }

    let (status, answer) = call(
        &state,
        "POST",
        "/api/v1/companies/acme/finance/chargebee/invoices",
        &member,
        Some(json!({
            "customer_email": "alan@example.com",
            "currency_code": "USD",
            "line_items": [{"description": "Consulting", "amount_in_minor_units": 125000}],
        })),
    )
    .await;
    assert!(
        status == StatusCode::FORBIDDEN || status == StatusCode::UNAUTHORIZED,
        "a member raised an invoice: {status} {answer}"
    );
}

// --- Error classification ---------------------------------------------
//
// Unit tests on the rendering, because the three-way split is the whole
// contract of this module and a route test can only reach one arm at a time.

async fn rendered(error: FinanceError) -> (StatusCode, Value) {
    let response = error.into_response();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn a_provider_refusal_is_a_502_carrying_the_providers_own_code() {
    // The operator's next step is at Chargebee, and `configuration_incompatible`
    // is what tells them which setting. A generic 502 would hide it.
    let (status, body) = rendered(FinanceError::from_provider(
        "chargebee",
        crate::error::OpenCompanyError::Chargebee {
            status: 400,
            code: "configuration_incompatible".to_string(),
            message: "Currency GBP is not enabled for this site".to_string(),
        },
    ))
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["code"], "provider_error");
    assert_eq!(body["providerCode"], "configuration_incompatible");
    assert_eq!(body["providerStatus"], 400);
    assert_eq!(body["provider"], "chargebee");
    assert!(
        body["error"].as_str().expect("error").contains("GBP"),
        "the provider's own message must survive: {body}"
    );
}

#[tokio::test]
async fn an_argument_rejected_before_the_network_is_a_400_not_a_502() {
    // `status: 0` is the `api` layer saying the call never left the process.
    // Rendering that as a 502 would send an operator to check PayPal's
    // status page over a date range they can fix themselves — and PayPal's
    // rewritten window message is exactly the text they need to see.
    let (status, body) = rendered(FinanceError::from_provider(
        "paypal",
        crate::error::OpenCompanyError::Paypal {
            status: 0,
            code: "invalid_arguments".to_string(),
            message: "`start_date` and `end_date` are both required".to_string(),
        },
    ))
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_arguments");
    assert_eq!(body["providerStatus"], 0);
    assert!(
        body["error"]
            .as_str()
            .expect("error")
            .contains("start_date")
    );
}

#[tokio::test]
async fn a_store_failure_is_not_relabelled_as_the_providers_fault() {
    // A secret store that will not answer is a 500 here, as it is
    // everywhere else. Calling it `provider_error` would blame Chargebee for
    // a local fault and send the operator to the wrong status page.
    let (status, body) = rendered(FinanceError::from_provider(
        "chargebee",
        crate::error::OpenCompanyError::Store("disk full".to_string()),
    ))
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_ne!(body["code"], "provider_error");
}

#[tokio::test]
async fn not_in_build_and_not_configured_are_different_answers() {
    // One means "get a different build", the other "fill in this form".
    // A surface that merged them would offer the form on a host that cannot
    // use it.
    let (status, body) = rendered(FinanceError::NotInBuild { provider: "paypal" }).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(body["code"], "not_in_build");

    let (status, body) = rendered(FinanceError::NotConfigured { provider: "paypal" }).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "not_configured");
}

// --- The projection reaches the wire unchanged ------------------------

/// A one-route Chargebee stub, so the happy path exercises the real client,
/// the real `api` projection and the real serialization.
#[cfg(feature = "chargebee")]
async fn stub_chargebee(path: &'static str, reply: Value) -> crate::chargebee::ChargebeeClient {
    use crate::chargebee::types::ChargebeeConfig;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let app = axum::Router::new().route(
        path,
        axum::routing::get(move || async move { axum::Json(reply) }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    crate::chargebee::ChargebeeClient::with_base_url(
        ChargebeeConfig {
            site: "acme-test".to_string(),
            api_key: "cb_test_key".to_string(),
        },
        format!("http://{addr}"),
    )
    .expect("client")
}

#[cfg(feature = "chargebee")]
#[tokio::test]
async fn an_invoice_list_reaches_the_console_in_minor_units_and_snake_case() {
    // Both halves matter. The unit, because a console that received a field
    // called plain `total` would render $1,250.00 as $125,000.00 or the
    // reverse and nothing on the wire would say which. The case, because it
    // is the same shape the agent sees from `chargebee_list_invoices` — one
    // invoice, one vocabulary, whether it is read in chat or in the console.
    let client = stub_chargebee(
        "/invoices",
        json!({
            "list": [{
                "invoice": {
                    "id": "INV-0042",
                    "customer_id": "cus_7",
                    "status": "paid",
                    "currency_code": "USD",
                    "total": 125000,
                    "amount_due": 0,
                    "amount_paid": 125000,
                    "line_items": [{"description": "Consulting"}],
                }
            }]
        }),
    )
    .await;

    let response = chargebee::list_invoices_on(&client, InvoiceQuery::default())
        .await
        .expect("listed");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let body: Value = serde_json::from_slice(&bytes).expect("json");

    assert_eq!(body[0]["id"], "INV-0042");
    assert_eq!(body[0]["total_in_minor_units"], 125000);
    assert_eq!(body[0]["amount_due_in_minor_units"], 0);
    assert_eq!(body[0]["status"], "paid");
    assert!(
        body[0].get("total").is_none(),
        "a bare `total` would hide the unit: {body}"
    );
}

#[cfg(feature = "chargebee")]
#[tokio::test]
async fn a_connection_test_names_the_site_that_actually_answered() {
    // "Connected ✓" against the wrong site is the confusion the whole
    // status surface exists to avoid, and a test result that said only
    // "OK" would reintroduce it here.
    let client = stub_chargebee("/invoices", json!({ "list": [] })).await;
    let result = chargebee::test_on(&client, "acme-test").await.expect("ok");
    assert!(result.ok);
    assert!(
        result.detail.contains("acme-test"),
        "the site must be named: {}",
        result.detail
    );
}
