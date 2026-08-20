//! Issue #899 (Stage 1) — approving a call gated **inside an agent node's own
//! tool loop** auto-continues the blocked run.
//!
//! # The hole this proves closed, and why the suite was blind to it
//!
//! A workflow agent node runs openhuman's tool loop. A policy-gated call mid-turn
//! is refused inside that loop, `park_gated_calls` opens a decidable card, and
//! the node returns `Err` so `WorkflowRun` reclassifies it Blocked (#881). Every
//! part worked — and approving the card still did **nothing to the run**. The
//! parked effect is tool-call-shaped (`agent: Some`), so approving it minted a
//! grant and the resolution ran a brain cycle; nothing re-dispatched the
//! settled run. The operator's only recourse was to re-run the whole workflow by
//! hand. `blocked_node_test` proves the block; `gated_tool_turn_test` proves the
//! card; neither drives the **resolve → continue** seam, because before this
//! there was nothing on the other side of it.
//!
//! # What this drives
//!
//! The real path, end to end: a real graph, the real engine through
//! [`HarnessWorkflowRunner`](super::runner::HarnessWorkflowRunner), the real
//! `ApprovalPolicy` gate, a real on-disk journal, and a real [`CompanyRuntime`]
//! resolving through `resolve_approval` — the same call the console's Approvals
//! route makes. The one thing scripted is the model's choices, over a loopback
//! OpenAI-compatible endpoint, exactly as its two ancestors script it. The grant
//! set is **shared** between the runtime that mints and the workflow policy that
//! redeems (the production wiring), so a continuation whose agent re-issues the
//! identical call passes rather than re-parking — the Stage-1 loop-safety.
//!
//! # Reading the count
//!
//! One number carries the issue, asserted as an **equality**: `runs_started`.
//! The cold run is 1. On `main` an approval adds nothing (the resolution is a
//! brain cycle, not a re-dispatch), so it stays 1 — this is the red. With the
//! fix an approval adds exactly one continuation, so it is 2; a refusal still
//! adds none; and a node blocked on two calls adds one, only after the second
//! decision. Two blocked runs never continue each other.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::company::{CompanyManifest, parse_workflow};
use crate::harness::HarnessPool;
use crate::harness::policy::ApprovalRequestQueue;
use crate::ports::types::{Actor, ActorKind, ApprovalId, CompanyId, CompanyRecord, Verdict};
use crate::ports::{WorkflowRun, WorkflowRunContext, WorkflowRunner};
use crate::runtime::RuntimeBuilder;
use crate::runtime::workflow_resume::workflow_node_turn_key;

use super::gated_tool_turn_test::{Turn, deps, spawn_script_recording};

/// `start -> work(agent, gates a shell call) -> done`. The minimal shape of the
/// reported case: one agent node whose turn is stopped by a policy gate.
const SOLO_TOML: &str = r#"
id = "solo"
name = "Solo"
[[node]]
id = "start"
kind = "trigger"
name = "Start"
[[node]]
id = "work"
kind = "agent"
name = "Work"
summary = "Do the gated thing."
agent = "ceo"
[[node]]
id = "done"
kind = "output"
name = "Done"
[[edge]]
from = "start"
to = "work"
[[edge]]
from = "work"
to = "done"
"#;

/// The agent node id every card in these tests is keyed under.
const NODE: &str = "work";

/// A company that gates `shell` under **full** autonomy — the strongest tier, so
/// the stop is the policy's explicit gate rather than a supervised classifier.
fn manifest() -> CompanyManifest {
    toml::from_str(
        r#"
[company]
name = "Acme"

[policy]
mode = "full"
always_approve = ["shell"]

[tools]
allow = ["shell"]

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

/// What the host actually asked the engine to run.
#[derive(Clone, Debug)]
struct StartedRun {
    #[allow(dead_code)]
    input: Value,
}

/// The real runner, wrapped so the test can count dispatches — the reported
/// symptom (approving started zero continuations) is a count the run history
/// cannot give back.
struct RecordingRunner {
    inner: super::runner::HarnessWorkflowRunner,
    started: Mutex<Vec<StartedRun>>,
}

impl RecordingRunner {
    fn started(&self) -> usize {
        self.started.lock().expect("recording runner").len()
    }
}

#[async_trait]
impl WorkflowRunner for RecordingRunner {
    async fn run(
        &self,
        company: &CompanyId,
        workflow: &crate::company::WorkflowFile,
        input: Value,
        ctx: &WorkflowRunContext,
    ) -> crate::Result<WorkflowRun> {
        self.started
            .lock()
            .expect("recording runner")
            .push(StartedRun {
                input: input.clone(),
            });
        self.inner.run(company, workflow, input, ctx).await
    }
}

/// A home whose `workflows/` directory holds the graph, so a continuation's
/// loader finds it by id exactly as the console run route would.
fn seed_home() -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("opencompany-blocked-cont-")
        .tempdir()
        .expect("tempdir");
    let workflows = dir.path().join("workflows");
    std::fs::create_dir_all(&workflows).expect("workflows dir");
    std::fs::write(workflows.join("solo.toml"), SOLO_TOML).expect("seed graph");
    dir
}

/// A runtime wired to the **real** workflow runner, parking into the runtime's
/// own gate/journal/continuation/blocked-node queues, with the grant set
/// **shared** so a continuation redeems what the resolve minted.
///
/// The model is served by `turns` on loopback. Everything the resolve path
/// touches is the runtime's own handle, or every assertion here would be vacuous.
async fn runtime(
    home: &std::path::Path,
    turns: Vec<Turn>,
) -> (
    Arc<crate::company::runtime::CompanyRuntime>,
    Arc<RecordingRunner>,
) {
    let mut rt = RuntimeBuilder::new(home.to_path_buf(), manifest())
        .with_seed_dir(home.to_path_buf())
        .build()
        .await
        .expect("runtime builds");

    let (base_url, _script) = spawn_script_recording(turns).await;
    let (mut deps, _unused) = deps(base_url, home);
    // Production wiring: the workflow policy redeems from the SAME grant set the
    // runtime mints into, so an approved continuation's identical call passes.
    deps.approval_requests = ApprovalRequestQueue::with_grants(rt.grants.clone());
    let delivery = deps.delivery.as_mut().expect("the fixture wires delivery");
    delivery.parking = Some(super::delivery::DeliveryParking {
        approvals: rt.approvals.clone(),
        journal: rt.journal().clone(),
        continuations: rt.continuations.clone(),
        gates: rt.workflow_gates().clone(),
        blocked_nodes: rt.blocked_nodes().clone(),
    });

    let pool = Arc::new(HarnessPool::new());
    pool.ensure(&record(), &deps).await.expect("roster builds");
    // Single-harness fixture: the default lane over the pool is the turn
    // (mirrors `run_workflow`'s single-pool entrypoint).
    let turn = Arc::new(crate::harness::built_in::run_turn::HarnessRunTurn::new(
        pool,
        Arc::new(deps.clone()),
    ));
    let runner = Arc::new(RecordingRunner {
        inner: super::runner::HarnessWorkflowRunner::new(turn, deps, record()),
        started: Mutex::new(Vec::new()),
    });
    rt.set_workflow_runner(runner.clone());
    (Arc::new(rt), runner)
}

/// Starts the graph through the runtime's own runner — the console run path — and
/// returns the run id. The agent node parks its gated call and the run settles
/// Blocked rather than erroring.
async fn cold_run(rt: &Arc<crate::company::runtime::CompanyRuntime>) -> String {
    let file = parse_workflow(SOLO_TOML).expect("graph parses");
    let ctx = WorkflowRunContext::new(false);
    let run_id = ctx.run_id.clone();
    let runner = rt.workflow_runner().cloned().expect("a runner is wired");
    runner
        .run(rt.id(), &file, json!({ "request": "the thing" }), &ctx)
        .await
        .expect("a blocked run settles rather than failing");
    run_id
}

/// The agent-internal cards a given run parked, oldest first — the ones keyed
/// under this issue's `workflow-node:` turn key.
fn cards_for(rt: &Arc<crate::company::runtime::CompanyRuntime>, run_id: &str) -> Vec<ApprovalId> {
    let turn = workflow_node_turn_key(run_id, NODE);
    rt.journal()
        .pending()
        .into_iter()
        .filter(|entry| entry.batch.as_deref() == Some(turn.as_str()))
        .map(|entry| entry.id)
        .collect()
}

/// Every agent-internal card still parked, across all runs.
fn all_blocked_cards(rt: &Arc<crate::company::runtime::CompanyRuntime>) -> usize {
    rt.journal()
        .pending()
        .into_iter()
        .filter(|entry| {
            entry
                .batch
                .as_deref()
                .is_some_and(|b| b.starts_with("workflow-node:"))
        })
        .count()
}

/// A resolve, plus the settling window the detached continuation needs — the
/// continuation is spawned rather than awaited (issue #380's drop safety), so an
/// immediate assertion would race it. Bounded, so a genuine failure fails.
async fn resolve_and_settle(
    rt: &Arc<crate::company::runtime::CompanyRuntime>,
    id: &ApprovalId,
    verdict: Verdict,
) {
    rt.resolve_approval(id, verdict, operator())
        .await
        .expect("the verdict is recorded");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
}

/// The headline. On `main` this is red: approving an agent-internal card starts
/// no run, so `runs_started` stays 1. With the fix it is 2 — one continuation.
#[tokio::test]
async fn approving_a_blocked_agent_node_call_continues_the_run() {
    let home = seed_home();
    // Cold turn gates one call; the continuation redeems the grant and finishes.
    let (rt, runner) = runtime(
        home.path(),
        vec![
            Turn::Call {
                tool: "shell",
                args: json!({ "command": "echo hi" }),
            },
            Turn::Say("I was refused, so I stopped."),
            Turn::Say("Done."),
        ],
    )
    .await;

    let run_id = cold_run(&rt).await;
    let cards = cards_for(&rt, &run_id);
    assert_eq!(cards.len(), 1, "the cold run parks exactly one card");
    assert_eq!(runner.started(), 1, "only the cold run has started");

    resolve_and_settle(&rt, &cards[0], Verdict::Approve).await;

    assert_eq!(
        runner.started(),
        2,
        "approving the card auto-continues the run — exactly one continuation"
    );
    assert_eq!(
        all_blocked_cards(&rt),
        0,
        "the continuation redeemed the grant and did not re-park (loop-safety)"
    );
}

/// A refused block starts no continuation — the run stays stopped, which is the
/// correct outcome for a denial.
#[tokio::test]
async fn a_denied_blocked_call_starts_no_continuation() {
    let home = seed_home();
    let (rt, runner) = runtime(
        home.path(),
        vec![
            Turn::Call {
                tool: "shell",
                args: json!({ "command": "echo hi" }),
            },
            Turn::Say("I was refused, so I stopped."),
        ],
    )
    .await;

    let run_id = cold_run(&rt).await;
    let cards = cards_for(&rt, &run_id);
    assert_eq!(cards.len(), 1);

    resolve_and_settle(&rt, &cards[0], Verdict::Deny).await;

    assert_eq!(
        runner.started(),
        1,
        "a denied block starts nothing — no continuation run"
    );
}

/// A node blocked on **two** calls owes ONE continuation, after the LAST
/// decision — the #469 batch property, on the agent-node key. Approving the
/// first releases nothing.
#[tokio::test]
async fn two_calls_on_one_node_continue_once_after_the_last_decision() {
    let home = seed_home();
    let (rt, runner) = runtime(
        home.path(),
        vec![
            // Cold turn: two gated calls, then the model stops.
            Turn::Call {
                tool: "shell",
                args: json!({ "command": "echo one" }),
            },
            Turn::Call {
                tool: "shell",
                args: json!({ "command": "echo two" }),
            },
            Turn::Say("Both were refused, so I stopped."),
            // Continuation: both grants redeem, then it finishes.
            Turn::Say("Done."),
        ],
    )
    .await;

    let run_id = cold_run(&rt).await;
    let cards = cards_for(&rt, &run_id);
    assert_eq!(cards.len(), 2, "the node parked two cards under one batch");
    assert_eq!(runner.started(), 1);

    resolve_and_settle(&rt, &cards[0], Verdict::Approve).await;
    assert_eq!(
        runner.started(),
        1,
        "the first of two decisions releases nothing"
    );

    resolve_and_settle(&rt, &cards[1], Verdict::Approve).await;
    assert_eq!(
        runner.started(),
        2,
        "the last decision releases exactly one continuation for the node"
    );
}

/// Two runs blocked at once do not continue each other — the stash is per
/// (run, node), so approving one run's card leaves the other's untouched and
/// starts a continuation for the approved run only.
#[tokio::test]
async fn two_blocked_runs_do_not_cross_continue() {
    let home = seed_home();
    let (rt, runner) = runtime(
        home.path(),
        vec![
            // Run A cold turn.
            Turn::Call {
                tool: "shell",
                args: json!({ "command": "echo a" }),
            },
            Turn::Say("A refused."),
            // Run B cold turn.
            Turn::Call {
                tool: "shell",
                args: json!({ "command": "echo b" }),
            },
            Turn::Say("B refused."),
            // A's continuation.
            Turn::Say("A done."),
        ],
    )
    .await;

    // Sequential cold runs, so the shared script is consumed deterministically.
    let run_a = cold_run(&rt).await;
    let run_b = cold_run(&rt).await;
    let cards_a = cards_for(&rt, &run_a);
    let cards_b = cards_for(&rt, &run_b);
    assert_eq!(cards_a.len(), 1, "run A parked its own card");
    assert_eq!(cards_b.len(), 1, "run B parked its own card");
    assert_eq!(runner.started(), 2, "two cold runs, no continuations yet");

    resolve_and_settle(&rt, &cards_a[0], Verdict::Approve).await;

    assert_eq!(
        runner.started(),
        3,
        "approving A starts exactly one continuation (A's)"
    );
    assert_eq!(
        cards_for(&rt, &run_b).len(),
        1,
        "run B's card is untouched — no cross-continuation"
    );
}
