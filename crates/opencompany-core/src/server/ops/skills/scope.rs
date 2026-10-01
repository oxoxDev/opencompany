//! Who a skill is scoped to, on the skill routes' answers.
//!
//! The inversion itself is [`crate::company::skill_scope`], which both
//! transports share. What lives here is the one piece that needs the server
//! layer: reading the company record so the roster's stored scopes can be handed
//! to it.
//!
//! Split out of `skills.rs` rather than added to it because that file is at the
//! source-layout cap; the seam is the same one `drift.rs` and `journal.rs` took.

use crate::company::runtime::CompanyRuntime;
use crate::company::skill_scope::{AgentSkillScope, SkillAgentScope};
use crate::error::Result;

use super::InstalledSkill;

impl InstalledSkill {
    /// Attaches every roster agent's standing on this skill.
    ///
    /// Always called on an answer the console folds into its list: a row whose
    /// `agents` went absent reads as "this host cannot say", and the skill's
    /// detail panel then offers no picker. That is the safe direction — an empty
    /// list would claim the company has no teammates — but it is not a state any
    /// route here should reach.
    pub(super) fn with_agents(mut self, agents: Vec<SkillAgentScope>) -> Self {
        self.agents = Some(agents);
        self
    }
}

/// Every roster agent's stored skill scope, for
/// [`agents_for_skill`](crate::company::skill_scope::agents_for_skill).
///
/// One record load for the whole answer, however many skills it carries: the
/// scope of a roster of N agents over M skills is one read of N rows, not N×M.
/// `agent_detail` already pairs this load with the company's enabled set, so the
/// cost is precedented on the routes a console page opens with.
///
/// The ids are [`roster_ids`](crate::server::ops::team::roster_ids) — the union
/// `CompanyRecord::is_roster_agent` accepts, which is a subset of what
/// `GET {scope}/team` lists. That direction matters: the panel ticks an agent
/// from this projection and computes its next list from the roster read's
/// `skills`, so an agent here that the roster read omits would be a checkbox
/// with nothing to write.
///
/// A company with no persisted record yet answers with no agents, the same
/// soft-fail `GET {scope}/team` uses rather than a 404.
///
/// Errors in the crate's own vocabulary rather than a transport's, because the
/// GraphQL resolver reads through here too — a second copy of this loop is how
/// one transport comes to report a scope the other does not.
pub(crate) async fn roster_scopes(runtime: &CompanyRuntime) -> Result<Vec<AgentSkillScope>> {
    let Some(record) = runtime.store().load(runtime.id()).await? else {
        return Ok(Vec::new());
    };
    Ok(crate::server::ops::team::roster_ids(&record)
        .map(|id| AgentSkillScope {
            id: id.clone(),
            requested: crate::server::ops::team_agent::requested_skills(&record, id),
        })
        .collect())
}

#[cfg(test)]
#[path = "scope_tests.rs"]
mod tests;
