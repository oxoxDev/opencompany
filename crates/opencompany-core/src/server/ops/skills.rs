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

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::company::skill_effective::{self, EffectiveSkill, valid_slug};
use crate::company::{SkillDoc, parse_skill_md, render_skill_md};
use crate::error::OpenCompanyError;
use crate::ports::skills_state::{SkillSource, SkillState};
use crate::ports::types::CompanyId;
use crate::server::error::ApiError;
use crate::server::ops::language;
use crate::server::ops::{AdminScopedCompany, ScopedCompany, scoped};

/// The default category stamped on a skill whose doc carries none.
const DEFAULT_CATEGORY: &str = "Ops";
/// The publisher stamped on shared-library skills (mirrors the GraphQL type).
const REGISTRY_PUBLISHER: &str = "OpenCompany";

/// The largest a skill's persisted `SKILL.md` (frontmatter and body together)
/// may be.
///
/// A skill's content lands in every agent's effective prompt, company-wide, so
/// this is a prompt budget rather than a storage limit. A quarter mebibyte
/// matches the codebase's existing ceiling for inline prose,
/// `MAX_ARTIFACT_BODY_BYTES` — generous for hand-authored instructions, and
/// still small enough that no single skill can quietly dominate what every
/// agent reads on every turn.
const MAX_SKILL_DOC_BYTES: usize = 256 * 1024;

/// Refuses a skill document over [`MAX_SKILL_DOC_BYTES`].
///
/// Checked on the assembled `SKILL.md` rather than the raw request fields, so
/// it bounds what actually lands in the agent's prompt regardless of which
/// field (name, description, or body) grew.
fn check_skill_doc_size(doc: &str) -> Result<(), ApiError> {
    if doc.len() > MAX_SKILL_DOC_BYTES {
        return Err(ApiError(OpenCompanyError::InvalidRequest(format!(
            "that skill is {:.1} KB — a skill's content has to be under {} KB.",
            doc.len() as f64 / 1024.0,
            MAX_SKILL_DOC_BYTES / 1024
        ))));
    }
    Ok(())
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
fn write_lock(company: &CompanyId) -> Arc<tokio::sync::Mutex<()>> {
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

/// Builds the skills route fragment.
pub fn router() -> Router<AppState> {
    scoped("/skills/{slug}/install", post(install))
        .merge(scoped("/skills/{slug}/uninstall", post(uninstall)))
        // `registry` is a static segment, so it wins over the `{slug}` pattern
        // above regardless of registration order (and the methods differ anyway).
        .merge(scoped("/skills/registry", get(list_registry)))
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
        }
    }

    /// Projects one entry of the company's effective set
    /// ([`skill_effective::resolve`]) to the console shape. An entry no layer
    /// supplied a document for is rendered from its slug alone.
    fn from_effective(skill: &EffectiveSkill) -> Self {
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
        }
    }
}

/// One skill in the shared library, as the console's registry tab browses it.
///
/// Deliberately **metadata only** — no `body`. Mirrors the GraphQL
/// `RegistrySkill` type so the two transports agree field for field.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RegistrySkill {
    id: String,
    name: String,
    description: String,
    category: String,
    publisher: String,
    /// The library revision this entry ships, from frontmatter. `None` for a
    /// skill authored before `version` existed.
    version: Option<String>,
}

impl RegistrySkill {
    fn from_doc(doc: &SkillDoc) -> Self {
        Self {
            id: doc.slug.clone(),
            name: doc.name.clone(),
            description: doc.description.clone(),
            category: doc
                .category
                .clone()
                .unwrap_or_else(|| DEFAULT_CATEGORY.to_string()),
            publisher: REGISTRY_PUBLISHER.to_string(),
            version: doc.version.clone(),
        }
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
}

/// The custom-skill body.
#[derive(Debug, Deserialize)]
struct CreateSkill {
    name: String,
    description: String,
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
    Ok(Json(
        effective
            .iter()
            .map(InstalledSkill::from_effective)
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
/// 3. **Empty registry** → fall back to the client's metadata, as before. An
///    empty registry means this host serves no shared library at all
///    (platform-provisioned mode, no `skills_root`), so there is nothing to
///    resolve against and refusing every install would break hosted tenants
///    outright.
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
    if !valid_slug(&slug) {
        return Err(ApiError(OpenCompanyError::InvalidRequest(format!(
            "`{slug}` is not a valid skill slug. Skills live under `skills/<slug>/`, so a slug \
             is `[a-z0-9][a-z0-9-]*`."
        ))));
    }
    let lock = write_lock(company.id());
    let _guard = lock.lock().await;
    let registry = state.shared_skill_registry()?;
    let doc = match registry.iter().find(|doc| doc.slug == slug) {
        Some(doc) => render_skill_md(doc),
        None if !registry.is_empty() => {
            return Err(ApiError(OpenCompanyError::NotFound(
                language::SKILL_NOT_IN_REGISTRY.to_string(),
            )));
        }
        None => {
            // No shared library backs this host. Persist a real `SKILL.md` built
            // from the client's metadata (the description doubles as the body) so
            // `EffectiveSkills::materialize` surfaces the skill to the agent
            // instead of skipping a content-less delta.
            let meta = body.map(|Json(b)| b).unwrap_or_default();
            let name = meta
                .name
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| titleize(&slug));
            let description = meta.description.unwrap_or_default();
            skill_md(&name, &description, meta.category.as_deref(), &description)
        }
    };
    check_skill_doc_size(&doc)?;
    let delta = SkillState {
        slug,
        enabled: true,
        source: SkillSource::Registry,
        custom_doc: Some(doc),
    };
    company.runtime.skills().set(company.id(), &delta).await?;
    Ok(Json(InstalledSkill::from_state(&delta)))
}

/// `GET …/skills/registry` — the shared skill library the console's registry tab
/// browses.
///
/// **Metadata only, by construction**: [`RegistrySkill`] has no `body` field, so
/// the payload stays flat regardless of how large the library grows. Install is
/// server-authoritative, so the client never needs a body — it posts a slug and
/// the host resolves the content.
///
/// Scoped (and so authorized) like every other console route even though the
/// library itself is host-global; the registry is not public.
async fn list_registry(
    State(state): State<AppState>,
    _company: ScopedCompany,
) -> Result<Json<Vec<RegistrySkill>>, ApiError> {
    Ok(Json(
        state
            .shared_skill_registry()?
            .iter()
            .map(RegistrySkill::from_doc)
            .collect(),
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
    company: AdminScopedCompany,
    Path(SlugPath { slug }): Path<SlugPath>,
    Json(body): Json<SetEnabled>,
) -> Result<Json<InstalledSkill>, ApiError> {
    if !valid_slug(&slug) {
        return Err(ApiError(OpenCompanyError::InvalidRequest(format!(
            "`{slug}` is not a valid skill slug. Skills live under `skills/<slug>/`, so a slug \
             is `[a-z0-9][a-z0-9-]*`."
        ))));
    }
    let lock = write_lock(company.id());
    let _guard = lock.lock().await;
    // Preserve an existing delta's source and custom doc; a first toggle of a
    // built-in company skill records a Company-sourced override.
    let existing = company
        .runtime
        .skills()
        .list(company.id())
        .await?
        .into_iter()
        .find(|s| s.slug == slug);
    let state = SkillState {
        slug,
        enabled: body.enabled,
        source: existing
            .as_ref()
            .map(|s| s.source)
            .unwrap_or(SkillSource::Company),
        custom_doc: existing.and_then(|s| s.custom_doc),
    };
    company.runtime.skills().set(company.id(), &state).await?;
    Ok(Json(InstalledSkill::from_state(&state)))
}

async fn create_custom(
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
    let slug = slugify(&body.name);
    let doc = skill_md(
        &body.name,
        &body.description,
        body.category.as_deref(),
        body.body.as_deref().unwrap_or(""),
    );
    check_skill_doc_size(&doc)?;
    let state = SkillState {
        slug,
        enabled: true,
        source: SkillSource::Custom,
        custom_doc: Some(doc),
    };
    company.runtime.skills().set(company.id(), &state).await?;
    Ok(Json(InstalledSkill::from_state(&state)))
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

/// Turns a display name into a filesystem-and-URL-safe slug.
fn slugify(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut prev_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    let trimmed = slug.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "skill".to_string()
    } else {
        trimmed
    }
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
mod tests {
    use std::path::Path as FsPath;

    use super::*;

    fn write_bundle(root: &FsPath, slug: &str, contents: &str) {
        let dir = root.join("skills").join(slug);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), contents).unwrap();
    }

    #[test]
    fn skill_md_frontmatter_resists_injection() {
        // A name carrying newlines, a stray `---`, and a fake field must not
        // inject frontmatter or hijack another field: newlines collapse to
        // spaces, so it all lands as the single `name` value.
        let nasty_name = "Evil\n---\ninjected: true\nname: hijacked";
        let doc = skill_md(nasty_name, "a real description", Some("Ops"), "body");
        let parsed = parse_skill_md("evil", &doc).expect("frontmatter stays valid");
        assert_eq!(parsed.name, "Evil --- injected: true name: hijacked");
        // The description was NOT overwritten by the injected `name: hijacked`.
        assert_eq!(parsed.description, "a real description");
        // A colon inside a value is preserved (split only on the first colon).
        let colon = skill_md("Name", "ratio 3:1 outcome", None, "body");
        assert_eq!(
            parse_skill_md("c", &colon).unwrap().description,
            "ratio 3:1 outcome"
        );
    }

    /// The projection every `GET …/skills` row goes through, over the same
    /// resolution the harness materializes.
    fn list(source_dir: Option<&FsPath>, deltas: &[SkillState]) -> Vec<InstalledSkill> {
        skill_effective::resolve(source_dir, &[], deltas)
            .expect("resolves")
            .iter()
            .map(InstalledSkill::from_effective)
            .collect()
    }

    fn global_slug() -> String {
        crate::globals::skills()[0].slug.clone()
    }

    /// The global baseline is what every agent has before a company adds
    /// anything, so it is what the console must list for a company with no
    /// bundles and no deltas — the shape a platform-provisioned tenant boots in.
    #[test]
    fn the_list_includes_the_global_baseline_with_no_bundles_and_no_deltas() {
        let out = list(None, &[]);
        for doc in crate::globals::skills() {
            let row = out
                .iter()
                .find(|s| s.id == doc.slug)
                .unwrap_or_else(|| panic!("no `{}` row", doc.slug));
            assert!(row.enabled);
            assert_eq!(row.name, doc.name);
            assert_eq!(row.description, doc.description);
            assert_eq!(
                row.source,
                SkillSource::Company,
                "a global is a baseline install, not something an operator added, \
                 so it carries no uninstall affordance"
            );
        }
    }

    /// A disabled global keeps its row. Hiding it would remove the only control
    /// that could turn it back on.
    #[test]
    fn a_disabled_global_is_listed_as_disabled_rather_than_hidden() {
        let slug = global_slug();
        let out = list(
            None,
            &[SkillState {
                slug: slug.clone(),
                enabled: false,
                source: SkillSource::Company,
                custom_doc: None,
            }],
        );

        let row = out.iter().find(|s| s.id == slug).expect("row still listed");
        assert!(!row.enabled);
        assert!(!row.name.is_empty(), "a disabled row keeps its name");
    }

    /// `[globals].disable` is honoured by the reader exactly as the harness
    /// honours it — via the same synthesized delta.
    #[test]
    fn a_manifest_opt_out_is_honoured_by_the_reader() {
        let slug = global_slug();
        let deltas = skill_effective::globals_skill_disables(&[format!("skill:{slug}")]);
        let out = list(None, &deltas);

        let row = out.iter().find(|s| s.id == slug).expect("row still listed");
        assert!(!row.enabled, "the manifest opt-out reaches the console");
    }

    #[test]
    fn the_list_unions_bundles_with_deltas() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_bundle(
            root,
            "onboard",
            "---\nname: Onboard\ndescription: Get set up\ncategory: Ops\n---\n# Onboard\n",
        );

        let deltas = vec![
            SkillState {
                slug: "onboard".to_string(),
                enabled: false,
                source: SkillSource::Company,
                custom_doc: None,
            },
            SkillState {
                slug: "my-skill".to_string(),
                enabled: true,
                source: SkillSource::Custom,
                custom_doc: Some(
                    "---\nname: My Skill\ndescription: Does a thing\n---\n# body\n".to_string(),
                ),
            },
        ];

        let out = list(Some(root), &deltas);

        let onboard = out
            .iter()
            .find(|s| s.id == "onboard")
            .expect("company bundle present");
        assert_eq!(onboard.name, "Onboard");
        assert_eq!(onboard.source, SkillSource::Company);
        assert!(!onboard.enabled, "delta flips the bundle disabled");

        let custom = out
            .iter()
            .find(|s| s.id == "my-skill")
            .expect("custom delta present");
        assert_eq!(custom.source, SkillSource::Custom);
        assert_eq!(custom.name, "My Skill");
        assert!(custom.enabled);

        let ids: Vec<&str> = out.iter().map(|s| s.id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "rows are ordered by slug");
    }

    /// A malformed company bundle costs that company its whole catalogue in the
    /// harness, so the reader reports the failure instead of a tidy subset that
    /// no agent actually has.
    #[test]
    fn a_malformed_company_bundle_surfaces_as_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        write_bundle(tmp.path(), "broken", "no frontmatter here\n");
        assert!(skill_effective::resolve(Some(tmp.path()), &[], &[]).is_err());
    }

    /// The REST list and the GraphQL resolver project the same resolution, so
    /// the two transports cannot report different skills for one company.
    #[test]
    fn the_rest_list_and_the_graphql_resolver_agree() {
        let tmp = tempfile::tempdir().unwrap();
        write_bundle(
            tmp.path(),
            "onboard",
            "---\nname: Onboard\ndescription: Get set up\ncategory: Ops\nversion: 2.0.0\n---\n# Onboard\n",
        );
        let deltas = vec![
            SkillState {
                slug: global_slug(),
                enabled: false,
                source: SkillSource::Company,
                custom_doc: None,
            },
            SkillState {
                slug: "onboard".to_string(),
                enabled: true,
                source: SkillSource::Custom,
                custom_doc: Some(
                    "---\nname: Onboard v2\ndescription: Rewritten\ncategory: Ops\nversion: 3.0.0\n---\n# v2\n"
                        .to_string(),
                ),
            },
        ];

        let effective = skill_effective::resolve(Some(tmp.path()), &[], &deltas).expect("resolves");
        let rest: Vec<InstalledSkill> = effective
            .iter()
            .map(InstalledSkill::from_effective)
            .collect();
        let gql = crate::server::graphql::skills::project(&effective);

        assert_eq!(rest.len(), gql.len());
        for (rest, gql) in rest.iter().zip(gql.iter()) {
            assert_eq!(rest.id, gql.id.0);
            assert_eq!(rest.name, gql.name);
            assert_eq!(rest.description, gql.description);
            assert_eq!(rest.category, gql.category);
            assert_eq!(rest.enabled, gql.enabled);
            assert_eq!(rest.version, gql.version);
            assert_eq!(
                serde_json::to_value(rest.source).unwrap(),
                serde_json::Value::String(gql.source.clone()),
            );
        }

        // The divergence this convergence closes: REST refreshed the display
        // fields from a delta's document where GraphQL kept the bundle's.
        let onboard = rest.iter().find(|s| s.id == "onboard").expect("row");
        assert_eq!(onboard.name, "Onboard v2");
        assert_eq!(onboard.version.as_deref(), Some("3.0.0"));
    }

    /// The test the bug needed: what the console lists and what the harness
    /// writes into an agent's skill tree are the same set.
    #[cfg(feature = "openhuman")]
    #[test]
    fn the_rest_list_and_the_harness_effective_set_agree() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        write_bundle(
            tmp.path(),
            "onboard",
            "---\nname: Onboard\ndescription: Get set up\n---\n# Onboard\n",
        );
        let deltas = vec![
            SkillState {
                slug: global_slug(),
                enabled: false,
                source: SkillSource::Company,
                custom_doc: None,
            },
            SkillState {
                slug: "my-skill".to_string(),
                enabled: true,
                source: SkillSource::Custom,
                custom_doc: Some(
                    "---\nname: My Skill\ndescription: Does a thing\n---\n# body\n".to_string(),
                ),
            },
        ];

        crate::harness::skills::EffectiveSkills::materialize(
            ws.path().to_path_buf(),
            Some(tmp.path()),
            &[],
            &deltas,
        )
        .expect("materializes");

        let mut materialized: Vec<String> = std::fs::read_dir(ws.path().join("skills"))
            .expect("skill tree")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        materialized.sort();

        let mut listed: Vec<String> = list(Some(tmp.path()), &deltas)
            .into_iter()
            .filter(|row| row.enabled)
            .map(|row| row.id)
            .collect();
        listed.sort();

        assert_eq!(
            listed, materialized,
            "the console's enabled rows are exactly the skills the agents read"
        );
        assert!(
            !materialized.contains(&global_slug()),
            "the disabled global really is withheld from the agents"
        );
        // Pinned against the baseline itself, so an agreement of two empty
        // halves cannot pass for agreement.
        for doc in crate::globals::skills() {
            if doc.slug == global_slug() {
                continue;
            }
            assert!(
                listed.contains(&doc.slug),
                "the console lists the global `{}` the agents read",
                doc.slug
            );
        }
    }

    /// `valid_slug` is the gate both write handlers share: a slug is also a
    /// directory name under `skills/<slug>/`, so a traversal (`..`) or a path
    /// separator (`/`) must never reach the filesystem, and the alphabet is
    /// lowercase-only. The path extractor can only ever hand a handler a single
    /// segment, so `a/b` cannot arrive as a path — but the function is the
    /// contract every slug-bearing caller routes through, so it is the right
    /// place to pin all three shapes the review named.
    #[test]
    fn valid_slug_rejects_traversal_separator_and_case() {
        // The shapes the review named.
        assert!(!valid_slug(".."), "parent traversal");
        assert!(!valid_slug("a/b"), "path separator");
        assert!(!valid_slug("A"), "uppercase start");
        // And the rest of the boundary.
        assert!(!valid_slug(""), "empty");
        assert!(!valid_slug("-leading"), "leading dash");
        assert!(!valid_slug("has space"), "interior space");
        assert!(
            !valid_slug("under_score"),
            "underscore is not in the alphabet"
        );
        assert!(!valid_slug("UPPER"), "all uppercase");
        // And the shape that must pass.
        assert!(valid_slug("a-1"), "lowercase, digit, dash");
        assert!(valid_slug("0"), "single digit");
        assert!(valid_slug("seo-audit"), "typical slug");
    }

    #[test]
    fn check_skill_doc_size_rejects_only_over_cap() {
        assert!(check_skill_doc_size(&"x".repeat(MAX_SKILL_DOC_BYTES)).is_ok());
        let err = check_skill_doc_size(&"x".repeat(MAX_SKILL_DOC_BYTES + 1))
            .expect_err("over-cap doc must be refused");
        assert!(matches!(err.0, OpenCompanyError::InvalidRequest(_)));
    }

    /// The mutual-exclusion property [`write_lock`] exists for: two holders of
    /// the same company's lock run strictly one after the other, never
    /// interleaved, which is what keeps `set_enabled`'s
    /// list-then-preserve-then-write window from landing between another
    /// handler's write and its own read.
    #[tokio::test]
    async fn write_lock_serializes_same_company_writes() {
        let id = CompanyId::new("acme");
        let lock_a = write_lock(&id);
        let lock_b = write_lock(&id);
        assert!(
            Arc::ptr_eq(&lock_a, &lock_b),
            "the same company id must resolve to the same lock"
        );
        // A different company gets its own lock, so one tenant's writes never
        // block another's.
        let other = write_lock(&CompanyId::new("other"));
        assert!(!Arc::ptr_eq(&lock_a, &other));

        let order: Arc<tokio::sync::Mutex<Vec<&'static str>>> =
            Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let guard = lock_a.lock().await;

        let order_clone = Arc::clone(&order);
        let waiter = tokio::spawn(async move {
            // Blocks here until the holder below drops its guard.
            let _guard = lock_b.lock().await;
            order_clone.lock().await.push("second");
        });

        // Give the spawned task a chance to actually reach the blocked
        // `.lock().await` before the holder records its own turn.
        tokio::task::yield_now().await;
        order.lock().await.push("first");
        drop(guard);
        waiter.await.expect("waiter task did not panic");

        assert_eq!(
            *order.lock().await,
            vec!["first", "second"],
            "the second acquirer must not have run until the first released"
        );
    }

    /// HTTP-level coverage of the two path-slug handlers. A slug that fails
    /// `valid_slug` must be rejected with `400` **before** any write, so the
    /// effective skill set is untouched; a valid slug succeeds and lands.
    ///
    /// `..` and `a/b` cannot be carried as a single path segment (a `/` splits
    /// them, and `..` is normalized away by the router), so their rejection is
    /// pinned in [`valid_slug_rejects_traversal_separator_and_case`]. `A` is a
    /// single segment the router will pass through, so it is the shape we drive
    /// through the handlers to prove the `400` and the no-mutation guarantee.
    mod http {
        use axum::body::{Body, to_bytes};
        use axum::http::{Request, StatusCode};
        use serde_json::Value;
        use tower::ServiceExt;

        use crate::company::CompanyManifest;
        use crate::ports::CompanyStore;
        use crate::ports::types::{CompanyId, CompanyRecord};
        use crate::runtime::RuntimeBuilder;
        use crate::server::ops::skills::{MAX_SKILL_DOC_BYTES, valid_slug, write_lock};
        use crate::server::router;
        use crate::server::test_support::{
            fixed_cookie, member_cookie, seed_fixed_admin, seed_fixed_member,
        };
        use crate::{AppConfig, AppState};

        async fn state_with_company(home: &std::path::Path) -> AppState {
            let id = CompanyId::new("acme");
            let manifest: CompanyManifest =
                toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n").unwrap();
            crate::store::FsCompanyStore::new(home.to_path_buf())
                .save(&CompanyRecord {
                    overlay_desk_hive: Vec::new(),
                    overlay_retired_agents: Vec::new(),
                    overlay_agent_edits: Vec::new(),
                    id: id.clone(),
                    manifest: manifest.clone(),
                    ledger: Vec::new(),
                    lifecycle: "running".to_string(),
                    overlay_agents: Vec::new(),
                    overlay_desk_members: Vec::new(),
                    overlay_desk_order: Vec::new(),
                    overlay_desks: Vec::new(),
                    overlay_workflows: Vec::new(),
                    overlay_budgets: Vec::new(),
                    overlay_policy: None,
                    overlay_tool_grants: None,
                    overlay_desk_tools: Default::default(),
                    disabled_workflows: Vec::new(),
                    template_provenance: None,
                    setup: None,
                    name_confirmed: false,
                    activation_completed_at: None,
                    created_at_millis: None,
                })
                .await
                .unwrap();
            let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest)
                .with_id(id.clone())
                .build()
                .await
                .unwrap();
            let state = AppState::new(AppConfig::default());
            state.registry().insert(id, std::sync::Arc::new(runtime));
            seed_fixed_admin(&state, "acme").await;
            state
        }

        /// Sends as the fixed admin session — the common case, since every write
        /// route here is admin-gated.
        async fn send(
            state: &AppState,
            method: &str,
            uri: &str,
            body: Option<&str>,
        ) -> (StatusCode, Value, String) {
            send_as(state, method, uri, body, Some(&fixed_cookie("acme"))).await
        }

        /// [`send`], with the caller's cookie explicit — `None` for no session at
        /// all, so the privilege boundary can be driven with an admin session, a
        /// member session, or nothing.
        async fn send_as(
            state: &AppState,
            method: &str,
            uri: &str,
            body: Option<&str>,
            cookie: Option<&str>,
        ) -> (StatusCode, Value, String) {
            let mut request = Request::builder().method(method).uri(uri);
            if let Some(cookie) = cookie {
                request = request.header("cookie", cookie);
            }
            let request = match body {
                Some(body) => request
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
                None => request.body(Body::empty()).unwrap(),
            };
            let response = router(state.clone()).oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let raw = String::from_utf8_lossy(&bytes).to_string();
            let value = if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).unwrap_or(Value::Null)
            };
            (status, value, raw)
        }

        /// The effective skill set, as the console reads it.
        async fn slugs(state: &AppState) -> Vec<String> {
            let (status, value, raw) = send(state, "GET", "/api/v1/company/skills", None).await;
            assert_eq!(status, StatusCode::OK, "list skills: {raw}");
            value
                .as_array()
                .expect("skills list is an array")
                .iter()
                .map(|s| s["id"].as_str().expect("an id").to_string())
                .collect()
        }

        /// Both write handlers reject an invalid slug with `400` and leave the
        /// effective skill set untouched; a valid slug then succeeds and lands.
        #[tokio::test]
        async fn invalid_slugs_are_400_and_leave_state_unchanged() {
            let home = tempfile::tempdir().unwrap();
            let state = state_with_company(home.path()).await;

            let before = slugs(&state).await;

            // `install` rejects the uppercase slug without writing.
            let (status, _, raw) =
                send(&state, "POST", "/api/v1/company/skills/A/install", None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "install A: {raw}");
            assert!(
                raw.contains("not a valid skill slug"),
                "the 400 explains why: {raw}"
            );

            // `set_enabled` rejects the same slug without writing.
            let (status, _, raw) = send(
                &state,
                "PUT",
                "/api/v1/company/skills/A",
                Some(r#"{"enabled":true}"#),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "set_enabled A: {raw}");

            // Neither attempt mutated the effective set.
            assert_eq!(
                slugs(&state).await,
                before,
                "a rejected slug must not land a delta"
            );

            // A valid slug succeeds on both handlers and does land.
            let (status, _, raw) =
                send(&state, "POST", "/api/v1/company/skills/a-1/install", None).await;
            assert_eq!(status, StatusCode::OK, "install a-1: {raw}");

            let (status, _, raw) = send(
                &state,
                "PUT",
                "/api/v1/company/skills/a-1",
                Some(r#"{"enabled":false}"#),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "set_enabled a-1: {raw}");

            assert!(
                slugs(&state).await.iter().any(|s| s == "a-1"),
                "the valid slug lands in the effective set"
            );
        }

        /// Every write route here decides something for the whole company (see
        /// the module doc): a Member is refused exactly like an unauthenticated
        /// caller, on all four of them, and neither refusal lands a delta.
        #[tokio::test]
        async fn every_write_route_refuses_a_member_and_an_unauthenticated_caller() {
            let home = tempfile::tempdir().unwrap();
            let state = state_with_company(home.path()).await;
            seed_fixed_member(&state, "acme").await;
            let member = member_cookie("acme");
            let before = slugs(&state).await;

            let attempts: [(&str, &str, Option<&str>); 4] = [
                ("POST", "/api/v1/company/skills/seo-audit/install", None),
                (
                    "PUT",
                    "/api/v1/company/skills/seo-audit",
                    Some(r#"{"enabled":true}"#),
                ),
                (
                    "POST",
                    "/api/v1/company/skills",
                    Some(r#"{"name":"Member Skill","description":"a member tried this"}"#),
                ),
                ("POST", "/api/v1/company/skills/seo-audit/uninstall", None),
            ];

            for (method, uri, body) in attempts {
                let (status, resp, raw) = send_as(&state, method, uri, body, Some(&member)).await;
                assert_eq!(
                    status,
                    StatusCode::FORBIDDEN,
                    "{method} {uri} as a member: {raw}"
                );
                assert_eq!(resp["code"], "forbidden", "{method} {uri}: {raw}");

                let (status, resp, raw) = send_as(&state, method, uri, body, None).await;
                assert_eq!(
                    status,
                    StatusCode::UNAUTHORIZED,
                    "{method} {uri} with no session: {raw}"
                );
                assert_eq!(resp["code"], "unauthorized", "{method} {uri}: {raw}");
            }

            assert_eq!(
                slugs(&state).await,
                before,
                "no member or unauthenticated attempt landed a delta"
            );
        }

        /// The other half of the boundary: an admin is not caught by the same
        /// gate, and every write route still does its job end to end —
        /// install, toggle, author, then uninstall the one route that allows
        /// it.
        #[tokio::test]
        async fn an_admin_can_use_every_write_route() {
            let home = tempfile::tempdir().unwrap();
            let state = state_with_company(home.path()).await;

            let (status, _, raw) = send(
                &state,
                "POST",
                "/api/v1/company/skills/seo-audit/install",
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "admin install: {raw}");

            let (status, _, raw) = send(
                &state,
                "PUT",
                "/api/v1/company/skills/seo-audit",
                Some(r#"{"enabled":false}"#),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "admin set_enabled: {raw}");

            let (status, resp, raw) = send(
                &state,
                "POST",
                "/api/v1/company/skills",
                Some(r#"{"name":"Admin Skill","description":"authored by an admin"}"#),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "admin create_custom: {raw}");
            assert_eq!(resp["id"], "admin-skill");

            let (status, _, raw) = send(
                &state,
                "POST",
                "/api/v1/company/skills/seo-audit/uninstall",
                None,
            )
            .await;
            assert_eq!(status, StatusCode::NO_CONTENT, "admin uninstall: {raw}");

            let after = slugs(&state).await;
            assert!(
                !after.iter().any(|s| s == "seo-audit"),
                "the uninstall landed: {after:?}"
            );
            assert!(
                after.iter().any(|s| s == "admin-skill"),
                "the authored skill landed: {after:?}"
            );
        }

        /// The affordance the missing rows were costing: a global reaches every
        /// agent, and the only control that can withhold it is the row's own
        /// switch. Driven over the real route, end to end.
        #[tokio::test]
        async fn a_global_can_be_disabled_and_re_enabled_through_the_put_route() {
            let home = tempfile::tempdir().unwrap();
            let state = state_with_company(home.path()).await;
            let slug = super::tests::global_slug();

            let listed = |state: &AppState, slug: String| {
                let state = state.clone();
                async move {
                    let (status, value, raw) =
                        send(&state, "GET", "/api/v1/company/skills", None).await;
                    assert_eq!(status, StatusCode::OK, "list skills: {raw}");
                    value
                        .as_array()
                        .expect("an array")
                        .iter()
                        .find(|row| row["id"] == slug)
                        .unwrap_or_else(|| panic!("no `{slug}` row: {raw}"))
                        .clone()
                }
            };

            let row = listed(&state, slug.clone()).await;
            assert_eq!(row["enabled"], true, "a global starts enabled");

            let (status, _, raw) = send(
                &state,
                "PUT",
                &format!("/api/v1/company/skills/{slug}"),
                Some(r#"{"enabled":false}"#),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "disable a global: {raw}");

            let row = listed(&state, slug.clone()).await;
            assert_eq!(row["enabled"], false, "the switch stuck");
            assert_eq!(row["source"], "company", "still not uninstallable");

            let (status, _, raw) = send(
                &state,
                "PUT",
                &format!("/api/v1/company/skills/{slug}"),
                Some(r#"{"enabled":true}"#),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "re-enable a global: {raw}");
            assert_eq!(listed(&state, slug).await["enabled"], true);
        }

        /// A skill's document becomes part of every agent's effective prompt,
        /// so [`MAX_SKILL_DOC_BYTES`] is enforced on the assembled `SKILL.md`,
        /// not just accepted and truncated later — and the refusal is the same
        /// `400 invalid_request` shape every other bad-input write already
        /// uses, not a bespoke code.
        #[tokio::test]
        async fn an_over_cap_custom_skill_body_is_refused() {
            let home = tempfile::tempdir().unwrap();
            let state = state_with_company(home.path()).await;
            let before = slugs(&state).await;

            let oversized = "x".repeat(MAX_SKILL_DOC_BYTES);
            let body = serde_json::json!({
                "name": "Huge Skill",
                "description": "short",
                "body": oversized,
            })
            .to_string();

            let (status, resp, raw) =
                send(&state, "POST", "/api/v1/company/skills", Some(&body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{raw}");
            assert_eq!(resp["code"], "invalid_request", "{raw}");

            assert_eq!(
                slugs(&state).await,
                before,
                "an over-cap body must not land a delta"
            );
        }

        /// The property [`write_lock`] exists for, asserted where it actually
        /// has to hold: on the handlers, not on the primitive.
        ///
        /// [`write_lock_serializes_same_company_writes`](super::write_lock_serializes_same_company_writes)
        /// proves the mutex is a mutex; it passes unchanged if every handler
        /// stops taking it. This holds the addressed company's lock and drives
        /// each write route over the real router: a route that reached the
        /// store anyway answers while the lock is held, which is the whole
        /// defect — `set_enabled`'s list-then-write window is only closed
        /// while *every* writer waits on the same lock.
        #[tokio::test]
        async fn every_write_route_waits_on_the_company_write_lock() {
            let home = tempfile::tempdir().unwrap();
            let state = state_with_company(home.path()).await;

            // One uncontended write first, so everything a request lazily opens
            // on its way to the handler (session lookup, the skill store) is
            // already warm and the wait below is measuring the lock alone.
            let (status, _, raw) = send(
                &state,
                "POST",
                "/api/v1/company/skills/warm-up/install",
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "warm-up install: {raw}");

            let attempts: [(&'static str, &'static str, Option<&'static str>); 4] = [
                ("POST", "/api/v1/company/skills/seo-audit/install", None),
                (
                    "PUT",
                    "/api/v1/company/skills/seo-audit",
                    Some(r#"{"enabled":false}"#),
                ),
                (
                    "POST",
                    "/api/v1/company/skills",
                    Some(r#"{"name":"Locked Skill","description":"waits its turn"}"#),
                ),
                ("POST", "/api/v1/company/skills/warm-up/uninstall", None),
            ];

            for (method, uri, body) in attempts {
                let lock = write_lock(&CompanyId::new("acme"));
                let guard = lock.lock().await;

                let held = state.clone();
                let mut pending = tokio::spawn(async move {
                    send_as(&held, method, uri, body, Some(&fixed_cookie("acme"))).await
                });

                let ran_anyway =
                    tokio::time::timeout(std::time::Duration::from_millis(750), &mut pending).await;
                assert!(
                    ran_anyway.is_err(),
                    "{method} {uri} reached the store while another writer held \
                     the company write lock"
                );

                drop(guard);
                let (status, _, raw) = pending.await.expect("the write task did not panic");
                assert!(
                    status.is_success(),
                    "{method} {uri} once the lock was free: {raw}"
                );
            }
        }

        /// Authoring derives the slug from the display name, so the name is the
        /// untrusted input that decides a store key and a `skills/<slug>/`
        /// directory name. Whatever `create_custom` accepts must therefore
        /// derive a slug the slug-bearing routes accept: an id `valid_slug`
        /// refuses is a skill nobody can toggle or uninstall afterwards, and a
        /// path segment nothing else in the product will honour.
        ///
        /// Asserted end to end — the derived id is fed straight back to
        /// `PUT …/skills/{slug}`, the route that does apply `valid_slug`.
        #[tokio::test]
        async fn an_authored_slug_is_always_one_the_slug_routes_accept() {
            let home = tempfile::tempdir().unwrap();
            let state = state_with_company(home.path()).await;

            for name in [
                "!!!",
                "  ---  ",
                "-leading dash",
                "Ünïcödé Skill",
                "42",
                "A/B\\C",
                "UPPER CASE",
                "under_score",
            ] {
                let body = serde_json::json!({
                    "name": name,
                    "description": "a description",
                })
                .to_string();
                let (status, resp, raw) =
                    send(&state, "POST", "/api/v1/company/skills", Some(&body)).await;
                assert_eq!(status, StatusCode::OK, "authoring {name:?}: {raw}");

                let slug = resp["id"].as_str().expect("an id").to_string();
                assert!(
                    valid_slug(&slug),
                    "{name:?} derived {slug:?}, which the slug routes refuse"
                );

                let (status, _, raw) = send(
                    &state,
                    "PUT",
                    &format!("/api/v1/company/skills/{slug}"),
                    Some(r#"{"enabled":false}"#),
                )
                .await;
                assert_eq!(
                    status,
                    StatusCode::OK,
                    "the skill authored from {name:?} cannot be managed by its own id \
                     {slug:?}: {raw}"
                );
            }
        }

        /// A refusal raised *inside* the guarded region must still hand the
        /// company's write lock back.
        ///
        /// Every write handler takes the lock before it validates, so the
        /// over-cap refusal returns with the guard live. A guard that outlived
        /// its request would not fail that request — it would wedge every
        /// later write for that one company, for the life of the process, with
        /// nothing in the failed response to say so.
        #[tokio::test]
        async fn a_write_refused_inside_the_lock_still_hands_it_back() {
            let home = tempfile::tempdir().unwrap();
            let state = state_with_company(home.path()).await;

            let body = serde_json::json!({
                "name": "Huge Skill",
                "description": "short",
                "body": "x".repeat(MAX_SKILL_DOC_BYTES),
            })
            .to_string();
            let (status, _, raw) =
                send(&state, "POST", "/api/v1/company/skills", Some(&body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{raw}");

            let next = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                send(&state, "POST", "/api/v1/company/skills/a-1/install", None),
            )
            .await
            .expect("the refused write left the company write lock held");
            assert_eq!(next.0, StatusCode::OK, "{}", next.2);
        }
    }
}
