//! Skill writes: install/uninstall a registry skill, toggle enabled, and author
//! a custom skill — under both scope forms.
//!
//! Deltas land in the [`SkillStateStore`](crate::ports::SkillStateStore); the
//! built-in skill content stays on disk (seeded by
//! [`RuntimeBuilder::build`](crate::runtime::RuntimeBuilder)). The `InstalledSkill`
//! response mirrors the console's `@/api/skills` types: a custom skill's fields
//! come from its `SKILL.md`, and so do a registry install's — install snapshots
//! the shared library's document, so the delta is self-describing.
//!
//! The console holds no skill catalog of its own; it browses the shared library
//! over `GET …/skills/registry` and installs by slug, with the host resolving
//! the content.
//!
//! Every write here is gated
//! [`AdminScopedCompany`](crate::server::ops::AdminScopedCompany): a skill's
//! content becomes part of every agent's effective prompt, company-wide, so
//! installing, uninstalling, toggling, or authoring one decides something for
//! the company rather than for the caller alone. The two reads —
//! `GET …/skills` and `GET …/skills/registry` — stay open to any member; only
//! the writes decide anything.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::company::skill_effective::{self, EffectiveSkill};
use crate::company::skill_scope::agents_for_skill;
use crate::company::skill_validate::{MAX_SLUG_CHARS, slugify, validate_slug, validate_slug_shape};
use crate::company::{
    SkillDoc, SkillDrift, VersionChange, effective_drift, parse_skill_md, render_skill_md,
    skill_digest,
};
use crate::error::OpenCompanyError;
use crate::ports::now_millis;
use crate::ports::skills_state::{SkillInstall, SkillSource, SkillState};
use crate::ports::types::SkillChange;
use crate::server::error::ApiError;
use crate::server::ops::language;
use crate::server::ops::{AdminScopedCompany, ScopedCompany, scoped};

/// The default category stamped on a skill whose doc carries none.
const DEFAULT_CATEGORY: &str = "Ops";

mod doc;
mod draft;
mod drift;
mod journal;
mod registry;
pub(crate) mod scope;
mod update;
mod upload;
pub(crate) mod vet;

// Re-exported rather than imported where used: the submodules reach these
// through `super::`, and the sibling test files resolve them through
// `use super::*`.
pub(crate) use vet::{ScanSummary, VetRefusal, check_skill_doc_size, vet_skill, write_lock};

/// Builds the skills route fragment.
pub fn router() -> Router<AppState> {
    scoped("/skills/{slug}/install", post(install))
        .merge(upload::router())
        .merge(doc::router())
        .merge(draft::router())
        .merge(update::router())
        .merge(scoped("/skills/{slug}/uninstall", post(uninstall)))
        .merge(registry::router())
        .merge(scoped("/skills/{slug}", put(set_enabled)))
        .merge(scoped("/skills", post(create_custom).get(list_skills)))
}

/// An installed skill as the console renders it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstalledSkill {
    id: String,
    name: String,
    description: String,
    category: String,
    source: SkillSource,
    enabled: bool,
    /// The library revision this install snapshotted, when its doc carries one.
    /// Lets a future "update available" affordance diff an install against the
    /// live registry without any extra stored state.
    version: Option<String>,
    /// When the operator last wrote this skill's delta, in epoch milliseconds.
    /// `None` for a skill no delta covers — a bundled or baseline skill nobody
    /// has touched — and for a row stored before the field existed.
    updated_at_millis: Option<u64>,
    /// What the scan said, on the write that stored this skill. Absent on a
    /// read: the report belongs to the write that produced the document, and
    /// re-deriving one on every list would report a verdict nobody acted on.
    #[serde(skip_serializing_if = "Option::is_none")]
    scan: Option<ScanSummary>,
    /// The revisions either side of a library change, when the library's
    /// document has moved since this install pinned its snapshot.
    ///
    /// Absent when it has not moved, and on every row that pinned nothing —
    /// a bundled skill has no library copy to be a revision *of*.
    #[serde(skip_serializing_if = "Option::is_none")]
    update_available: Option<VersionChange>,
    /// Whether the stored document no longer matches the digest recorded at
    /// install.
    ///
    /// A plain boolean, always on the wire, `false` on a row that pinned
    /// nothing. An absent boolean reads as "unknown", and the console would
    /// then have to decide whether to warn about a skill nothing can be said
    /// about.
    modified: bool,
    /// Where every roster agent stands on this skill — the read-side inversion
    /// of the per-agent allowlist ([`crate::company::skill_scope`]), which the
    /// skill's detail panel renders its picker from.
    ///
    /// Absent means **this answer does not report the roster**, which the
    /// console reads as "cannot say" and renders without a picker. An empty list
    /// is a different statement — the company has no teammates — so the two must
    /// not collapse, and the absent form is the safe one for a route that has
    /// not resolved the roster.
    #[serde(skip_serializing_if = "Option::is_none")]
    agents: Option<Vec<crate::company::skill_scope::SkillAgentScope>>,
}

impl InstalledSkill {
    /// Projects a [`SkillState`] to the console shape, parsing a custom skill's
    /// `SKILL.md` for its name/description/category and falling back to a
    /// slug-derived name for registry/built-in deltas.
    fn from_state(state: &SkillState) -> Self {
        let fallback = || {
            (
                titleize(&state.slug),
                String::new(),
                DEFAULT_CATEGORY.to_string(),
                None,
            )
        };
        let (name, description, category, version) = match &state.custom_doc {
            Some(doc) => match parse_skill_md(&state.slug, doc) {
                Ok(parsed) => (
                    parsed.name,
                    parsed.description,
                    parsed
                        .category
                        .unwrap_or_else(|| DEFAULT_CATEGORY.to_string()),
                    parsed.version,
                ),
                Err(_) => fallback(),
            },
            None => fallback(),
        };
        Self {
            id: state.slug.clone(),
            name,
            description,
            category,
            source: state.source,
            enabled: state.enabled,
            version,
            updated_at_millis: state.updated_at_millis,
            scan: None,
            update_available: None,
            modified: false,
            agents: None,
        }
    }

    /// Attaches the report of the write that stored this skill.
    fn with_scan(mut self, scan: ScanSummary) -> Self {
        self.scan = Some(scan);
        self
    }

    /// Projects one entry of the company's effective set
    /// ([`skill_effective::resolve`]) to the console shape. An entry no layer
    /// supplied a document for is rendered from its slug alone.
    ///
    /// `registry` is the host's shared library, which a pinned install is
    /// measured against — the list is where an operator learns that one has
    /// moved on without them.
    fn from_effective(skill: &EffectiveSkill, registry: &[SkillDoc]) -> Self {
        let doc = skill.doc();
        Self {
            id: skill.slug.clone(),
            name: doc
                .map(|doc| doc.name.clone())
                .unwrap_or_else(|| titleize(&skill.slug)),
            description: doc.map(|doc| doc.description.clone()).unwrap_or_default(),
            category: doc
                .and_then(|doc| doc.category.clone())
                .unwrap_or_else(|| DEFAULT_CATEGORY.to_string()),
            source: skill.source,
            enabled: skill.enabled,
            version: doc.and_then(|doc| doc.version.clone()),
            updated_at_millis: skill.updated_at_millis,
            scan: None,
            update_available: None,
            modified: false,
            agents: None,
        }
        .with_drift(effective_drift(skill, registry))
    }
}

/// The sub-resource path (`slug`).
#[derive(Debug, Deserialize)]
struct SlugPath {
    slug: String,
}

/// The toggle body.
#[derive(Debug, Deserialize)]
struct SetEnabled {
    enabled: bool,
}

/// The install body — the registry entry's metadata, so the installed skill
/// carries a real `SKILL.md` the embedded agent can act on (a bare slug has no
/// content, so it would never reach the agent's effective set).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallSkill {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    category: Option<String>,
    /// Install despite a blocking scan verdict, for this request only.
    #[serde(default)]
    force: bool,
}

/// The custom-skill body.
#[derive(Debug, Deserialize)]
struct CreateSkill {
    name: String,
    description: String,
    /// Save despite a blocking scan verdict, for this request only.
    #[serde(default)]
    force: bool,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    body: Option<String>,
}

/// `GET …/skills` — the company's **effective** skill set, resolved by
/// [`skill_effective::resolve`]: the global baseline, the company's on-disk
/// bundles (`companies/<name>/skills/*/SKILL.md`), and the operator's
/// [`SkillStateStore`] deltas, with the manifest's `[globals].disable` folded in
/// as disabling deltas.
///
/// That is the same derivation the harness materializes for every agent, so the
/// console reports the set the agents actually have — a disabled skill included,
/// since its row is what carries the switch that turns it back on.
async fn list_skills(
    State(state): State<AppState>,
    company: ScopedCompany,
) -> Result<Json<Vec<InstalledSkill>>, ApiError> {
    let mut deltas = company.runtime.skills().list(company.id()).await?;
    deltas.extend(skill_effective::globals_skill_disables(
        &company.runtime.globals_disable().await?,
    ));
    let registry = state.shared_skill_registry()?;
    let effective = skill_effective::resolve(company.runtime.source_dir(), &registry, &deltas)?;
    let roster = scope::roster_scopes(&company.runtime).await?;
    Ok(Json(
        effective
            .iter()
            .map(|skill| {
                InstalledSkill::from_effective(skill, &registry).with_agents(agents_for_skill(
                    &skill.slug,
                    skill.enabled,
                    &roster,
                ))
            })
            .collect(),
    ))
}

/// `POST …/skills/{slug}/install` — install a shared-library skill by slug.
///
/// **Server-authoritative.** The persisted `SKILL.md` is the shared library's own
/// document — frontmatter *and* body verbatim, so the agent gets the whole
/// procedure. The request body is ignored whenever the library can serve the
/// slug: a client cannot dictate what a registry skill contains.
///
/// Resolution, in order:
///
/// 1. **Slug in the registry** → persist that document. The snapshot is pinned:
///    a later library edit does not rewrite an existing install.
/// 2. **Slug absent from a non-empty registry** → `404`. This is a typo or a
///    stale client; silently persisting a stub is what produced content-less
///    installs in the first place.
/// 3. **Empty registry** → fall back to the client's metadata, recorded as
///    [`SkillSource::Custom`] since no library supplied the document. An
///    empty registry means this host serves no shared library at all
///    (platform-provisioned mode, no `skills_root`), so there is nothing to
///    resolve against and refusing every install would break hosted tenants
///    outright. The row records [`SkillSource::Custom`], because the document
///    is the client's own and no library copy exists to compare it against.
///
/// A *configured* library that fails to load is a `500`, never case 3: silently
/// degrading a broken shared library to "no library" would hand the client
/// authorship of a registry skill's contents on exactly the hosts that meant to
/// be server-authoritative.
async fn install(
    State(state): State<AppState>,
    company: AdminScopedCompany,
    Path(SlugPath { slug }): Path<SlugPath>,
    body: Option<Json<InstallSkill>>,
) -> Result<Json<InstalledSkill>, ApiError> {
    if let Err(problem) = validate_slug(&slug) {
        return Err(ApiError(OpenCompanyError::InvalidRequest(problem)));
    }
    let meta = body.map(|Json(body)| body).unwrap_or_default();
    let force = meta.force;
    let lock = write_lock(company.id());
    let _guard = lock.lock().await;
    let registry = state.shared_skill_registry()?;
    let (doc, source, install) = match registry.iter().find(|doc| doc.slug == slug) {
        Some(doc) => {
            let rendered = render_skill_md(doc);
            let pin = SkillInstall {
                digest: skill_digest(&rendered),
                version: doc.version.clone(),
                installed_by: Some(company.actor()),
                installed_at_millis: now_millis(),
            };
            (rendered, SkillSource::Registry, Some(pin))
        }
        None if !registry.is_empty() => {
            return Err(ApiError(OpenCompanyError::NotFound(
                language::SKILL_NOT_IN_REGISTRY.to_string(),
            )));
        }
        None => {
            // No shared library backs this host. Persist a real `SKILL.md` built
            // from the client's metadata (the description doubles as the body) so
            // `EffectiveSkills::materialize` surfaces the skill to the agent
            // instead of skipping a content-less delta. A client that supplies no
            // description gets the name as one: an empty scalar is a document the
            // parser refuses, and the delta it stored reached no agent.
            //
            // `Custom` is the honest provenance: nothing about this document
            // came from a shared library, so nothing can ever be diffed against
            // one to say it is stale or authentic.
            let name = meta
                .name
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| titleize(&slug));
            let description = meta
                .description
                .filter(|description| !description.trim().is_empty())
                .unwrap_or_else(|| name.clone());
            (
                skill_md(&name, &description, meta.category.as_deref(), &description),
                SkillSource::Custom,
                None,
            )
        }
    };
    check_skill_doc_size(&doc)?;
    let scan = vet_skill(&slug, &doc, force).map_err(ApiError::from)?;
    let delta = SkillState {
        slug,
        enabled: true,
        source,
        custom_doc: Some(doc),
        updated_at_millis: Some(now_millis()),
        install,
    };
    company.runtime.skills().set(company.id(), &delta).await?;
    journal::journal_write(
        &company.runtime,
        &company.actor(),
        &delta,
        SkillChange::Installed,
    )
    .await?;
    // A pin minted from the library's current document, over a document stored
    // from the same render: current and unmodified by construction, so this
    // needs no second comparison to say so.
    let stood = delta.install.as_ref().map(|_| SkillDrift::default());
    let roster = scope::roster_scopes(&company.runtime).await?;
    Ok(Json(
        InstalledSkill::from_state(&delta)
            .with_scan(scan)
            .with_drift(stood)
            .with_agents(agents_for_skill(&delta.slug, delta.enabled, &roster)),
    ))
}

async fn uninstall(
    company: AdminScopedCompany,
    Path(SlugPath { slug }): Path<SlugPath>,
) -> Result<StatusCode, ApiError> {
    let lock = write_lock(company.id());
    let _guard = lock.lock().await;
    let existing = company
        .runtime
        .skills()
        .list(company.id())
        .await?
        .into_iter()
        .find(|s| s.slug == slug);
    match existing {
        // Only registry installs and custom skills can be uninstalled.
        Some(state) if matches!(state.source, SkillSource::Registry | SkillSource::Custom) => {
            company.runtime.skills().remove(company.id(), &slug).await?;
            journal::journal_removal(&company.runtime, &company.actor(), &slug, state.source)
                .await?;
            Ok(StatusCode::NO_CONTENT)
        }
        // A built-in (company) skill — with or without a delta row — cannot be
        // removed; it can only be disabled.
        _ => Err(ApiError(OpenCompanyError::Conflict(
            language::BUILTIN_UNINSTALL.to_string(),
        ))),
    }
}

async fn set_enabled(
    State(app): State<AppState>,
    company: AdminScopedCompany,
    Path(SlugPath { slug }): Path<SlugPath>,
    Json(body): Json<SetEnabled>,
) -> Result<Json<InstalledSkill>, ApiError> {
    if let Err(problem) = validate_slug_shape(&slug) {
        return Err(ApiError(OpenCompanyError::InvalidRequest(problem)));
    }
    let lock = write_lock(company.id());
    let _guard = lock.lock().await;
    // A toggle writes no document, so everything an install recorded carries
    // through unchanged; a first toggle of a built-in company skill records a
    // Company-sourced override.
    let existing = company
        .runtime
        .skills()
        .list(company.id())
        .await?
        .into_iter()
        .find(|s| s.slug == slug);
    let (source, custom_doc, install) = match existing {
        Some(row) => (row.source, row.custom_doc, row.install),
        None => (SkillSource::Company, None, None),
    };
    let state = SkillState {
        slug,
        enabled: body.enabled,
        source,
        custom_doc,
        updated_at_millis: Some(now_millis()),
        install,
    };
    company.runtime.skills().set(company.id(), &state).await?;
    // The toggle moved nothing a pin measures, so the row stands where it stood
    // — which the answer has to say, because the console folds this row into the
    // list it is already showing.
    let stood = drift::row_drift(&app.shared_skill_registry()?, &state);
    let roster = scope::roster_scopes(&company.runtime).await?;
    Ok(Json(
        InstalledSkill::from_state(&state)
            .with_drift(stood)
            .with_agents(agents_for_skill(&state.slug, state.enabled, &roster)),
    ))
}

async fn create_custom(
    State(state): State<AppState>,
    company: AdminScopedCompany,
    Json(body): Json<CreateSkill>,
) -> Result<Json<InstalledSkill>, ApiError> {
    if body.name.trim().is_empty() || body.description.trim().is_empty() {
        return Err(ApiError(OpenCompanyError::InvalidRequest(
            language::SKILL_FIELDS_REQUIRED.to_string(),
        )));
    }
    let lock = write_lock(company.id());
    let _guard = lock.lock().await;
    let slug = unique_slug(
        &slugify(&body.name),
        &taken_slugs(&state, &company.runtime).await?,
    );
    let doc = skill_md(
        &body.name,
        &body.description,
        body.category.as_deref(),
        body.body.as_deref().unwrap_or(""),
    );
    check_skill_doc_size(&doc)?;
    let scan = vet_skill(&slug, &doc, body.force).map_err(ApiError::from)?;
    let state = SkillState {
        slug,
        enabled: true,
        source: SkillSource::Custom,
        custom_doc: Some(doc),
        install: None,
        updated_at_millis: Some(now_millis()),
    };
    company.runtime.skills().set(company.id(), &state).await?;
    journal::journal_write(
        &company.runtime,
        &company.actor(),
        &state,
        SkillChange::Installed,
    )
    .await?;
    let roster = scope::roster_scopes(&company.runtime).await?;
    Ok(Json(
        InstalledSkill::from_state(&state)
            .with_scan(scan)
            .with_agents(agents_for_skill(&state.slug, state.enabled, &roster)),
    ))
}

/// Builds a `SKILL.md` document from a name, description, optional category, and
/// body. Shared by custom-skill authoring and registry install (which passes
/// the description as the body).
///
/// The frontmatter parser is line-based (`key: value`), so each scalar is
/// collapsed to a single line: newlines become spaces. That prevents a
/// name/description from injecting extra frontmatter fields or emitting a bare
/// `---` line that would close the block early. (Colons within a value are
/// safe — the parser splits only on the first one.)
fn skill_md(name: &str, description: &str, category: Option<&str>, content: &str) -> String {
    let one_line = |s: &str| s.replace(['\n', '\r'], " ");
    let mut frontmatter = format!(
        "name: {}\ndescription: {}\n",
        one_line(name).trim(),
        one_line(description).trim()
    );
    if let Some(category) = category {
        frontmatter.push_str(&format!("category: {}\n", one_line(category).trim()));
    }
    format!("---\n{frontmatter}---\n{content}\n")
}

/// Every slug the company already resolves — bundled, registry-installed and
/// authored alike.
///
/// Authoring has to avoid all three, not just the stored deltas: a bundled
/// skill has no delta row at all, so a check against the store alone would
/// still let an authored skill take `web-research` from the bundle.
async fn taken_slugs(
    state: &AppState,
    runtime: &crate::company::runtime::CompanyRuntime,
) -> Result<std::collections::HashSet<String>, ApiError> {
    let mut deltas = runtime.skills().list(runtime.id()).await?;
    deltas.extend(skill_effective::globals_skill_disables(
        &runtime.globals_disable().await?,
    ));
    let registry = state.shared_skill_registry()?;
    Ok(
        skill_effective::resolve(runtime.source_dir(), &registry, &deltas)?
            .into_iter()
            .map(|skill| skill.slug)
            .collect(),
    )
}

/// `base`, or the first free `base-2`, `base-3`, … within [`MAX_SLUG_CHARS`].
///
/// A slug is a store key and a directory name, and authoring derives it from a
/// free-text display name, so two names can arrive at one slug: they differ
/// only past the truncation point, or they contain no alphanumerics at all and
/// both fall back to `skill`. Writing under a taken slug replaces whatever
/// holds it — another authored skill, or a bundled document an agent reads —
/// so the collision is resolved here rather than at the store.
fn unique_slug(base: &str, taken: &std::collections::HashSet<String>) -> String {
    if !taken.contains(base) {
        return base.to_string();
    }
    for n in 2..=1000 {
        let suffix = format!("-{n}");
        let room = MAX_SLUG_CHARS.saturating_sub(suffix.chars().count());
        let stem = base.chars().take(room).collect::<String>();
        let stem = stem.trim_end_matches('-');
        let candidate = if stem.is_empty() {
            format!("skill{suffix}")
        } else {
            format!("{stem}{suffix}")
        };
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    format!("skill-{}", crate::ports::now_millis())
}

/// Turns a slug into a human title (`web-research` → `Web Research`).
fn titleize(slug: &str) -> String {
    slug.split('-')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
#[path = "skills_part2_tests.rs"]
mod tests_part2;
#[cfg(test)]
#[path = "skills_scan_tests.rs"]
mod tests_scan;
#[cfg(test)]
#[path = "skills_skill_md_frontmatter_resists_tests.rs"]
mod tests_skill_md_frontmatter_resists;
