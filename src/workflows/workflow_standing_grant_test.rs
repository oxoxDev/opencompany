//! Issue #1098 — a scheduled workflow stops re-asking the same question on
//! every run, once its operator has granted it a standing permission.
//!
//! # The defect, and why nothing already in the suite caught it
//!
//! Every part worked alone. `web_fetch` has been [`ScopedGrantable`] since
//! #673, the console has offered a duration picker since #374, and the gate
//! pass has parked `tool_call` nodes since #460. But a standing permission could
//! only ever name a *teammate*, and a scheduled graph has none — so the one
//! caller whose calls an operator had **pre-declared in a graph** was the one
//! caller that could not hold a permission, while an agent choosing its
//! arguments at run time could.
//!
//! Net for a three-fetch daily job: three identical cards, every morning,
//! forever. The harm is not the clicks. An operator trained to approve twenty
//! meaningless cards approves the meaningful one the same way.
//!
//! Two things had to change together, and a test below pins each:
//!
//! * the **subject** — a permission may now name a workflow
//!   ([`GrantSubject::Workflow`]), not only a teammate;
//! * the **classifier** — a gate's `kind` is the `workflow.approve` wrapper, so
//!   asking about it returned the undeclared fallback and the `web_fetch` on the
//!   card was never seen. Fixing only the subject would have left every gate
//!   refused for that second, unrelated reason.
//!
//! # What this drives
//!
//! Nothing is stubbed on the path under test, on the same terms as
//! [`super::parallel_gate_fanout_test`]: a real graph, the real engine through
//! [`HarnessWorkflowRunner`](super::runner::HarnessWorkflowRunner), the real
//! `ApprovalPolicy` gate, a real on-disk journal, and a real [`CompanyRuntime`]
//! resolving through `resolve_approval_spawned` — the same call the console's
//! Approvals route makes when the operator picks a duration.
//!
//! **Two runs, not one.** Every claim here is about the *second* run, and a
//! single-run test cannot see any of it. The first run establishes the cards; the
//! second is the assertion.
//!
//! # The hosts never resolve, deliberately
//!
//! The three nodes fetch `*.invalid` — reserved by RFC 2606 and guaranteed never
//! to resolve. That is itself an assertion: these tests are about whether a node
//! was **stopped**, and a suite that reached the real internet to prove it would
//! be measuring something else. A permission that works lets the node run, the
//! fetch then fails on DNS, and the run settles without a card — which is exactly
//! the distinction being drawn, because a *gated* node leaves a card behind.
//!
//! [`ScopedGrantable`]: crate::policy::Standing::ScopedGrantable
//! [`GrantSubject::Workflow`]: crate::runtime::grants::GrantSubject::Workflow
//! [`CompanyRuntime`]: crate::company::runtime::CompanyRuntime

use std::sync::Arc;

use serde_json::json;

use crate::company::{CompanyManifest, parse_workflow};
use crate::ports::WorkflowRunContext;
use crate::ports::types::{Actor, ActorKind, ApprovalId, CompanyRecord, Verdict};
use crate::runtime::RuntimeBuilder;
use crate::runtime::grants::GrantScope;
use crate::runtime::workflow_resume::{PAYLOAD_NODE_ID, WORKFLOW_APPROVE_KIND};

/// The reported shape, minimised: a daily digest fanning out to three fetches
/// over three distinct hosts, converging on one output node.
///
/// Three *different* hosts rather than three paths on one, because the host is
/// what a permission is scoped to. One host would mint one permission covering
/// all three nodes and the run-2 assertion would pass without the subject change
/// having done anything.
const DIGEST_TOML: &str = r#"
id = "sports_digest"
name = "Sports digest"
[[node]]
id = "start"
kind = "trigger"
name = "Start"
[[node]]
id = "fetch_bbc"
kind = "tool_call"
name = "Fetch BBC"
on_error = "continue"
[node.config]
slug = "web_fetch"
[node.config.args]
url = "https://bbc.invalid/sport"
[[node]]
id = "fetch_espn"
kind = "tool_call"
name = "Fetch ESPN"
on_error = "continue"
[node.config]
slug = "web_fetch"
[node.config.args]
url = "https://espn.invalid/nfl"
[[node]]
id = "fetch_guardian"
kind = "tool_call"
name = "Fetch Guardian"
on_error = "continue"
[node.config]
slug = "web_fetch"
[node.config.args]
url = "https://guardian.invalid/football"
[[node]]
id = "rank"
kind = "output"
name = "Rank"
[[edge]]
from = "start"
to = "fetch_bbc"
[[edge]]
from = "start"
to = "fetch_espn"
[[edge]]
from = "start"
to = "fetch_guardian"
[[edge]]
from = "fetch_bbc"
to = "rank"
[[edge]]
from = "fetch_espn"
to = "rank"
[[edge]]
from = "fetch_guardian"
to = "rank"
"#;

/// The same graph with one node repointed at a fourth host, for the
/// scope-invalidation test.
const DIGEST_REPOINTED_TOML: &str = r#"
id = "sports_digest"
name = "Sports digest"
[[node]]
id = "start"
kind = "trigger"
name = "Start"
[[node]]
id = "fetch_bbc"
kind = "tool_call"
name = "Fetch BBC"
on_error = "continue"
[node.config]
slug = "web_fetch"
[node.config.args]
url = "https://bbc.invalid/sport"
[[node]]
id = "fetch_espn"
kind = "tool_call"
name = "Fetch ESPN"
on_error = "continue"
[node.config]
slug = "web_fetch"
[node.config.args]
url = "https://espn.invalid/nfl"
[[node]]
id = "fetch_guardian"
kind = "tool_call"
name = "Fetch Guardian"
on_error = "continue"
[node.config]
slug = "web_fetch"
[node.config.args]
url = "https://sky.invalid/football"
[[node]]
id = "rank"
kind = "output"
name = "Rank"
[[edge]]
from = "start"
to = "fetch_bbc"
[[edge]]
from = "start"
to = "fetch_espn"
[[edge]]
from = "start"
to = "fetch_guardian"
[[edge]]
from = "fetch_bbc"
to = "rank"
[[edge]]
from = "fetch_espn"
to = "rank"
[[edge]]
from = "fetch_guardian"
to = "rank"
"#;

/// The same graph plus a fourth node on an **already-approved** host, for the
/// Q1 decision (the subject is the workflow, not the node).
const DIGEST_EXTRA_NODE_TOML: &str = r#"
id = "sports_digest"
name = "Sports digest"
[[node]]
id = "start"
kind = "trigger"
name = "Start"
[[node]]
id = "fetch_bbc"
kind = "tool_call"
name = "Fetch BBC"
on_error = "continue"
[node.config]
slug = "web_fetch"
[node.config.args]
url = "https://bbc.invalid/sport"
[[node]]
id = "fetch_bbc_weather"
kind = "tool_call"
name = "Fetch BBC weather"
on_error = "continue"
[node.config]
slug = "web_fetch"
[node.config.args]
url = "https://bbc.invalid/weather"
[[node]]
id = "fetch_espn"
kind = "tool_call"
name = "Fetch ESPN"
on_error = "continue"
[node.config]
slug = "web_fetch"
[node.config.args]
url = "https://espn.invalid/nfl"
[[node]]
id = "fetch_guardian"
kind = "tool_call"
name = "Fetch Guardian"
on_error = "continue"
[node.config]
slug = "web_fetch"
[node.config.args]
url = "https://guardian.invalid/football"
[[node]]
id = "rank"
kind = "output"
name = "Rank"
[[edge]]
from = "start"
to = "fetch_bbc"
[[edge]]
from = "start"
to = "fetch_bbc_weather"
[[edge]]
from = "start"
to = "fetch_espn"
[[edge]]
from = "start"
to = "fetch_guardian"
[[edge]]
from = "fetch_bbc"
to = "rank"
[[edge]]
from = "fetch_bbc_weather"
to = "rank"
[[edge]]
from = "fetch_espn"
to = "rank"
[[edge]]
from = "fetch_guardian"
to = "rank"
"#;

/// A graph whose one gated node calls a tool that is `PerCall` forever.
const SHELL_TOML: &str = r#"
id = "sports_digest"
name = "Sports digest"
[[node]]
id = "start"
kind = "trigger"
name = "Start"
[[node]]
id = "run_it"
kind = "tool_call"
name = "Run it"
[node.config]
slug = "shell"
[node.config.args]
command = "echo hi"
[[node]]
id = "rank"
kind = "output"
name = "Rank"
[[edge]]
from = "start"
to = "run_it"
[[edge]]
from = "run_it"
to = "rank"
"#;

const FETCHES: [&str; 3] = ["fetch_bbc", "fetch_espn", "fetch_guardian"];

/// A week, the ceiling a console duration picker offers.
const A_WEEK_MILLIS: u64 = 7 * 24 * 60 * 60 * 1000;

/// A company that grants the `web` namespace and gates it, under `full`
/// autonomy.
///
/// `full` with `always_approve`, on [`super::parallel_gate_fanout_test`]'s
/// reasoning: it is the stronger claim (the call is stopped whatever the tier)
/// and it keeps exec-security out of the way, so the only thing that can stop
/// the call is the gate. It also makes the run-2 assertion sharper — the
/// standing arm sits **above** `always_approve` in `ApprovalPolicy::check`, so a
/// silent second run is the permission winning against an explicit fence, not a
/// tier quietly letting the call through.
fn manifest() -> CompanyManifest {
    toml::from_str(
        r#"
[company]
name = "Acme"

[policy]
mode = "full"
always_approve = ["web_fetch", "shell"]

[tools]
allow = ["web", "shell"]

[[agent]]
id = "ceo"
role = "Chief Executive"
tier = "orchestrator"
"#,
    )
    .expect("manifest parses")
}

fn record() -> CompanyRecord {
    CompanyRecord {
        manifest: manifest(),
        ..super::gated_tool_turn_test::record()
    }
}

fn operator() -> Actor {
    Actor {
        kind: ActorKind::Operator,
        id: "owner".into(),
    }
}

/// A home whose `workflows/` directory holds `graph`, so a continuation's loader
/// finds it exactly as the console run route would.
fn seed_home(graph: &str) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("opencompany-standing-")
        .tempdir()
        .expect("tempdir");
    let workflows = dir.path().join("workflows");
    std::fs::create_dir_all(&workflows).expect("workflows dir");
    std::fs::write(workflows.join("sports_digest.toml"), graph).expect("seed graph");
    dir
}

/// Rewrites the seeded graph, standing in for an operator editing it between
/// runs.
fn reseed(home: &std::path::Path, graph: &str) {
    std::fs::write(home.join("workflows").join("sports_digest.toml"), graph).expect("reseed graph");
}

/// A runtime wired to the **real** workflow runner, parking into its own gate,
/// journal and continuation queues.
///
/// The parking bundle is rebuilt over the runtime's own handles for the reason
/// [`super::parallel_gate_fanout_test`] gives: a card has to land in the queue
/// the runtime resolves from, or every assertion here is vacuous.
async fn runtime(home: &std::path::Path) -> Arc<crate::company::runtime::CompanyRuntime> {
    let mut rt = RuntimeBuilder::new(home.to_path_buf(), manifest())
        .with_seed_dir(home.to_path_buf())
        .build()
        .await
        .expect("runtime builds");

    // A base URL nothing calls: this graph has no agent node, so no model is
    // reached. A dead address is the assertion.
    let (mut deps, _unused) =
        super::gated_tool_turn_test::deps("http://127.0.0.1:1/unused".to_string(), home);
    // Issue #243/#1098: the runtime mints permissions into `rt.grants` and the
    // gate pass reads them back off the deps queue, so the two must be one set —
    // exactly what `RuntimeBuilder` wires in production. The `gated_tool_turn_test`
    // fixture defaults to its own, which would make every assertion here vacuous.
    deps.approval_requests =
        crate::harness::policy::ApprovalRequestQueue::with_grants(rt.grants.clone());
    let delivery = deps.delivery.as_mut().expect("the fixture wires delivery");
    delivery.parking = Some(super::delivery::DeliveryParking {
        approvals: rt.approvals.clone(),
        journal: rt.journal().clone(),
        continuations: rt.continuations.clone(),
        gates: rt.workflow_gates().clone(),
        blocked_nodes: rt.blocked_nodes().clone(),
    });

    let pool = Arc::new(crate::harness::HarnessPool::new());
    pool.ensure(&record(), &deps).await.expect("roster builds");
    rt.set_workflow_runner(Arc::new(super::runner::HarnessWorkflowRunner::new(
        pool,
        deps,
        record(),
    )));
    Arc::new(rt)
}

/// Fires the graph through the runtime's own runner — the path both the console
/// run route and the scheduler take — and answers which nodes it paused on.
async fn fire(rt: &Arc<crate::company::runtime::CompanyRuntime>, graph: &str) -> Vec<String> {
    let file = parse_workflow(graph).expect("graph parses");
    let ctx = WorkflowRunContext::new(false);
    let runner = rt.workflow_runner().cloned().expect("a runner is wired");
    // A run whose granted nodes executed and then failed DNS is a *failed run*
    // carrying a partial — and its `pending_approvals` is exactly the claim
    // under test. Unwrapping would throw that away and read the DNS failure as
    // a test failure, when it is the designed outcome of a permission working.
    match runner
        .run(rt.id(), &file, json!({ "topic": "sports" }), &ctx)
        .await
    {
        Ok(run) => sorted(run.pending_approvals),
        Err(crate::error::OpenCompanyError::WorkflowRunFailed { partial, .. }) => {
            sorted(partial.pending_approvals)
        }
        Err(other) => panic!("the run failed for a reason this test does not model: {other}"),
    }
}

fn sorted(mut items: Vec<String>) -> Vec<String> {
    items.sort();
    items
}

/// The gate cards currently on the Approvals page, as `(id, node)`.
fn gate_cards(rt: &Arc<crate::company::runtime::CompanyRuntime>) -> Vec<(ApprovalId, String)> {
    rt.journal()
        .pending()
        .into_iter()
        .filter(|entry| entry.effect.kind == WORKFLOW_APPROVE_KIND)
        .map(|entry| {
            let node = entry.effect.payload[PAYLOAD_NODE_ID]
                .as_str()
                .expect("a gate card names its node")
                .to_string();
            (entry.id, node)
        })
        .collect()
}

/// Approves every open gate card with a standing permission lasting
/// `expires_in_millis`, the way the console does when an operator picks a
/// duration.
async fn grant_every_open_card(
    rt: &Arc<crate::company::runtime::CompanyRuntime>,
    expires_in_millis: u64,
) {
    for (id, _node) in gate_cards(rt) {
        rt.resolve_approval_spawned(
            &id,
            Verdict::Approve,
            operator(),
            GrantScope::Tool {
                expires_at_millis: crate::ports::now_millis() + expires_in_millis,
            },
        )
        .await
        .expect("a gate card accepts a standing permission");
    }
    settle().await;
}

/// The continuation is spawned rather than awaited (#380's drop safety), so a
/// test that asserted immediately would race it. Bounded, so a genuine failure
/// fails rather than hangs.
async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
}

/// **The headline.** Run one parks three cards; the operator grants each a
/// week; run two parks **nothing**.
///
/// Asserted as equalities rather than bounds, because the defect is that the
/// second number was three.
#[tokio::test]
async fn a_granted_workflow_does_not_ask_again_on_the_next_run() {
    let home = seed_home(DIGEST_TOML);
    let rt = runtime(home.path()).await;

    let first = fire(&rt, DIGEST_TOML).await;
    assert_eq!(
        first,
        FETCHES.map(str::to_string).to_vec(),
        "the cold run must pause on all three fetches"
    );
    assert_eq!(gate_cards(&rt).len(), 3, "one card per gated fetch");

    grant_every_open_card(&rt, A_WEEK_MILLIS).await;

    let second = fire(&rt, DIGEST_TOML).await;
    assert!(
        second.is_empty(),
        "the next run must not ask again — it paused on {second:?}"
    );
    assert_eq!(
        gate_cards(&rt).len(),
        0,
        "and it must leave no new card behind"
    );
}

/// Scope equality **is** the invalidation. Repointing one node at a fourth host
/// makes that node ask again, and leaves its siblings quiet — no separate
/// change-detection machinery, and no all-or-nothing revocation.
#[tokio::test]
async fn a_repointed_host_asks_again_while_its_siblings_stay_quiet() {
    let home = seed_home(DIGEST_TOML);
    let rt = runtime(home.path()).await;

    assert_eq!(fire(&rt, DIGEST_TOML).await.len(), 3);
    grant_every_open_card(&rt, A_WEEK_MILLIS).await;

    reseed(home.path(), DIGEST_REPOINTED_TOML);
    let second = fire(&rt, DIGEST_REPOINTED_TOML).await;

    assert_eq!(
        second,
        vec!["fetch_guardian".to_string()],
        "only the repointed node asks again"
    );
}

/// The Q1 decision, made executable: the subject is the **workflow**, so a node
/// added later on an already-approved host does not re-ask.
///
/// This is the deliberate consequence of not keying on the node, and the reason
/// it is deliberate is the Composio toolkit scope: the operator consented to a
/// host, and re-parking a second call to that host is the workflow-shaped
/// version of the slug-exactness #457 rejected. Asserted rather than left
/// implicit, so that a future change to node-level keying has to come here and
/// argue with it.
#[tokio::test]
async fn a_node_added_on_an_already_approved_host_does_not_ask() {
    let home = seed_home(DIGEST_TOML);
    let rt = runtime(home.path()).await;

    assert_eq!(fire(&rt, DIGEST_TOML).await.len(), 3);
    grant_every_open_card(&rt, A_WEEK_MILLIS).await;

    reseed(home.path(), DIGEST_EXTRA_NODE_TOML);
    let second = fire(&rt, DIGEST_EXTRA_NODE_TOML).await;

    assert!(
        second.is_empty(),
        "a fourth node on an approved host rides the same permission — got {second:?}"
    );
}

/// A permission is bounded, and the bound is enforced. Granted for a
/// millisecond, it is gone by the next run.
#[tokio::test]
async fn an_expired_permission_asks_again() {
    let home = seed_home(DIGEST_TOML);
    let rt = runtime(home.path()).await;

    assert_eq!(fire(&rt, DIGEST_TOML).await.len(), 3);
    // `settle()` inside the grant helper is two orders of magnitude longer than
    // this, so the permission is certainly past its deadline by the second run.
    grant_every_open_card(&rt, 1).await;

    let second = fire(&rt, DIGEST_TOML).await;
    assert_eq!(
        second,
        FETCHES.map(str::to_string).to_vec(),
        "an expired permission admits nothing"
    );
}

/// `shell` is `Standing::PerCall` and stays that way. A workflow subject does
/// not widen *what* may be granted — only *who* may hold a grant.
#[tokio::test]
async fn a_per_call_tool_is_refused_a_workflow_permission() {
    let home = seed_home(SHELL_TOML);
    let rt = runtime(home.path()).await;

    assert_eq!(fire(&rt, SHELL_TOML).await, vec!["run_it".to_string()]);
    let (id, _node) = gate_cards(&rt).into_iter().next().expect("one card");

    let refused = rt
        .resolve_approval_spawned(
            &id,
            Verdict::Approve,
            operator(),
            GrantScope::Tool {
                expires_at_millis: crate::ports::now_millis() + A_WEEK_MILLIS,
            },
        )
        .await;

    assert!(
        refused.is_err(),
        "a per-call tool must refuse a standing permission even on a workflow gate"
    );

    // An `Err` return says only what the resolver *answered*. `resolve_approval`
    // checks grantability before it touches anything precisely so "a bad request
    // changes nothing at all" — the approval stays parked, no verdict is
    // journaled, and the operator can approve it once instead. That invariant was
    // asserted only by a comment; these three pin it, so a future reordering that
    // minted first and refused second fails here rather than silently opening
    // `shell` for a week.
    assert_eq!(
        rt.grants.standing_count(),
        0,
        "a refused request must mint no standing permission"
    );
    assert_eq!(rt.grants.live_count(), 0, "and no single-use grant either");
    assert_eq!(
        gate_cards(&rt).len(),
        1,
        "the card must still be there to decide again"
    );
}

/// The invariant `crate::workflows::gate` exists to protect: **no single-use
/// grant is ever minted on this path**, whatever the operator picks.
///
/// A `GrantedCall` is redeemed by re-dispatching the agent that asked, and on a
/// workflow path nobody asked — one minted here would expire on its TTL and tell
/// the operator "the agent did not act" about a call no agent made. Issue #1098
/// opened the standing arm and this asserts it left the other one shut.
#[tokio::test]
async fn no_single_use_grant_is_minted_for_a_workflow_gate() {
    let home = seed_home(DIGEST_TOML);
    let rt = runtime(home.path()).await;

    assert_eq!(fire(&rt, DIGEST_TOML).await.len(), 3);
    grant_every_open_card(&rt, A_WEEK_MILLIS).await;

    assert_eq!(
        rt.grants.live_count(),
        0,
        "approving a gate must mint no single-use grant"
    );
    assert_eq!(
        rt.grants.standing_count(),
        3,
        "it mints one standing permission per card instead"
    );
}

/// A permission is scoped to the workflow that holds it. Another workflow of the
/// company, fetching the identical host, is a different subject and asks for
/// itself.
#[tokio::test]
async fn another_workflow_on_the_same_host_still_asks() {
    let home = seed_home(DIGEST_TOML);
    let rt = runtime(home.path()).await;

    assert_eq!(fire(&rt, DIGEST_TOML).await.len(), 3);
    grant_every_open_card(&rt, A_WEEK_MILLIS).await;

    // The same three fetches, under a different workflow id.
    let other = DIGEST_TOML.replacen("id = \"sports_digest\"", "id = \"other_digest\"", 1);
    std::fs::write(
        home.path().join("workflows").join("other_digest.toml"),
        &other,
    )
    .expect("seed the second graph");

    let second = fire(&rt, &other).await;
    assert_eq!(
        second,
        FETCHES.map(str::to_string).to_vec(),
        "a permission held by one workflow must not open another's gates"
    );
}

/// Sanity on the fixture itself: without a permission the second run asks again,
/// so every silence asserted above is the permission's doing and not something
/// about firing the same graph twice.
#[tokio::test]
async fn without_a_permission_the_second_run_asks_again() {
    let home = seed_home(DIGEST_TOML);
    let rt = runtime(home.path()).await;

    assert_eq!(fire(&rt, DIGEST_TOML).await.len(), 3);
    for (id, _node) in gate_cards(&rt) {
        rt.resolve_approval(&id, Verdict::Approve, operator())
            .await
            .expect("a plain approve is recorded");
    }
    settle().await;

    let second = fire(&rt, DIGEST_TOML).await;
    assert_eq!(
        second,
        FETCHES.map(str::to_string).to_vec(),
        "approving once buys one run — this is the behaviour #1098 changes"
    );
}
