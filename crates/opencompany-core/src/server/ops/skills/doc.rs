//! `GET`/`PUT …/skills/{slug}/doc` — a skill's `SKILL.md`, served and rewritten.
//!
//! Every other read in this folder is metadata: a name, a description, a
//! category. The document itself — the procedure an agent actually follows — had
//! no address, which is why the console could only ever offer a greyed-out Edit.
//! An editor that loaded metadata alone would save it back over a body it never
//! read.
//!
//! ## Reading is a member's read; writing is the company's decision
//!
//! The document joins every agent's prompt company-wide, so any member may read
//! what their teammates are told to do, and only an admin may change it — the
//! same split `GET …/skills` and the writes beside it already draw.
//!
//! ## Which skills a write may touch
//!
//! [`SkillSource::Custom`] and [`SkillSource::Registry`], the same two arms
//! `uninstall` accepts, and for the same reason: a [`SkillSource::Company`]
//! skill is authored in the repository — the global baseline or this company's
//! own bundle — and its document travels with sibling resource files a stored
//! delta cannot carry. Storing an inline body over it would silently drop them.
//!
//! Editing a registry install is the local-copy flow rather than a separate
//! mechanism: the stored snapshot diverges from the digest the install pinned,
//! so the row reports `modified`, and `update` then refuses to overwrite the
//! edit instead of discarding it.

use std::fs;

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::company::skill_effective::{self, SkillBody};
use crate::company::skill_scope::agents_for_skill;
use crate::company::skill_validate::validate_slug;
use crate::error::OpenCompanyError;
use crate::ports::now_millis;
use crate::ports::skills_state::{SkillSource, SkillState};
use crate::ports::types::SkillChange;
use crate::server::error::ApiError;
use crate::server::ops::language;
use crate::server::ops::{AdminScopedCompany, ScopedCompany, scoped};

use super::drift::row_drift;
use super::journal::journal_write;
use super::{InstalledSkill, SlugPath, check_skill_doc_size, vet_skill, write_lock};

/// Builds the document route fragment.
pub(super) fn router() -> Router<AppState> {
    scoped("/skills/{slug}/doc", get(read_doc).put(write_doc))
}

/// A skill's document as the console's editor loads it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SkillDocDto {
    slug: String,
    /// The whole `SKILL.md` — frontmatter and body — exactly as an agent reads
    /// it.
    markdown: String,
    /// Whether a `PUT` here would be accepted at all, provenance aside from the
    /// caller's own role.
    ///
    /// Served rather than re-derived in the console, so the editor it offers and
    /// the write it would make cannot disagree about which skills are editable.
    editable: bool,
}

/// The write body.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriteDoc {
    /// The replacement `SKILL.md`, whole. Not a patch: the stored document is
    /// what every agent reads, so a partial write would leave the authoritative
    /// copy somewhere between two versions.
    markdown: String,
    /// Save despite a blocking scan verdict, for this request only.
    #[serde(default)]
    force: bool,
}

/// Whether a stored delta over this skill may have its document rewritten here.
fn editable(source: SkillSource) -> bool {
    matches!(source, SkillSource::Registry | SkillSource::Custom)
}

/// `GET …/skills/{slug}/doc` — the document the company's agents read for this
/// slug.
///
/// Resolved through [`skill_effective::resolve`], the same derivation
/// `GET …/skills` reports from, so the editor opens on what is actually in
/// effect rather than on whichever layer happened to be consulted. A bundle
/// entry is read off disk; every other layer already carries its rendered text.
async fn read_doc(
    State(state): State<AppState>,
    company: ScopedCompany,
    Path(SlugPath { slug }): Path<SlugPath>,
) -> Result<Json<SkillDocDto>, ApiError> {
    if let Err(problem) = validate_slug(&slug) {
        return Err(ApiError(OpenCompanyError::InvalidRequest(problem)));
    }
    let mut deltas = company.runtime.skills().list(company.id()).await?;
    deltas.extend(skill_effective::globals_skill_disables(
        &company.runtime.globals_disable().await?,
    ));
    let registry = state.shared_skill_registry()?;
    let effective = skill_effective::resolve(company.runtime.source_dir(), &registry, &deltas)?;
    let found = effective
        .into_iter()
        .find(|skill| skill.slug == slug)
        .ok_or_else(|| {
            ApiError(OpenCompanyError::NotFound(
                language::SKILL_NO_DOC.to_string(),
            ))
        })?;
    // A row no layer supplied a document for reaches no agent either, so there
    // is nothing to serve and nothing an editor could usefully open.
    let content = found.content.as_ref().ok_or_else(|| {
        ApiError(OpenCompanyError::NotFound(
            language::SKILL_NO_DOC.to_string(),
        ))
    })?;
    let markdown = match &content.body {
        SkillBody::Inline(text) => text.clone(),
        SkillBody::Bundle(dir) => {
            let path = dir.join("SKILL.md");
            fs::read_to_string(&path)
                .map_err(|source| ApiError(OpenCompanyError::DataRead { path, source }))?
        }
    };
    Ok(Json(SkillDocDto {
        slug,
        markdown,
        editable: editable(found.source),
    }))
}

/// `PUT …/skills/{slug}/doc` — replace the stored document.
///
/// Held to every gate a write path shares: the size cap, the shared validator
/// and the content scan, under the same per-company lock, so a document that an
/// install of the same text would have been refused cannot arrive through the
/// editor instead.
///
/// The row's `install` pin is carried through untouched. That is what makes the
/// edit visible: the pin still names what the library shipped, the stored
/// document no longer matches it, and `modified` reports the difference.
async fn write_doc(
    State(app): State<AppState>,
    company: AdminScopedCompany,
    Path(SlugPath { slug }): Path<SlugPath>,
    Json(body): Json<WriteDoc>,
) -> Result<Json<InstalledSkill>, ApiError> {
    if let Err(problem) = validate_slug(&slug) {
        return Err(ApiError(OpenCompanyError::InvalidRequest(problem)));
    }
    let lock = write_lock(company.id());
    let _guard = lock.lock().await;
    let existing = company
        .runtime
        .skills()
        .list(company.id())
        .await?
        .into_iter()
        .find(|row| row.slug == slug);
    let Some(row) = existing.filter(|row| editable(row.source)) else {
        return Err(ApiError(OpenCompanyError::Conflict(
            language::SKILL_DOC_NOT_EDITABLE.to_string(),
        )));
    };
    check_skill_doc_size(&body.markdown)?;
    let scan = vet_skill(&slug, &body.markdown, body.force).map_err(ApiError::from)?;
    let delta = SkillState {
        slug,
        // An edit says nothing about wanting a disabled skill switched on.
        enabled: row.enabled,
        source: row.source,
        custom_doc: Some(body.markdown),
        install: row.install,
        updated_at_millis: Some(now_millis()),
    };
    company.runtime.skills().set(company.id(), &delta).await?;
    journal_write(
        &company.runtime,
        &company.actor(),
        &delta,
        SkillChange::Edited,
    )
    .await?;
    let drift = row_drift(&app.shared_skill_registry()?, &delta);
    let roster = super::scope::roster_scopes(&company.runtime).await?;
    Ok(Json(
        InstalledSkill::from_state(&delta)
            .with_scan(scan)
            .with_drift(drift)
            .with_agents(agents_for_skill(&delta.slug, delta.enabled, &roster)),
    ))
}

#[cfg(test)]
#[path = "doc_tests.rs"]
mod tests;
