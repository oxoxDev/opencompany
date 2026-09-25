//! `Company.skills` reports a pinned install's standing the same way
//! `GET …/skills` does.
//!
//! The two transports have disagreed about one install before — issue #239 found
//! REST and GraphQL reporting different `version`s for the same row, which is
//! why both now project [`skill_effective::resolve`](crate::company::skill_effective::resolve)
//! rather than reading the library themselves. `updateAvailable` and `modified`
//! are computed from that same resolution, and these cases are what holds them
//! there.

use std::sync::Arc;

use crate::company::{parse_skill_md, skill_digest};
use crate::ports::skills_state::{SkillInstall, SkillSource, SkillState};
use crate::ports::types::CompanyId;
use crate::server::router;

use super::graphql_test_group_1::query;
use super::graphql_test_support_1::*;

/// A slug the global baseline does not ship, so the row under test is the
/// delta's own.
const SLUG: &str = "library-skill";

const SKILLS_QUERY: &str = r#"{"query":"{ company(id:\"acme\"){ skills { id version modified updateAvailable { from to } } } }"}"#;

fn doc(version: &str, body: &str) -> String {
    format!(
        "---\nname: Library Skill\ndescription: A skill the shared library ships.\ncategory: Ops\nversion: {version}\n---\n{body}\n"
    )
}

/// The library's copy of `SLUG`, in the `companies/`-shaped tree the catalog
/// loader reads.
async fn library(root: &std::path::Path, doc: &str) {
    let dir = root.join("shared").join("skills").join(SLUG);
    tokio::fs::create_dir_all(&dir).await.unwrap();
    tokio::fs::write(dir.join("SKILL.md"), doc).await.unwrap();
}

/// Seeds the row an install of `stored`, pinned to `pinned`, would have written.
///
/// Written straight to the store because no route can reach this state on its
/// own: an install pins the document it just read, so the pin and the library
/// agree by construction until the library is republished.
async fn seed(state: &crate::AppState, stored: &str, pinned: &str) {
    let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();
    runtime
        .skills()
        .set(
            runtime.id(),
            &SkillState {
                slug: SLUG.to_string(),
                enabled: true,
                source: SkillSource::Registry,
                custom_doc: Some(stored.to_string()),
                updated_at_millis: Some(1_700_000_000_000),
                install: Some(SkillInstall {
                    digest: skill_digest(pinned),
                    version: parse_skill_md(SLUG, pinned).unwrap().version,
                    installed_by: None,
                    installed_at_millis: 1_700_000_000_000,
                }),
            },
        )
        .await
        .unwrap();
}

/// Builds the state, its library seeded with `live`, and returns the row for
/// `SLUG` (or for `slug` when one is named) out of `Company.skills`.
async fn row(
    home: &std::path::Path,
    library_root: &std::path::Path,
    live: Option<&str>,
    stored: Option<(&str, &str)>,
    slug: &str,
) -> serde_json::Value {
    if let Some(live) = live {
        library(library_root, live).await;
    }
    let state = state_with_company(home)
        .await
        .with_skills_root(library_root.to_path_buf());
    if let Some((stored, pinned)) = stored {
        seed(&state, stored, pinned).await;
    }
    let value = query(router(state), SKILLS_QUERY).await;
    value["data"]["company"]["skills"]
        .as_array()
        .unwrap_or_else(|| panic!("no skills array in {value}"))
        .iter()
        .find(|row| row["id"] == serde_json::json!(slug))
        .cloned()
        .unwrap_or_else(|| panic!("no `{slug}` row in {value}"))
}

#[tokio::test]
async fn company_skills_offer_an_update_when_the_library_has_moved_on() {
    let home_dir = home();
    let library_dir = home();
    let installed = doc("1.0.0", "# Library Skill\nStep one.");

    let row = row(
        home_dir.path(),
        library_dir.path(),
        Some(&doc("2.0.0", "# Library Skill\nRewritten.")),
        Some((&installed, &installed)),
        SLUG,
    )
    .await;

    assert_eq!(row["version"], "1.0.0", "the pinned snapshot still");
    assert_eq!(row["modified"], serde_json::json!(false));
    assert_eq!(row["updateAvailable"]["from"], "1.0.0");
    assert_eq!(row["updateAvailable"]["to"], "2.0.0");
}

#[tokio::test]
async fn company_skills_report_a_stored_copy_that_no_longer_matches_its_pin() {
    let home_dir = home();
    let library_dir = home();
    let installed = doc("1.0.0", "# Library Skill\nStep one.");
    let edited = installed.replace("Step one.", "Do it the way we do it here.");

    let row = row(
        home_dir.path(),
        library_dir.path(),
        Some(&installed),
        Some((&edited, &installed)),
        SLUG,
    )
    .await;

    assert_eq!(row["modified"], serde_json::json!(true));
    assert_eq!(
        row["updateAvailable"],
        serde_json::Value::Null,
        "the library has not moved, so there is nothing to offer"
    );
}

/// A baseline row pinned nothing. `updateAvailable` is null — GraphQL has no
/// absent field — and `modified` is a plain `false` rather than nullable, so a
/// reader is never handed "unknown".
#[tokio::test]
async fn company_skills_report_an_unpinned_row_as_unmodified_with_no_update() {
    let home_dir = home();
    let library_dir = home();
    let baseline = crate::globals::skills()
        .first()
        .expect("the baseline ships at least one skill")
        .slug
        .clone();

    let row = row(home_dir.path(), library_dir.path(), None, None, &baseline).await;

    assert_eq!(row["updateAvailable"], serde_json::Value::Null);
    assert_eq!(row["modified"], serde_json::json!(false));
}

/// Both transports over one state, asserted against each other rather than
/// against two copies of the same expectation.
#[tokio::test]
async fn rest_and_graphql_agree_about_one_install_standing() {
    let home_dir = home();
    let library_dir = home();
    let installed = doc("1.0.0", "# Library Skill\nStep one.");
    library(
        library_dir.path(),
        &doc("2.0.0", "# Library Skill\nRewritten."),
    )
    .await;
    let state = state_with_company(home_dir.path())
        .await
        .with_skills_root(library_dir.path().to_path_buf());
    seed(&state, &installed, &installed).await;

    let gql = query(router(state.clone()), SKILLS_QUERY).await;
    let gql_row = gql["data"]["company"]["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == serde_json::json!(SLUG))
        .cloned()
        .unwrap();

    let deltas = {
        let runtime: Arc<_> = state.registry().get(&CompanyId::new("acme")).unwrap();
        runtime.skills().list(runtime.id()).await.unwrap()
    };
    let registry = state.shared_skill_registry().unwrap();
    let effective = crate::company::skill_effective::resolve(None, &registry, &deltas).unwrap();
    let entry = effective.iter().find(|e| e.slug == SLUG).unwrap();
    let drifted = crate::company::effective_drift(entry, &registry).expect("a pinned install");

    assert_eq!(gql_row["modified"], serde_json::json!(drifted.modified));
    let change = drifted.update_available.expect("the library moved");
    assert_eq!(
        gql_row["updateAvailable"]["from"],
        serde_json::json!(change.from)
    );
    assert_eq!(
        gql_row["updateAvailable"]["to"],
        serde_json::json!(change.to)
    );
}
