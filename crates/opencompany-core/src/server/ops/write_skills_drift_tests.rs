//! What `GET …/skills` says about where a pinned install stands.
//!
//! The pin is only worth recording if a reader can act on it, and the reader is
//! this list. Each case here is one of the three answers an operator can get:
//! the library moved on, the stored copy was edited, or the question does not
//! apply to this row at all.

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::write_test_support::*;
use crate::company::{render_skill_md, skill_digest};
use crate::ports::skills_state::{SkillInstall, SkillSource, SkillState};

/// A slug the global baseline does not ship, so the row under test is the
/// delta's own and not a baseline entry a delta happens to supersede.
const SLUG: &str = "library-skill";

fn doc(version: &str, body: &str) -> String {
    format!(
        "---\nname: Library Skill\ndescription: A skill the shared library ships.\ncategory: Ops\nversion: {version}\n---\n{body}\n"
    )
}

/// The row an install of `stored` would have written, pinned to `pinned`.
fn pinned_delta(stored: &str, pinned: &str) -> SkillState {
    SkillState {
        slug: SLUG.to_string(),
        enabled: true,
        source: SkillSource::Registry,
        custom_doc: Some(stored.to_string()),
        updated_at_millis: Some(1_700_000_000_000),
        install: Some(SkillInstall {
            digest: skill_digest(pinned),
            version: version_of(pinned),
            installed_by: None,
            installed_at_millis: 1_700_000_000_000,
        }),
    }
}

/// The `version` frontmatter of `doc`, read the way the install path reads it.
fn version_of(doc: &str) -> Option<String> {
    crate::company::parse_skill_md(SLUG, doc)
        .expect("the fixture parses")
        .version
}

async fn listed(state: &crate::AppState, slug: &str) -> Value {
    let (status, body) = send(state, "GET", "/api/v1/company/skills", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body.as_array()
        .expect("the list is an array")
        .iter()
        .find(|row| row["id"] == json!(slug))
        .cloned()
        .unwrap_or_else(|| panic!("no `{slug}` row in {body}"))
}

/// The library republished the skill. The install still serves its own snapshot,
/// and the list is where the operator learns a newer document exists.
#[tokio::test]
async fn the_list_offers_an_update_when_the_library_has_moved_on() {
    let home_dir = home();
    let library_dir = home();
    let installed = doc("1.0.0", "# Library Skill\nStep one.");
    seed_library_skill(
        library_dir.path(),
        SLUG,
        &doc("2.0.0", "# Library Skill\nRewritten."),
    )
    .await;
    let state = state_with_library(home_dir.path(), library_dir.path()).await;
    seed_skill_delta(&state, &pinned_delta(&installed, &installed)).await;

    let row = listed(&state, SLUG).await;

    assert_eq!(
        row["updateAvailable"],
        json!({"from": "1.0.0", "to": "2.0.0"})
    );
    assert_eq!(
        row["modified"],
        json!(false),
        "nothing edited the stored copy"
    );
    assert_eq!(
        row["version"], "1.0.0",
        "the row still describes the pinned snapshot"
    );
}

/// The stored copy was edited after it was pinned. The list must say so, and
/// must not also offer an update — applying one would discard the edit.
#[tokio::test]
async fn the_list_reports_a_stored_copy_that_no_longer_matches_its_pin() {
    let home_dir = home();
    let library_dir = home();
    let installed = doc("1.0.0", "# Library Skill\nStep one.");
    seed_library_skill(library_dir.path(), SLUG, &installed).await;
    let state = state_with_library(home_dir.path(), library_dir.path()).await;
    let edited = installed.replace("Step one.", "Do it the way we do it here.");
    seed_skill_delta(&state, &pinned_delta(&edited, &installed)).await;

    let row = listed(&state, SLUG).await;

    assert_eq!(row["modified"], json!(true));
    assert_eq!(
        row["updateAvailable"],
        Value::Null,
        "the library has not moved, so there is nothing to offer"
    );
}

/// A baseline row pinned nothing, so `updateAvailable` is absent rather than
/// null and `modified` is a plain `false`.
///
/// The absence is the assertion: a row carrying `updateAvailable: null` would be
/// indistinguishable from a pinned install that was checked and found current,
/// and the console would have to guess which it was looking at.
#[tokio::test]
async fn a_baseline_row_carries_no_update_field_and_is_not_modified() {
    let home_dir = home();
    let library_dir = home();
    let state = state_with_library(home_dir.path(), library_dir.path()).await;
    let baseline = crate::globals::skills()
        .first()
        .expect("the baseline ships at least one skill")
        .slug
        .clone();

    let row = listed(&state, &baseline).await;

    assert!(
        row.get("updateAvailable").is_none(),
        "a row with nothing pinned must not claim to have been checked: {row}"
    );
    assert_eq!(
        row["modified"],
        json!(false),
        "always present, so the console never reads it as unknown: {row}"
    );
}

/// A toggle answers with the drift the row still stands at.
///
/// The console folds a write response straight into the list it is showing, so a
/// toggle that answered with the defaults would clear the badge next to the
/// switch the operator just flipped.
#[tokio::test]
async fn a_toggle_answers_with_the_drift_the_row_still_stands_at() {
    let home_dir = home();
    let library_dir = home();
    let installed = doc("1.0.0", "# Library Skill\nStep one.");
    seed_library_skill(
        library_dir.path(),
        SLUG,
        &doc("2.0.0", "# Library Skill\nRewritten."),
    )
    .await;
    let state = state_with_library(home_dir.path(), library_dir.path()).await;
    seed_skill_delta(&state, &pinned_delta(&installed, &installed)).await;

    let (status, body) = send(
        &state,
        "PUT",
        &format!("/api/v1/company/skills/{SLUG}"),
        Some(json!({ "enabled": false })),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], json!(false));
    assert_eq!(
        body["updateAvailable"],
        json!({"from": "1.0.0", "to": "2.0.0"})
    );
}

/// An install answers with drift it can state without a second comparison: it
/// has just pinned the library's current document and stored that same render.
#[tokio::test]
async fn an_install_answers_current_and_unmodified() {
    let home_dir = home();
    let library_dir = home();
    seed_library_skill(
        library_dir.path(),
        SLUG,
        &doc("1.0.0", "# Library Skill\nStep one."),
    )
    .await;
    let state = state_with_library(home_dir.path(), library_dir.path()).await;

    let (status, body) = send(
        &state,
        "POST",
        &format!("/api/v1/company/skills/{SLUG}/install"),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.get("updateAvailable").is_none(),
        "a fresh install is current by construction: {body}"
    );
    assert_eq!(body["modified"], json!(false));
    // The pin the answer describes is the one the library render produced.
    let row = persisted_skills(&state)
        .await
        .into_iter()
        .find(|row| row.slug == SLUG)
        .expect("the install stored a row");
    let library = crate::company::load_catalog_skills(library_dir.path()).expect("the library");
    let live = library
        .iter()
        .find(|doc| doc.slug == SLUG)
        .expect("the library entry");
    assert_eq!(
        row.install.expect("the install pinned").digest,
        skill_digest(&render_skill_md(live))
    );
}
