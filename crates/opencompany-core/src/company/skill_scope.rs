//! One skill's answer to "who is this scoped to" — the inversion of the
//! per-agent allowlist the agent record stores.
//!
//! Scope lives on the agent, as `[[agent]].skills` with any operator override
//! applied, and the teammate's own page reads it forwards: one agent, every
//! skill. A skill's detail panel asks the transposed question — one skill,
//! every agent — and both transports that serve it need the same answer, so
//! the transposition happens once, here.
//!
//! **A read-side projection only.** Nothing in this module stores anything, and
//! there is no per-skill scope field for it to store into: the write stays
//! `PATCH {scope}/team/{agent_id}`, whichever surface issues it.
//!
//! The inputs are already-extracted values rather than a `CompanyRecord`, so
//! this depends on neither the server layer nor the record — which is what lets
//! the REST list and the GraphQL resolver both call it instead of each
//! inverting the allowlist their own way.

use serde::Serialize;

/// Where one agent stands on one skill.
///
/// Three states, because the record's `skills` field has three and collapsing
/// any pair of them reports the opposite of the truth for the agents that are
/// in it. [`Inherited`](Self::Inherited) and [`Excluded`](Self::Excluded) are
/// the pair a naive projection loses: an agent that has never been scoped and
/// an agent scoped to an explicit empty list both hold nothing on a company
/// where the skill is disabled, and only one of them will hold it again when
/// the switch goes back on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillScopeState {
    /// The agent lists no skills of its own, so this skill reaches it whenever
    /// the company has it enabled.
    Inherited,
    /// The agent's own list names this skill.
    Included,
    /// The agent has a list and this skill is not on it — whether the list is
    /// empty or simply narrower.
    Excluded,
}

/// One roster agent's stored scope, as the inversion takes it.
///
/// `requested` is the record's field verbatim, in its three states: `None`
/// inherits, `Some(vec![])` is a deliberate no-skills scope, `Some(slugs)`
/// narrows. It is carried rather than pre-resolved because the state a skill
/// reports is a property of what is *stored*, and an effective set cannot say
/// which of the three produced it.
#[derive(Clone, Debug)]
pub struct AgentSkillScope {
    /// The roster agent's id.
    pub id: String,
    /// The `skills` field of its record, untouched.
    pub requested: Option<Vec<String>>,
}

/// What one skill's scope says about one agent.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillAgentScope {
    /// The roster agent's id.
    pub id: String,
    /// Which of the three stored states this agent is in for this skill.
    pub state: SkillScopeState,
    /// Whether the agent actually gets this skill right now — the scope
    /// resolved against the company's switch, not the scope alone.
    pub holds: bool,
}

/// Every roster agent's standing on one skill.
///
/// `enabled` is the company's switch for this slug. A disabled skill still
/// reports each agent's `state`, because the scope is real and editable while
/// the switch is off; what it cannot report is any agent holding the skill, and
/// `holds` says so.
///
/// Every agent in `agents` gets a row, always. A sparse answer would need a
/// client-side rule for what an absent agent means, and the only honest one
/// ("inherits") is exactly the state that must not be inferred.
///
/// `holds` comes from
/// [`agent_effective_skills`](crate::runtime::builder::agent_effective_skills)
/// — the function the harness materializes each agent's skill tree from —
/// applied to a ceiling holding this slug alone. Narrowing filters the ceiling,
/// so restricting the ceiling to one slug gives that slug's answer out of the
/// full one; deriving it here instead would let the panel advertise reach the
/// harness does not grant.
pub fn agents_for_skill(
    slug: &str,
    enabled: bool,
    agents: &[AgentSkillScope],
) -> Vec<SkillAgentScope> {
    let ceiling: Vec<String> = if enabled {
        vec![slug.to_string()]
    } else {
        Vec::new()
    };
    agents
        .iter()
        .map(|agent| SkillAgentScope {
            id: agent.id.clone(),
            state: state_for(slug, agent.requested.as_deref()),
            holds: crate::runtime::builder::agent_effective_skills(
                &ceiling,
                agent.requested.as_deref(),
            )
            .iter()
            .any(|held| held == slug),
        })
        .collect()
}

/// Which state a stored `skills` value puts one slug in.
///
/// Matching is exact, mirroring `agent_effective_skills`: a slug is a flat
/// identifier and a prefix match would silently reach a skill installed after
/// the scope was written. If a glob vocabulary is ever added, this is the one
/// place it expands.
fn state_for(slug: &str, requested: Option<&[String]>) -> SkillScopeState {
    match requested {
        None => SkillScopeState::Inherited,
        Some(slugs) if slugs.iter().any(|want| want == slug) => SkillScopeState::Included,
        Some(_) => SkillScopeState::Excluded,
    }
}

#[cfg(test)]
#[path = "skill_scope_tests.rs"]
mod tests;
