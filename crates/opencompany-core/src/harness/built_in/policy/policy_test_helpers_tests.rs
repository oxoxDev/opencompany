use super::*;
use crate::harness::built_in::run_origin::{DispatchSource, RunOrigin, claim};
use crate::ports::{SampleKind, UsageSample};
use oh::agent::tool_policy::{ToolCallContext, ToolPolicyRequest};

// Issue #470: the `composio_execute` fixtures are built here, from the same
// key the classifier reads, so a call in a test reaches the same catalogue
// lookup a call in production does.

pub(crate) fn policy(mode: &str, always: &[&str], auto_under: Option<f64>) -> ApprovalPolicy {
    let p = Policy {
        mode: mode.to_string(),
        always_approve: always.iter().map(|s| s.to_string()).collect(),
        auto_approve_under_usd: auto_under,
        approval_ttl_hours: None,
    };
    ApprovalPolicy::new(&p, Some(25.0))
}

pub(crate) fn request(tool: &str, args: serde_json::Value) -> ToolPolicyRequest {
    let ctx = ToolCallContext::session("s", "chat", "ceo", "call-1", 0);
    ToolPolicyRequest::new(tool, args, ctx)
}

pub(crate) fn grantable(tool: &str, args: &serde_json::Value) -> bool {
    crate::policy::consequence_of(tool, args)
        .standing
        .is_grantable()
}

pub(crate) fn queued_policy(mode: &str, always: &[&str]) -> (ApprovalPolicy, ApprovalRequestQueue) {
    let queue = ApprovalRequestQueue::default();
    (
        policy(mode, always, None).with_requests(queue.clone()),
        queue,
    )
}

/// A policy bound to `agent`, plus the grant set its queue carries.
pub(crate) fn granting_policy(
    mode: &str,
    always: &[&str],
    agent: &str,
) -> (ApprovalPolicy, crate::runtime::grants::GrantSet) {
    let queue = ApprovalRequestQueue::default();
    let grants = queue.grants();
    (
        policy(mode, always, None)
            .with_requests(queue)
            .with_agent(agent),
        grants,
    )
}

pub(crate) fn granted(
    agent: &str,
    tool: &str,
    args: serde_json::Value,
) -> crate::runtime::grants::GrantedCall {
    crate::runtime::grants::GrantedCall {
        approval_id: crate::ports::types::ApprovalId::new("appr-1"),
        agent: agent.to_string(),
        tool: tool.to_string(),
        args,
        at_millis: 1_000,
        origin_thread: None,
        origin_parent: None,
        origin_task: None,
    }
}

pub(crate) fn gated(kind: &str) -> ApprovalRequest {
    ApprovalRequest {
        tool: kind.to_string(),
        reason: "gated".to_string(),
        effect: Effect {
            kind: kind.to_string(),
            group: EffectGroup::Other,
            amount_usd: None,
            established_thread: false,
            first_time_counterparty: false,
            payload: serde_json::json!({ "kind": kind }),
            agent: Some("ceo".to_string()),
            run_id: None,
        },
    }
}

/// An agent-scoped standing deny, for the four cases below. `scope`
/// mirrors the URL a `web_fetch` call to `docs.rs` computes through
/// `standing_scope_of`, so a call with a different (or absent) `url`
/// argument does not fall under it.
pub(crate) fn agent_standing_deny(
    id: &str,
    agent: &str,
    expires_at_millis: u64,
) -> crate::runtime::grants::StandingGrant {
    crate::runtime::grants::StandingGrant {
        id: crate::runtime::grants::GrantId::new(id),
        agent: agent.to_string(),
        workflow: None,
        tool: "web_fetch".to_string(),
        verdict: Verdict::Deny,
        granted_by: crate::ports::types::Actor {
            kind: crate::ports::types::ActorKind::User,
            id: "user-1".into(),
        },
        approval_id: crate::ports::types::ApprovalId::new("appr-1"),
        at_millis: 1_000,
        expires_at_millis,
        origin_thread: None,
        origin_parent: None,
        origin_task: None,
        scope: Some("https://docs.rs".to_string()),
    }
}

/// An inference sample costing `usd`, stamped at `at_millis`.
pub(crate) fn spend_sample(agent: &str, usd: f64, at_millis: u64) -> UsageSample {
    UsageSample {
        at_millis,
        agent: agent.into(),
        provider: "managed".into(),
        input_tokens: 0,
        output_tokens: 0,
        cached_input_tokens: 0,
        cost_usd: usd,
        kind: SampleKind::Inference,
        run_id: None,
        model: None,
    }
}

/// Some instant today, comfortably after UTC midnight.
pub(crate) fn today() -> u64 {
    crate::ports::now_millis()
}

/// The last millisecond of yesterday, UTC.
pub(crate) fn yesterday() -> u64 {
    crate::metering::utc_day_start_millis(today()).saturating_sub(1)
}

/// A cap-bearing policy bound to `agent`, reading spend from `meter`, plus
/// the grant set its queue carries.
pub(crate) fn capped_policy(
    mode: &str,
    auto_under: Option<f64>,
    cap: f64,
    agent: &str,
    meter: Arc<dyn UsageMeter>,
) -> (ApprovalPolicy, crate::runtime::grants::GrantSet) {
    let p = Policy {
        mode: mode.to_string(),
        always_approve: Vec::new(),
        auto_approve_under_usd: auto_under,
        approval_ttl_hours: None,
    };
    let queue = ApprovalRequestQueue::default();
    let grants = queue.grants();
    (
        ApprovalPolicy::new(&p, Some(cap))
            .with_requests(queue)
            .with_agent(agent)
            .with_spend(meter, CompanyId::new("acme")),
        grants,
    )
}

pub(crate) fn standing(
    agent: &str,
    tool: &str,
    expires_at_millis: u64,
) -> crate::runtime::grants::StandingGrant {
    crate::runtime::grants::StandingGrant {
        id: crate::runtime::grants::GrantId::new("g1"),
        agent: agent.to_string(),
        workflow: None,
        tool: tool.to_string(),
        verdict: crate::ports::types::Verdict::Approve,
        granted_by: crate::ports::types::Actor {
            kind: crate::ports::types::ActorKind::User,
            id: "user-1".to_string(),
        },
        approval_id: crate::ports::types::ApprovalId::new("appr-1"),
        at_millis: 1_000,
        expires_at_millis,
        origin_thread: None,
        origin_parent: None,
        origin_task: None,
        scope: None,
    }
}

/// The same fixture, confined to one Composio toolkit (issue #457).
pub(crate) fn scoped_standing(
    agent: &str,
    tool: &str,
    scope: &str,
    expires_at_millis: u64,
) -> crate::runtime::grants::StandingGrant {
    crate::runtime::grants::StandingGrant {
        scope: Some(scope.to_string()),
        ..standing(agent, tool, expires_at_millis)
    }
}

/// Far enough ahead that wall-clock drift during a test run cannot reach it.
pub(crate) fn far_future() -> u64 {
    crate::ports::now_millis() + 60 * 60 * 1000
}

pub(crate) fn decision_name(d: &ToolPolicyDecision) -> &'static str {
    match d {
        ToolPolicyDecision::Allow => "allow",
        ToolPolicyDecision::RequireApproval { .. } => "park",
        ToolPolicyDecision::Deny { .. } => "deny",
    }
}

/// A policy that would otherwise wave everything through (`full`), so any
/// deny in these tests is the S2 arm and nothing else.
pub(crate) fn full_with_connected(toolkits: &[&str]) -> ApprovalPolicy {
    policy("full", &[], None)
        .with_connected_composio_toolkits(toolkits.iter().map(|t| t.to_string()).collect())
}

pub(crate) fn dispatched(agent: &str) -> crate::harness::built_in::run_origin::RunOriginClaim {
    claim(RunOrigin::Dispatched {
        agent: agent.to_string(),
        source: DispatchSource::Task,
        scope: None,
    })
}

/// [`crate::policy::consequence::standing_scope_of`] is the same reader a
/// standing grant's mint side uses. Pinned **directly** against
/// `trusted_dispatch_admits` rather than through `check()` — no tool in
/// today's declaration table reaches `Standing::ScopedGrantable` with a
/// reach that parks (`web_fetch` is the only classifier that produces the
/// variant, and it pairs the variant exclusively with `Reach::ExternalRead`,
/// which parks nowhere — see `a_grant_scoped_to_one_provider_does_not_admit_another_providers_read`
/// above for the same gap on the standing-grant path). A synthetic
/// `Consequence` is the only way to exercise the branch this codebase's own
/// tables cannot reach yet.
pub(crate) fn scoped_grantable() -> crate::policy::Consequence {
    crate::policy::Consequence {
        group: EffectGroup::Other,
        reach: crate::policy::Reach::Consequence,
        standing: Standing::ScopedGrantable,
    }
}

/// [`standing`] mints an `Approve` grant; this is its `Deny` twin, needed
/// once here to prove a standing deny still outranks a trusted origin.
pub(crate) fn standing_verdict(
    agent: &str,
    tool: &str,
    expires_at_millis: u64,
    verdict: Verdict,
) -> crate::runtime::grants::StandingGrant {
    crate::runtime::grants::StandingGrant {
        id: crate::runtime::grants::GrantId::new("deny-standing-1"),
        agent: agent.to_string(),
        workflow: None,
        tool: tool.to_string(),
        verdict,
        granted_by: crate::ports::types::Actor {
            kind: crate::ports::types::ActorKind::User,
            id: "user-1".into(),
        },
        approval_id: crate::ports::types::ApprovalId::new("appr-deny-1"),
        at_millis: 1_000,
        expires_at_millis,
        origin_thread: None,
        origin_parent: None,
        origin_task: None,
        scope: None,
    }
}

/// Runs `fut` under the chat cycle's approval claim, the way every production
/// agent turn runs, so a gated call parks instead of being refused as
/// unrecordable.
pub(crate) async fn in_cycle<F: std::future::Future>(fut: F) -> F::Output {
    CURRENT_SCOPE.scope(ApprovalScope::Cycle, fut).await
}
