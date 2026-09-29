//! How a desk routes and paces the episodes it opens: the `[group_chat.routing]`
//! block, its resolved policy, and the console's view of both.
//!
//! One block, three readers. The manifest declares it (`RoutingConfig`, every
//! key optional so "not said" stays distinct from any value it could hold);
//! the runtime resolves it into the `tinyhivemind_embed::RoutingPolicy` a
//! `CompletionDriver` and a Jev route are frozen on (`EffectiveRouting`); and
//! the console reads the two side by side (`DeskRoutingDto`), because a
//! `round_width = 2` is either an operator's decision or the library default
//! and the two behave differently the moment the manifest changes.
//!
//! The overlay analogue (`PUT {scope}/desks/{id}/routing`) stores the same
//! `RoutingConfig` wholesale, exactly as the move grammar it replaced did: a
//! routing block is one artefact, and merging a field into a stored block is
//! how a desk ends up paced by numbers nobody authored.
//!
//! `RoutingPlanDto` is tinyhivemind's `RoutingPlan` on the wire — the shape
//! every episode frame and every `episode.routedBy` carries — kept here so the
//! journal, the SSE projection and the history projection cannot spell it
//! three ways.

use serde::{Deserialize, Serialize};
use tinyhivemind::referral::{ReferralPolicy, ReferralReach};
use tinyhivemind::responder::{PROBABILITY_SCALE, Probability};
use tinyhivemind_embed::{RoutingFallback, RoutingPlan, RoutingPolicy};

use crate::ports::types::CompanyRecord;

/// Seats a round may run at once, when the block does not say.
pub const DEFAULT_ROUND_WIDTH: usize = 5;
/// Alternatives one System One Choice may hold (including `none`), by default.
pub const DEFAULT_CHOICE_OPTION_LIMIT: usize = 8;
/// Rounds an episode may run before the host closes it as `round_cap`.
pub const DEFAULT_MAX_ROUNDS: u32 = 12;
/// Seconds one seat turn may take, counted from the moment it holds its lock.
pub const DEFAULT_TURN_TIMEOUT_SECS: u64 = 600;
/// Referral hops allowed when `[group_chat.routing.referral]` enables crossing
/// without saying how far.
pub const DEFAULT_REFERRAL_MAX_HOPS: u32 = 1;

/// The words `referral.reach` accepts, in the manifest's own spelling.
pub const REACH_WORDS: &[&str] = &["local", "channels", "desks"];

/// The `[group_chat.routing]` block as authored.
///
/// Snake_case on every wire on purpose: this **is** the manifest block, the
/// console's editor round-trips it, and a camelCase twin would be a second
/// shape to keep in step with the TOML. Unknown keys are ignored rather than
/// refused, so a stored overlay written by a newer host still loads.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RoutingConfig {
    /// Seats that may run in one round, the primary included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round_width: Option<usize>,
    /// Alternatives one System One Choice may hold, `none` included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice_option_limit: Option<usize>,
    /// Least Choice concentration accepted for the primary seat, `0..=1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_confidence: Option<f64>,
    /// The higher bar a high-impact request must clear, `0..=1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high_impact_minimum_confidence: Option<f64>,
    /// Probability at which missing information stops routing, `0..=1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clarification_threshold: Option<f64>,
    /// Probability at which the high-impact rule applies, `0..=1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high_impact_threshold: Option<f64>,
    /// Rounds an episode may run before the host closes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<u32>,
    /// Seconds one seat turn may take once it holds its turn lock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_timeout_secs: Option<u64>,
    /// Whether and how far a seat may put a question to another desk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referral: Option<ReferralConfig>,
}

// Probabilities are `f64`, which is not `Eq`; the record types that hold a
// block derive `Eq`, and validation refuses `NaN`, so total equality holds for
// every value a block can carry.
impl Eq for RoutingConfig {}

/// The `[group_chat.routing.referral]` block as authored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferralConfig {
    /// Whether a seat's `@#desk` / `@agent` may open an episode elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// How many crossings one question may make.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_hops: Option<u32>,
    /// `local` | `channels` | `desks` — see [`REACH_WORDS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reach: Option<String>,
    /// Whether the far desk's answer is carried back to the asking desk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<bool>,
}

impl RoutingConfig {
    /// Whether nothing was said — the serializer's skip rule and the
    /// overlay's "nothing installed" test.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Every problem with the block, in the words the manifest reports them.
    ///
    /// One implementation for the manifest loader and the install route, so
    /// the runtime cannot accept a block `company.toml` would refuse. `label`
    /// names the desk in the caller's words.
    #[must_use]
    pub fn problems(&self, label: &str) -> Vec<String> {
        let mut problems = Vec::new();
        if self.round_width == Some(0) {
            problems.push(format!(
                "{label} sets `routing.round_width = 0` — a round of nobody never speaks; omit the key to run {DEFAULT_ROUND_WIDTH} seats at once."
            ));
        }
        if self.choice_option_limit.is_some_and(|limit| limit < 2) {
            problems.push(format!(
                "{label} sets `routing.choice_option_limit` below 2 — a Choice needs one seat and `none`; omit the key for the default of {DEFAULT_CHOICE_OPTION_LIMIT}."
            ));
        }
        for (key, value) in [
            ("minimum_confidence", self.minimum_confidence),
            (
                "high_impact_minimum_confidence",
                self.high_impact_minimum_confidence,
            ),
            ("clarification_threshold", self.clarification_threshold),
            ("high_impact_threshold", self.high_impact_threshold),
        ] {
            if value.is_some_and(|value| !(0.0..=1.0).contains(&value) || value.is_nan()) {
                problems.push(format!(
                    "{label} sets `routing.{key}` outside 0..=1 — it is a probability."
                ));
            }
        }
        if self.max_rounds == Some(0) {
            problems.push(format!(
                "{label} sets `routing.max_rounds = 0` — an episode with no rounds can never complete; omit the key for the default of {DEFAULT_MAX_ROUNDS}."
            ));
        }
        if self.turn_timeout_secs == Some(0) {
            problems.push(format!(
                "{label} sets `routing.turn_timeout_secs = 0` — every seat would time out before it spoke; omit the key for the default of {DEFAULT_TURN_TIMEOUT_SECS}."
            ));
        }
        if let Some(referral) = &self.referral {
            if let Some(reach) = referral.reach.as_deref()
                && !REACH_WORDS.contains(&reach)
            {
                problems.push(format!(
                    "{label} `routing.referral.reach` must be one of {REACH_WORDS:?}; got `{reach}`."
                ));
            }
            if referral.max_hops == Some(0) && referral.enabled != Some(false) {
                problems.push(format!(
                    "{label} sets `routing.referral.max_hops = 0` — a referral budget of nothing never asks anybody anything; omit `routing.referral` to keep the desk inside its own room."
                ));
            }
        }
        problems
    }
}

/// The numbers the runtime will actually use, every default resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectiveRouting {
    /// Seats one round runs at once.
    pub round_width: usize,
    /// Alternatives one Choice may hold.
    pub choice_option_limit: usize,
    /// Rounds before the host closes the episode as `round_cap`.
    pub max_rounds: u32,
    /// Seat turn timeout, counted from lock acquisition.
    pub turn_timeout_secs: u64,
    /// Acceptance thresholds, `0..=1`.
    pub minimum_confidence: f64,
    /// The high-impact bar, `0..=1`.
    pub high_impact_minimum_confidence: f64,
    /// Clarification threshold, `0..=1`.
    pub clarification_threshold: f64,
    /// High-impact threshold, `0..=1`.
    pub high_impact_threshold: f64,
    /// Cross-desk referral policy.
    pub referral: EffectiveReferral,
}

/// The referral policy in force.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveReferral {
    /// Whether crossing is on.
    pub enabled: bool,
    /// Hops one question may make.
    pub max_hops: u32,
    /// How far a mention may reach.
    pub reach: ReferralReach,
    /// Whether the answer comes home.
    pub returns: bool,
}

impl EffectiveRouting {
    /// Resolves a declared block against the defaults.
    #[must_use]
    pub fn resolve(config: &RoutingConfig) -> Self {
        let referral = config.referral.clone().unwrap_or_default();
        let enabled = referral.enabled.unwrap_or(false);
        Self {
            round_width: config.round_width.unwrap_or(DEFAULT_ROUND_WIDTH).max(1),
            choice_option_limit: config
                .choice_option_limit
                .unwrap_or(DEFAULT_CHOICE_OPTION_LIMIT)
                .max(2),
            max_rounds: config.max_rounds.unwrap_or(DEFAULT_MAX_ROUNDS).max(1),
            turn_timeout_secs: config
                .turn_timeout_secs
                .unwrap_or(DEFAULT_TURN_TIMEOUT_SECS)
                .max(1),
            minimum_confidence: config.minimum_confidence.unwrap_or(0.0),
            high_impact_minimum_confidence: config.high_impact_minimum_confidence.unwrap_or(0.0),
            clarification_threshold: config.clarification_threshold.unwrap_or(1.0),
            high_impact_threshold: config.high_impact_threshold.unwrap_or(1.0),
            referral: EffectiveReferral {
                enabled,
                max_hops: referral.max_hops.unwrap_or(DEFAULT_REFERRAL_MAX_HOPS),
                reach: match referral.reach.as_deref() {
                    Some("local") => ReferralReach::Local,
                    Some("channels") => ReferralReach::Channels,
                    // Enabled without a word: crossing is the point of the
                    // block, so the widest reach is the one it meant.
                    Some("desks") | None => ReferralReach::Desks,
                    Some(_) => ReferralReach::Local,
                },
                returns: referral.returns.unwrap_or(true),
            },
        }
    }

    /// The frozen acceptance policy a Jev route and a broadcast fold read.
    #[must_use]
    pub fn policy(&self) -> RoutingPolicy {
        RoutingPolicy {
            minimum_confidence: probability(self.minimum_confidence),
            high_impact_minimum_confidence: probability(self.high_impact_minimum_confidence),
            clarification_threshold: probability(self.clarification_threshold),
            high_impact_threshold: probability(self.high_impact_threshold),
            round_width: self.round_width,
            choice_option_limit: self.choice_option_limit,
        }
    }

    /// The referral policy `tinyhivemind::referral::referral` decides under.
    #[must_use]
    pub fn referral_policy(&self) -> ReferralPolicy {
        ReferralPolicy {
            enabled: self.referral.enabled,
            max_hops: self.referral.max_hops,
            reach: self.referral.reach,
            returns: self.referral.returns,
        }
    }

    /// The seat turn timeout as a duration.
    #[must_use]
    pub fn turn_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.turn_timeout_secs)
    }
}

/// A unit-interval float as tinyhivemind's fixed-point probability.
#[must_use]
pub fn probability(value: f64) -> Probability {
    let parts = (value.clamp(0.0, 1.0) * f64::from(PROBABILITY_SCALE)).round();
    // The clamp keeps `parts` inside `0..=PROBABILITY_SCALE`, so the cast
    // cannot truncate and `new` cannot refuse.
    Probability::new(parts as u32).unwrap_or(Probability::ZERO)
}

/// Where a desk's routing block in force came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoutingSource {
    /// An operator installed it through `PUT {scope}/desks/{id}/routing`.
    Overlay,
    /// The manifest's `[[group_chat]]` declares it.
    Manifest,
    /// Neither says anything; the defaults apply.
    Default,
}

/// Which router chose the seats of a round.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Router {
    /// The System One (Jev) router answered.
    Jev,
    /// The host's deterministic lead/order fallback.
    Fallback,
    /// An explicit mention named the seat.
    Explicit,
}

/// Which router a plan came from, read off the plan itself.
#[must_use]
pub fn router_of(plan: &RoutingPlan) -> Router {
    match plan {
        RoutingPlan::Fallback {
            reason: RoutingFallback::ExplicitMention,
            ..
        } => Router::Explicit,
        RoutingPlan::Fallback { .. } => Router::Fallback,
        RoutingPlan::One { .. } | RoutingPlan::Hive { .. } | RoutingPlan::Clarify { .. } => {
            Router::Jev
        }
    }
}

/// tinyhivemind's `RoutingPlan` on the wire. Mirrors `RoutingPlanDto` in
/// `frontend/src/api/types.ts`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum RoutingPlanDto {
    /// Run one seat.
    #[serde(rename_all = "camelCase")]
    One {
        /// The seat.
        primary_id: String,
    },
    /// Run the primary and the invited seats concurrently.
    #[serde(rename_all = "camelCase")]
    Hive {
        /// The primary seat.
        primary_id: String,
        /// The invited seats, in invitation order.
        invited_ids: Vec<String>,
    },
    /// Ask before routing.
    #[serde(rename_all = "camelCase")]
    Clarify {
        /// The question the host relays, when it has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        question: Option<String>,
    },
    /// The deterministic destination, and why the router did not decide.
    #[serde(rename_all = "camelCase")]
    Fallback {
        /// The seat.
        primary_id: String,
        /// `snake_case` of `tinyhivemind_embed::RoutingFallback`.
        reason: String,
    },
}

impl RoutingPlanDto {
    /// The seats the plan runs, primary first. Empty for a clarification.
    #[must_use]
    pub fn agent_ids(&self) -> Vec<String> {
        match self {
            Self::One { primary_id } | Self::Fallback { primary_id, .. } => {
                vec![primary_id.clone()]
            }
            Self::Hive {
                primary_id,
                invited_ids,
            } => std::iter::once(primary_id.clone())
                .chain(invited_ids.iter().cloned())
                .collect(),
            Self::Clarify { .. } => Vec::new(),
        }
    }

    /// The router a plan of this shape came from. A fallback carrying
    /// `explicit_mention` is the mention rung; every other fallback is the
    /// host's deterministic rule.
    #[must_use]
    pub fn router(&self) -> Router {
        match self {
            Self::Fallback { reason, .. } if reason == "explicit_mention" => Router::Explicit,
            Self::Fallback { .. } => Router::Fallback,
            Self::One { .. } | Self::Hive { .. } | Self::Clarify { .. } => Router::Jev,
        }
    }
}

impl From<&RoutingPlan> for RoutingPlanDto {
    fn from(plan: &RoutingPlan) -> Self {
        match plan {
            RoutingPlan::One { responder_id, .. } => Self::One {
                primary_id: responder_id.clone(),
            },
            RoutingPlan::Hive {
                primary_id,
                invited_ids,
                ..
            } => Self::Hive {
                primary_id: primary_id.clone(),
                invited_ids: invited_ids.clone(),
            },
            RoutingPlan::Clarify { .. } => Self::Clarify { question: None },
            RoutingPlan::Fallback {
                responder_id,
                reason,
            } => Self::Fallback {
                primary_id: responder_id.clone(),
                reason: fallback_word(*reason).to_string(),
            },
        }
    }
}

/// `snake_case` of a fallback reason, matching its serde spelling.
#[must_use]
pub fn fallback_word(reason: RoutingFallback) -> &'static str {
    match reason {
        RoutingFallback::ExplicitMention => "explicit_mention",
        RoutingFallback::DirectConversation => "direct_conversation",
        RoutingFallback::SurfaceRule => "surface_rule",
        RoutingFallback::ProviderUnavailable => "provider_unavailable",
        RoutingFallback::RejectedOutput => "rejected_output",
        RoutingFallback::InvalidBroadcast => "invalid_broadcast",
        RoutingFallback::StaleRoster => "stale_roster",
        RoutingFallback::EscalationFailed => "escalation_failed",
        RoutingFallback::NoEligibleCandidate => "no_eligible_candidate",
    }
}

/// The routing block in force on a desk, and where it came from.
///
/// Precedence is [`CompanyRecord::effective_desk_hive`]'s; this only names the
/// rung that answered. A console-created desk's own block counts as
/// `Manifest`: it is the desk's declaration, not an operator override of one.
#[must_use]
pub fn effective_routing(record: &CompanyRecord, desk_id: &str) -> (RoutingConfig, RoutingSource) {
    let config = record.effective_desk_hive(desk_id);
    let source = if record.desk_hive_is_installed(desk_id) {
        RoutingSource::Overlay
    } else if config.is_default() {
        RoutingSource::Default
    } else {
        RoutingSource::Manifest
    };
    (config, source)
}

/// The resolved numbers a desk runs under.
#[must_use]
pub fn desk_routing(record: &CompanyRecord, desk_id: &str) -> EffectiveRouting {
    EffectiveRouting::resolve(&effective_routing(record, desk_id).0)
}

/// `GET/PUT/DELETE {scope}/desks/{id}/routing`. Mirrors `DeskRoutingDto` in
/// `frontend/src/api/types.ts`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeskRoutingDto {
    /// The desk.
    pub desk_id: String,
    /// Where the block in force came from.
    pub source: RoutingSource,
    /// The block as authored — snake_case, it is the manifest block.
    pub declared: RoutingConfig,
    /// What the runtime will use.
    pub effective: EffectiveRoutingDto,
    /// Every seat the router may pick, with the other desks it also sits on.
    pub candidates: Vec<RoutingCandidateDto>,
}

/// The resolved numbers, camelCase for the console.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveRoutingDto {
    /// Seats per round.
    pub round_width: usize,
    /// Choice alternatives.
    pub choice_option_limit: usize,
    /// Round cap.
    pub max_rounds: u32,
    /// Seat timeout.
    pub turn_timeout_secs: u64,
    /// Which router picks the seats on this host.
    pub router: Router,
    /// Acceptance thresholds, present whenever the block or the default says.
    pub minimum_confidence: f64,
    /// The high-impact bar.
    pub high_impact_minimum_confidence: f64,
    /// Clarification threshold.
    pub clarification_threshold: f64,
    /// High-impact threshold.
    pub high_impact_threshold: f64,
    /// Referral policy in force.
    pub referral: EffectiveReferralDto,
}

/// The referral policy, camelCase for the console.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveReferralDto {
    /// Whether crossing is on.
    pub enabled: bool,
    /// Hop budget.
    pub max_hops: u32,
    /// Reach word.
    pub reach: String,
    /// Whether answers come home.
    pub returns: bool,
}

/// One seat the router may pick.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingCandidateDto {
    /// The agent id.
    pub agent_id: String,
    /// Display label.
    pub label: String,
    /// The agent's role.
    pub role: String,
    /// The other desks this agent also sits on — the seat whose turn another
    /// desk's round can delay.
    pub shared_with: Vec<String>,
}

/// The compact summary `DeskDto.routing` carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeskRoutingSummaryDto {
    /// Where the block came from.
    pub source: RoutingSource,
    /// Seats per round.
    pub round_width: usize,
    /// Choice alternatives.
    pub choice_option_limit: usize,
    /// Round cap.
    pub max_rounds: u32,
    /// Seat timeout.
    pub turn_timeout_secs: u64,
    /// Which router picks the seats.
    pub router: Router,
}

impl EffectiveRouting {
    /// The console's view of the resolved numbers.
    #[must_use]
    pub fn dto(&self, router: Router) -> EffectiveRoutingDto {
        EffectiveRoutingDto {
            round_width: self.round_width,
            choice_option_limit: self.choice_option_limit,
            max_rounds: self.max_rounds,
            turn_timeout_secs: self.turn_timeout_secs,
            router,
            minimum_confidence: self.minimum_confidence,
            high_impact_minimum_confidence: self.high_impact_minimum_confidence,
            clarification_threshold: self.clarification_threshold,
            high_impact_threshold: self.high_impact_threshold,
            referral: EffectiveReferralDto {
                enabled: self.referral.enabled,
                max_hops: self.referral.max_hops,
                reach: match self.referral.reach {
                    ReferralReach::Local => "local",
                    ReferralReach::Channels => "channels",
                    ReferralReach::Desks => "desks",
                }
                .to_string(),
                returns: self.referral.returns,
            },
        }
    }
}

/// The summary a desk carries on the list.
#[must_use]
pub fn desk_routing_summary(
    record: &CompanyRecord,
    desk_id: &str,
    router: Router,
) -> DeskRoutingSummaryDto {
    let (config, source) = effective_routing(record, desk_id);
    let effective = EffectiveRouting::resolve(&config);
    DeskRoutingSummaryDto {
        source,
        round_width: effective.round_width,
        choice_option_limit: effective.choice_option_limit,
        max_rounds: effective.max_rounds,
        turn_timeout_secs: effective.turn_timeout_secs,
        router,
    }
}

/// The full payload for one desk.
#[must_use]
pub fn desk_routing_dto(record: &CompanyRecord, desk_id: &str, router: Router) -> DeskRoutingDto {
    let (config, source) = effective_routing(record, desk_id);
    let effective = EffectiveRouting::resolve(&config);
    let agents = record.effective_agents();
    let desks = crate::runtime::delegation_tools::desk_ids(record);
    let candidates = record
        .effective_desk_members(desk_id)
        .into_iter()
        .filter(|id| record.is_roster_agent(id))
        .map(|id| {
            let agent = agents.iter().find(|agent| agent.id == id);
            RoutingCandidateDto {
                label: agent
                    .and_then(|agent| agent.name.clone())
                    .unwrap_or_else(|| id.clone()),
                role: agent.map(|agent| agent.role.clone()).unwrap_or_default(),
                shared_with: desks
                    .iter()
                    .filter(|other| other.as_str() != desk_id)
                    .filter(|other| record.effective_desk_members(other).contains(&id))
                    .cloned()
                    .collect(),
                agent_id: id,
            }
        })
        .collect();
    DeskRoutingDto {
        desk_id: desk_id.to_string(),
        source,
        declared: config,
        effective: effective.dto(router),
        candidates,
    }
}

/// Which router this host would route a desk with: Jev when a TinyHumans
/// credential resolves, else the deterministic fallback.
///
/// The default build carries no Jev transport (it lives behind `openhuman`),
/// so it always answers `Fallback` — which is also the honest answer for a
/// host that cannot run a round at all.
#[must_use]
pub fn host_router() -> Router {
    #[cfg(feature = "openhuman")]
    {
        if crate::hive::jev::jev_router(&crate::app::config::ProcessEnv, None)
            .ok()
            .flatten()
            .is_some()
        {
            return Router::Jev;
        }
    }
    Router::Fallback
}

#[cfg(test)]
#[path = "routing_tests.rs"]
mod tests;
