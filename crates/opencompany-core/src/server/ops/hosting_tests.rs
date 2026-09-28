use super::*;

// These drive the real router rather than the helpers, because the
// properties worth holding are route-level: that a credential goes in and
// never comes back out, that clearing clears all of it, and that a provider
// this build cannot use is refused at the door rather than stored.

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::ports::types::CompanyId;

/// A running company with a manifest that grants `hosting`.
async fn state_with_company(home: &std::path::Path, grant_hosting: bool) -> AppState {
    use crate::ports::CompanyStore;
    use crate::ports::types::CompanyRecord;

    let id = CompanyId::new("acme");
    let allow = if grant_hosting {
        "\n[tools]\nallow = [\"hosting\"]\n"
    } else {
        ""
    };
    let manifest: crate::company::CompanyManifest = ::toml::from_str(&format!(
        "[company]\nname = \"Acme\"\n[[agent]]\nid = \"ceo\"\nrole = \"Chief\"\n[policy]\nmode = \"full\"\n{allow}"
    ))
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
async fn a_saved_token_is_reported_as_configured_and_never_returned() {
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path(), true).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    let (status, before) = call(
        &state,
        "GET",
        "/api/v1/companies/acme/hosting",
        &admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{before}");
    assert_eq!(before["apiKeyConfigured"], false);
    // The default provider is reported before anything is stored, so a
    // settings form can render its picker.
    assert_eq!(before["provider"], "vercel");
    assert_eq!(before["granted"], true);

    let (status, saved) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/hosting",
        &admin,
        Some(json!({
            "apiKey": "vercel_live_supersecret",
            "provider": "Vercel",
            "team": " team_abc ",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");

    let (_, after) = call(
        &state,
        "GET",
        "/api/v1/companies/acme/hosting",
        &admin,
        None,
    )
    .await;
    assert_eq!(after["apiKeyConfigured"], true);
    // The provider is normalised, and the team — the one non-secret field —
    // comes back trimmed.
    assert_eq!(after["provider"], "vercel");
    assert_eq!(after["team"], "team_abc");

    // The whole contract of this surface: it reports WHETHER a token is
    // stored, never what it is. A response that echoed it back would put it
    // in the browser, the network log, and any screen share.
    for rendered in [saved.to_string(), after.to_string()] {
        assert!(!rendered.contains("supersecret"), "{rendered}");
    }
}

#[tokio::test]
async fn a_provider_this_build_cannot_use_is_refused_rather_than_stored() {
    // Stored, it would leave the settings page reading "Connected" over a
    // slug that wires no tools at all.
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path(), true).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    let (status, body) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/hosting",
        &admin,
        Some(json!({"apiKey": "k", "provider": "heroku"})),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (_, after) = call(
        &state,
        "GET",
        "/api/v1/companies/acme/hosting",
        &admin,
        None,
    )
    .await;
    // The rejected request stored nothing at all — not even the key that
    // came with it.
    assert_eq!(after["apiKeyConfigured"], false, "{after}");
    assert_eq!(after["provider"], "vercel");
}

#[tokio::test]
async fn clearing_removes_the_team_too_not_just_the_token() {
    // The route is named `…/key`, which reads as if it clears only the
    // token. A team left behind would be silently inherited by the next
    // token saved without one — deploying into the previous account's scope.
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path(), true).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    call(
        &state,
        "PUT",
        "/api/v1/companies/acme/hosting",
        &admin,
        Some(json!({"apiKey": "vercel_key", "team": "team_abc"})),
    )
    .await;

    let (status, cleared) = call(
        &state,
        "DELETE",
        "/api/v1/companies/acme/hosting/key",
        &admin,
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert_eq!(cleared["apiKeyConfigured"], false);
    assert_eq!(cleared["team"], Value::Null, "{cleared}");
    assert_eq!(cleared["provider"], "vercel", "{cleared}");
}

#[tokio::test]
async fn a_stored_token_without_the_grant_reports_that_it_reaches_nobody() {
    // Both halves can be right and still nothing happens. The status says so
    // separately, because the fix is the manifest rather than this page.
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path(), false).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    call(
        &state,
        "PUT",
        "/api/v1/companies/acme/hosting",
        &admin,
        Some(json!({"apiKey": "vercel_key"})),
    )
    .await;

    let (_, after) = call(
        &state,
        "GET",
        "/api/v1/companies/acme/hosting",
        &admin,
        None,
    )
    .await;

    assert_eq!(after["apiKeyConfigured"], true);
    assert_eq!(after["granted"], false, "{after}");
}

#[tokio::test]
async fn saving_a_team_alone_leaves_the_token_alone() {
    // A patch, not a replace: an operator correcting a team must not have to
    // re-enter a token they can never see again.
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path(), true).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    call(
        &state,
        "PUT",
        "/api/v1/companies/acme/hosting",
        &admin,
        Some(json!({"apiKey": "vercel_key"})),
    )
    .await;
    let (_, after) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/hosting",
        &admin,
        Some(json!({"team": "team_xyz"})),
    )
    .await;

    assert_eq!(after["apiKeyConfigured"], true, "{after}");
    assert_eq!(after["team"], "team_xyz");
}
