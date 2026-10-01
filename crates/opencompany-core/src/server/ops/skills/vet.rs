//! What a skill document must pass before it is stored, and the lock that
//! serializes the writes that store it.
//!
//! Separated from the routes because it answers a different question: a handler
//! decides what a request means, this decides whether the document it carries
//! may land in every agent's prompt at all. The size cap, the scanner's verdict
//! and the per-company write lock are the three gates every write path shares,
//! so they sit together rather than once per handler.

use std::sync::Arc;

use serde::Serialize;

use crate::company::skill_scan::{Verdict, scan_skill};
use crate::company::skill_validate::validate_skill_md;
use crate::error::OpenCompanyError;
use crate::ports::types::CompanyId;
use crate::server::error::ApiError;

/// The largest a skill's persisted `SKILL.md` (frontmatter and body together)
/// may be.
///
/// A skill's content lands in every agent's effective prompt, company-wide, so
/// this is a prompt budget rather than a storage limit. A quarter mebibyte
/// matches the codebase's existing ceiling for inline prose,
/// `MAX_ARTIFACT_BODY_BYTES` — generous for hand-authored instructions, and
/// still small enough that no single skill can quietly dominate what every
/// agent reads on every turn.
pub(crate) const MAX_SKILL_DOC_BYTES: usize = 256 * 1024;

/// Refuses a skill document over [`MAX_SKILL_DOC_BYTES`].
///
/// Checked on the assembled `SKILL.md` rather than the raw request fields, so
/// it bounds what actually lands in the agent's prompt regardless of which
/// field (name, description, or body) grew.
pub(crate) fn check_skill_doc_size(doc: &str) -> Result<(), ApiError> {
    if doc.len() > MAX_SKILL_DOC_BYTES {
        return Err(ApiError(OpenCompanyError::InvalidRequest(format!(
            "that skill is {:.1} KB — a skill's content has to be under {} KB.",
            doc.len() as f64 / 1024.0,
            MAX_SKILL_DOC_BYTES / 1024
        ))));
    }
    Ok(())
}

/// What the shared validator and the scan said about a document, as the
/// console renders it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScanSummary {
    verdict: Verdict,
    /// One line per finding, in operator-facing language.
    findings: Vec<String>,
    /// Spec rules this document diverges from without being refused.
    spec_deltas: Vec<String>,
    /// Whether a blocking verdict was overridden for this one request.
    forced: bool,
}
/// Why [`vet_skill`] refused a document.
///
/// The two are not interchangeable to a caller: a blocking scan verdict is the
/// one refusal `force` overrides, so the console offers to send it again and
/// the drafting route reports it as a scan refusal. Everything else is a
/// document that is simply not valid, which resending cannot fix. Carrying that
/// as a variant rather than leaving callers to read the sentence keeps the
/// distinction from depending on the wording of the sentence.
pub(crate) enum VetRefusal {
    /// The document did not validate — unparseable, or failing a stated limit.
    Invalid { message: String },
    /// The content scan blocked it, and `force` was not set.
    Blocked { message: String },
}

impl VetRefusal {
    /// The operator-facing sentence.
    pub(crate) fn message(&self) -> &str {
        match self {
            Self::Invalid { message } | Self::Blocked { message } => message,
        }
    }

    /// Whether resending with `force` would store this document.
    pub(crate) fn is_scan_block(&self) -> bool {
        matches!(self, Self::Blocked { .. })
    }
}

impl From<VetRefusal> for ApiError {
    fn from(refusal: VetRefusal) -> Self {
        ApiError(OpenCompanyError::InvalidRequest(
            refusal.message().to_string(),
        ))
    }
}

/// Validates and scans an assembled `SKILL.md` before it can be stored.
///
/// Every entry point that accepts content an operator did not write calls this,
/// so registry install, the empty-registry fallback and console authoring
/// cannot disagree about what a skill is or what is wrong with one.
///
/// A blocking verdict refuses the write outright. `force` overrides that for
/// the one request and records that it did; there is deliberately no setting
/// that turns a class of finding off for a whole host, because a switch that
/// silences an alarm is the failure this scan exists to prevent.
pub(crate) fn vet_skill(slug: &str, doc: &str, force: bool) -> Result<ScanSummary, VetRefusal> {
    let valid = validate_skill_md(slug, doc).map_err(|problems| VetRefusal::Invalid {
        message: problems.join(" "),
    })?;
    let report = scan_skill(&valid.doc, &[]);

    if report.is_blocked() && !force {
        return Err(VetRefusal::Blocked {
            message: format!(
                "that skill was refused by the content scan: {}. Review it, or resend with \
                 `force: true` to install it anyway.",
                report.messages().join("; ")
            ),
        });
    }

    Ok(ScanSummary {
        verdict: report.verdict(),
        findings: report.messages(),
        spec_deltas: valid.deltas.iter().map(|delta| delta.message()).collect(),
        forced: force && report.is_blocked(),
    })
}

/// Per-company serialization for the skill write routes.
///
/// `install` and `create_custom` write a fresh [`SkillState`] straight through
/// [`SkillStateStore::set`](crate::ports::SkillStateStore::set) and are
/// raceless on their own — the store upserts by slug, so two of them landing
/// concurrently is an ordinary last-write-wins. `set_enabled` is the one
/// genuine read-modify-write: it lists the existing delta so it can preserve
/// the slug's `source` and `custom_doc`, then writes a new one back. An
/// install or an authoring landing in the middle of that window would be
/// silently reverted — its fresh doc and source overwritten by whatever
/// `set_enabled` read before it ran. Taking this lock unconditionally in every
/// write handler, exactly as `smtp.rs`'s `write_lock` does for its own
/// read-modify-write, keeps that ordering rule in one place rather than in
/// each handler.
pub(crate) fn write_lock(company: &CompanyId) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<CompanyId, Arc<tokio::sync::Mutex<()>>>>,
    > = std::sync::OnceLock::new();
    let locks = LOCKS.get_or_init(Default::default);
    let mut locks = locks.lock().expect("skill write locks poisoned");
    Arc::clone(
        locks
            .entry(company.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
    )
}
