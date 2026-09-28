use super::*;

// Route-level, like the hosting tests beside them: the properties worth
// holding are that a key goes in and never comes back out, that an
// incomplete selection reports itself as still on managed search, and that
// an unsupported provider is refused at the door rather than stored.

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::ports::types::CompanyId;

/// A running company whose manifest grants `search` (or does not).
async fn state_with_company(home: &std::path::Path, grant_search: bool) -> AppState {
    use crate::ports::CompanyStore;
    use crate::ports::types::CompanyRecord;

    let id = CompanyId::new("acme");
    // Both arms state `[tools]` explicitly. Leaving the ungranted arm
    // empty used to mean "no grant", but the global default belt carries
    // `search` now, so an absent section is a company that *does* grant it
    // — and the ungranted test would have been asserting the opposite of
    // what it set up.
    let allow = if grant_search {
        "\n[tools]\nallow = [\"search\"]\n"
    } else {
        "\n[tools]\nallow = [\"*\"]\n"
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
async fn a_legacy_patch_that_fails_validation_changes_nothing() {
    // The provider-less `PUT …/search` wrote the key before validating the
    // address. A request carrying a new key and an invalid address answered
    // 400 with the credential already replaced — so a client that treated
    // the failed patch as unapplied was searching with a key it believed it
    // never set.
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path(), true).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    // Connected and selected, with no key yet — so any key write is visible.
    call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search",
        &admin,
        Some(json!({"provider": "exa"})),
    )
    .await;

    let (status, body) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search",
        &admin,
        Some(json!({"apiKey": "exa-not-a-real-key", "endpoint": "not a url"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (_, after) = call(&state, "GET", "/api/v1/companies/acme/search", &admin, None).await;
    let exa = after["providers"]
        .as_array()
        .expect("providers")
        .iter()
        .find(|row| row["slug"] == "exa")
        .expect("exa row")
        .clone();
    assert_eq!(
        exa["keyConfigured"], false,
        "a rejected patch must not have stored its key: {after}"
    );
}

#[tokio::test]
async fn a_hostile_slug_never_reaches_the_secret_store() {
    // Credential addresses are `search/provider/<slug>/key`, so a slug
    // carrying a slash or an unbounded run of text is a slug that writes
    // somewhere other than where it claims.
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path(), true).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    for slug in ["%2e%2e%2fexa", &"x".repeat(300), "%E6%A4%9C%E7%B4%A2"] {
        let (status, body) = call(
            &state,
            "DELETE",
            &format!("/api/v1/companies/acme/search/providers/{slug}"),
            &admin,
            None,
        )
        .await;
        assert!(
            !status.is_success(),
            "a slug that could address another provider's credential must never \
             succeed — axum may refuse it at the router before the handler, which is \
             equally fine: {slug}: {body}"
        );
    }
}

// ── in-use guards (#2306): confirmInUse gates a disable/remove/key-clear
//    of the search default, and the marker survives every one of them ──

/// Connects `exa` and `brave`, and marks `exa` as the default. Returns the
/// state and admin cookie, ready for a guarded mutation on `exa`.
///
/// Goes through the legacy `PUT …/search` route (`select_provider`)
/// rather than `POST …/search/providers` (`connect_provider`): the modern
/// connect route always probes the provider over the network
/// (`check`/`probe::probe`), which a fake key like `exa-not-a-real-key`
/// would fail as an `Auth`-class rejection and roll back — exactly the
/// network dependency the existing tests in this module avoid by using
/// this same legacy route with fake keys. `select_provider` writes the
/// row and marks it default with no probe, and calling it once per slug,
/// **exa last**, connects both and leaves `exa` as the final default
/// (each call unconditionally re-marks the named provider — see
/// `switching_providers_does_not_hand_one_providers_key_to_another`
/// above for the same ordering trick).
async fn state_with_two_providers_exa_default(home: &std::path::Path) -> (AppState, String) {
    let state = state_with_company(home, true).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;
    for (slug, key) in [
        ("brave", "brave-not-a-real-key"),
        ("exa", "exa-not-a-real-key"),
    ] {
        call(
            &state,
            "PUT",
            "/api/v1/companies/acme/search",
            &admin,
            Some(json!({"provider": slug, "apiKey": key})),
        )
        .await;
    }
    (state, admin)
}

#[tokio::test]
async fn disabling_the_search_default_is_refused_without_confirmation() {
    let home = ::tempfile::tempdir().expect("tempdir");
    let (state, admin) = state_with_two_providers_exa_default(home.path()).await;

    let (status, body) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search/providers/exa",
        &admin,
        Some(json!({"enabled": false})),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "in_use", "{body}");
    assert_eq!(body["error"], "Exa is the search default.", "{body}");
    assert_eq!(body["usedBy"]["default"], true, "{body}");
    assert!(body["usedBy"]["agents"].is_null(), "{body}");
    assert!(body["usedBy"]["surfaces"].is_null(), "{body}");

    // Refused means unchanged: still enabled, still the default.
    let (_, after) = call(&state, "GET", "/api/v1/companies/acme/search", &admin, None).await;
    let exa = after["providers"]
        .as_array()
        .expect("providers")
        .iter()
        .find(|row| row["slug"] == "exa")
        .expect("exa row");
    assert_eq!(exa["enabled"], true, "{after}");
}

#[tokio::test]
async fn disabling_the_search_default_with_confirmation_succeeds_and_keeps_the_marker() {
    let home = ::tempfile::tempdir().expect("tempdir");
    let (state, admin) = state_with_two_providers_exa_default(home.path()).await;

    let (status, after) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search/providers/exa",
        &admin,
        Some(json!({"enabled": false, "confirmInUse": true})),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{after}");
    let exa = after["providers"]
        .as_array()
        .expect("providers")
        .iter()
        .find(|row| row["slug"] == "exa")
        .expect("exa row");
    assert_eq!(
        exa["enabled"], false,
        "the row is actually disabled: {after}"
    );
    // X14: the marker is never rewritten, so the row's own `usedBy` still
    // reports it, echoing what the refusal above carried.
    assert_eq!(exa["usedBy"]["default"], true, "{after}");
    // And the resolved view degrades gracefully — `brave` takes over,
    // rather than nothing answering.
    assert_eq!(after["effectiveProvider"], "brave", "{after}");
    // The status banner names what happened.
    assert_eq!(
        after["defaultNotice"],
        "The search default uses Exa, which is turned off. Choose a new search \
         default in Connections \u{2192} API Keys \u{2192} Search.",
        "{after}"
    );
}

#[tokio::test]
async fn removing_the_search_default_is_refused_without_confirmation_and_succeeds_with_it() {
    let home = ::tempfile::tempdir().expect("tempdir");
    let (state, admin) = state_with_two_providers_exa_default(home.path()).await;

    let (status, body) = call(
        &state,
        "DELETE",
        "/api/v1/companies/acme/search/providers/exa",
        &admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "in_use", "{body}");
    assert_eq!(body["usedBy"]["default"], true, "{body}");

    let (status, after) = call(
        &state,
        "DELETE",
        "/api/v1/companies/acme/search/providers/exa?confirmInUse=true",
        &admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert!(
        after["providers"]
            .as_array()
            .expect("providers")
            .iter()
            .all(|row| row["slug"] != "exa"),
        "exa's row is gone: {after}"
    );
    // X14: the marker survives even though nothing now answers to it.
    assert_eq!(after["effectiveProvider"], "brave", "{after}");
    assert_eq!(
        after["defaultNotice"],
        "The search default uses Exa, which is removed. Choose a new search \
         default in Connections \u{2192} API Keys \u{2192} Search.",
        "{after}"
    );
}

#[tokio::test]
async fn clearing_the_search_defaults_key_is_refused_without_confirmation_and_succeeds_with_it() {
    let home = ::tempfile::tempdir().expect("tempdir");
    let (state, admin) = state_with_two_providers_exa_default(home.path()).await;

    let (status, body) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search/providers/exa/key",
        &admin,
        Some(json!({"apiKey": ""})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["usedBy"]["default"], true, "{body}");

    let (status, after) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search/providers/exa/key",
        &admin,
        Some(json!({"apiKey": "", "confirmInUse": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{after}");
    let exa = after["providers"]
        .as_array()
        .expect("providers")
        .iter()
        .find(|row| row["slug"] == "exa")
        .expect("exa row");
    assert_eq!(exa["keyConfigured"], false, "{after}");
    // A key-less exa is not complete, so managed — not exa — resolves,
    // but the row is still enabled and still the marked default.
    assert_eq!(exa["enabled"], true, "{after}");
    assert_eq!(exa["usedBy"]["default"], true, "{after}");
}

/// A rotate (a non-empty replacement key) is never guarded — it keeps
/// serving whatever already depended on it — matching
/// `docs/key-reworks/in-use-guards.md` §2's rotate carve-out.
#[tokio::test]
async fn rotating_the_search_defaults_key_needs_no_confirmation() {
    let home = ::tempfile::tempdir().expect("tempdir");
    let (state, admin) = state_with_two_providers_exa_default(home.path()).await;

    let (status, after) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search/providers/exa/key",
        &admin,
        Some(json!({"apiKey": "exa-rotated-not-a-real-key"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{after}");
}

#[tokio::test]
async fn disabling_or_removing_a_provider_that_is_not_the_default_needs_no_confirmation() {
    let home = ::tempfile::tempdir().expect("tempdir");
    let (state, admin) = state_with_two_providers_exa_default(home.path()).await;

    let (status, after) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search/providers/brave",
        &admin,
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{after}");
    let brave = after["providers"]
        .as_array()
        .expect("providers")
        .iter()
        .find(|row| row["slug"] == "brave")
        .expect("brave row");
    assert!(brave.get("usedBy").is_none(), "{after}");

    let (status, after) = call(
        &state,
        "DELETE",
        "/api/v1/companies/acme/search/providers/brave",
        &admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{after}");
}

// ── disconnect-all (`DELETE …/search/key`): the same guard, applied in
//    bulk (item 1, keys rework #2306 review) ──

#[tokio::test]
async fn disconnect_all_is_refused_without_confirmation_when_a_default_is_set() {
    let home = ::tempfile::tempdir().expect("tempdir");
    let (state, admin) = state_with_two_providers_exa_default(home.path()).await;

    let (status, body) = call(
        &state,
        "DELETE",
        "/api/v1/companies/acme/search/key",
        &admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "in_use", "{body}");
    assert_eq!(body["error"], "Exa is the search default.", "{body}");
    assert_eq!(body["usedBy"]["default"], true, "{body}");
    assert!(body["usedBy"]["agents"].is_null(), "{body}");
    assert!(body["usedBy"]["surfaces"].is_null(), "{body}");

    // Refused, so nothing was touched: both rows survive.
    let (_, after) = call(&state, "GET", "/api/v1/companies/acme/search", &admin, None).await;
    let slugs: Vec<&str> = after["providers"]
        .as_array()
        .expect("providers")
        .iter()
        .map(|row| row["slug"].as_str().unwrap())
        .collect();
    assert!(slugs.contains(&"exa"), "{after}");
    assert!(slugs.contains(&"brave"), "{after}");
}

#[tokio::test]
async fn disconnect_all_confirmed_clears_every_provider_but_keeps_the_default_marker() {
    let home = ::tempfile::tempdir().expect("tempdir");
    let (state, admin) = state_with_two_providers_exa_default(home.path()).await;

    let (status, after) = call(
        &state,
        "DELETE",
        "/api/v1/companies/acme/search/key?confirmInUse=true",
        &admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert!(
        after["providers"].as_array().expect("providers").is_empty(),
        "every row is gone: {after}"
    );
    // X14: the marker is never cleared, even by a confirmed disconnect-all
    // — `search/default` still names Exa, and the status banner says so.
    assert_eq!(after["effectiveProvider"], "managed", "{after}");
    assert_eq!(
        after["defaultNotice"],
        "The search default uses Exa, which is removed. Choose a new search \
         default in Connections \u{2192} API Keys \u{2192} Search.",
        "{after}"
    );
}

#[tokio::test]
async fn disconnect_all_needs_no_confirmation_when_nothing_is_marked() {
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path(), true).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    // Nothing connected, nothing marked — there is nothing a disconnect-all
    // could strand.
    let (status, after) = call(
        &state,
        "DELETE",
        "/api/v1/companies/acme/search/key",
        &admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert!(after.get("defaultNotice").is_none(), "{after}");
}

#[tokio::test]
async fn no_default_notice_when_nothing_is_marked_or_the_marked_provider_is_healthy() {
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path(), true).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    // Nothing connected, nothing marked.
    let (_, unconfigured) =
        call(&state, "GET", "/api/v1/companies/acme/search", &admin, None).await;
    assert!(
        unconfigured.get("defaultNotice").is_none(),
        "{unconfigured}"
    );

    // Connected, marked, enabled and complete. The legacy `PUT …/search`
    // route (`select_provider`), not `POST …/search/providers`, which
    // probes over the network — see `state_with_two_providers_exa_default`
    // above for why that would be flaky with a fake key.
    call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search",
        &admin,
        Some(json!({"provider": "exa", "apiKey": "exa-not-a-real-key"})),
    )
    .await;
    let (_, healthy) = call(&state, "GET", "/api/v1/companies/acme/search", &admin, None).await;
    assert!(healthy.get("defaultNotice").is_none(), "{healthy}");
}
