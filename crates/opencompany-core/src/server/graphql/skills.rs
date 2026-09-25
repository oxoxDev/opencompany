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
use crate::company::runtime::CompanyRuntime;
use crate::company::skill_effective::{self, EffectiveSkill};
use crate::company::skill_scope::{
    AgentSkillScope, SkillAgentScope, SkillScopeState, agents_for_skill,
};
use crate::company::{SkillDoc, VersionChange, effective_drift};
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
    /// The revisions either side of a library change, when the library's
    /// document has moved since this install pinned its snapshot. `None` when it
    /// has not, and on a row that pinned nothing.
    pub update_available: Option<SkillUpdateGql>,
    /// Whether the stored document no longer matches the digest recorded at
    /// install. `false` on a row that pinned nothing, so a reader never has to
    /// tell "clean" apart from "absent".
    pub modified: bool,
    /// Where every roster agent stands on this skill — the read-side inversion
    /// of the per-agent allowlist, the same projection `GET …/skills` reports.
    ///
    /// A list, never null: this resolver loads the company record, so it can
    /// always say. An empty list means the company has no teammates.
    pub agents: Vec<SkillAgentScopeGql>,
}

/// One roster agent's standing on one skill.
///
/// A GraphQL type of its own rather than an `async_graphql` derive on
/// [`SkillAgentScope`]: that type is `company`'s vocabulary, shared with the
/// REST projection, and a transport's schema attributes have no business on it.
#[derive(SimpleObject)]
#[graphql(name = "SkillAgentScope")]
pub struct SkillAgentScopeGql {
    /// The roster agent's id.
    pub id: ID,
    /// Which of the three stored states this agent is in for this skill:
    /// `inherited` | `included` | `excluded`.
    ///
    /// `inherited` and `excluded` are not interchangeable. An agent that has
    /// never been scoped and one given an explicit empty list both hold nothing
    /// while the skill is disabled, and only the first holds it again when the
    /// switch goes back on.
    pub state: String,
    /// Whether the agent actually gets this skill right now — the scope resolved
    /// against the company's switch, not the scope alone.
    pub holds: bool,
}

impl SkillAgentScopeGql {
    /// Projects `company`'s [`SkillAgentScope`] onto this transport's shape.
    fn of(scope: &SkillAgentScope) -> Self {
        Self {
            id: ID(scope.id.clone()),
            state: match scope.state {
                SkillScopeState::Inherited => "inherited",
                SkillScopeState::Included => "included",
                SkillScopeState::Excluded => "excluded",
            }
            .to_string(),
            holds: scope.holds,
        }
    }
}

/// The two revisions either side of a library change.
///
/// A GraphQL type of its own rather than an `async_graphql` derive on
/// [`VersionChange`]: that type is `company`'s vocabulary, shared with the REST
/// projection and the store, and a transport's schema attributes have no
/// business on it.
///
/// Neither side is ordered against the other — `version` is free text a
/// publisher writes, so a reader may say the document *changed*, never that it
/// is *newer*.
#[derive(SimpleObject)]
#[graphql(name = "SkillUpdate")]
pub struct SkillUpdateGql {
    /// The revision recorded when the install pinned its snapshot.
    pub from: Option<String>,
    /// The revision the library's current document declares.
    pub to: Option<String>,
}

impl SkillUpdateGql {
    /// Projects `company`'s [`VersionChange`] onto this transport's shape.
    ///
    /// A named constructor rather than a `From` impl: `from` is also one of this
    /// type's own fields, and the derive generates a resolver for it.
    fn of(change: VersionChange) -> Self {
        Self {
            from: change.from,
            to: change.to,
        }
    }
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

    Ok(project(
        &skill_effective::resolve(runtime.source_dir(), &registry, &deltas)?,
        &registry,
        &crate::server::ops::skills::scope::roster_scopes(runtime).await?,
    ))
}

/// Projects a resolved effective set into the GraphQL shape.
///
/// `registry` is the host's shared library, which a pinned install's drift is
/// measured against — the same input the REST list projects from, so the two
/// transports cannot report a different standing for one install.
pub(crate) fn project(
    effective: &[EffectiveSkill],
    registry: &[SkillDoc],
    roster: &[AgentSkillScope],
) -> Vec<SkillGql> {
    effective
        .iter()
        .map(|skill| from_effective(skill, registry, roster))
        .collect()
}

/// Projects one effective entry into a `Skill`. An entry no layer supplied a
/// document for is rendered from its slug alone.
fn from_effective(
    skill: &EffectiveSkill,
    registry: &[SkillDoc],
    roster: &[AgentSkillScope],
) -> SkillGql {
    let drifted = effective_drift(skill, registry);
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
        update_available: drifted
            .as_ref()
            .and_then(|drifted| drifted.update_available.clone())
            .map(SkillUpdateGql::of),
        modified: drifted.is_some_and(|drifted| drifted.modified),
        agents: agents_for_skill(&skill.slug, skill.enabled, roster)
            .iter()
            .map(SkillAgentScopeGql::of)
            .collect(),
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
