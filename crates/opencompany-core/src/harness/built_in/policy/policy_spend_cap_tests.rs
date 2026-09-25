use super::*;

// Issue #470: the `composio_execute` fixtures are built here, from the same
// key the classifier reads, so a call in a test reaches the same catalogue
// lookup a call in production does.
use super::policy_test_helpers_tests::*;

// --- The per-agent daily spend cap (issue #304) ---------------------------

use crate::ports::usage::{UsageMeter, UsageSample};
use std::sync::atomic::AtomicUsize;

/// A meter over a fixed sample set that **respects `since_millis`**.
///
/// The respecting is the whole point of the double. A meter that ignored
/// `since` — which the crate's other test meters do, harmlessly, because
/// nothing they back reads a window — would make the day-boundary tests
/// pass no matter what boundary the code computed, including no boundary at
/// all. It also counts queries, so "a policy with no cap never asks the
/// meter" is an assertion rather than an assumption.
#[derive(Default)]
pub(super) struct FixedMeter {
    samples: Vec<UsageSample>,
    queries: AtomicUsize,
}

impl FixedMeter {
    pub(super) fn with(samples: Vec<UsageSample>) -> Arc<Self> {
        Arc::new(Self {
            samples,
            queries: AtomicUsize::new(0),
        })
    }

    fn query_count(&self) -> usize {
        self.queries.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl UsageMeter for FixedMeter {
    async fn record(&self, _company: &CompanyId, _sample: &UsageSample) -> crate::Result<()> {
        Ok(())
    }
    async fn query(&self, _company: &CompanyId, since: u64) -> crate::Result<Vec<UsageSample>> {
        self.queries.fetch_add(1, Ordering::Relaxed);
        Ok(self
            .samples
            .iter()
            .filter(|sample| sample.at_millis >= since)
            .cloned()
            .collect())
    }
}

/// A meter whose reads fail — the transient-uncertainty case.
struct FailingMeter;

#[async_trait]
impl UsageMeter for FailingMeter {
    async fn record(&self, _company: &CompanyId, _sample: &UsageSample) -> crate::Result<()> {
        Ok(())
    }
    async fn query(&self, _company: &CompanyId, _since: u64) -> crate::Result<Vec<UsageSample>> {
        Err(crate::error::OpenCompanyError::Store(
            "meter unavailable".into(),
        ))
    }
}

/// Production disables policy-generated approvals, which puts the daily cap
/// below the `Allow` that `check` returns first — so a manifest cap does not
/// judge a priced call on the shipped path. Every other test here leaves
/// that switch on, a configuration production never runs.
///
/// Pins the reporting, not the bypass: the gap must stay detectable.
#[test]
fn a_declared_cap_reports_itself_unenforced_once_policy_hitl_is_off() {
    let p = Policy {
        mode: "full".to_string(),
        always_approve: Vec::new(),
        auto_approve_under_usd: None,
        approval_ttl_hours: None,
    };
    let priced = serde_json::json!({ "amount_usd": 1.0 });

    let shipped = ApprovalPolicy::new(&p, Some(5.0))
        .with_policy_hitl_disabled()
        .with_agent("writer".to_string());
    assert_eq!(
        shipped.unenforced_daily_cap("pay_invoice", &priced),
        Some(5.0),
        "a priced call under a declared cap must be reportable as ungated"
    );

    let gating = ApprovalPolicy::new(&p, Some(5.0)).with_agent("writer".to_string());
    assert_eq!(
        gating.unenforced_daily_cap("pay_invoice", &priced),
        None,
        "with policy approvals on, the cap is in force and there is nothing to report"
    );

    let uncapped = ApprovalPolicy::new(&p, None)
        .with_policy_hitl_disabled()
        .with_agent("writer".to_string());
    assert_eq!(
        uncapped.unenforced_daily_cap("pay_invoice", &priced),
        None,
        "no cap declared is not an unenforced cap"
    );

    assert_eq!(
        shipped.unenforced_daily_cap("file_read", &serde_json::json!({})),
        None,
        "an unpriced call was never the cap's business"
    );
}

/// The core of #304: at cap, a **priced** call parks — and it parks through
/// the two carve-outs that would otherwise wave it straight through.
///
/// * `web_search` under `supervised` is the #238 metered-read carve-out: it
///   never parks, precisely *because* it spends money and the daily call cap
///   was the boundary. A per-agent spend cap is a second, tighter boundary,
///   and it has to outrank the carve-out or the tightest limit in the
///   manifest would be the one that does nothing.
/// * Under `full` there is no per-call gate at all. "Full autonomy" means
///   the operator is not asked about each action; it does not mean the
///   budget they wrote down is advisory.
#[tokio::test]
async fn at_cap_a_priced_call_parks_through_the_metered_read_and_full_carve_outs() {
    in_cycle(async {
        let meter = FixedMeter::with(vec![spend_sample("analyst", 5.00, today())]);

        let (supervised, _) = capped_policy(
            "supervised",
            None,
            5.0,
            "analyst",
            meter.clone() as Arc<dyn UsageMeter>,
        );
        let decision = supervised
            .check(&request(
                "web_search",
                serde_json::json!({ "query": "acme pricing" }),
            ))
            .await;
        assert!(
            matches!(decision, ToolPolicyDecision::RequireApproval { .. }),
            "a metered read must park once the agent is out of budget: {decision:?}"
        );

        let (full, _) = capped_policy(
            "full",
            None,
            5.0,
            "analyst",
            meter.clone() as Arc<dyn UsageMeter>,
        );
        assert!(
            matches!(
                full.check(&request("media_generate_image", serde_json::json!({})))
                    .await,
                ToolPolicyDecision::RequireApproval { .. }
            ),
            "full autonomy is not a budget exemption"
        );
        assert!(
            matches!(
                full.check(&request(
                    "pay_invoice",
                    serde_json::json!({ "amount_usd": 1.0 })
                ))
                .await,
                ToolPolicyDecision::RequireApproval { .. }
            ),
            "an amount-bearing call must park at cap under full autonomy too"
        );
    })
    .await;
}

/// The remaining-budget boundary, and why the arm sits **above**
/// `auto_approve_under_usd`: a declared amount that would carry the agent
/// past its cap parks even though it is under the auto-approve threshold,
/// while an amount that still fits is waved through as before.
///
/// Below the threshold instead, an agent with a $5 cap and a $5 auto-approve
/// threshold could spend $4.99 at a time forever and the cap would be
/// unreachable by construction.
#[tokio::test]
async fn a_declared_amount_that_breaches_the_remaining_budget_parks() {
    in_cycle(async {
        let meter = FixedMeter::with(vec![spend_sample("analyst", 4.20, today())]);
        let (p, _) = capped_policy(
            "supervised",
            Some(5.0),
            5.0,
            "analyst",
            meter.clone() as Arc<dyn UsageMeter>,
        );

        // $4.20 spent + $3.00 = $7.20 > $5.00 cap — parks despite being under
        // the $5 auto-approve threshold.
        assert!(
            matches!(
                p.check(&request(
                    "pay_invoice",
                    serde_json::json!({ "amount_usd": 3.0 })
                ))
                .await,
                ToolPolicyDecision::RequireApproval { .. }
            ),
            "a sub-threshold spend that breaches the day's remaining budget must park"
        );

        // $4.20 + $0.50 = $4.70 <= $5.00 — still fits, so auto-approve applies.
        assert_eq!(
            p.check(&request(
                "pay_invoice",
                serde_json::json!({ "amount_usd": 0.5 })
            ))
            .await,
            ToolPolicyDecision::Allow,
            "a spend that fits inside the remaining budget is unaffected"
        );
    })
    .await;
}

/// A spend cap caps **spend**. At cap a teammate can still read and can
/// still park a send for the ordinary supervised reason — it has not been
/// muted, it has been defunded.
///
/// The send assertion checks the *reason text*, not just the decision:
/// `send_email` parks under supervised either way, so only the wording
/// distinguishes "parked because it reaches outside" from "parked because
/// the budget arm swallowed a free call".
#[tokio::test]
async fn free_reads_and_sends_are_untouched_at_cap() {
    in_cycle(async {
        let meter = FixedMeter::with(vec![spend_sample("analyst", 9.99, today())]);
        let (p, _) = capped_policy(
            "supervised",
            None,
            5.0,
            "analyst",
            meter.clone() as Arc<dyn UsageMeter>,
        );

        assert_eq!(
            p.check(&request("read_file", serde_json::json!({}))).await,
            ToolPolicyDecision::Allow,
            "a free read costs nothing and must survive the cap"
        );

        let decision = p
            .check(&request(
                "send_email",
                serde_json::json!({ "to": "a@b.test" }),
            ))
            .await;
        match decision {
            ToolPolicyDecision::RequireApproval { reason, .. } => assert!(
                reason.contains("supervised"),
                "a free send parks for the ordinary tier reason, not the budget: {reason}"
            ),
            other => panic!("send_email must still park under supervised: {other:?}"),
        }
    })
    .await;
}

/// The ordering pin, mirroring `a_grant_beats_always_approve`: the operator's
/// approval of a budget-parked call actually **runs** it, once.
///
/// If the budget arm sat above grant consumption, approving an at-cap call
/// would re-park it forever — the park exists to ask the operator a
/// question, and the grant is their answer. Ranking the question above the
/// answer makes approval mean nothing.
#[tokio::test]
async fn a_grant_releases_a_budget_parked_call_once_then_it_re_parks() {
    in_cycle(async {
        let meter = FixedMeter::with(vec![spend_sample("analyst", 5.00, today())]);
        let (p, grants) = capped_policy(
            "supervised",
            None,
            5.0,
            "analyst",
            meter.clone() as Arc<dyn UsageMeter>,
        );
        let args = serde_json::json!({ "query": "acme pricing" });

        // Out of budget: parks.
        assert!(matches!(
            p.check(&request("web_search", args.clone())).await,
            ToolPolicyDecision::RequireApproval { .. }
        ));

        // The operator approves that exact call.
        grants.grant(granted("analyst", "web_search", args.clone()));
        assert_eq!(
            p.check(&request("web_search", args.clone())).await,
            ToolPolicyDecision::Allow,
            "an approved at-cap call must run; otherwise approval authorises nothing"
        );

        // Single-use: the budget is still exhausted, so the next one re-parks.
        assert!(
            matches!(
                p.check(&request("web_search", args)).await,
                ToolPolicyDecision::RequireApproval { .. }
            ),
            "one approval buys one over-budget call, not a raised cap"
        );
    })
    .await;
}

/// `readonly` outranks the budget arm, as it outranks the grant: the brake
/// denies outright rather than offering the operator something to approve.
#[tokio::test]
async fn the_readonly_brake_still_denies_before_the_budget_arm() {
    let meter = FixedMeter::with(vec![spend_sample("analyst", 9.99, today())]);
    let (p, _) = capped_policy(
        "readonly",
        None,
        5.0,
        "analyst",
        meter.clone() as Arc<dyn UsageMeter>,
    );
    assert!(
        matches!(
            p.check(&request(
                "pay_invoice",
                serde_json::json!({ "amount_usd": 1.0 })
            ))
            .await,
            ToolPolicyDecision::Deny { .. }
        ),
        "a read-only desk denies a spend; it does not offer it for approval"
    );
}

/// "Daily" is the UTC calendar day: yesterday's $9 does not hold today's
/// budget hostage. This is what the `since`-respecting double buys.
#[tokio::test]
async fn yesterdays_spend_does_not_count_against_todays_cap() {
    let meter = FixedMeter::with(vec![spend_sample("analyst", 9.00, yesterday())]);
    let (p, _) = capped_policy(
        "supervised",
        None,
        5.0,
        "analyst",
        meter.clone() as Arc<dyn UsageMeter>,
    );
    assert_eq!(
        p.check(&request(
            "web_search",
            serde_json::json!({ "query": "acme" })
        ))
        .await,
        ToolPolicyDecision::Allow,
        "the cap resets at 00:00Z; yesterday's spend is spent"
    );
}

/// An uncapped agent never pays for a meter round-trip — the arm
/// short-circuits on the cap before anything else.
#[tokio::test]
async fn an_uncapped_agent_never_queries_the_meter() {
    let meter = FixedMeter::with(vec![spend_sample("analyst", 99.0, today())]);
    let p = Policy {
        mode: "full".to_string(),
        always_approve: Vec::new(),
        auto_approve_under_usd: None,
        approval_ttl_hours: None,
    };
    let uncapped = ApprovalPolicy::new(&p, None)
        .with_requests(ApprovalRequestQueue::default())
        .with_agent("analyst")
        .with_spend(meter.clone() as Arc<dyn UsageMeter>, CompanyId::new("acme"));

    // `web_search`, not `pay_invoice`. Both are priced calls — `web_search`
    // is declared `EffectGroup::Spend`, which is what `is_priced_call`
    // reads — so this still exercises the cap arm. `pay_invoice` would now
    // also be stopped by the per-call judgement arm (issue #338), and a
    // test about the *meter* should not be able to fail for a reason that
    // has nothing to do with the meter.
    assert_eq!(
        uncapped
            .check(&request("web_search", serde_json::json!({})))
            .await,
        ToolPolicyDecision::Allow
    );
    assert_eq!(
        meter.query_count(),
        0,
        "no cap means no question to ask the meter"
    );
}

/// No meter wired — every non-harness construction site, and a host without
/// one — leaves the cap inert rather than parking every priced call forever.
/// A permanent deployment fact must not brick spend tools with a park no
/// approval can clear.
#[tokio::test]
async fn a_cap_with_no_meter_is_inert() {
    let p = Policy {
        mode: "full".to_string(),
        always_approve: Vec::new(),
        auto_approve_under_usd: None,
        approval_ttl_hours: None,
    };
    let no_meter = ApprovalPolicy::new(&p, Some(5.0))
        .with_requests(ApprovalRequestQueue::default())
        .with_agent("analyst");

    // `web_search` for the same reason as `an_uncapped_agent_never_queries_
    // the_meter`: a priced call the per-call judgement arm is silent on, so
    // this keeps testing the cap and only the cap.
    assert_eq!(
        no_meter
            .check(&request("web_search", serde_json::json!({})))
            .await,
        ToolPolicyDecision::Allow,
        "an unenforceable cap must not park what it can never release"
    );
}

/// A meter read that **errors** is transient uncertainty about money, and
/// reads as *ask*, not *allow*: the priced call parks, naming the
/// uncertainty. A free call is untouched — the arm never saw it.
///
/// The deliberate opposite of the dispatch gate's fail-open. There the
/// alternative is bricking the company's cognition with no recourse; here it
/// is one call waiting on a human who can wave it through.
#[tokio::test]
async fn a_failing_meter_parks_priced_calls_and_leaves_free_ones_alone() {
    in_cycle(async {
        let (p, _) = capped_policy(
            "full",
            None,
            5.0,
            "analyst",
            Arc::new(FailingMeter) as Arc<dyn UsageMeter>,
        );

        let decision = p
            .check(&request(
                "pay_invoice",
                serde_json::json!({ "amount_usd": 1.0 }),
            ))
            .await;
        match decision {
            ToolPolicyDecision::RequireApproval { reason, .. } => assert!(
                reason.contains("could not be verified"),
                "the park must say the budget is unknown, not that it is exceeded: {reason}"
            ),
            other => panic!("an unreadable budget must park a spend: {other:?}"),
        }

        assert_eq!(
            p.check(&request("read_file", serde_json::json!({}))).await,
            ToolPolicyDecision::Allow,
            "a free call never reaches the budget arm, so a meter outage cannot gate it"
        );
    })
    .await;
}
