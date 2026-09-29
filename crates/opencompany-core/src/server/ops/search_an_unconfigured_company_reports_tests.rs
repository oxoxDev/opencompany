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
async fn an_unconfigured_company_reports_managed_search() {
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path(), true).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    let (status, body) = call(&state, "GET", "/api/v1/companies/acme/search", &admin, None).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["provider"], "managed");
    assert_eq!(body["effectiveProvider"], "managed");
    assert_eq!(body["apiKeyConfigured"], false);
    assert_eq!(body["needsApiKey"], false);
    assert_eq!(body["granted"], true);
    assert!(
        body["supportedProviders"]
            .as_array()
            .expect("providers")
            .contains(&json!("exa")),
        "{body}"
    );
}

/// `status_of`'s own comment says a company record that cannot be loaded
/// reports `granted: false` rather than failing the whole status — "the
/// operator still needs to see what IS configured, and a settings page
/// that 500s tells them nothing." That fallback only runs when
/// `store().load()` actually errors, which an absent record does not do
/// (`Ok(None)`, not `Err`) — so this corrupts the on-disk manifest after
/// the company has already booted, forcing a real `FsCompanyStore::load`
/// failure on the route's own re-read rather than mocking the store.
#[tokio::test]
async fn a_company_that_fails_to_load_reports_ungranted_instead_of_500() {
    let home_dir = ::tempfile::tempdir().expect("tempdir");
    let home = home_dir.path();
    let state = state_with_company(home, true).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    // Baseline: the manifest grants `search`, so the route reports it.
    let (status, body) = call(&state, "GET", "/api/v1/companies/acme/search", &admin, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["granted"], true);

    let toml_path = crate::store::Bundle::new(home, &CompanyId::new("acme")).company_toml();
    tokio::fs::write(&toml_path, b"not valid toml [[[")
        .await
        .expect("corrupt company.toml");

    let (status, body) = call(&state, "GET", "/api/v1/companies/acme/search", &admin, None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a store-load failure must still answer with a status, not a 500: {body}"
    );
    assert_eq!(
        body["granted"], false,
        "an unreadable record must fall back to ungranted rather than keep reporting the \
         last-known grant: {body}"
    );
}

#[tokio::test]
async fn a_saved_key_is_reported_as_configured_and_never_returned() {
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path(), true).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    let (status, saved) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search",
        &admin,
        Some(json!({"provider": "Exa", "apiKey": "exa_supersecret"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");

    let (_, after) = call(&state, "GET", "/api/v1/companies/acme/search", &admin, None).await;
    // The slug is normalised, and the company now searches through its own
    // account rather than the platform's.
    assert_eq!(after["provider"], "exa");
    assert_eq!(after["effectiveProvider"], "exa");
    assert_eq!(after["apiKeyConfigured"], true);

    // The whole contract of this surface: it reports WHETHER a key is
    // stored, never what it is.
    for rendered in [saved.to_string(), after.to_string()] {
        assert!(!rendered.contains("supersecret"), "{rendered}");
    }
}

/// The distinction the page exists to make: a selected provider with no key
/// is not a connection, and the agents are still on managed search.
#[tokio::test]
async fn a_provider_selected_without_its_key_still_reports_managed_as_effective() {
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path(), true).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    let (_, body) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search",
        &admin,
        Some(json!({"provider": "brave"})),
    )
    .await;

    assert_eq!(body["provider"], "brave", "{body}");
    assert_eq!(body["effectiveProvider"], "managed", "{body}");
    assert_eq!(body["needsApiKey"], true, "{body}");
}

#[tokio::test]
async fn searxng_needs_an_endpoint_and_the_endpoint_must_be_a_url() {
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path(), true).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    let (_, selected) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search",
        &admin,
        Some(json!({"provider": "searxng"})),
    )
    .await;
    assert_eq!(selected["needsEndpoint"], true, "{selected}");
    assert_eq!(selected["needsApiKey"], false, "{selected}");

    let (status, refused) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search",
        &admin,
        Some(json!({"endpoint": "searx.example"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");

    let (_, saved) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search",
        &admin,
        Some(json!({"endpoint": "https://searx.example"})),
    )
    .await;
    assert_eq!(saved["effectiveProvider"], "searxng", "{saved}");
    assert_eq!(saved["endpoint"], "https://searx.example", "{saved}");
}

#[tokio::test]
async fn the_legacy_route_refuses_what_the_modern_one_refuses() {
    // Four checks guard an operator-supplied address, and only one of the
    // four write paths ran all four. `POST …/search/providers` did;
    // `PUT …/search` ran the http(s) prefix and the metadata guard and
    // skipped the length cap and the control-character check, so the
    // compatibility route stored addresses the modern route refuses.
    //
    // The length one is the case `MAX_ENDPOINT_LEN`'s own comment was
    // written about: an unbounded operator-supplied string reaching the
    // store is how a row that cannot be deleted gets made.
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path(), true).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    let too_long = format!(
        "http://search.acme.internal/{}",
        "a".repeat(MAX_ENDPOINT_LEN)
    );
    let (status, _) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search",
        &admin,
        Some(json!({"provider": "searxng", "endpoint": too_long})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "the cap applies here too");

    let (status, _) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search",
        &admin,
        Some(json!({"provider": "searxng", "endpoint": "http://search.acme.internal/\u{7}"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "so does the control-character check"
    );

    // Nothing was stored on the way to either refusal.
    let (_, after) = call(&state, "GET", "/api/v1/companies/acme/search", &admin, None).await;
    assert_eq!(after["effectiveProvider"], "managed", "{after}");
    assert!(
        after["providers"].as_array().expect("providers").is_empty(),
        "{after}"
    );
}

#[tokio::test]
async fn a_provider_this_build_cannot_use_is_refused_rather_than_stored() {
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path(), true).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    let (status, body) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search",
        &admin,
        Some(json!({"provider": "google", "apiKey": "k"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (_, after) = call(&state, "GET", "/api/v1/companies/acme/search", &admin, None).await;
    // The rejected request stored nothing at all — not even the key that
    // came with it.
    assert_eq!(after["provider"], "managed", "{after}");
    assert_eq!(after["apiKeyConfigured"], false, "{after}");
}

#[tokio::test]
async fn clearing_drops_the_provider_and_the_endpoint_too_not_just_the_key() {
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path(), true).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search",
        &admin,
        Some(json!({
            "provider": "searxng",
            "endpoint": "https://searx.example",
        })),
    )
    .await;

    // The legacy `PUT …/search` route marks whatever it connects as the
    // default, so this disconnect-all is exactly the case item 1's guard
    // exists for (`in-use-guards.md` §1/§2) — hence `confirmInUse: true`.
    // This test is about the cleanup's scope (provider AND endpoint, not
    // just the key), not the guard, so it confirms.
    let (status, cleared) = call(
        &state,
        "DELETE",
        "/api/v1/companies/acme/search/key?confirmInUse=true",
        &admin,
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert_eq!(cleared["provider"], "managed", "{cleared}");
    assert_eq!(cleared["endpoint"], Value::Null, "{cleared}");
    assert_eq!(cleared["apiKeyConfigured"], false, "{cleared}");
}

#[tokio::test]
async fn a_configured_provider_without_the_grant_reports_that_it_reaches_nobody() {
    // Both halves can be right and still nothing happens. The status says so
    // separately, because the fix is the manifest rather than this page.
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path(), false).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search",
        &admin,
        Some(json!({"provider": "exa", "apiKey": "k"})),
    )
    .await;

    let (_, after) = call(&state, "GET", "/api/v1/companies/acme/search", &admin, None).await;

    assert_eq!(after["apiKeyConfigured"], true, "{after}");
    assert_eq!(after["granted"], false, "{after}");
}

#[tokio::test]
async fn switching_providers_does_not_hand_one_providers_key_to_another() {
    // This test used to assert the opposite, and that assertion WAS the bug.
    //
    // With one `search/api_key` for the whole company, switching to Querit
    // without pasting a key left Exa's key in the slot — and every layer
    // then agreed the company was correctly configured, because a key was
    // present: `configuration_complete` said yes, the badge said Querit, and
    // the harness wired Querit's tools around Exa's credential. The first
    // agent to search got a 401 that nothing on the page could explain.
    //
    // Each provider holds its own credential now, so the switch reports
    // Querit as selected with no key, and searches stay on managed until one
    // is pasted. Exa keeps its key and its row.
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path(), true).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search",
        &admin,
        Some(json!({"provider": "exa", "apiKey": "exa-not-a-real-key"})),
    )
    .await;
    let (_, after) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search",
        &admin,
        Some(json!({"provider": "querit"})),
    )
    .await;

    assert_eq!(after["provider"], "querit", "{after}");
    assert_eq!(
        after["apiKeyConfigured"], false,
        "querit must not inherit exa's key: {after}"
    );
    assert_eq!(
        after["effectiveProvider"], "managed",
        "a keyless selection searches through managed, not through a borrowed key: {after}"
    );

    let rows = after["providers"].as_array().expect("providers");
    let exa = rows
        .iter()
        .find(|row| row["slug"] == "exa")
        .expect("exa row survives the switch");
    assert_eq!(
        exa["keyConfigured"], true,
        "exa keeps its own credential: {after}"
    );
    let querit = rows
        .iter()
        .find(|row| row["slug"] == "querit")
        .expect("querit row");
    assert_eq!(querit["keyConfigured"], false, "{after}");
}

#[tokio::test]
async fn selecting_managed_actually_stops_searching_through_the_account() {
    // The compatibility route answered 200 for `{"provider":"managed"}` and
    // changed nothing an agent could feel. It cleared the default marker,
    // and `resolve::active` reads an absent marker as "the first usable
    // provider" — so a company with a working Exa connection kept searching
    // through Exa, billed to Exa, after explicitly asking to stop.
    //
    // Worse where it matters most: an upgraded legacy company has no marker
    // at all, so the clear was already a no-op there and the route was pure
    // theatre.
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path(), true).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    let (_, connected) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search",
        &admin,
        Some(json!({"provider": "exa", "apiKey": "exa-not-a-real-key"})),
    )
    .await;
    assert_eq!(
        connected["effectiveProvider"], "exa",
        "the setup has to actually be searching through exa: {connected}"
    );

    let (status, after) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search",
        &admin,
        Some(json!({"provider": "managed"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        after["effectiveProvider"], "managed",
        "asking for managed and being told 200 has to mean it: {after}"
    );

    // Switched off, not destroyed. The credential is write-only and is
    // never shown back, so an operator who lost one here could not retype it
    // from the screen.
    let exa = after["providers"]
        .as_array()
        .expect("providers")
        .iter()
        .find(|row| row["slug"] == "exa")
        .expect("the exa row survives")
        .clone();
    assert_eq!(exa["keyConfigured"], true, "the key is kept: {after}");
    assert_eq!(exa["enabled"], false, "the connection is off: {after}");

    // And naming it again turns it back on, rather than marking a default
    // that resolves to nothing.
    let (_, back) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search",
        &admin,
        Some(json!({"provider": "exa"})),
    )
    .await;
    assert_eq!(
        back["effectiveProvider"], "exa",
        "the round trip has to come back: {back}"
    );
}

/// DEPRECATED(keys-rework #2306) carve-out, pinned: unlike every guarded
/// route on this page (X14/D-never-clear-default,
/// `docs/key-reworks/in-use-guards.md` §4), the legacy `PUT …/search`
/// route's "select managed" branch still clears `search/default` outright
/// rather than leaving the marker in place — so no `defaultNotice` banner
/// ever appears afterward, even though the provider it just switched off
/// is exactly the shape that banner exists for. §4 now documents this as
/// the accepted, intentional exception: the route has no console caller
/// (`saveSearch` in `frontend/src/api/search.ts` is defined but never
/// called), so nothing reachable is affected by the discrepancy — but it
/// must not drift further in silence, which is what this test is for.
#[tokio::test]
async fn legacy_select_managed_clears_the_default_marker_unlike_the_guarded_routes() {
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path(), true).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search",
        &admin,
        Some(json!({"provider": "exa", "apiKey": "exa-not-a-real-key"})),
    )
    .await;

    let (status, after) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search",
        &admin,
        Some(json!({"provider": "managed"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{after}");
    // If the marker had survived (the guarded routes' behavior under
    // X14), this would read "The search default uses Exa, which is
    // turned off. …". It does not: the legacy route's own clear removed
    // the marker outright.
    assert!(
        after.get("defaultNotice").is_none(),
        "pinning the legacy route's own clear of search/default: {after}"
    );
}

#[tokio::test]
async fn two_providers_hold_two_independent_credentials() {
    // The central claim of the rework, asserted end to end through the
    // routes rather than only against the store.
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path(), true).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    for (slug, key) in [
        ("exa", "exa-not-a-real-key"),
        ("brave", "brave-not-a-real-key"),
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

    let (_, body) = call(&state, "GET", "/api/v1/companies/acme/search", &admin, None).await;
    let rows = body["providers"].as_array().expect("providers");
    assert_eq!(rows.len(), 2, "{body}");
    for row in rows {
        assert_eq!(row["keyConfigured"], true, "{body}");
    }

    // Clearing one leaves the other untouched. Under the old single slot
    // this was not expressible at all.
    //
    // The status is asserted because it has already hidden a bug once: with
    // `Path<String>` under `scoped`, the platform form captures `{id}` too
    // and every one of these returned 400 while the assertions below still
    // read as "the key was not cleared".
    let (cleared_status, cleared_body) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search/providers/exa/key",
        &admin,
        Some(json!({"apiKey": ""})),
    )
    .await;
    assert_eq!(cleared_status, StatusCode::OK, "{cleared_body}");

    let (_, after) = call(&state, "GET", "/api/v1/companies/acme/search", &admin, None).await;
    let rows = after["providers"].as_array().expect("providers");
    let keyed: Vec<&str> = rows
        .iter()
        .filter(|row| row["keyConfigured"] == true)
        .map(|row| row["slug"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(keyed, vec!["brave"], "{after}");
}

#[tokio::test]
async fn a_key_cannot_be_replaced_on_a_provider_that_is_not_connected() {
    // `PUT …/search/providers/{slug}/key` is **replace**, so there has to be
    // something to replace. Without the check it wrote a credential to
    // `search/provider/exa/key` that the status route never reports and
    // `DELETE …/search/key` never clears — its loop visits indexed
    // providers only. An invisible credential the operator can neither see
    // nor delete is the orphaned-secret shape this module refuses
    // everywhere else.
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path(), true).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    let (status, _) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search/providers/exa/key",
        &admin,
        Some(json!({"apiKey": "exa-not-a-real-key"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (_, after) = call(&state, "GET", "/api/v1/companies/acme/search", &admin, None).await;
    assert!(
        after["providers"].as_array().expect("providers").is_empty(),
        "{after}"
    );

    // And it still works for one that IS connected — the refusal must not
    // have cost the ordinary path.
    call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search",
        &admin,
        Some(json!({"provider": "exa", "apiKey": "exa-not-a-real-key"})),
    )
    .await;
    let (status, after) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search/providers/exa/key",
        &admin,
        Some(json!({"apiKey": "exa-also-not-a-real-key"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{after}");
}

#[tokio::test]
async fn a_check_cannot_send_an_account_providers_key_to_an_address_of_your_choosing() {
    // The credential is write-only on this surface: no route returns it and
    // no page renders it back. `POST …/search/test` with an `endpoint` for
    // Brave would have put the stored Brave key in a header to whatever
    // address was named — handing it straight out, through the one route
    // whose whole point is that it is safe to press.
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path(), true).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search",
        &admin,
        Some(json!({"provider": "brave", "apiKey": "brave-not-a-real-key"})),
    )
    .await;

    let (status, body) = call(
        &state,
        "POST",
        "/api/v1/companies/acme/search/test",
        &admin,
        Some(json!({"slug": "brave", "endpoint": "http://127.0.0.1:1/collect"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // SearXNG is the provider that genuinely has an address, so it must
    // still accept one — the refusal is about which providers have one, not
    // about overrides in general.
    let (status, _) = call(
        &state,
        "POST",
        "/api/v1/companies/acme/search/test",
        &admin,
        Some(json!({"slug": "searxng", "endpoint": "http://127.0.0.1:1/"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a self-hosted instance still takes an address"
    );
}
