use crate::ports::CompanyStore;
use crate::ports::types::CompanyId;
use axum::http::StatusCode;

use super::setup_test_support_1::*;

/// The roster arrives over the wire after an operator edited it, so neither the
/// bounds nor the de-duplication can be assumed to have survived. Validation
/// runs again on the way in rather than trusting the client.
#[tokio::test]
async fn an_edited_roster_is_revalidated_on_the_way_in() {
    let home = home();
    let state = fresh_state(home.path());

    let mut company = designed_company(None);
    // Two rows that slug alike, and a blank one — all three are things a client
    // could send and `validate` would refuse.
    company["agents"] = serde_json::json!([
        { "name": "Ops", "role": "Ops Lead", "description": "a" },
        { "name": "Ops", "role": "ops  lead", "description": "b" },
        { "name": "", "role": "   ", "description": "c" },
        { "name": "Accounts", "role": "Accountant", "description": "d" }
    ]);

    let (status, body) = post_setup(state.clone(), serde_json::json!({ "company": company })).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let seeded = body["seeded_company"].as_str().expect("seeded");
    let manifest = seeded_manifest(home.path(), seeded).await;
    assert!(
        manifest.validate().is_empty(),
        "a registered company must be valid: {:?}",
        manifest.validate()
    );
    let ids: Vec<&str> = manifest.agents.iter().map(|a| a.id.as_str()).collect();
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "duplicate ids survived: {ids:?}");
}

/// Phase 2 builds this company's workflows from the same answers, so it must
/// never have to ask again. The company-scoped route already stores them; the
/// wizard is the *default* path, and a company created through it arriving
/// without them would be the one that gets re-interrogated.
#[tokio::test]
async fn the_answers_are_stored_on_the_company_the_wizard_built() {
    let home = home();
    let state = fresh_state(home.path());

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({ "company": designed_company(Some("ada@example.com")) }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let seeded = body["seeded_company"].as_str().expect("seeded");
    let store = crate::store::FsCompanyStore::new(home.path().to_path_buf());
    let record = store
        .load(&CompanyId::new(seeded))
        .await
        .expect("load")
        .expect("record");
    let answers = record.setup.expect("the answers were stored");
    assert_eq!(answers.industry, "E-commerce — I sell homeware online");
    assert_eq!(answers.automate, "Meta ads, order dispatch");
}

const ACCOUNT_KEY: &str = "th-not-a-real-key";

/// Reads one of a company's secrets, or `None` when it holds nothing.
async fn secret(runtime: &crate::company::runtime::CompanyRuntime, key: &str) -> Option<String> {
    runtime
        .secrets()
        .get(runtime.id(), key)
        .await
        .unwrap()
        .map(|crate::ports::types::SecretValue(value)| value)
}

/// The wizard's managed branch collects the company's TinyHumans account key
/// and runs it through the same fan-out `PUT …/credential` does, so the
/// Composio and LLM copies are filled rather than left empty.
#[tokio::test]
async fn the_wizards_account_key_fans_out_onto_the_company_it_seeds() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());
    crate::server::ops::company_key::prober_override::set(
        "acme",
        Ok(vec!["acme/test-model".to_string()]),
    );

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({
            "fields": {},
            "template": "law_firm",
            "name": "Acme",
            "tinyhumans_key": ACCOUNT_KEY,
            "tinyhumans_model": "acme/test-model",
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["seeded_company"], "acme", "{body}");
    assert!(
        !body.to_string().contains(ACCOUNT_KEY),
        "the apply response must never echo the key: {body}"
    );

    let runtime = state
        .registry()
        .get(&CompanyId::new("acme"))
        .expect("the seeded company is registered");

    assert_eq!(
        secret(&runtime, crate::company::company_key::KEY_KEY).await,
        Some(ACCOUNT_KEY.to_string()),
        "the key belongs to the company, in the slot the Account page writes"
    );
    assert_eq!(
        secret(&runtime, crate::company::composio::TINYHUMANS_KEY_KEY).await,
        Some(ACCOUNT_KEY.to_string()),
        "the Composio copy must have been filled from it"
    );
    assert_eq!(
        secret(
            &runtime,
            &crate::company::inference::store::provider_key_key(
                crate::company::inference::MANAGED_SLUG
            )
        )
        .await,
        Some(ACCOUNT_KEY.to_string()),
        "and the LLM copy with it"
    );

    let note = body["credential_note"]
        .as_str()
        .unwrap_or_else(|| panic!("the fan-out's own words must come back: {body}"));
    assert!(note.contains("acme/test-model"), "{note}");
}

/// A key sent with nothing to attach it to is dropped, not written onto
/// whichever company happened to already exist.
#[tokio::test]
async fn an_account_key_with_no_company_to_own_it_is_not_written_anywhere() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());
    let existing = with_company(&state, home_dir.path()).await;

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({
            "fields": {},
            "template": "law_firm",
            "tinyhumans_key": ACCOUNT_KEY,
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["seeded_company"].is_null(), "{body}");
    assert!(
        body["credential_note"].is_null(),
        "nothing happened, so nothing is claimed: {body}"
    );

    let runtime = state.registry().get(&existing).expect("still registered");
    assert_eq!(
        secret(&runtime, crate::company::company_key::KEY_KEY).await,
        None,
        "a company this wizard did not create must not be given a wallet"
    );
}

#[cfg(feature = "openhuman")]
mod setup_model_preference {
    use crate::server::inference_models::InferenceModel;
    use crate::server::setup::{PREFERRED_SETUP_MODEL, probe_model_candidates};

    fn model(id: &str) -> InferenceModel {
        InferenceModel {
            id: id.to_string(),
            name: None,
            context_length: None,
        }
    }

    #[test]
    fn the_preferred_setup_model_is_deepseek_v4_flash() {
        assert_eq!(PREFERRED_SETUP_MODEL, "deepseek/deepseek-v4-flash");
    }

    #[test]
    fn the_preferred_model_is_probed_before_whatever_the_catalogue_lists_first() {
        let candidates = probe_model_candidates(vec![
            model("vendor/reasoning-heavy"),
            model("vendor/another"),
            model(PREFERRED_SETUP_MODEL),
        ]);

        assert_eq!(
            candidates.first().map(|model| model.id.as_str()),
            Some(PREFERRED_SETUP_MODEL),
            "a first company must not inherit a default from catalogue position"
        );
    }

    #[test]
    fn a_catalogue_without_the_preferred_model_keeps_its_own_order() {
        let candidates = probe_model_candidates(vec![
            model("vendor/first"),
            model("vendor/second"),
            model("vendor/text-embed-3"),
        ]);

        assert_eq!(
            candidates
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["vendor/first", "vendor/second", "vendor/text-embed-3"],
            "preference must not reorder a catalogue that does not offer it"
        );
    }

    #[test]
    fn an_embedding_model_never_outranks_the_preferred_one() {
        let candidates = probe_model_candidates(vec![
            model("vendor/text-embed-3"),
            model(PREFERRED_SETUP_MODEL),
        ]);

        assert_eq!(
            candidates.first().map(|model| model.id.as_str()),
            Some(PREFERRED_SETUP_MODEL)
        );
    }
}
