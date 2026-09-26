//! The **team** section of every agent's system prompt: who else is at the
//! company, which desks they sit on, and who this agent may hand work to.
//!
//! # Why every agent gets one
//!
//! Before this section existed, a non-orchestrator agent was told who *it* was
//! and nothing about anybody else. The orchestrator could learn the roster with
//! `query_company`; a desk lead or a specialist had no such tool, no roster in
//! its prompt, and — unless its manifest entry opted in with `delegates_to` —
//! no hand-off tool either. Asked for something a teammate owned, it did the
//! only thing its briefing allowed: declined, guessed, or said it "could not
//! contact" a colleague sitting on the same desk. The model was not wrong about
//! its situation; the situation was wrong.
//!
//! So the roster is rendered into every agent's prompt, statically, beside the
//! tools that act on it. It is a listing of names, roles and mandates — what a
//! new hire is told on day one — with the id each tool call takes kept beside
//! the name rather than in front of it. It is not a live status board; what a teammate is
//! doing right now is a question for the board tools, not for this section.
//!
//! # Where it sits
//!
//! Appended after the persona and the bundle documents and before the tool
//! briefs, on the same cache-stability argument the rest of the prompt follows
//! (`docs/spec/runtime/agents.md`): the roster changes when an operator adds or
//! removes a teammate, which is exactly when the belt is rebuilt anyway, so it
//! is as static as the briefs behind it.
//!
//! Always compiled, like [`prompt`](crate::company::prompt): `opencompany prompt`
//! renders this section from a manifest alone, so what an operator reads in the
//! dump is what the agent is briefed with.

use crate::ports::types::CompanyRecord;
use crate::runtime::delegation_tools::{
    DELEGATE_TO_TEAMMATE_TOOL, desk_lead, desks_of_member, reach_is_unrestricted, roster_agent_ids,
    teammate_targets,
};

/// The heading the section opens with. Named so the tool descriptions and the
/// orchestrator brief can point at it ("as listed under Your team").
pub const TEAM_HEADING: &str = "## Your team";

/// The team section for `agent_id`, or `""` when it is the only agent at the
/// company — a roster of one has nobody to hand work to, and a heading over an
/// empty list would read as a team that exists and says nothing.
///
/// Lists every *other* roster teammate (manifest agents in declaration order,
/// then operator-added ones; removed ones are not on the effective roster and
/// so not here), then every desk with its members and lead, then which of
/// those this agent sits on and which it may hand work to.
///
/// The reach line is rendered from the same rule the tools enforce at call
/// time ([`teammate_targets`]), so the prompt never names a teammate the tool
/// would then refuse. With an unrestricted reach — the ordinary case, a
/// manifest entry that says nothing — the line says so in one clause rather
/// than repeating the roster.
pub fn team_section(record: &CompanyRecord, agent_id: &str) -> String {
    render(record, agent_id, Audience::Roster)
}

/// [`team_section`] for a hive episode seat.
///
/// A seat's belt has no hand-off tools, so this variant describes who does
/// what and how to name them, and says nothing about handing work on.
pub fn seat_team_section(record: &CompanyRecord, agent_id: &str) -> String {
    render(record, agent_id, Audience::Seat)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Audience {
    Roster,
    Seat,
}

fn render(record: &CompanyRecord, agent_id: &str, audience: Audience) -> String {
    let manifest_roster = record.effective_agents();
    // The manifest roster, then the operator-added teammates — the same order
    // `roster_agent_ids` (and every refusal message) uses, so the listing and
    // the tools agree about who comes first. An overlay teammate has no
    // manifest row; it carries its display name, its role and its mandate,
    // which is all this section needs of anybody.
    let roster: Vec<Teammate<'_>> = manifest_roster
        .iter()
        .map(|agent| Teammate {
            id: &agent.id,
            name: agent.name.as_deref(),
            role: &agent.role,
            description: agent.description.as_deref(),
        })
        .chain(
            record
                .overlay_agents
                .iter()
                .filter(|overlay| !manifest_roster.iter().any(|agent| agent.id == overlay.id))
                .map(|overlay| Teammate {
                    id: &overlay.id,
                    name: Some(&overlay.name),
                    role: &overlay.role,
                    description: overlay.description.as_deref(),
                }),
        )
        .collect();
    let others: Vec<&Teammate<'_>> = roster.iter().filter(|agent| agent.id != agent_id).collect();
    if others.is_empty() {
        return String::new();
    }
    let orchestrator = crate::company::orchestrator_id(&manifest_roster).map(str::to_string);
    let company = record.manifest.company.name.trim();
    let delegates_to: &[String] = manifest_roster
        .iter()
        .find(|agent| agent.id == agent_id)
        .map(|agent| agent.delegates_to.as_slice())
        .unwrap_or(&[]);

    // Whether the reach line at the bottom will narrow the roster. Decided up
    // front so the opening sentence and that line cannot contradict each other.
    let unrestricted = reach_is_unrestricted(delegates_to);
    let reachable = match unrestricted {
        true => Vec::new(),
        false => teammate_targets(record, agent_id, delegates_to),
    };
    let narrowed =
        !unrestricted && reachable.len() < roster_agent_ids(record).len().saturating_sub(1);

    let mut out = String::new();
    out.push_str("\n\n");
    out.push_str(TEAM_HEADING);
    out.push_str(&format!(
        "\n\nYou are one of {} teammates at {company}, and you are not working alone. ",
        roster.len(),
    ));
    match audience {
        Audience::Seat => out.push_str(
            "Every teammate below is a real agent at this company. Call them by name when you \
             write; an id is for a tool call.\n\nTeammates (name, role: mandate):\n",
        ),
        Audience::Roster if orchestrator.as_deref() == Some(agent_id) => {
            out.push_str(match narrowed {
                false => {
                    "Every teammate below is a real agent you can hand work to: they run it and \
                     hand their answer back to you in this same turn. Never tell anyone a \
                     teammate is out of reach or that you cannot contact them — you can, with "
                }
                true => {
                    "Every teammate below is a real agent; the ones you may hand work to are \
                     named at the end of this section, and they hand their answer back to you \
                     in this same turn. The tool for that is "
                }
            });
            out.push_str(&format!(
                "`{DELEGATE_TO_TEAMMATE_TOOL}`.\n\nTeammates (name, role: mandate). Hand work \
                 to one with `{DELEGATE_TO_TEAMMATE_TOOL}`: pass the id in the tool call; call \
                 them by name when you write.\n"
            ));
        }
        Audience::Roster => out.push_str(
            "Every teammate below is a real agent. When you are in a room with one — a desk, or \
             a conversation somebody opened with you — you can ask them directly and their \
             answer reaches you there. Otherwise the way to put work on a teammate is to open a \
             card for it with `spawn_task`, naming them; never say a teammate is out of \
             reach. Call them by name when you write; an id is for a tool call.\n\nTeammates \
             (name, role: mandate):\n",
        ),
    }
    for agent in &others {
        out.push_str("- ");
        out.push_str(agent.label());
        if !agent.label().eq_ignore_ascii_case(agent.role.trim()) {
            out.push_str(", ");
            out.push_str(agent.role.trim());
        }
        if orchestrator.as_deref() == Some(agent.id) {
            out.push_str(
                " (the orchestrator: the operator's point of contact, who can bring anyone in \
                 and owns the board)",
            );
        }
        if let Some(description) = agent.description.map(str::trim)
            && !description.is_empty()
        {
            out.push_str(": ");
            out.push_str(description);
        }
        out.push_str(&format!(" (id `{}` for tool calls)\n", agent.id));
    }

    // **The desks this agent sits on, not every desk the company has.**
    //
    // A seat acts where it sits. The full org chart was the whole of this
    // section on a large roster, listing membership and a lead for rooms this
    // agent will never take a turn in — and in a hive turn it arrives beside
    // `EpisodePrompt::peers`, which lists the desks it may actually ask,
    // filtered by the referral policy. The same desk was therefore described
    // twice in one turn, once as somewhere to hand work whose lead answers and
    // once as somewhere to put a question that the room answers, which are
    // different mechanisms with different costs (#2368).
    //
    // What is deliberately NOT filtered is the roster above: knowing who does
    // what is how a seat knows who is worth asking, and hiding that is the
    // failure this whole section exists to fix — "declined, guessed, or said it
    // could not contact a colleague sitting on the same desk".
    let desks = desks_of_member(record, agent_id);
    if !desks.is_empty() {
        out.push_str("\nYou sit on (desk: members):\n");
        for desk in &desks {
            let lead = desk_lead(record, desk);
            let members: Vec<String> = record
                .effective_desk_members(desk)
                .into_iter()
                .filter(|member| record.is_roster_agent(member))
                .map(|member| {
                    let name = roster
                        .iter()
                        .find(|agent| agent.id == member)
                        .map_or(member.as_str(), Teammate::label);
                    match lead.as_deref() == Some(member.as_str()) {
                        true => format!("{name} (lead)"),
                        false => name.to_string(),
                    }
                })
                .collect();
            out.push_str("- ");
            out.push_str(&desk_label(record, desk));
            out.push_str(": ");
            out.push_str(match members.is_empty() {
                true => "nobody on the roster yet",
                false => "",
            });
            out.push_str(&members.join(", "));
            out.push_str(&format!(" (id `{desk}` for tool calls)\n"));
        }
    }

    // The reach, rendered from the rule the tool enforces. Only worth a line
    // when it is narrower than "everyone above", which the opening already says.
    if narrowed && audience == Audience::Roster {
        out.push_str(&match reachable.is_empty() {
            true => "\nYour manifest entry does not let you hand work to anyone listed above. \
                     They are listed so you know who does what: answer what you can yourself, \
                     and say plainly who should be brought in.\n"
                .to_string(),
            false => format!(
                "\nYou may hand work to: {}. The rest are listed so you know who does what — \
                 say who should be brought in rather than handing to them.\n",
                reachable
                    .iter()
                    .map(|id| {
                        let name = roster
                            .iter()
                            .find(|agent| agent.id == id.as_str())
                            .map_or(id.as_str(), Teammate::label);
                        format!("{name} (`{id}`)")
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
    }
    out
}

/// One roster entry as this section renders it — the four things a new hire
/// is told about a colleague, whichever of the two roster halves they are on.
struct Teammate<'a> {
    id: &'a str,
    name: Option<&'a str>,
    role: &'a str,
    description: Option<&'a str>,
}

impl Teammate<'_> {
    /// What a person calls them: their display name, else their role.
    fn label(&self) -> &str {
        self.name
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(self.role.trim())
    }
}

/// A desk's operator-facing name, or its id when it has none to show.
pub(crate) fn desk_label(record: &CompanyRecord, desk_id: &str) -> String {
    record
        .manifest
        .group_chats
        .iter()
        .find(|chat| chat.id == desk_id)
        .map(|chat| chat.name.clone())
        .or_else(|| {
            record
                .overlay_desks
                .iter()
                .find(|desk| desk.id == desk_id)
                .map(|desk| desk.name.clone())
        })
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| desk_id.to_string())
}

#[cfg(test)]
#[path = "team_brief_tests.rs"]
mod tests;
