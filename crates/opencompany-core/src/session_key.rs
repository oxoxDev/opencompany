//! The name an agent's openhuman session answers to.
//!
//! # Why this is a module and not a `format!` at the builder
//!
//! Every OpenCompany teammate already **is** an openhuman session: the pool
//! holds one `openhuman_embed::Agent` — "stateful agent session, the single
//! execution tier" in the vendored crate's own words — per `(company,
//! agent_id)`, behind a mutex, because a turn takes `&mut self` and one session
//! must serialise its own turns. What it did not have was a *name*.
//!
//! The previous `AgentBuilder` defaulted `event_session_id` to the
//! literal string `"standalone"` and `event_channel` to `"internal"`, and
//! OpenCompany never set either. Those two fields are the identity every
//! `DomainEvent` the session publishes is tagged with — `AgentTurnStarted`,
//! `AgentTurnCompleted`, `AgentError` — plus the `PromptEnforcementContext` a
//! blocked prompt is reported against and the `report_error` tags beside it. So
//! every agent of every company on the process announced itself as the same
//! session, and a bus subscriber could not tell whose turn had started, whose
//! had failed, or whose prompt had been refused.
//!
//! That cost nothing while one agent ran at a time. It stops being free now:
//! openhuman's library host runs many sessions over one core concurrently (the
//! upstream `HostKind::Library` work sharded the conversation store's
//! process-wide mutex into per-root lifecycle, per-root metadata and per-thread
//! transcript locks, and proved 100 overlapping turns on distinct session ids).
//! Concurrency is exactly the condition under which an unlabelled event stream
//! stops being readable — with one turn in flight, `session=standalone` is
//! unambiguous by luck.
//!
//! So the key is minted here, once, and the two consumers that must agree about
//! it read the same function: the builder that stamps it onto the session, and
//! the speech tools, which name the destination session when one teammate
//! leaves a DM for another. A DM is a hop from one openhuman session to
//! another, and it can only be reported as one if both ends spell the session
//! the same way.
//!
//! # Shape
//!
//! `{company}:{agent_id}` — company first, because the process is multi-tenant
//! and an `agent_id` is only unique within its own company. Two companies that
//! both call a teammate `designer` are two sessions, and sorting the bus by
//! prefix groups a tenant's traffic together.
//!
//! Ungated, and deliberately free of any openhuman type: the speech tools and
//! the operator routes are not behind `feature = "openhuman"`, and a key they
//! cannot name is a key they cannot report.

use crate::ports::CompanyId;

/// The `event_channel` every OpenCompany session declares.
///
/// openhuman's own hosts use this to say which front end a turn came from
/// (`"cli"`, `"telegram"`, `"rpc"`); the builder's default is `"internal"`,
/// which is what a session that nobody labelled looks like. Every session here
/// is driven by this product, so the honest answer is one constant rather than
/// a per-surface value: the surface an OpenCompany turn arrived on — a desk, a
/// DM, a card, a workflow — is carried by the cue the session is handed
/// (the conversation cue on the turn text), not by the bus label.
pub const SESSION_CHANNEL: &str = "opencompany";

/// The openhuman session id for one teammate of one company.
///
/// Stable across rebuilds by construction — it is a pure function of the two
/// ids, and neither moves for the life of a teammate. That matters because the
/// roster is rebuilt whenever any of its ten freshness fingerprints moves (an
/// MCP server, a skill, a budget, a persona edit…): a key derived from anything
/// that a rebuild disturbs would rename the session under a subscriber roughly
/// whenever an operator touched a setting.
pub fn openhuman_session_key(company: &CompanyId, agent_id: &str) -> String {
    format!("{company}:{agent_id}")
}

/// The id a company agent is registered under on the process-wide OpenHuman
/// [`Runtime`](openhuman_embed::Runtime): `{company}--{agent_id}`, normalised
/// to what `openhuman_embed` accepts (`^[a-z0-9][a-z0-9_-]{0,63}$`).
///
/// Distinct from [`openhuman_session_key`] on purpose. The session key names
/// a *thread* (`Turn::session`) and may carry any character; the runtime id
/// names the agent whose transcripts directory and skills root that thread
/// lives under, and the runtime validates it. Lower-cased, every other
/// character folded to `-`, and — because an id longer than 64 bytes is
/// refused outright — truncated to 56 bytes plus a 7-character hash of the
/// full form so two long ids that agree on their first 56 bytes still get
/// two agents.
pub fn runtime_agent_id(company: &CompanyId, agent_id: &str) -> String {
    let raw = format!("{company}--{agent_id}").to_ascii_lowercase();
    let mut folded: String = raw
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' | '_' | '-' => c,
            _ => '-',
        })
        .collect();
    if !folded
        .as_bytes()
        .first()
        .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        folded.insert(0, 'a');
    }
    if folded.len() <= 64 {
        return folded;
    }
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(raw.as_bytes());
    let hash: String = digest
        .iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
        .chars()
        .take(7)
        .collect();
    let mut head: String = folded.chars().take(56).collect();
    while head.ends_with('-') {
        head.pop();
    }
    format!("{head}-{hash}")
}

#[cfg(test)]
#[path = "session_key_tests.rs"]
mod tests;
