//! The audit line a skill write leaves behind.
//!
//! The store keeps **one row per slug and rewrites it in place**, so nothing in
//! it says a change happened — only what the skill is now. The journal is the
//! entire record that someone installed, replaced or removed a document that
//! joins every agent's prompt company-wide, which is why an append that fails
//! fails the write rather than being logged and dropped.
//!
//! No document text goes on a row. The digest is the anchor: it identifies the
//! document written without putting a skill's procedure — or whatever an
//! operator pasted into one — into a permanent journal.
//!
//! A **toggle appends nothing**, deliberately. [`SkillChange`] has no variant
//! for it, a toggle writes no document, and recording one as `Updated` would
//! tell an audit reader that a re-pin happened when none did.

use crate::company::runtime::CompanyRuntime;
use crate::company::{skill_digest, trust_tier};
use crate::ports::skills_state::{SkillSource, SkillState};
use crate::ports::types::{Actor, CompanyEvent, SkillChange};
use crate::server::error::ApiError;

/// Records a write that stored a document.
///
/// Takes the delta rather than its parts so slug, provenance and digest come
/// from the row that was actually persisted — a call site cannot describe a
/// write it did not perform.
pub(super) async fn journal_write(
    runtime: &CompanyRuntime,
    actor: &Actor,
    delta: &SkillState,
    change: SkillChange,
) -> Result<(), ApiError> {
    append(
        runtime,
        actor,
        &delta.slug,
        change,
        delta.source,
        delta.custom_doc.as_deref().map(skill_digest),
    )
    .await
}

/// Records a removal, which writes no document and so anchors to none.
pub(super) async fn journal_removal(
    runtime: &CompanyRuntime,
    actor: &Actor,
    slug: &str,
    source: SkillSource,
) -> Result<(), ApiError> {
    append(runtime, actor, slug, SkillChange::Removed, source, None).await
}

/// Appends one `SkillChanged` row.
///
/// The tier is derived here rather than passed in: it is
/// [`trust_tier`] of the row's own provenance plus whether the global baseline
/// ships the slug, and deriving it once means no call site can label a skill
/// with a tier it did not earn.
async fn append(
    runtime: &CompanyRuntime,
    actor: &Actor,
    slug: &str,
    change: SkillChange,
    source: SkillSource,
    digest: Option<String>,
) -> Result<(), ApiError> {
    let from_baseline = crate::globals::skills().iter().any(|doc| doc.slug == slug);
    runtime
        .events()
        .append(
            runtime.id(),
            CompanyEvent::SkillChanged {
                slug: slug.to_string(),
                change,
                tier: trust_tier(source, from_baseline),
                digest,
                by: Some(actor.clone()),
            },
        )
        .await
        .map_err(ApiError)?;
    Ok(())
}
