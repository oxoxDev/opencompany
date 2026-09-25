//! What the journal records when a skill changes.
//!
//! The store keeps one row per slug and rewrites it in place, so the journal is
//! the whole record that anyone installed, replaced or removed a document every
//! agent in the company then reads. Each case here pins one write site's row —
//! and the last two pin what is deliberately *not* recorded: a toggle, and any
//! part of a skill's text.

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::write_test_support::*;
use crate::company::skill_digest;
use crate::ports::EventSeq;
use crate::ports::skills_state::SkillTier;
use crate::ports::skills_state::{SkillInstall, SkillSource, SkillState};
use crate::ports::types::{CompanyId, SkillChange};

const SLUG: &str = "library-skill";

/// A distinctive sentence in the document's body. If it reaches a journal row,
/// the row is carrying skill text — which is what P3 forbids, and what a
/// `contains` assertion can actually falsify.
const BODY_MARKER: &str = "Telephone the incumbent before drafting.";

fn doc(version: &str) -> String {
    format!(
        "---\nname: Library Skill\ndescription: A skill the shared library ships.\ncategory: Ops\nversion: {version}\n---\n# Library Skill\n{BODY_MARKER}\n"
    )
}

/// Every `SkillChanged` row the company has journalled, as raw JSON so the wire
/// shape is what is asserted rather than a re-parse into the enum.
async fn skill_rows(state: &crate::AppState) -> Vec<Value> {
    let runtime = state
        .registry()
        .get(&CompanyId::new("acme"))
        .expect("company");
    runtime
        .events()
        .read_from(runtime.id(), EventSeq::new(0), usize::MAX)
        .await
        .expect("journal")
        .into_iter()
        .filter_map(|stored| {
            let value = serde_json::to_value(&stored.event).expect("serializes");
            value
                .get("kind")
                .is_some_and(|kind| kind == "SkillChanged")
                .then_some(value)
        })
        .collect()
}

/// The single `SkillChanged` row, or a panic naming how many there were.
async fn only_row(state: &crate::AppState) -> Value {
    let rows = skill_rows(state).await;
    assert_eq!(rows.len(), 1, "expected exactly one skill row: {rows:?}");
    rows[0].clone()
}

fn pinned_delta(stored: &str, pinned: &str) -> SkillState {
    SkillState {
        slug: SLUG.to_string(),
        enabled: true,
        source: SkillSource::Registry,
        custom_doc: Some(stored.to_string()),
        updated_at_millis: Some(1_700_000_000_000),
        install: Some(SkillInstall {
            digest: skill_digest(pinned),
            version: crate::company::parse_skill_md(SLUG, pinned)
                .expect("the fixture parses")
                .version,
            installed_by: None,
            installed_at_millis: 1_700_000_000_000,
        }),
    }
}

/// A library install records one row, tiered `registry`, anchored to the digest
/// of the document that was written, and attributed.
#[tokio::test]
async fn installing_from_the_library_journals_one_registry_row() {
    let home_dir = home();
    let library_dir = home();
    seed_library_skill(library_dir.path(), SLUG, &doc("1.0.0")).await;
    let state = state_with_library(home_dir.path(), library_dir.path()).await;

    let (status, _) = send(
        &state,
        "POST",
        &format!("/api/v1/company/skills/{SLUG}/install"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let row = only_row(&state).await;
    assert_eq!(row["slug"], json!(SLUG));
    assert_eq!(row["change"], json!(SkillChange::Installed));
    assert_eq!(row["tier"], json!(SkillTier::Registry));
    assert!(
        row["by"].is_object(),
        "an admin route knows who acted: {row}"
    );
    let stored = persisted_skills(&state)
        .await
        .into_iter()
        .find(|row| row.slug == SLUG)
        .expect("the install stored a row");
    assert_eq!(
        row["digest"],
        json!(skill_digest(stored.custom_doc.as_deref().unwrap())),
        "the row anchors to the document that was written"
    );
}

/// The empty-registry fallback writes a document of the client's making, so the
/// row says `custom` — the tier is computed from the row's provenance, never
/// claimed by the caller.
#[tokio::test]
async fn installing_without_a_library_journals_a_custom_row() {
    let home_dir = home();
    let state = state_with_company(home_dir.path()).await;

    let (status, _) = send(
        &state,
        "POST",
        "/api/v1/company/skills/desk-notes/install",
        Some(json!({ "name": "Desk notes", "description": "Keep desk notes." })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let row = only_row(&state).await;
    assert_eq!(row["change"], json!(SkillChange::Installed));
    assert_eq!(row["tier"], json!(SkillTier::Custom));
}

/// Authoring in the console is an install of a custom document.
#[tokio::test]
async fn authoring_a_custom_skill_journals_a_custom_row() {
    let home_dir = home();
    let state = state_with_company(home_dir.path()).await;

    let (status, _) = send(
        &state,
        "POST",
        "/api/v1/company/skills",
        Some(json!({ "name": "Press Outreach", "description": "Pitch a story." })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let row = only_row(&state).await;
    assert_eq!(row["slug"], json!("press-outreach"));
    assert_eq!(row["change"], json!(SkillChange::Installed));
    assert_eq!(row["tier"], json!(SkillTier::Custom));
}

/// The update route records the one change `SkillChange` has a word for that
/// nothing else emits.
#[tokio::test]
async fn updating_an_install_journals_an_updated_row() {
    let home_dir = home();
    let library_dir = home();
    seed_library_skill(library_dir.path(), SLUG, &doc("2.0.0")).await;
    let state = state_with_library(home_dir.path(), library_dir.path()).await;
    let installed = doc("1.0.0");
    seed_skill_delta(&state, &pinned_delta(&installed, &installed)).await;

    let (status, body) = send(
        &state,
        "POST",
        &format!("/api/v1/company/skills/{SLUG}/update"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let row = only_row(&state).await;
    assert_eq!(row["change"], json!(SkillChange::Updated));
    assert_eq!(row["tier"], json!(SkillTier::Registry));
    let stored = persisted_skills(&state)
        .await
        .into_iter()
        .find(|row| row.slug == SLUG)
        .expect("the update stored a row");
    assert_eq!(
        row["digest"],
        json!(skill_digest(stored.custom_doc.as_deref().unwrap())),
        "the row anchors to the document the update wrote, not the one it replaced"
    );
}

/// A removal writes no document, so the row carries no digest at all rather
/// than the digest of what used to be there.
#[tokio::test]
async fn uninstalling_journals_a_removal_with_no_digest() {
    let home_dir = home();
    let library_dir = home();
    seed_library_skill(library_dir.path(), SLUG, &doc("1.0.0")).await;
    let state = state_with_library(home_dir.path(), library_dir.path()).await;
    let installed = doc("1.0.0");
    seed_skill_delta(&state, &pinned_delta(&installed, &installed)).await;

    let (status, _) = send(
        &state,
        "POST",
        &format!("/api/v1/company/skills/{SLUG}/uninstall"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let row = only_row(&state).await;
    assert_eq!(row["change"], json!(SkillChange::Removed));
    assert_eq!(row["tier"], json!(SkillTier::Registry));
    assert!(
        row.get("digest").is_none(),
        "a removal wrote no document to anchor to: {row}"
    );
}

/// A toggle journals **nothing**, and that is a decision rather than an
/// omission: `SkillChange` has no word for it, a toggle writes no document, and
/// recording one as `Updated` would tell an audit reader a re-pin happened that
/// did not.
#[tokio::test]
async fn toggling_a_skill_journals_no_skill_change() {
    let home_dir = home();
    let library_dir = home();
    seed_library_skill(library_dir.path(), SLUG, &doc("1.0.0")).await;
    let state = state_with_library(home_dir.path(), library_dir.path()).await;
    let installed = doc("1.0.0");
    seed_skill_delta(&state, &pinned_delta(&installed, &installed)).await;

    let (status, _) = send(
        &state,
        "PUT",
        &format!("/api/v1/company/skills/{SLUG}"),
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    assert!(
        skill_rows(&state).await.is_empty(),
        "a toggle recorded a change to a document it never wrote"
    );
}

/// No row carries any part of a skill's text.
///
/// Falsifiable rather than asserted: the fixture's body holds a sentence that
/// appears nowhere else, so a row that ever forwarded the document — or its
/// name, description or a truncated preview — fails here.
#[tokio::test]
async fn no_journal_row_carries_the_skill_document() {
    let home_dir = home();
    let library_dir = home();
    seed_library_skill(library_dir.path(), SLUG, &doc("2.0.0")).await;
    let state = state_with_library(home_dir.path(), library_dir.path()).await;
    let installed = doc("1.0.0");
    seed_skill_delta(&state, &pinned_delta(&installed, &installed)).await;

    for (method, path, body) in [
        (
            "POST",
            format!("/api/v1/company/skills/{SLUG}/update"),
            None::<Value>,
        ),
        (
            "POST",
            format!("/api/v1/company/skills/{SLUG}/uninstall"),
            None,
        ),
    ] {
        let (status, answer) = send(&state, method, &path, body).await;
        assert!(status.is_success(), "{path}: {status} {answer}");
    }

    let rows = skill_rows(&state).await;
    assert_eq!(
        rows.len(),
        2,
        "the two writes journalled, so there is something to inspect: {rows:?}"
    );
    let wire = serde_json::to_string(&rows).expect("serializes");
    assert!(
        !wire.contains(BODY_MARKER),
        "a journal row carried the skill's body: {wire}"
    );
    assert!(
        !wire.contains("A skill the shared library ships"),
        "a journal row carried the skill's description: {wire}"
    );
}
