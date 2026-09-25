//! Where a pinned install stands, as the write plane reports it.
//!
//! The comparison itself is [`skill_provenance`](crate::company::skill_provenance):
//! [`effective_drift`](crate::company::effective_drift) for the list, which reads
//! one entry of the shared [`resolve`](crate::company::skill_effective::resolve)
//! derivation, and [`row_drift`] here for a write response, which has the row it
//! just stored and no reason to resolve the whole company to describe it.
//!
//! Both end in [`drift`](crate::company::drift), so the list and the write
//! response cannot disagree about one install.
//!
//! Write responses carry the fields at all because the console folds a response
//! row straight into the list it is showing. A handler that answered without
//! them would blank the badge the operator is looking at — an upload over a
//! pinned skill makes it `modified`, which is exactly the moment the badge has
//! to appear rather than wait for the next read.

use crate::company::{SkillDoc, SkillDrift, drift};
use crate::ports::skills_state::SkillState;

use super::InstalledSkill;

/// Where a stored row stands against `registry`, or `None` when the question
/// does not apply to it.
///
/// `None` for a row that pinned nothing and for one that holds no document,
/// matching [`effective_drift`](crate::company::effective_drift): an empty
/// [`SkillDrift`] would serialize as "checked, and clean", which is a different
/// claim from "there was nothing to check".
pub(super) fn row_drift(registry: &[SkillDoc], row: &SkillState) -> Option<SkillDrift> {
    let install = row.install.as_ref()?;
    let stored = row.custom_doc.as_deref()?;
    Some(drift(
        install,
        stored,
        registry.iter().find(|doc| doc.slug == row.slug),
    ))
}

impl InstalledSkill {
    /// Attaches where this row stands against the library.
    ///
    /// `None` leaves the projection at its defaults — no `updateAvailable` key
    /// and a plain `modified: false` — which is the honest shape for a row that
    /// was never pinned.
    pub(super) fn with_drift(mut self, drift: Option<SkillDrift>) -> Self {
        if let Some(drift) = drift {
            self.update_available = drift.update_available;
            self.modified = drift.modified;
        }
        self
    }
}
