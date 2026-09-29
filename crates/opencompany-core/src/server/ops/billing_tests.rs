use super::*;

// --- The routes, end to end -------------------------------------------
//
// The unit tests below cover the helpers. These drive the real router,
// because the properties worth holding are route-level: that a credential
// goes in and never comes back out, that clearing clears ALL of it, and
// that a member cannot read or write another role's billing settings.

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

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
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            id: id.clone(),
            manifest: manifest.clone(),
            ledger: Vec::new(),
            lifecycle: "running".to_string(),
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
    // `seed_admin` / `seed_session` hand back a ready `Cookie` header value —
    // these routes authenticate a signed-in human, not a bearer token.
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

#[tokio::test]
async fn a_saved_credential_is_reported_as_configured_and_never_returned() {
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path()).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    // Nothing stored yet.
    let (status, before) = call(
        &state,
        "GET",
        "/api/v1/companies/acme/billing/chargebee",
        &admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{before}");
    assert_eq!(before["apiKeyConfigured"], false);
    assert_eq!(before["webhookConfigured"], false);

    let (status, saved) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/billing/chargebee",
        &admin,
        Some(json!({
            "apiKey": "cb_live_supersecret",
            "site": "https://acme-test.chargebee.com",
            "webhookSecret": "cbuser:cbpass",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");

    // The whole contract of this surface: it reports WHETHER a credential
    // is stored, never what it is. A response that echoed the key back
    // would put it in the browser, the network log and any screen share.
    let (_, after) = call(
        &state,
        "GET",
        "/api/v1/companies/acme/billing/chargebee",
        &admin,
        None,
    )
    .await;
    assert_eq!(after["apiKeyConfigured"], true);
    assert_eq!(after["webhookConfigured"], true);
    // The site is the one NON-secret field, and comes back normalised.
    assert_eq!(after["site"], "acme-test");
    for rendered in [saved.to_string(), after.to_string()] {
        assert!(!rendered.contains("cb_live_supersecret"), "{rendered}");
        assert!(!rendered.contains("cbpass"), "{rendered}");
    }
}

#[tokio::test]
async fn clearing_removes_the_webhook_secret_too_not_just_the_key() {
    // The route is named `…/key`, which reads as if it clears only the API
    // key — leaving a webhook credential behind would keep the endpoint
    // live while the UI reported the integration as cleared.
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path()).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    call(
        &state,
        "PUT",
        "/api/v1/companies/acme/billing/chargebee",
        &admin,
        Some(json!({
            "apiKey": "cb_key",
            "site": "acme-test",
            "webhookSecret": "cbuser:cbpass",
        })),
    )
    .await;
    let (status, cleared) = call(
        &state,
        "DELETE",
        "/api/v1/companies/acme/billing/chargebee/key",
        &admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert_eq!(cleared["apiKeyConfigured"], false);
    assert_eq!(cleared["webhookConfigured"], false, "{cleared}");
    assert_eq!(
        cleared["site"],
        Value::Null,
        "the site is cleared as well: {cleared}"
    );
}

#[tokio::test]
async fn paypal_clears_its_environment_so_a_reconnect_starts_at_sandbox() {
    // Inheriting `live` from a previous account is the failure worth
    // preventing here: the next connection would read real money.
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path()).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    call(
        &state,
        "PUT",
        "/api/v1/companies/acme/billing/paypal",
        &admin,
        Some(json!({
            "clientId": "AY_id",
            "clientSecret": "EL_secret",
            "environment": "live",
        })),
    )
    .await;
    let (_, live) = call(
        &state,
        "GET",
        "/api/v1/companies/acme/billing/paypal",
        &admin,
        None,
    )
    .await;
    assert_eq!(live["environment"], "live");
    assert_eq!(live["clientSecretConfigured"], true);
    assert!(!live.to_string().contains("EL_secret"), "{live}");

    let (status, cleared) = call(
        &state,
        "DELETE",
        "/api/v1/companies/acme/billing/paypal/key",
        &admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert_eq!(cleared["clientIdConfigured"], false);
    assert_eq!(cleared["clientSecretConfigured"], false);
    assert_eq!(cleared["environment"], "sandbox", "{cleared}");
}

/// An in-memory secret store whose `set` refuses one nominated key.
///
/// The failure worth simulating is a store that works, then stops working
/// mid-batch — a `set` that times out, a full disk, a dropped Mongo
/// connection. A store that fails everything would never get far enough to
/// leave the half-configured state.
#[derive(Default)]
struct FailsOnOneKey {
    refuse: &'static str,
    stored: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

#[async_trait::async_trait]
impl crate::ports::SecretStore for FailsOnOneKey {
    async fn get(
        &self,
        _company: &CompanyId,
        key: &str,
    ) -> crate::error::Result<Option<SecretValue>> {
        Ok(self
            .stored
            .lock()
            .expect("lock")
            .get(key)
            .map(|value| SecretValue(value.clone())))
    }

    async fn set(
        &self,
        _company: &CompanyId,
        key: &str,
        value: SecretValue,
    ) -> crate::error::Result<()> {
        if key == self.refuse {
            return Err(crate::error::OpenCompanyError::Store(
                "the secret store went away mid-write".into(),
            ));
        }
        self.stored
            .lock()
            .expect("lock")
            .insert(key.to_string(), value.0);
        Ok(())
    }
}

/// A host whose company's secret store refuses to write `refuse`.
async fn state_with_failing_secrets(home: &std::path::Path, refuse: &'static str) -> AppState {
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
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            id: id.clone(),
            manifest: manifest.clone(),
            ledger: Vec::new(),
            lifecycle: "running".to_string(),
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

    let secrets = std::sync::Arc::new(FailsOnOneKey {
        refuse,
        ..Default::default()
    });
    let runtime = crate::runtime::RuntimeBuilder::new(home.to_path_buf(), manifest)
        .with_id(id.clone())
        .with_secrets(secrets)
        .build()
        .await
        .expect("runtime");
    let state = AppState::new(crate::AppConfig::default());
    state.registry().insert(id, std::sync::Arc::new(runtime));
    state
}

#[tokio::test]
async fn a_save_that_fails_part_way_stores_nothing_at_all() {
    // The module header claims a half-configured company is impossible to
    // express by accident. Written one `?` at a time it was not: a store
    // that took the API key and then failed on the webhook credential
    // answered the operator with an error while keeping the key. The
    // credential pair is meaningless apart, so "the save failed" and "the
    // key is stored" must not both be true.
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_failing_secrets(home.path(), WEBHOOK_SECRET_KEY).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    let (status, answer) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/billing/chargebee",
        &admin,
        Some(json!({
            "apiKey": "cb_live_supersecret",
            "site": "acme-test",
            "webhookSecret": "cbuser:cbpass",
        })),
    )
    .await;
    assert!(
        status.is_server_error() || status.is_client_error(),
        "a failed write must not answer OK: {status} {answer}"
    );

    // Read the store directly. Going through `GET` would prove only that the
    // status agrees with itself; what matters is that nothing was left on
    // disk for the next request — or the next agent turn — to pick up.
    let runtime = state
        .registry()
        .get(&CompanyId::new("acme"))
        .expect("company");
    for key in [API_KEY_SECRET, SITE_SECRET, WEBHOOK_SECRET_KEY] {
        let stored = runtime
            .secrets()
            .get(runtime.id(), key)
            .await
            .expect("read secret");
        assert!(
            stored
                .as_ref()
                .is_none_or(|value| value.expose().is_empty()),
            "{key} survived a failed save: {stored:?}"
        );
    }
}

#[tokio::test]
async fn a_failed_paypal_save_does_not_leave_half_a_credential() {
    // Same rule on the PayPal side, where half a credential is worse than
    // none: a client id with no secret cannot obtain a token, so the tools
    // fail on first use rather than never being wired.
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_failing_secrets(home.path(), CLIENT_SECRET_SECRET).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    let (status, answer) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/billing/paypal",
        &admin,
        Some(json!({
            "clientId": "AY_id",
            "clientSecret": "EL_secret",
            "environment": "live",
        })),
    )
    .await;
    assert!(
        status.is_server_error() || status.is_client_error(),
        "a failed write must not answer OK: {status} {answer}"
    );

    let runtime = state
        .registry()
        .get(&CompanyId::new("acme"))
        .expect("company");
    for key in [CLIENT_ID_SECRET, CLIENT_SECRET_SECRET, ENVIRONMENT_SECRET] {
        let stored = runtime
            .secrets()
            .get(runtime.id(), key)
            .await
            .expect("read secret");
        assert!(
            stored
                .as_ref()
                .is_none_or(|value| value.expose().is_empty()),
            "{key} survived a failed save: {stored:?}"
        );
    }
}

#[tokio::test]
async fn a_rolled_back_save_restores_what_was_there_before() {
    // Rollback restores the PRIOR value, not "empty". An operator correcting
    // a site who hits a store failure must still have the connection they
    // had before they touched the form.
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_failing_secrets(home.path(), WEBHOOK_SECRET_KEY).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;
    let runtime = state
        .registry()
        .get(&CompanyId::new("acme"))
        .expect("company");

    // A working connection, stored directly so the failing key stays out of it.
    for (key, value) in [(API_KEY_SECRET, "cb_original"), (SITE_SECRET, "acme-test")] {
        runtime
            .secrets()
            .set(runtime.id(), key, SecretValue(value.to_string()))
            .await
            .expect("seed");
    }

    let (status, _) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/billing/chargebee",
        &admin,
        Some(json!({
            "apiKey": "cb_replacement",
            "site": "acme-live",
            "webhookSecret": "cbuser:cbpass",
        })),
    )
    .await;
    assert!(!status.is_success(), "the save failed: {status}");

    for (key, expected) in [(API_KEY_SECRET, "cb_original"), (SITE_SECRET, "acme-test")] {
        let stored = runtime
            .secrets()
            .get(runtime.id(), key)
            .await
            .expect("read secret")
            .map(|value| value.expose().to_string());
        assert_eq!(
            stored.as_deref(),
            Some(expected),
            "{key} was not restored to what it was before the failed save"
        );
    }
}

#[tokio::test]
async fn a_member_may_read_the_status_but_never_write_a_credential() {
    // The split is deliberate. `GET` carries no secret — booleans, the site
    // slug, the webhook URL — so a member seeing "not connected" is how they
    // know to ask an admin. Writing is another matter: a member who could
    // `PUT` here would point the company's invoicing at a Chargebee site
    // they control, and one who could `DELETE` could silently stop every
    // payment notification.
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path()).await;
    let member = crate::server::test_support::seed_session(
        &state,
        "acme",
        crate::ports::users::UserRole::Member,
    )
    .await;

    for uri in [
        "/api/v1/companies/acme/billing/chargebee",
        "/api/v1/companies/acme/billing/paypal",
    ] {
        let (status, answer) = call(&state, "GET", uri, &member, None).await;
        assert_eq!(status, StatusCode::OK, "GET {uri}: {answer}");
    }

    for (method, uri, body) in [
        (
            "PUT",
            "/api/v1/companies/acme/billing/chargebee",
            Some(json!({"apiKey": "cb_key", "site": "attacker-site"})),
        ),
        (
            "PUT",
            "/api/v1/companies/acme/billing/paypal",
            Some(json!({"clientId": "AY_id", "clientSecret": "EL_secret"})),
        ),
        (
            "DELETE",
            "/api/v1/companies/acme/billing/chargebee/key",
            None,
        ),
        ("DELETE", "/api/v1/companies/acme/billing/paypal/key", None),
    ] {
        let (status, answer) = call(&state, method, uri, &member, body).await;
        assert!(
            status == StatusCode::FORBIDDEN || status == StatusCode::UNAUTHORIZED,
            "{method} {uri} answered {status}: {answer}"
        );
    }

    // And nothing the member attempted was written. Read the store
    // directly rather than through another principal: the refusals above
    // are only worth having if they refused the WRITE, not merely the
    // response.
    let runtime = state
        .registry()
        .get(&CompanyId::new("acme"))
        .expect("company");
    for key in [
        API_KEY_SECRET,
        SITE_SECRET,
        WEBHOOK_SECRET_KEY,
        CLIENT_ID_SECRET,
        CLIENT_SECRET_SECRET,
        ENVIRONMENT_SECRET,
    ] {
        let stored = runtime
            .secrets()
            .get(runtime.id(), key)
            .await
            .expect("read secret");
        assert!(stored.is_none(), "{key} was written by a member");
    }
}

#[test]
fn a_site_is_normalized_from_every_shape_an_operator_pastes() {
    for raw in [
        "acme-test",
        " acme-test ",
        "acme-test.chargebee.com",
        "https://acme-test.chargebee.com",
        "https://acme-test.chargebee.com/",
        "http://acme-test.chargebee.com/",
    ] {
        assert_eq!(normalize_site(raw), "acme-test", "from {raw:?}");
    }
}

#[test]
fn an_empty_site_stays_empty_rather_than_becoming_a_url_fragment() {
    assert_eq!(normalize_site(""), "");
    assert_eq!(normalize_site("   "), "");
    assert_eq!(normalize_site("https://"), "");
}

#[test]
fn status_never_serializes_a_credential() {
    // The whole contract of this module: whatever else changes, no field
    // here may carry the key. Asserted on the serialized form, because that
    // is what actually reaches a browser.
    let status = BillingStatus {
        api_key_configured: true,
        site: Some("acme-test".to_string()),
        webhook_configured: true,
        webhook_url: Some("https://oc.example/hooks/acme/chargebee".to_string()),
        granted: true,
        in_build: true,
    };
    let json = serde_json::to_string(&status).expect("serializes");
    assert!(json.contains("apiKeyConfigured"));
    assert!(json.contains("acme-test"));
    // No field may be named in a way that could carry the secret itself.
    assert!(!json.contains("apiKey\""), "{json}");
    assert!(!json.contains("webhookSecret"), "{json}");
}

#[test]
fn a_paypal_status_never_serializes_a_credential() {
    let status = PaypalStatus {
        client_id_configured: true,
        client_secret_configured: true,
        environment: "sandbox".to_string(),
        granted: true,
        in_build: true,
    };
    let json = serde_json::to_string(&status).expect("serializes");
    assert!(json.contains("clientIdConfigured"));
    assert!(json.contains("sandbox"));
    // No field may carry either half of the credential itself.
    assert!(!json.contains("clientId\""), "{json}");
    assert!(!json.contains("clientSecret\""), "{json}");
}

#[test]
fn the_three_failure_modes_stay_distinguishable() {
    // Credentials present, grant missing: the operator's remedy is the
    // manifest, not the settings form. Collapsing these into one
    // "connected" boolean is what sends them to the wrong place.
    let configured_but_ungranted = BillingStatus {
        api_key_configured: true,
        site: Some("acme-test".to_string()),
        webhook_configured: false,
        webhook_url: None,
        granted: false,
        in_build: true,
    };
    assert!(configured_but_ungranted.api_key_configured);
    assert!(!configured_but_ungranted.granted);
    assert!(!configured_but_ungranted.webhook_configured);
}
