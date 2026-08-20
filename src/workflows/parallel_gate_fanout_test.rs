//! Issue #978 — a run that fans out to N gated nodes is cleared by approving,
//! not multiplied by it.
//!
//! # The defect, and why nothing already in the suite caught it
//!
//! Every part worked alone, which is exactly why this needed a test at this
//! altitude. The policy gated each `tool_call` node (#460). The engine paused
//! and `park_pending_gates` wrote a card each (#395). `resume_from_effect`
//! replayed the graph with the approved gate cleared (#243). The continuation
//! queue continued a turn once (#469). Approving still made things worse:
//!
//! * the park recorded **no turn key**, so `approval_cycle` answered
//!   `Some(None)` and each of the three branches believed it was the only
//!   decision outstanding — `stillAwaiting: 0` on all three, including the
//!   first;
//! * the re-dispatch lived in `perform_effect`, which fires **once per approved
//!   effect**, so three approvals started three runs;
//! * each of those replays carried an `approvals` array naming one node, so the
//!   other two paused and parked again.
//!
//! Net for a three-way fan-out: clear 3, create 6. Then 12, then 24. The
//! reported staging tenant accumulated 77 runs of one *disabled* workflow, 17 of
//! which executed exactly one node — the fingerprint of a re-dispatch fragment.
//!
//! Each of the three mechanisms is individually correct and individually tested.
//! Only their composition is wrong, so only a test that drives the whole
//! composition can see it.
//!
//! # What this drives
//!
//! Nothing is stubbed on the path under test. A real graph, the real engine
//! through [`HarnessWorkflowRunner`](super::runner::HarnessWorkflowRunner), the
//! real [`WorkflowToolInvoker`](super::caps), the real `ApprovalPolicy` gate, a
//! real on-disk journal, and a real [`CompanyRuntime`] resolving through
//! `resolve_approval` — the same call the console's Approvals route makes.
//!
//! A `tool_call` graph is the right shape for that: it has no agent node, so
//! there is no model to script and no scripted turn to disagree with the engine.
//! The base URL handed to the provider is a dead port, which is itself an
//! assertion — if anything dispatched a turn, the run would fail rather than
//! quietly pass.
//!
//! # Reading the counts
//!
//! Two numbers carry the whole issue, and both are asserted as **equalities**
//! rather than bounds, because the defect is that they were larger:
//! `runs_started` (3 before the fix, 1 after) and the pending-approval count
//! after each resolve (3 → 6 before, 3 → 2 → 1 → 0 after).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use serde_json::{Value, json};

use crate::company::{CompanyManifest, parse_workflow};
use crate::ports::types::{Actor, ActorKind, ApprovalId, CompanyId, CompanyRecord, Verdict};
use crate::ports::{WorkflowRun, WorkflowRunContext, WorkflowRunner};
use crate::runtime::RuntimeBuilder;
use crate::runtime::workflow_resume::{
    PAYLOAD_NODE_ID, WORKFLOW_APPROVE_KIND, denied_in_input, workflow_turn_key,
};

/// The reported shape, minimised: one trigger fanning out to three `tool_call`
/// nodes over **one** gated slug, converging on a single downstream node.
///
/// `shell` rather than `web_fetch` so the side effect is a file this test can
/// look for — the distinction between "the call was stopped" and "the call ran
/// and the run stopped afterwards" is not visible in the run outcome alone.
const FANOUT_TOML: &str = r#"
id = "fanout"
name = "Fan out"
[[node]]
id = "start"
kind = "trigger"
name = "Start"
[[node]]
id = "fetch_bbc"
kind = "tool_call"
name = "Fetch BBC"
[node.config]
slug = "shell"
[node.config.args]
command = "echo bbc > bbc.txt"
[[node]]
id = "fetch_espn"
kind = "tool_call"
name = "Fetch ESPN"
[node.config]
slug = "shell"
[node.config.args]
command = "echo espn > espn.txt"
[[node]]
id = "fetch_guardian"
kind = "tool_call"
name = "Fetch Guardian"
[node.config]
slug = "shell"
[node.config.args]
command = "echo guardian > guardian.txt"
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

/// The three gated nodes, in graph order.
const FETCHES: [&str; 3] = ["fetch_bbc", "fetch_espn", "fetch_guardian"];

/// What the host actually asked the engine to run.
#[derive(Clone, Debug)]
struct StartedRun {
    input: Value,
}

/// The real runner, wrapped so the test can count dispatches and read the
/// trigger input each one carried.
///
/// A decorator rather than a double: the engine underneath is the production
/// one, so a continuation really does replay the graph and really would re-park
/// its siblings if it were handed the wrong `approvals`. What the wrapper adds
/// is the only two observations the run history cannot give back — **how many
/// runs the host started** (the reported symptom: three, where one was owed) and
/// **what input each carried** (the cause: one approval, where three were owed).
struct RecordingRunner {
    inner: super::runner::HarnessWorkflowRunner,
    started: Mutex<Vec<StartedRun>>,
}

impl RecordingRunner {
    fn started(&self) -> Vec<StartedRun> {
        self.started.lock().expect("recording runner").clone()
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

/// A company that grants `shell` and gates it, under `full` autonomy.
///
/// `full` rather than `supervised` for `gated_tool_call_test`'s reason: it is the
/// stronger claim (the call is stopped whatever the tier) and it keeps
/// exec-security out of the way, so the only thing that can stop the call is the
/// gate this issue is about.
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

/// A home whose `workflows/` directory holds the fan-out graph, so the
/// continuation's loader finds it exactly as the console run route would.
fn seed_home() -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("opencompany-fanout-")
        .tempdir()
        .expect("tempdir");
    let workflows = dir.path().join("workflows");
    std::fs::create_dir_all(&workflows).expect("workflows dir");
    std::fs::write(workflows.join("fanout.toml"), FANOUT_TOML).expect("seed graph");
    dir
}

/// A runtime wired to the **real** workflow runner, parking into its own gate,
/// journal and continuation queues.
///
/// The parking bundle is rebuilt over the runtime's handles rather than left as
/// the deps fixture's own, and that is the point of the fixture: a card has to
/// land in the queue the runtime resolves from, and the park has to arm the
/// counters the resolve releases. Two sets of handles would make every
/// assertion here vacuous.
async fn runtime(
    home: &std::path::Path,
) -> (
    Arc<crate::company::runtime::CompanyRuntime>,
    Arc<RecordingRunner>,
) {
    let mut rt = RuntimeBuilder::new(home.to_path_buf(), manifest())
        .with_seed_dir(home.to_path_buf())
        .build()
        .await
        .expect("runtime builds");

    // A base URL nothing calls: this graph has no agent node, so no model is
    // reached. A dead address is the assertion.
    let (mut deps, _unused) =
        super::gated_tool_turn_test::deps("http://127.0.0.1:1/unused".to_string(), home);
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

/// Starts the fan-out graph through the runtime's own runner — the path the
/// console run route takes — and returns the run id.
async fn cold_run(rt: &Arc<crate::company::runtime::CompanyRuntime>) -> String {
    let file = parse_workflow(FANOUT_TOML).expect("graph parses");
    let ctx = WorkflowRunContext::new(false);
    let run_id = ctx.run_id.clone();
    let runner = rt.workflow_runner().cloned().expect("a runner is wired");
    let run = runner
        .run(rt.id(), &file, json!({ "topic": "sports" }), &ctx)
        .await
        .expect("the run settles — a gated node pauses it, it does not error");
    assert_eq!(
        sorted(run.pending_approvals.clone()),
        FETCHES.map(str::to_string).to_vec(),
        "the cold run must pause on all three fan-out branches"
    );
    run_id
}

fn sorted(mut items: Vec<String>) -> Vec<String> {
    items.sort();
    items
}

/// The gate cards currently on the Approvals page, oldest first.
fn gate_cards(
    rt: &Arc<crate::company::runtime::CompanyRuntime>,
) -> Vec<(ApprovalId, String, Option<String>)> {
    rt.journal()
        .pending()
        .into_iter()
        .filter(|entry| entry.effect.kind == WORKFLOW_APPROVE_KIND)
        .map(|entry| {
            let node = entry.effect.payload[PAYLOAD_NODE_ID]
                .as_str()
                .expect("a gate card names its node")
                .to_string();
            (entry.id, node, entry.batch)
        })
        .collect()
}

/// How many gate cards are waiting. The number issue #978 says must never go up
/// when an operator works through the queue.
fn pending_gates(rt: &Arc<crate::company::runtime::CompanyRuntime>) -> usize {
    gate_cards(rt).len()
}

/// How many runs the host has started. The reported symptom, counted directly.
fn runs_started(runner: &Arc<RecordingRunner>) -> usize {
    runner.started().len()
}

/// A resolve, plus the settling window the detached continuation needs.
///
/// The continuation is spawned rather than awaited (issue #380's drop safety),
/// so a test that asserted immediately would race it. Bounded, so a genuine
/// failure fails rather than hangs.
async fn resolve_and_settle(
    rt: &Arc<crate::company::runtime::CompanyRuntime>,
    id: &ApprovalId,
    verdict: Verdict,
) {
    rt.resolve_approval(id, verdict, operator())
        .await
        .expect("the verdict is recorded");
    settle().await;
}

async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
}

/// Whether a `shell` node's side effect landed anywhere under the workspace
/// root. The per-run workspace is a hashed path, so this looks for the file.
fn wrote(root: &std::path::Path, name: &str) -> bool {
    fn walk(dir: &std::path::Path, name: &str) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if walk(&path, name) {
                    return true;
                }
            } else if path.file_name().is_some_and(|n| n == name) {
                return true;
            }
        }
        false
    }
    walk(root, name)
}

/// **T1** — a cold run of a three-way fan-out parks exactly three cards and runs
/// no node, and all three cards name **one** batch: the run.
///
/// The batch key is the accounting fix in one assertion. Before #978 every card
/// carried `None` here, which is what made three branches each believe they were
/// the last decision outstanding.
#[tokio::test]
async fn a_cold_fanout_parks_three_gates_under_one_run_batch() {
    let home = seed_home();
    let (rt, _runner) = runtime(home.path()).await;
    let run_id = cold_run(&rt).await;

    let cards = gate_cards(&rt);
    assert_eq!(cards.len(), 3, "one card per gated branch: {cards:?}");
    assert_eq!(
        sorted(cards.iter().map(|(_, node, _)| node.clone()).collect()),
        FETCHES.map(str::to_string).to_vec()
    );

    // No branch executed. This is the gate doing its job, and it is what makes
    // the counts below about approvals rather than about work already done.
    for file in ["bbc.txt", "espn.txt", "guardian.txt"] {
        assert!(
            !wrote(home.path(), file),
            "{file} must not have been written"
        );
    }

    let want = Some(workflow_turn_key(&run_id));
    for (id, node, batch) in &cards {
        assert_eq!(
            batch, &want,
            "gate {node} ({id}) must be batched under its run, not left unbatched"
        );
    }
}

/// **T2** — the first two approvals release **nothing**: no new run, no new
/// card, and the outstanding count counts down honestly.
///
/// `decisions_still_awaited` is the number the console renders as "the agent
/// picks this up once N more are decided". It read `0` on all three of three
/// before this issue, which is what told an operator their first click had
/// completed the action.
#[tokio::test]
async fn the_first_two_approvals_release_nothing_and_count_down() {
    let home = seed_home();
    let (rt, runner) = runtime(home.path()).await;
    cold_run(&rt).await;
    let before = runs_started(&runner);
    let cards = gate_cards(&rt);

    assert_eq!(
        rt.decisions_still_awaited(&cards[0].0),
        2,
        "three parked, one being decided — two others outstanding"
    );
    resolve_and_settle(&rt, &cards[0].0, Verdict::Approve).await;
    assert_eq!(
        runs_started(&runner),
        before,
        "the first approval starts no run"
    );
    assert_eq!(pending_gates(&rt), 2, "and creates no card");

    assert_eq!(rt.decisions_still_awaited(&cards[1].0), 1);
    resolve_and_settle(&rt, &cards[1].0, Verdict::Approve).await;
    assert_eq!(runs_started(&runner), before, "nor does the second");
    assert_eq!(pending_gates(&rt), 1);

    assert_eq!(
        rt.decisions_still_awaited(&cards[2].0),
        0,
        "the last decision is the one that releases the run"
    );
}

/// **T3** — the headline. The **last** approval starts exactly **one** run.
///
/// The bug is that three appeared, so this asserts the count rather than merely
/// that something ran. Run three of three is the only one that may start
/// anything at all.
#[tokio::test]
async fn approving_all_three_starts_exactly_one_continuation_run() {
    let home = seed_home();
    let (rt, runner) = runtime(home.path()).await;
    cold_run(&rt).await;
    let before = runs_started(&runner);

    for (id, _, _) in gate_cards(&rt) {
        resolve_and_settle(&rt, &id, Verdict::Approve).await;
    }

    assert_eq!(
        runs_started(&runner) - before,
        1,
        "three approvals owe ONE continuation; before #978 each started its own"
    );
}

/// **T4** — that one run carries all three approvals, so every branch executes
/// and the graph converges instead of re-gating its siblings.
///
/// The three side-effect files are the proof that the replay actually ran the
/// branches rather than merely being handed their ids.
#[tokio::test]
async fn the_continuation_carries_every_approval_and_runs_every_branch() {
    let home = seed_home();
    let (rt, runner) = runtime(home.path()).await;
    cold_run(&rt).await;

    for (id, _, _) in gate_cards(&rt) {
        resolve_and_settle(&rt, &id, Verdict::Approve).await;
    }
    settle().await;

    let started = runner.started();
    let continuation = started.last().expect("a continuation ran");
    assert_eq!(
        sorted(
            continuation.input["approvals"]
                .as_array()
                .expect("the continuation carries an approvals array")
                .iter()
                .map(|id| id.as_str().expect("node ids are strings").to_string())
                .collect()
        ),
        FETCHES.map(str::to_string).to_vec(),
        "the ONE continuation must clear all three gates, not just the last clicked: {:?}",
        continuation.input
    );

    for file in ["bbc.txt", "espn.txt", "guardian.txt"] {
        assert!(
            wrote(home.path(), file),
            "every approved branch must have executed; {file} is missing"
        );
    }
    assert_eq!(
        pending_gates(&rt),
        0,
        "and the queue is empty rather than refilled"
    );
}

/// **T5** — the invariant, by name: **approving never increases the number of
/// pending approvals.**
///
/// Asserted as a monotone sequence rather than only at the end, because the
/// reported failure is a *growth curve* (3 → 6 → 12 → 24) and an end-state
/// assertion would pass on a run that spiked in the middle.
#[tokio::test]
async fn the_pending_approval_count_never_increases_across_a_round() {
    let home = seed_home();
    let (rt, _runner) = runtime(home.path()).await;
    cold_run(&rt).await;

    let mut seen = vec![pending_gates(&rt)];
    for (id, _, _) in gate_cards(&rt) {
        resolve_and_settle(&rt, &id, Verdict::Approve).await;
        seen.push(pending_gates(&rt));
    }

    assert_eq!(
        seen,
        vec![3, 2, 1, 0],
        "each decision must clear one card and create none; before #978 this read [3, 6, ..]"
    );
    for pair in seen.windows(2) {
        assert!(
            pair[1] <= pair[0],
            "approving increased the queue from {} to {}: {seen:?}",
            pair[0],
            pair[1]
        );
    }
}

/// **T6** — a mixed verdict is still one run, and a refusal is **final**.
///
/// The trap this pins: the denied node is not in the approved set, so a
/// continuation that knew only about approvals would replay into it, pause, and
/// park a brand-new card — an approval round that cleared three and created one.
/// The denial ledger is what makes the refusal stick.
#[tokio::test]
async fn a_denied_branch_does_not_re_park_and_still_yields_one_run() {
    let home = seed_home();
    let (rt, runner) = runtime(home.path()).await;
    cold_run(&rt).await;
    let before = runs_started(&runner);

    let cards = gate_cards(&rt);
    let denied = cards
        .iter()
        .find(|(_, node, _)| node == "fetch_guardian")
        .expect("the guardian branch is parked")
        .clone();
    for (id, node, _) in &cards {
        let verdict = if node == &denied.1 {
            Verdict::Deny
        } else {
            Verdict::Approve
        };
        resolve_and_settle(&rt, id, verdict).await;
    }
    settle().await;

    assert_eq!(
        runs_started(&runner) - before,
        1,
        "a mixed verdict owes one continuation, not one per approval"
    );
    assert_eq!(
        pending_gates(&rt),
        0,
        "the refused branch must NOT come back as a fresh card"
    );

    let started = runner.started();
    let continuation = started.last().expect("the continuation ran");
    assert_eq!(
        denied_in_input(&continuation.input),
        vec!["fetch_guardian".to_string()],
        "the continuation must carry the refusal: {:?}",
        continuation.input
    );
    // The approved work is not discarded by one refusal…
    assert!(wrote(home.path(), "bbc.txt"));
    assert!(wrote(home.path(), "espn.txt"));
    // …and the refused call never happened.
    assert!(
        !wrote(home.path(), "guardian.txt"),
        "a denied branch must not execute"
    );
}

/// **T7** — recovery. A restart with two of three decided comes back blocked on
/// **one**, not zero.
///
/// The failure this guards is the opposite of the headline and just as bad: a
/// rehydrate that forgot the run was partly decided would fire its continuation
/// on the next decision regardless of how many were still owed — or, if it
/// re-armed all three, would wait forever for two decisions that already
/// happened.
#[tokio::test]
async fn a_restart_mid_round_comes_back_blocked_on_what_is_left() {
    let home = seed_home();
    let remaining = {
        let (rt, _runner) = runtime(home.path()).await;
        cold_run(&rt).await;
        let cards = gate_cards(&rt);
        resolve_and_settle(&rt, &cards[0].0, Verdict::Approve).await;
        resolve_and_settle(&rt, &cards[1].0, Verdict::Approve).await;
        assert_eq!(pending_gates(&rt), 1);
        cards[2].1.clone()
    }; // the "process" goes away

    let (rt, _runner) = runtime(home.path()).await;
    rt.recover().await.expect("replay rehydrates the parks");

    let cards = gate_cards(&rt);
    assert_eq!(
        cards.len(),
        1,
        "only the undecided gate survives the restart: {cards:?}"
    );
    assert_eq!(cards[0].1, remaining);
    assert_eq!(
        rt.decisions_still_awaited(&cards[0].0),
        0,
        "one gate left, so deciding it is what releases the run"
    );
    let turn = cards[0]
        .2
        .clone()
        .expect("the rehydrated card keeps its batch");
    assert_eq!(
        rt.workflow_gates().undecided(&turn),
        1,
        "the rehydrated batch is blocked on exactly the gate still parked"
    );
}

/// The unit companion to T1's batching, on the default lane: the turn key a park
/// writes is the one the release reads.
///
/// A round-trip rather than two literals, because drift here does not fail
/// loudly — it reads a workflow key as a brain turn and runs an agent cycle over
/// a run that then never continues.
#[test]
fn a_run_turn_key_round_trips() {
    let key = workflow_turn_key("01a013e576f7");
    assert_eq!(
        crate::runtime::workflow_resume::run_id_from_turn(&key),
        Some("01a013e576f7")
    );
    // A brain turn's cycle id is not a run, and must not be read as one.
    assert_eq!(
        crate::runtime::workflow_resume::run_id_from_turn("cycle-1"),
        None
    );
    // Nor is a prefix with nothing behind it.
    assert_eq!(
        crate::runtime::workflow_resume::run_id_from_turn("workflow-run:"),
        None
    );
}

/// The denial ledger's own round-trip: what a continuation writes is what the
/// next park reads back.
#[test]
fn the_denial_ledger_round_trips_through_a_trigger_input() {
    assert!(denied_in_input(&json!({})).is_empty());
    let input: Value = json!({ "__opencompany_denied": ["fetch_guardian", "fetch_espn"] });
    assert_eq!(
        denied_in_input(&input),
        vec!["fetch_guardian".to_string(), "fetch_espn".to_string()]
    );
    // Tolerant, on `delivered_in_input`'s terms: a malformed row is not a denial.
    assert!(denied_in_input(&json!({ "__opencompany_denied": "nope" })).is_empty());
    assert_eq!(
        denied_in_input(&json!({ "__opencompany_denied": ["ok", 7, null] })),
        vec!["ok".to_string()]
    );
}
