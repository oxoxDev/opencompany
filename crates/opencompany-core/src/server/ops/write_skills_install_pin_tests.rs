//! The install pin's write-plane guarantees: an install records one, and no
//! later write that is not itself an install may erase it.

use axum::http::StatusCode;
use serde_json::json;

use super::write_test_support::*;
use crate::ports::skills_state::{SkillSource, SkillState};

/// The one row `slug` names, read back from the store rather than from a
/// handler's answer — a projection can look right over a row that is wrong.
async fn stored(state: &crate::AppState, slug: &str) -> SkillState {
    persisted_skills(state)
        .await
        .into_iter()
        .find(|row| row.slug == slug)
        .unwrap_or_else(|| panic!("no stored row for {slug}"))
}

async fn install_competitor_scan(state: &crate::AppState) {
    let (status, _) = send(
        state,
        "POST",
        "/api/v1/company/skills/competitor-scan/install",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

async fn set_enabled(state: &crate::AppState, enabled: bool) {
    let (status, _) = send(
        state,
        "PUT",
        "/api/v1/company/skills/competitor-scan",
        Some(json!({ "enabled": enabled })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "toggle to {enabled}");
}

/// Installing from the library pins what was installed. Without this the
/// whole drift feature is uncomputable, not merely unexposed.
#[tokio::test]
async fn installing_from_the_library_records_what_was_pinned() {
    let home_dir = home();
    let state = state_with_registry(home_dir.path()).await;

    install_competitor_scan(&state).await;

    let install = stored(&state, "competitor-scan")
        .await
        .install
        .expect("the install recorded a pin");
    assert_eq!(
        install.digest.len(),
        64,
        "the digest is hex SHA-256: {}",
        install.digest
    );
    assert!(
        install.digest.chars().all(|c| c.is_ascii_hexdigit()),
        "the digest is lowercase hex: {}",
        install.digest
    );
    assert_eq!(install.version.as_deref(), Some("1.0.0"));
    assert!(
        install.installed_by.is_some(),
        "an admin route always knows who acted"
    );
    assert!(install.installed_at_millis > 0);
}

/// A host with no shared library writes a Custom document of its own making,
/// so there is nothing it could honestly claim to have pinned.
#[tokio::test]
async fn installing_without_a_library_pins_nothing() {
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

    let row = stored(&state, "desk-notes").await;
    assert_eq!(row.source, SkillSource::Custom);
    assert_eq!(
        row.install, None,
        "nothing came from a library, so nothing can be diffed against one"
    );
}

/// Disabling and re-enabling an installed skill must not erase what it pinned.
///
/// Two toggles, not one: the round trip is the shape an operator actually
/// performs, and it leaves no room for a re-pin on enable to mask the loss.
/// The assertion is whole-struct against the pin the install wrote, because a
/// handler that mints a fresh pin — today's timestamp, a different installer —
/// passes `is_some()` while having destroyed the record.
#[tokio::test]
async fn toggling_a_skill_off_and_on_keeps_its_install_pin() {
    let home_dir = home();
    let state = state_with_registry(home_dir.path()).await;

    install_competitor_scan(&state).await;
    let pinned = stored(&state, "competitor-scan")
        .await
        .install
        .expect("the install recorded a pin");

    set_enabled(&state, false).await;
    set_enabled(&state, true).await;

    let after = stored(&state, "competitor-scan").await;
    assert!(after.enabled, "the round trip ends enabled");
    assert_eq!(
        after.install,
        Some(pinned),
        "the toggle rewrote the row and dropped what it had pinned"
    );
}

/// A toggle writes no document, so `enabled` and the write stamp are the only
/// fields it may move.
///
/// This is the invariant rather than the instance: it catches the *next* field
/// a write path forgets to forward, which is the failure that actually
/// recurs — `source` and `custom_doc` were carried through while `install` was
/// not.
#[tokio::test]
async fn a_toggle_moves_only_enabled_and_the_write_stamp() {
    let home_dir = home();
    let state = state_with_registry(home_dir.path()).await;

    install_competitor_scan(&state).await;
    let before = stored(&state, "competitor-scan").await;

    set_enabled(&state, false).await;

    let after = stored(&state, "competitor-scan").await;
    assert!(!after.enabled);
    assert_eq!(
        SkillState {
            enabled: before.enabled,
            updated_at_millis: before.updated_at_millis,
            ..after
        },
        before,
        "a toggle changed a field it does not own"
    );
}
