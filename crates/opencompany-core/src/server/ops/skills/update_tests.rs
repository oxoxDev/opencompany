//! The update route: what it refuses, and what it leaves in the store when it
//! accepts.
//!
//! Each refusal is asserted against the exact sentence in
//! [`language`](crate::server::ops::language), not a status code. Four different
//! things are being declined and they ask the operator for four different
//! actions; a shared 409 would collapse them back into "can't update this".

use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::AppState;
use crate::company::{render_skill_md, skill_digest};
use crate::ports::skills_state::{SkillInstall, SkillSource, SkillState};
use crate::server::ops::language;
use crate::server::ops::write_test_support::*;

/// A slug the global baseline does not ship, so the row under test is the
/// delta's own.
const SLUG: &str = "library-skill";

fn doc(version: &str, body: &str) -> String {
    format!(
        "---\nname: Library Skill\ndescription: A skill the shared library ships.\ncategory: Ops\nversion: {version}\n---\n{body}\n"
    )
}

fn v1() -> String {
    doc("1.0.0", "# Library Skill\nStep one.")
}

fn v2() -> String {
    doc("2.0.0", "# Library Skill\nRewritten upstream.")
}

/// The row an install of `stored` pinned to `pinned` would have written.
fn pinned_delta(stored: &str, pinned: &str, enabled: bool) -> SkillState {
    SkillState {
        slug: SLUG.to_string(),
        enabled,
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

/// The refusal sentence out of the error envelope.
///
/// The envelope prefixes the variant's own word (`conflict: …`), so the
/// assertion is containment of the exact sentence rather than equality with a
/// string that would also pin the prefix.
fn refusal(body: &Value) -> String {
    body["error"]
        .as_str()
        .unwrap_or_else(|| panic!("no error sentence in {body}"))
        .to_string()
}

async fn post_update(state: &AppState) -> (StatusCode, Value) {
    send(
        state,
        "POST",
        &format!("/api/v1/company/skills/{SLUG}/update"),
        None,
    )
    .await
}

async fn stored_row(state: &AppState) -> SkillState {
    persisted_skills(state)
        .await
        .into_iter()
        .find(|row| row.slug == SLUG)
        .unwrap_or_else(|| panic!("no stored row for {SLUG}"))
}

/// Nothing is installed under the slug at all.
#[tokio::test]
async fn updating_a_skill_that_is_not_installed_says_it_was_never_pinned() {
    let home_dir = home();
    let library_dir = home();
    seed_library_skill(library_dir.path(), SLUG, &v2()).await;
    let state = state_with_library(home_dir.path(), library_dir.path()).await;

    let (status, body) = post_update(&state).await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        refusal(&body).contains(language::SKILL_NOT_PINNED),
        "{body}"
    );
}

/// A console-authored skill has a row but no pin: there is no library copy for
/// it to be moved to, and saying "not found" about a skill in the list teaches
/// the operator nothing.
#[tokio::test]
async fn updating_an_authored_skill_says_it_was_never_pinned() {
    let home_dir = home();
    let library_dir = home();
    seed_library_skill(library_dir.path(), SLUG, &v2()).await;
    let state = state_with_library(home_dir.path(), library_dir.path()).await;
    seed_skill_delta(
        &state,
        &SkillState {
            slug: SLUG.to_string(),
            enabled: true,
            source: SkillSource::Custom,
            custom_doc: Some(v1()),
            updated_at_millis: Some(1_700_000_000_000),
            install: None,
        },
    )
    .await;

    let (status, body) = post_update(&state).await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        refusal(&body).contains(language::SKILL_NOT_PINNED),
        "{body}"
    );
}

/// The slug has left the library. Not a broken install — it keeps working from
/// the copy it holds, and the sentence says so.
#[tokio::test]
async fn updating_an_install_whose_slug_left_the_library_says_so() {
    let home_dir = home();
    let library_dir = home();
    let state = state_with_library(home_dir.path(), library_dir.path()).await;
    seed_skill_delta(&state, &pinned_delta(&v1(), &v1(), true)).await;

    let (status, body) = post_update(&state).await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        refusal(&body).contains(language::SKILL_LEFT_REGISTRY),
        "{body}"
    );
}

/// The stored copy was edited after it was pinned, so the update refuses rather
/// than overwrite an edit that exists nowhere else — and refuses it even though
/// the library has *also* moved on, which is the case that would otherwise read
/// as an ordinary available update.
#[tokio::test]
async fn updating_a_locally_edited_copy_refuses_and_stores_nothing() {
    let home_dir = home();
    let library_dir = home();
    seed_library_skill(library_dir.path(), SLUG, &v2()).await;
    let state = state_with_library(home_dir.path(), library_dir.path()).await;
    let edited = v1().replace("Step one.", "Do it the way we do it here.");
    seed_skill_delta(&state, &pinned_delta(&edited, &v1(), true)).await;

    let (status, body) = post_update(&state).await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        refusal(&body).contains(language::SKILL_MODIFIED_NO_UPDATE),
        "{body}"
    );
    assert_eq!(
        stored_row(&state).await.custom_doc.as_deref(),
        Some(edited.as_str()),
        "the refusal must leave the operator's edit exactly as it was"
    );
}

/// The install already matches the library.
#[tokio::test]
async fn updating_a_current_install_says_there_is_nothing_to_do() {
    let home_dir = home();
    let library_dir = home();
    seed_library_skill(library_dir.path(), SLUG, &v1()).await;
    let state = state_with_library(home_dir.path(), library_dir.path()).await;
    let library_render = {
        let library = crate::company::load_catalog_skills(library_dir.path()).expect("library");
        render_skill_md(library.iter().find(|doc| doc.slug == SLUG).expect("entry"))
    };
    seed_skill_delta(
        &state,
        &pinned_delta(&library_render, &library_render, true),
    )
    .await;

    let (status, body) = post_update(&state).await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        refusal(&body).contains(language::SKILL_ALREADY_CURRENT),
        "{body}"
    );
}

/// The accept path: the store holds the library's render, the pin moves to it,
/// and the answer no longer offers the update it just applied.
#[tokio::test]
async fn updating_re_pins_the_install_to_the_library_document() {
    let home_dir = home();
    let library_dir = home();
    seed_library_skill(library_dir.path(), SLUG, &v2()).await;
    let state = state_with_library(home_dir.path(), library_dir.path()).await;
    seed_skill_delta(&state, &pinned_delta(&v1(), &v1(), true)).await;

    let (status, body) = post_update(&state).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let library = crate::company::load_catalog_skills(library_dir.path()).expect("library");
    let live = library.iter().find(|doc| doc.slug == SLUG).expect("entry");
    let expected = render_skill_md(live);

    let row = stored_row(&state).await;
    assert_eq!(
        row.custom_doc.as_deref(),
        Some(expected.as_str()),
        "the store holds the library's own document"
    );
    let install = row.install.expect("the update re-pinned");
    assert_eq!(install.digest, skill_digest(&expected));
    assert_eq!(install.version.as_deref(), Some("2.0.0"));
    assert!(
        install.installed_by.is_some(),
        "an admin route always knows who acted"
    );

    assert_eq!(body["version"], "2.0.0");
    assert!(
        body.get("updateAvailable").is_none(),
        "the badge must not survive its own fix: {body}"
    );
    assert_eq!(body["modified"], json!(false));
    assert!(
        body["scan"].is_object(),
        "the document was re-scanned on the way in: {body}"
    );
}

/// An update takes a newer document. It says nothing about wanting a skill the
/// operator switched off switched back on.
#[tokio::test]
async fn updating_a_disabled_install_leaves_it_disabled() {
    let home_dir = home();
    let library_dir = home();
    seed_library_skill(library_dir.path(), SLUG, &v2()).await;
    let state = state_with_library(home_dir.path(), library_dir.path()).await;
    seed_skill_delta(&state, &pinned_delta(&v1(), &v1(), false)).await;

    let (status, body) = post_update(&state).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], json!(false));
    assert!(
        !stored_row(&state).await.enabled,
        "the update re-enabled a skill nobody asked it to"
    );
}

/// The same admin gate every other skill write sits behind: a document that
/// joins every agent's prompt is not a member's call.
#[tokio::test]
async fn a_member_cannot_update_a_skill() {
    let home_dir = home();
    let library_dir = home();
    seed_library_skill(library_dir.path(), SLUG, &v2()).await;
    let state = state_with_library(home_dir.path(), library_dir.path()).await;
    seed_skill_delta(&state, &pinned_delta(&v1(), &v1(), true)).await;
    crate::server::test_support::seed_fixed_member(&state, "acme").await;

    let (status, _) = send_cookie(
        &state,
        "POST",
        &format!("/api/v1/company/skills/{SLUG}/update"),
        None,
        &crate::server::test_support::member_cookie("acme"),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        stored_row(&state).await.custom_doc.as_deref(),
        Some(v1().as_str()),
        "a refused request must not have written"
    );
}
