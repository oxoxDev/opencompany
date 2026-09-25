//! `POST …/skills/{slug}/update` — move a registry install onto the library's
//! current document.
//!
//! An install pins a snapshot, so a library edit never reaches an existing
//! install on its own. That is deliberate: a document that joins every agent's
//! prompt company-wide must not change under a company because someone
//! republished it. This route is the other half of that decision — the explicit
//! act that takes the newer document, with the operator having seen what moves.
//!
//! ## Four refusals, each naming itself
//!
//! There is no newer document to take (`SKILL_NOT_PINNED`,
//! `SKILL_LEFT_REGISTRY`, `SKILL_ALREADY_CURRENT`), or taking it would destroy
//! something (`SKILL_MODIFIED_NO_UPDATE`). They are separate sentences rather
//! than one "can't update this" because they ask the operator for four different
//! things — and the modified case asks for a decision no route may make for
//! them, since the edit it would overwrite exists nowhere else.
//!
//! The yes/no itself is [`SkillDrift::update_allowed`], the same predicate the
//! reads project and the console mirrors. A second copy of that rule here would
//! be a rule that drifts.
//!
//! ## An update is a write, held to a write's gates
//!
//! The library's document is re-rendered, size-checked and **re-scanned** before
//! it is stored. A library entry that has since grown something the content scan
//! refuses must not reach a company through an update when an install of the
//! same slug would have been refused. `enabled` is carried from the existing row:
//! taking a newer document says nothing about wanting a disabled skill switched
//! back on.

use axum::extract::{Path, State};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;

use crate::AppState;
use crate::company::skill_validate::validate_slug;
use crate::company::{drift, render_skill_md, skill_digest};
use crate::error::OpenCompanyError;
use crate::ports::now_millis;
use crate::ports::skills_state::{SkillInstall, SkillSource, SkillState};
use crate::ports::types::SkillChange;
use crate::server::error::ApiError;
use crate::server::ops::language;
use crate::server::ops::{AdminScopedCompany, scoped};

use super::drift::row_drift;
use super::journal::journal_write;
use super::{InstalledSkill, SlugPath, check_skill_doc_size, vet_skill, write_lock};

/// Builds the update route fragment.
pub(super) fn router() -> Router<AppState> {
    scoped("/skills/{slug}/update", post(update))
}

/// The update body. Optional in full — there is nothing to say but the override.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateSkill {
    /// Re-pin despite a blocking scan verdict, for this request only.
    #[serde(default)]
    force: bool,
}

/// One of the four refusals, as a conflict rather than a not-found: the skill
/// exists and the operator is looking at it; what is being declined is the move.
fn refuse(sentence: &str) -> ApiError {
    ApiError(OpenCompanyError::Conflict(sentence.to_string()))
}

async fn update(
    State(app): State<AppState>,
    company: AdminScopedCompany,
    Path(SlugPath { slug }): Path<SlugPath>,
    body: Option<Json<UpdateSkill>>,
) -> Result<Json<InstalledSkill>, ApiError> {
    if let Err(problem) = validate_slug(&slug) {
        return Err(ApiError(OpenCompanyError::InvalidRequest(problem)));
    }
    let force = body.map(|Json(body)| body).unwrap_or_default().force;
    // The second genuine read-modify-write on this store, after the toggle: the
    // decision is made against the row that is read and then written back, so an
    // install or an upload landing inside that window would be reverted.
    let lock = write_lock(company.id());
    let _guard = lock.lock().await;
    let registry = app.shared_skill_registry()?;

    let row = company
        .runtime
        .skills()
        .list(company.id())
        .await?
        .into_iter()
        .find(|row| row.slug == slug)
        .ok_or_else(|| refuse(language::SKILL_NOT_PINNED))?;
    let install = row
        .install
        .as_ref()
        .ok_or_else(|| refuse(language::SKILL_NOT_PINNED))?;
    let live = registry
        .iter()
        .find(|doc| doc.slug == slug)
        .ok_or_else(|| refuse(language::SKILL_LEFT_REGISTRY))?;

    let stood = drift(
        install,
        row.custom_doc.as_deref().unwrap_or_default(),
        Some(live),
    );
    if !stood.update_allowed() {
        return Err(refuse(if stood.modified {
            language::SKILL_MODIFIED_NO_UPDATE
        } else {
            language::SKILL_ALREADY_CURRENT
        }));
    }

    let doc = render_skill_md(live);
    check_skill_doc_size(&doc)?;
    let scan = vet_skill(&slug, &doc, force).map_err(ApiError::from)?;
    let delta = SkillState {
        slug,
        // Carried, not set: an update takes a newer document and decides nothing
        // about whether the skill is switched on.
        enabled: row.enabled,
        source: SkillSource::Registry,
        install: Some(SkillInstall {
            digest: skill_digest(&doc),
            version: live.version.clone(),
            installed_by: Some(company.actor()),
            installed_at_millis: now_millis(),
        }),
        custom_doc: Some(doc),
        updated_at_millis: Some(now_millis()),
    };
    company.runtime.skills().set(company.id(), &delta).await?;
    journal_write(
        &company.runtime,
        &company.actor(),
        &delta,
        SkillChange::Updated,
    )
    .await?;
    // The drift after the write, not before it: the answer is what the console
    // folds into the row it is showing, and a badge that survived its own fix
    // would send the operator round again.
    let stood = row_drift(&registry, &delta);
    Ok(Json(
        InstalledSkill::from_state(&delta)
            .with_scan(scan)
            .with_drift(stood),
    ))
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
