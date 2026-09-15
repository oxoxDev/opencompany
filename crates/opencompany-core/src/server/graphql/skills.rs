//! Skill reads: `Company.skills` and the top-level `skillRegistry` (the
//! repo-level shared library).
//!
//! The store holds deltas only. What the company's effective set *is* — the
//! global baseline, its on-disk `skills/*/SKILL.md` bundles, and those deltas —
//! is resolved by [`crate::company::skill_effective`], which the REST list and
//! the harness read too, so no two of the three can drift.

use std::sync::Arc;

use async_graphql::{Context, ID, SimpleObject};

use crate::AppState;
use crate::company::SkillDoc;
use crate::company::runtime::CompanyRuntime;
use crate::company::skill_effective::{self, EffectiveSkill};
use crate::ports::skills_state::SkillSource;

/// One skill installed in a company. Mirrors the console's `@/api/skills` types.
#[derive(SimpleObject)]
#[graphql(name = "Skill")]
pub struct SkillGql {
    /// The skill slug.
    pub id: ID,
    /// The display name.
    pub name: String,
    /// A one-line description.
    pub description: String,
    /// The skill category (e.g. `Marketing`, `Ops`).
    pub category: String,
    /// Provenance: `company` | `registry` | `custom`.
    pub source: String,
    /// Whether the skill is enabled for the company.
    pub enabled: bool,
    /// The library revision this skill's document carries, when it has one.
    pub version: Option<String>,
}

/// One skill in the shared repo-level registry, installable into any company.
#[derive(SimpleObject)]
#[graphql(name = "RegistrySkill")]
pub struct RegistrySkillGql {
    /// The skill slug.
    pub id: ID,
    /// The display name.
    pub name: String,
    /// A one-line description.
    pub description: String,
    /// The skill category.
    pub category: String,
    /// The publisher of the registry skill.
    pub publisher: String,
    /// The library revision this entry ships, from frontmatter. `None` for a
    /// skill authored before `version` existed.
    pub version: Option<String>,
}

/// The default category when a skill doc carries none.
const DEFAULT_CATEGORY: &str = "Ops";
/// The publisher stamped on repo-level registry skills.
const REGISTRY_PUBLISHER: &str = "OpenCompany";

fn source_str(source: SkillSource) -> &'static str {
    match source {
        SkillSource::Company => "company",
        SkillSource::Registry => "registry",
        SkillSource::Custom => "custom",
    }
}

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

/// The repo-level skill registry docs, loaded from the shared `skills/` library
/// directory. Empty when no source checkout is present (platform-provisioned
/// mode), where the registry has nothing to serve. A configured library that
/// fails to load surfaces as a query error rather than as an empty registry.
fn registry_docs(state: &AppState) -> async_graphql::Result<Arc<[SkillDoc]>> {
    Ok(state.shared_skill_registry()?)
}

/// Resolves `Company.skills` from the company's effective set
/// ([`skill_effective::resolve`]) — the same derivation the harness materializes
/// for every agent, and the same one `GET …/skills` answers with.
///
/// Disabled entries are reported rather than dropped: the console's switch needs
/// a row to sit on.
pub(crate) async fn resolve_company(
    ctx: &Context<'_>,
    runtime: &Arc<CompanyRuntime>,
) -> async_graphql::Result<Vec<SkillGql>> {
    let state = ctx.data::<AppState>()?;
    let registry = registry_docs(state)?;

    let mut deltas = runtime.skills().list(runtime.id()).await?;
    deltas.extend(skill_effective::globals_skill_disables(
        &runtime.globals_disable().await?,
    ));

    Ok(project(&skill_effective::resolve(
        runtime.source_dir(),
        &registry,
        &deltas,
    )?))
}

/// Projects a resolved effective set into the GraphQL shape.
pub(crate) fn project(effective: &[EffectiveSkill]) -> Vec<SkillGql> {
    effective.iter().map(from_effective).collect()
}

/// Projects one effective entry into a `Skill`. An entry no layer supplied a
/// document for is rendered from its slug alone.
fn from_effective(skill: &EffectiveSkill) -> SkillGql {
    let doc = skill.doc();
    SkillGql {
        id: ID(skill.slug.clone()),
        name: doc
            .map(|doc| doc.name.clone())
            .unwrap_or_else(|| titleize(&skill.slug)),
        description: doc.map(|doc| doc.description.clone()).unwrap_or_default(),
        category: doc
            .and_then(|doc| doc.category.clone())
            .unwrap_or_else(|| DEFAULT_CATEGORY.to_string()),
        source: source_str(skill.source).to_string(),
        enabled: skill.enabled,
        version: doc.and_then(|doc| doc.version.clone()),
    }
}

/// Resolves the top-level `skillRegistry`.
pub(crate) async fn resolve_registry(
    ctx: &Context<'_>,
) -> async_graphql::Result<Vec<RegistrySkillGql>> {
    let state = ctx.data::<AppState>()?;
    Ok(registry_docs(state)?
        .iter()
        .map(|doc| RegistrySkillGql {
            id: ID(doc.slug.clone()),
            name: doc.name.clone(),
            description: doc.description.clone(),
            category: doc
                .category
                .clone()
                .unwrap_or_else(|| DEFAULT_CATEGORY.to_string()),
            publisher: REGISTRY_PUBLISHER.to_string(),
            version: doc.version.clone(),
        })
        .collect())
}
