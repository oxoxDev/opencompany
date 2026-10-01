//! The document route: what it serves, what it refuses to rewrite, and what the
//! rewrite leaves behind.
//!
//! The reads matter as much as the writes here. An editor that opens on the
//! wrong layer saves the wrong document back, so the test that the baseline's
//! text is served verbatim is the one that keeps the editor honest about what it
//! is editing.

use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::AppState;
use crate::company::skill_digest;
use crate::ports::skills_state::{SkillInstall, SkillSource, SkillState};
use crate::ports::types::SkillChange;
use crate::server::ops::language;
use crate::server::ops::write_test_support::*;

/// A slug neither the global baseline nor the repository library ships.
const SLUG: &str = "library-skill";

/// A slug the global baseline does ship, so its document comes off disk and no
/// delta covers it.
const BASELINE_SLUG: &str = "web-research";

fn doc(body: &str) -> String {
    format!(
        "---\nname: Library Skill\ndescription: A skill the shared library ships.\ncategory: Ops\nversion: 1.0.0\n---\n{body}\n"
    )
}

fn stored() -> String {
    doc("# Library Skill\nStep one.")
}

fn rewritten() -> String {
    doc("# Library Skill\nStep one, then step two.")
}

/// A registry install of `pinned`, with `pinned` also the stored copy — the
/// state an install leaves.
fn install_of(pinned: &str) -> SkillState {
    SkillState {
        slug: SLUG.to_string(),
        enabled: true,
        source: SkillSource::Registry,
        custom_doc: Some(pinned.to_string()),
        updated_at_millis: Some(1_700_000_000_000),
        install: Some(SkillInstall {
            digest: skill_digest(pinned),
            version: Some("1.0.0".to_string()),
            installed_by: None,
            installed_at_millis: 1_700_000_000_000,
        }),
    }
}

fn authored() -> SkillState {
    SkillState {
        slug: SLUG.to_string(),
        enabled: true,
        source: SkillSource::Custom,
        custom_doc: Some(stored()),
        updated_at_millis: Some(1_700_000_000_000),
        install: None,
    }
}

async fn get_doc(state: &AppState, slug: &str) -> (StatusCode, Value) {
    send(
        state,
        "GET",
        &format!("/api/v1/company/skills/{slug}/doc"),
        None,
    )
    .await
}

async fn put_doc(state: &AppState, slug: &str, markdown: &str) -> (StatusCode, Value) {
    send(
        state,
        "PUT",
        &format!("/api/v1/company/skills/{slug}/doc"),
        Some(json!({ "markdown": markdown })),
    )
    .await
}

async fn stored_doc(state: &AppState, slug: &str) -> Option<String> {
    persisted_skills(state)
        .await
        .into_iter()
        .find(|row| row.slug == slug)
        .and_then(|row| row.custom_doc)
}

/// The refusal sentence out of the error envelope, which prefixes the variant's
/// own word.
fn refusal(body: &Value) -> String {
    body["error"]
        .as_str()
        .unwrap_or_else(|| panic!("no error sentence in {body}"))
        .to_string()
}

#[tokio::test]
async fn reading_an_authored_skill_serves_the_whole_document() {
    let home_dir = home();
    let library_dir = home();
    let state = state_with_library(home_dir.path(), library_dir.path()).await;
    seed_skill_delta(&state, &authored()).await;

    let (status, body) = get_doc(&state, SLUG).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    // Frontmatter included: the editor edits the document an agent reads, not a
    // body it would have to re-wrap in metadata it never saw.
    assert_eq!(body["markdown"], json!(stored()));
    assert_eq!(body["editable"], json!(true), "{body}");
}

/// The baseline's text lives on disk and is served off it. Without this the
/// editor would open empty on every skill nobody has touched.
#[tokio::test]
async fn reading_a_baseline_skill_serves_its_text_but_calls_it_uneditable() {
    let home_dir = home();
    let library_dir = home();
    let state = state_with_library(home_dir.path(), library_dir.path()).await;

    let (status, body) = get_doc(&state, BASELINE_SLUG).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let markdown = body["markdown"].as_str().unwrap_or_default();
    assert!(
        markdown.contains("name:") && markdown.len() > 200,
        "a real document, not a stub: {markdown:?}"
    );
    assert_eq!(body["editable"], json!(false), "{body}");
}

#[tokio::test]
async fn reading_a_slug_the_company_does_not_have_says_there_is_no_document() {
    let home_dir = home();
    let library_dir = home();
    let state = state_with_library(home_dir.path(), library_dir.path()).await;

    let (status, body) = get_doc(&state, SLUG).await;

    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(refusal(&body).contains(language::SKILL_NO_DOC), "{body}");
}

#[tokio::test]
async fn writing_an_authored_skill_replaces_the_stored_document() {
    let home_dir = home();
    let library_dir = home();
    let state = state_with_library(home_dir.path(), library_dir.path()).await;
    seed_skill_delta(&state, &authored()).await;

    let (status, body) = put_doc(&state, SLUG, &rewritten()).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        stored_doc(&state, SLUG).await.as_deref(),
        Some(&*rewritten())
    );
    // The answer describes the document just stored, so the list the console is
    // already showing can be folded forward without a refetch.
    assert_eq!(body["id"], json!(SLUG), "{body}");
    assert_eq!(body["enabled"], json!(true), "{body}");
}

/// An edit must not quietly switch a disabled skill back on: the two are
/// separate decisions and only one of them was made.
#[tokio::test]
async fn writing_a_disabled_skill_leaves_it_disabled() {
    let home_dir = home();
    let library_dir = home();
    let state = state_with_library(home_dir.path(), library_dir.path()).await;
    seed_skill_delta(
        &state,
        &SkillState {
            enabled: false,
            ..authored()
        },
    )
    .await;

    let (status, body) = put_doc(&state, SLUG, &rewritten()).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], json!(false), "{body}");
}

/// Editing a registry install is the local-copy flow: the pin stays where the
/// install put it, so the divergence is reported rather than erased.
#[tokio::test]
async fn writing_a_registry_install_keeps_the_pin_and_reports_it_modified() {
    let home_dir = home();
    let library_dir = home();
    seed_library_skill(library_dir.path(), SLUG, &stored()).await;
    let state = state_with_library(home_dir.path(), library_dir.path()).await;
    seed_skill_delta(&state, &install_of(&stored())).await;

    let (status, body) = put_doc(&state, SLUG, &rewritten()).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["modified"], json!(true), "{body}");
    let row = persisted_skills(&state)
        .await
        .into_iter()
        .find(|row| row.slug == SLUG)
        .expect("the row survives");
    assert_eq!(
        row.source,
        SkillSource::Registry,
        "provenance is not rewritten"
    );
    assert_eq!(
        row.install.map(|pin| pin.digest),
        Some(skill_digest(&stored())),
        "the pin still names what the library shipped"
    );
}

/// The one case the heal arm would otherwise swallow: an operator's edit whose
/// body happens to equal its own description looks exactly like the pre-fix
/// registry stub it was written to repair.
#[tokio::test]
async fn an_edit_that_looks_like_a_stub_is_served_back_not_healed() {
    let degenerate = "---\nname: Library Skill\ndescription: A skill the shared library ships.\ncategory: Ops\n---\nA skill the shared library ships.\n";
    let home_dir = home();
    let library_dir = home();
    seed_library_skill(library_dir.path(), SLUG, &stored()).await;
    let state = state_with_library(home_dir.path(), library_dir.path()).await;
    seed_skill_delta(&state, &install_of(&stored())).await;

    let (status, body) = put_doc(&state, SLUG, degenerate).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = get_doc(&state, SLUG).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["markdown"], json!(degenerate), "{body}");
}

#[tokio::test]
async fn writing_a_baseline_skill_is_refused_and_stores_nothing() {
    let home_dir = home();
    let library_dir = home();
    let state = state_with_library(home_dir.path(), library_dir.path()).await;

    let (status, body) = put_doc(&state, BASELINE_SLUG, &rewritten()).await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        refusal(&body).contains(language::SKILL_DOC_NOT_EDITABLE),
        "{body}"
    );
    assert_eq!(stored_doc(&state, BASELINE_SLUG).await, None);
}

/// A document the shared validator refuses must not reach the store through the
/// editor when an install of the same text would have been refused.
#[tokio::test]
async fn writing_a_document_the_validator_refuses_stores_nothing() {
    let home_dir = home();
    let library_dir = home();
    let state = state_with_library(home_dir.path(), library_dir.path()).await;
    seed_skill_delta(&state, &authored()).await;

    let (status, body) = put_doc(&state, SLUG, "no frontmatter here at all").await;

    assert_ne!(status, StatusCode::OK, "{body}");
    assert_eq!(
        stored_doc(&state, SLUG).await.as_deref(),
        Some(&*stored()),
        "the previous document survives a refused write"
    );
}

/// The store rewrites one row per slug in place, so the journal is the only
/// record that a rewrite happened at all — and it has to say *rewrite*, not the
/// re-pin `Updated` means.
#[tokio::test]
async fn an_edit_is_journalled_as_its_own_kind_of_change() {
    let home_dir = home();
    let library_dir = home();
    let state = state_with_library(home_dir.path(), library_dir.path()).await;
    seed_skill_delta(&state, &authored()).await;

    let (status, body) = put_doc(&state, SLUG, &rewritten()).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let runtime = state
        .registry()
        .get(&crate::ports::types::CompanyId::new("acme"))
        .expect("company");
    let rows = runtime
        .events()
        .read_from(
            runtime.id(),
            crate::ports::types::EventSeq::new(0),
            usize::MAX,
        )
        .await
        .expect("journal");
    let edited = rows
        .iter()
        .filter_map(|row| match &row.event {
            crate::ports::types::CompanyEvent::SkillChanged {
                slug,
                change,
                digest,
                ..
            } => Some((slug.clone(), *change, digest.clone())),
            _ => None,
        })
        .find(|(slug, _, _)| slug == SLUG)
        .expect("a skill row for the slug");
    assert_eq!(edited.1, SkillChange::Edited);
    assert_eq!(
        edited.2,
        Some(skill_digest(&rewritten())),
        "anchored to the document actually stored"
    );
}
