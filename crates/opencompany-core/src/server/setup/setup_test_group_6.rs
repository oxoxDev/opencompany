//! Setup tests for the self-managed branch's Composio credential (slice
//! 4b-ii): which of the two the apply wrote, and what a skipped step leaves
//! behind.
//!
//! Beside `setup_test_group_5` for the same reason that one exists: these ask
//! what the **apply** left in the company's own stores, which is a different
//! question from what the wizard collected.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::company::runtime::CompanyRuntime;
use crate::ports::types::{CompanyId, SecretValue};
use crate::server::router;

use super::setup_test_support_1::*;

/// Reads one of a company's secrets, or `None` when it holds nothing.
async fn secret(runtime: &CompanyRuntime, key: &str) -> Option<String> {
    runtime
        .secrets()
        .get(runtime.id(), key)
        .await
        .unwrap()
        .map(|SecretValue(value)| value)
}

/// Composio credentials shaped like real ones and worth nothing.
const COMPOSIO_BYOK_KEY: &str = "ak-not-a-real-key";
const COMPOSIO_TOKEN: &str = "th-not-a-real-token";

/// The own-account variant writes the BYOK key **and** selects the mode.
///
/// The two go together or the company has no Composio tools at all: `store_api_key`
/// writes both because a mode with no key fails closed rather than borrowing the
/// platform identity, and a key with no mode is a credential nothing reads.
/// Asserted as both, because a wizard-side write of the key alone would look
/// identical until an agent reached for Gmail.
#[tokio::test]
async fn the_wizards_composio_api_key_selects_the_companys_own_account() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({
            "fields": {},
            "template": "law_firm",
            "name": "Acme",
            "composio_draft": { "credential": "composio-api-key", "value": COMPOSIO_BYOK_KEY },
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !body.to_string().contains(COMPOSIO_BYOK_KEY),
        "the apply response must never echo the key: {body}"
    );

    let runtime = state
        .registry()
        .get(&CompanyId::new("acme"))
        .expect("the seeded company is registered");

    assert_eq!(
        secret(&runtime, crate::company::composio::BYOK_KEY_KEY).await,
        Some(COMPOSIO_BYOK_KEY.to_string()),
    );
    assert_eq!(
        secret(&runtime, crate::company::composio::MODE_KEY)
            .await
            .as_deref(),
        Some(crate::company::composio::BYOK_MODE),
        "a key with no mode is a credential nothing reads"
    );
    assert!(body["composio_note"].is_string(), "{body}");
}

/// The managed-route variant writes the token slot and **nothing else**.
///
/// A different credential with a different lifecycle: it overrides what the
/// managed chain resolves to, and it does not move the company off that chain.
/// So the mode must stay unwritten — a token that also flipped the mode would
/// be the own-account write wearing another name.
#[tokio::test]
async fn the_wizards_composio_token_fills_the_managed_route_only() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({
            "fields": {},
            "template": "law_firm",
            "name": "Acme",
            "composio_draft": { "credential": "composio-token", "value": COMPOSIO_TOKEN },
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let runtime = state
        .registry()
        .get(&CompanyId::new("acme"))
        .expect("the seeded company is registered");

    assert_eq!(
        secret(&runtime, crate::company::composio::TINYHUMANS_KEY_KEY).await,
        Some(COMPOSIO_TOKEN.to_string()),
    );
    assert_eq!(
        secret(&runtime, crate::company::composio::BYOK_KEY_KEY).await,
        None,
        "the managed route's token is not the company's own account key"
    );
    assert_eq!(
        secret(&runtime, crate::company::composio::MODE_KEY).await,
        None,
        "and it does not move the company off the managed route"
    );
}

/// No draft, no write — and a blank one is no draft.
///
/// `store_api_key` reads an empty key as "clear this company back to managed",
/// which on a company that was never on BYOK is a mode write nobody asked for:
/// the wizard would silently record a decision the operator skipped.
#[tokio::test]
async fn an_apply_with_no_composio_credential_writes_nothing() {
    for draft in [
        serde_json::Value::Null,
        serde_json::json!({ "credential": "composio-api-key", "value": "   " }),
    ] {
        let home_dir = home();
        let state = fresh_state(home_dir.path());

        let (status, body) = post_setup(
            state.clone(),
            serde_json::json!({
                "fields": {},
                "template": "law_firm",
                "name": "Acme",
                "composio_draft": draft,
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body["composio_note"].is_null(),
            "nothing happened, so nothing is claimed: {body}"
        );

        let runtime = state
            .registry()
            .get(&CompanyId::new("acme"))
            .expect("the seeded company is registered");
        assert_eq!(
            secret(&runtime, crate::company::composio::MODE_KEY).await,
            None,
            "a skipped step must not write a mode"
        );
        assert_eq!(
            secret(&runtime, crate::company::composio::BYOK_KEY_KEY).await,
            None,
        );
        assert_eq!(
            secret(&runtime, crate::company::composio::TINYHUMANS_KEY_KEY).await,
            None,
        );
    }
}

/// A Composio credential with nowhere to go is dropped, not guessed at.
#[tokio::test]
async fn a_composio_draft_with_no_company_to_own_it_is_not_written_anywhere() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());
    let existing = with_company(&state, home_dir.path()).await;

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({
            "fields": {},
            "template": "law_firm",
            "composio_draft": { "credential": "composio-api-key", "value": COMPOSIO_BYOK_KEY },
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["seeded_company"].is_null(), "{body}");
    assert!(body["composio_note"].is_null(), "{body}");

    let runtime = state.registry().get(&existing).expect("still registered");
    assert_eq!(
        secret(&runtime, crate::company::composio::BYOK_KEY_KEY).await,
        None,
        "a company this wizard did not create must not be moved onto another account"
    );
}

/// The Composio key check is behind the same gate every other setup route is.
#[tokio::test]
async fn the_composio_key_check_is_refused_on_a_routable_host() {
    let home_dir = home();
    let response = router(routable_state(home_dir.path()))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/setup/composio/api-key/test")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "apiKey": COMPOSIO_BYOK_KEY }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_ne!(
        response.status(),
        StatusCode::OK,
        "a routable host must not spend an outbound request on an anonymous caller's key"
    );
}

/// And it reports the class rather than storing anything.
///
/// Driven through the transport override so no test dials Composio. The scope
/// the setup route names itself with is `setup`, since there is no company.
#[tokio::test]
async fn the_composio_key_check_reports_the_verdict_and_writes_nothing() {
    crate::server::ops::composio::probe_override::set("setup", Err("401 Unauthorized".to_string()));
    let home_dir = home();
    let state = fresh_state(home_dir.path());
    let response = router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/setup/composio/api-key/test")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "apiKey": COMPOSIO_BYOK_KEY }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["ok"], false, "{body}");
    assert!(body["message"].is_string(), "{body}");
    assert!(
        !body.to_string().contains("401 Unauthorized"),
        "the upstream text must not leave the host: {body}"
    );
    assert!(
        state.registry().is_empty(),
        "a check creates nothing and configures nothing"
    );
    // `setup` is a shared slot, not a company id, so this must not outlive the
    // test that forced it.
    crate::server::ops::composio::probe_override::clear("setup");
}
