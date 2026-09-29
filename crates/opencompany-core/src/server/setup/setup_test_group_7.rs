//! Setup tests: a credential-store failure after setup has already committed
//! must surface as a note on the apply response, not fail it.

use axum::http::StatusCode;

use crate::ports::types::CompanyId;

use super::setup_test_support_1::*;

/// Makes the next write of `key` for `company_id` fail: the exact path the
/// secret would be written to is pre-created as a directory, so writing the
/// secret's contents there fails at the filesystem, not by permission bits
/// `ensure_dirs` would otherwise reset before every write.
fn block_secret_write(home: &std::path::Path, company_id: &str, key: &str) {
    let path =
        crate::store::Bundle::new(home.to_path_buf(), &CompanyId::new(company_id)).secret(key);
    std::fs::create_dir_all(&path).unwrap();
}

#[tokio::test]
async fn a_provider_store_failure_after_setup_is_committed_is_a_note_not_an_error() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());
    let slug = crate::company::inference::store::slugify("Acme Models");
    block_secret_write(
        home_dir.path(),
        "acme",
        &crate::company::inference::store::provider_key_key(&slug),
    );

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({
            "fields": {},
            "template": "law_firm",
            "name": "Acme",
            "provider_draft": {
                "kind": "custom",
                "label": "Acme Models",
                "baseUrl": "http://127.0.0.1:1/v1",
                "key": "sk-not-a-real-key",
                "model": "acme/test-model",
            },
        }),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "a store failure must not fail the apply: {body}"
    );
    assert_eq!(body["complete"], true, "{body}");
    assert_eq!(body["seeded_company"], "acme", "{body}");

    let runtime = state
        .registry()
        .get(&CompanyId::new("acme"))
        .expect("the seeded company is registered");
    let providers =
        crate::company::inference::store::list_providers(runtime.id(), runtime.secrets().as_ref())
            .await
            .unwrap();
    assert!(
        providers.is_empty(),
        "a failed credential write must not leave a provider row behind: {providers:?}"
    );

    let note = body["provider_note"]
        .as_str()
        .expect("a failed store is still reported as a note");
    assert!(
        note.contains("could not be connected"),
        "the operator must be told the provider failed, not left guessing: {note}"
    );
}

#[tokio::test]
async fn a_composio_store_failure_after_setup_is_committed_is_a_note_not_an_error() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());
    block_secret_write(
        home_dir.path(),
        "acme",
        crate::company::composio::BYOK_KEY_KEY,
    );

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({
            "fields": {},
            "template": "law_firm",
            "name": "Acme",
            "composio_draft": { "credential": "composio-api-key", "value": "ak-not-a-real-key" },
        }),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "a store failure must not fail the apply: {body}"
    );
    assert_eq!(body["complete"], true, "{body}");
    assert_eq!(body["seeded_company"], "acme", "{body}");
    let note = body["composio_note"]
        .as_str()
        .expect("a failed store is still reported as a note");
    assert!(
        note.contains("could not be stored"),
        "the operator must be told the credential failed, not left guessing: {note}"
    );
}
