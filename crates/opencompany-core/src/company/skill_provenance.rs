//! Content digest and trust tier for an installed skill.
//!
//! An install pins a snapshot: the shared library's `SKILL.md` is persisted
//! verbatim and a later library edit does not rewrite it. What the pin lacked
//! was a way to *check* it. [`skill_digest`] supplies that — it is the value a
//! stored document is measured against, so a copy edited after install is
//! detectable and the library's current entry can be compared without keeping
//! a second copy of either document.
//!
//! [`trust_tier`] turns [`SkillSource`] into the label an operator reads. It is
//! computed here rather than stored beside the delta, so no write path can
//! promote a skill into a tier it did not earn.

use sha2::{Digest, Sha256};

use crate::ports::skills_state::{SkillSource, SkillTier};

/// The lowercase-hex SHA-256 of a `SKILL.md` document.
///
/// Over the rendered document exactly as persisted — frontmatter and body —
/// because that whole string is what reaches the agent, and a digest over only
/// the body would call a rewritten `description` unchanged. The catalogue line
/// is built from the frontmatter, so frontmatter is prompt-bound too.
pub fn skill_digest(doc: &str) -> String {
    let digest = Sha256::digest(doc.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The trust label for a skill with this [`SkillSource`].
///
/// `from_baseline` distinguishes the two halves of [`SkillSource::Company`]:
/// the embedded global baseline every company gets, versus a skill committed
/// in this company's own bundle. The source alone cannot tell them apart, and
/// they are different trust stories — one is reviewed once for every host, the
/// other by whoever reviews that bundle. Callers supply it by asking
/// [`globals::skills`](crate::globals::skills) whether it ships the slug; it is
/// a parameter rather than a lookup so this stays a pure function of its
/// inputs and a test need not pin itself to whatever the baseline ships today.
///
/// It is ignored for the other sources: a registry install and a console-authored
/// skill are never baseline content whatever the baseline happens to contain.
pub fn trust_tier(source: SkillSource, from_baseline: bool) -> SkillTier {
    match source {
        SkillSource::Company if from_baseline => SkillTier::Builtin,
        SkillSource::Company => SkillTier::Company,
        SkillSource::Registry => SkillTier::Registry,
        SkillSource::Custom => SkillTier::Custom,
    }
}

#[cfg(test)]
#[path = "skill_provenance_tests.rs"]
mod tests;
