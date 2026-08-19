//! End-to-end proof of issue #988's two halves, driven by a *model*: the turn's
//! tool-iteration ceiling really is [`MAX_TOOL_ITERATIONS`], and a teammate who
//! has declared a `budget_usd_daily` gets an in-turn brake that halts a turn
//! outrunning it — while a teammate who has declared none gets no such brake at
//! all, matching openhuman's own opt-in `GoalBudgetStopHook` posture rather
//! than a blanket ceiling this crate would invent and own alone.
//!
//! Neither half can be shown by a unit test. The cap lives on the vendored
//! session's config and is only spent by the real tool loop, and
//! openhuman's `BudgetStopHook` fires from inside
//! openhuman's `tinyagents` middleware off usage the *provider* reported — so a
//! test that never makes a provider call never fires it. The offline
//! [`MockProvider`](crate::harness::provider::MockProvider) cannot stand in
//! either: it issues no tool calls at all, so a turn against it is one
//! iteration long by construction.
//!
//! So this drives the **real** harness — real [`build_agent`], real
//! [`CompanyAgent::run`], real [`HostedProvider`] (which advertises
//! `tool_calling: true`, putting the turn on the production
//! `NativeToolDispatcher` path), real [`ApprovalPolicy`] under the default
//! `supervised` mode, real sandboxed file tools — and stubs exactly one thing,
//! at the one boundary that needs a credential: the model's choices, via a
//! scripted OpenAI-compatible endpoint on loopback (the shape
//! [`workspace_turn_test`](super::workspace_turn_test) and
//! [`search_turn_test`](super::search_turn_test) established).
//!
//! The scripted model reads a different file each iteration. Distinct arguments
//! are load-bearing: openhuman's repeat-progress guard halts a run that reissues
//! an *identical* successful tool batch, so a loop of one repeated call would
//! stop for a reason that has nothing to do with the cap under test.
//!
//! The load-bearing assertions are the two a shorter test cannot make:
//!
//! * a turn that spends **more than the old ceiling of 10** iterations now
//!   delivers its answer instead of pausing at a checkpoint; and
//! * a budget halt and an iteration-cap pause are **different outcomes** —
//!   openhuman reports the latter through `Agent::last_turn_hit_cap`, which
//!   stays `false` for the former. Part 1 of #926 makes the cap pause
//!   operator-visible, so the two must never be conflated; and
//! * a teammate with **no declared `budget_usd_daily`** gets no in-turn brake
//!   at all — a turn that would have blown past any invented blanket figure
//!   still finishes, because there is no hook installed to stop it.

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::routing::post;
use serde_json::{Value, json};

use crate::company::credentials::Credential;
use crate::company::{Agent as ManifestAgent, Policy};
use crate::harness::build::{MAX_TOOL_ITERATIONS, agent_workspace, build_agent};
use crate::harness::mcp_probe::McpFailureQueue;
use crate::harness::orchestrator::{DelegationQueue, WorkflowRunnerHandle};
use crate::harness::policy::{ApprovalPolicy, ApprovalRequestQueue};
use crate::harness::provider::{HostedProvider, HostedProviderConfig};
use crate::harness::{CompanyAgent, HarnessDeps};
use crate::ports::types::CompanyId;
use crate::store::{FsCompanyStore, FsContextStore};

/// The vendored `AgentConfig::default().max_tool_iterations` this crate used to
/// inherit by omission — the number #988 exists to leave behind.
///
/// Restated here rather than read from openhuman on purpose: the tests below
/// assert that a turn outruns *this* number, and a future vendored bump would
/// otherwise silently weaken them into asserting nothing.
const INHERITED_CAP: usize = 10;

// ---------------------------------------------------------------------------
// The scripted model
// ---------------------------------------------------------------------------

/// What the scripted model does on each successive call.
#[derive(Clone, Debug)]
enum Turn {
    /// Emit a tool call with these literal arguments.
    Call { tool: String, args: Value },
    /// Finish the turn with plain assistant text.
    Say(&'static str),
}

/// A scripted OpenAI-compatible `/chat/completions` endpoint.
struct Script {
    turns: Mutex<Vec<Turn>>,
    /// Every request body the harness sent, for post-hoc assertions.
    seen: Mutex<Vec<Value>>,
    /// `prompt_tokens` echoed on every response. The stop-hook middleware folds
    /// this into the turn's openhuman `TurnCost`, so it is the
    /// knob that decides whether the budget hook fires.
    prompt_tokens: u64,
}

/// One assistant message carrying a native `tool_calls` array — the shape the
/// provider's `tool_calling: true` profile puts the turn loop on.
fn tool_call_message(tool: &str, args: &Value) -> Value {
    json!({
        "role": "assistant",
        "content": null,
        "tool_calls": [{
            "id": format!("call-{tool}"),
            "type": "function",
            "function": { "name": tool, "arguments": args.to_string() }
        }]
    })
}

/// Serve the script on loopback and return its base URL plus the shared handle.
async fn spawn_script(turns: Vec<Turn>, prompt_tokens: u64) -> (String, Arc<Script>) {
    let script = Arc::new(Script {
        turns: Mutex::new(turns),
        seen: Mutex::new(Vec::new()),
        prompt_tokens,
    });
    let handle = Arc::clone(&script);
    let app = axum::Router::new().route(
        "/chat/completions",
        post(move |Json(body): Json<Value>| {
            let script = Arc::clone(&handle);
            async move {
                script.seen.lock().unwrap().push(body.clone());
                let next = {
                    let mut turns = script.turns.lock().unwrap();
                    if turns.is_empty() {
                        None
                    } else {
                        Some(turns.remove(0))
                    }
                };
                // Running off the end of the script means the turn looped more
                // than expected; end it with text rather than hanging.
                let next = next.unwrap_or(Turn::Say("ran off the end of the script"));
                let message = match next {
                    Turn::Say(text) => json!({ "role": "assistant", "content": text }),
                    Turn::Call { tool, args } => tool_call_message(&tool, &args),
                };
                Json(json!({
                    "choices": [{ "index": 0, "message": message }],
                    "usage": {
                        "prompt_tokens": script.prompt_tokens,
                        "completion_tokens": 4
                    }
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), script)
}

/// How many times the scripted model was actually called this turn.
fn model_calls(script: &Script) -> usize {
    script.seen.lock().unwrap().len()
}

/// A script that reads `n` distinct files and then answers.
///
/// Distinct paths, not one path `n` times: an identical successful tool batch
/// reissued back to back is what openhuman's repeat-progress guard halts on, and
/// a run stopped by *that* would prove nothing about the iteration cap.
fn read_then_answer(n: usize, answer: &'static str) -> Vec<Turn> {
    let mut turns: Vec<Turn> = (0..n)
        .map(|i| Turn::Call {
            tool: "file_read".to_string(),
            args: json!({ "path": format!("note-{i:02}.md") }),
        })
        .collect();
    turns.push(Turn::Say(answer));
    turns
}

// ---------------------------------------------------------------------------
// The harness under test
// ---------------------------------------------------------------------------

/// Wire real dependencies against the scripted model. No search backend, no
/// workspace store, no meter — the two things under test are the turn's own
/// iteration ceiling and its in-turn spend brake, and neither reads any of them.
///
/// `meter: None` is a deliberate choice, not a shortcut: it is exactly the state
/// in which the **pre-dispatch** daily-spend gate documents itself as failing
/// open (`HarnessPool::run_inner` warns and runs the turn rather than bricking
/// the teammate). That is the host on which the in-turn brake is the only spend
/// control left standing, which is the condition #988 is about.
fn deps(model_url: String, dir: &std::path::Path) -> HarnessDeps {
    HarnessDeps {
        ledgers: None,
        ledger_registry: Default::default(),
        provider: Arc::new(HostedProvider::new(HostedProviderConfig {
            base_url: model_url,
            credential: Credential::from_value("stub-key"),
            extra_headers: Vec::new(),
        })),
        provider_slug: "managed".to_string(),
        context: Arc::new(FsContextStore::new(dir)),
        store: Arc::new(FsCompanyStore::new(dir)),
        meter: None,
        workspace_root: dir.to_path_buf(),
        workspace_git_enabled: false,
        audit_root: dir.to_path_buf(),
        // Left unset so the turn runs on the tier the manifest resolves
        // (`chat-v1`), which is a *priced* row in openhuman's tier table. The
        // budget hook reads an estimate off that table when the backend echoes no
        // charged amount, so pinning a made-up model name here would make the
        // spend figure depend on a pricing fallback instead of a stated rate.
        model_override: None,
        tasks: None,
        artifacts: None,
        skills: None,
        skills_source_dir: None,
        skills_registry: std::sync::Arc::from([]),
        default_mcp_servers: Vec::new(),
        mcp_servers: Vec::new(),
        facts: None,
        events: None,
        delegations: DelegationQueue::default(),
        workflow_runner: WorkflowRunnerHandle::default(),
        mcp_failures: McpFailureQueue::default(),
        pending_publishes: crate::harness::publish::PendingPublishQueue::default(),
        workflow_refs: crate::harness::workflow_refs::WorkflowRefQueue::default(),
        run_outputs: crate::harness::orchestrator::RunOutputCache::default(),
        run_output_store: None,
        workflow_revisions: None,
        approval_requests: ApprovalRequestQueue::default(),
        secrets: None,
        web_allowed_domains: Vec::new(),
        capabilities: crate::harness::toolbelt::CapabilityFilter::AllowAll,
        workflow_source_dir: None,
        plan: None,
        media: None,
        composio: None,
        #[cfg(feature = "chargebee")]
        chargebee: None,
        #[cfg(feature = "paypal")]
        paypal: None,
        hosting: None,
        steer: crate::company::steer::InflightRegistry::default(),
        run_supervisor: crate::runtime::RunSupervisor::default(),
        delivery: None,
        search: None,
        workspace: None,
        repos: None,
        repo_bindings: Vec::new(),
        checkouts: crate::harness::repo::CheckoutLedger::default(),
    }
}

/// One real company agent, plus `files` notes seeded in its sandbox so every
/// scripted `file_read` succeeds.
///
/// A failing read would be a different experiment: repeated tool *failures* trip
/// openhuman's circuit breaker, which halts the run for a third reason on top of
/// the cap and the budget.
async fn company_agent(
    model_url: String,
    dir: &std::path::Path,
    budget_usd_daily: Option<f64>,
    notes: usize,
) -> CompanyAgent {
    let deps = deps(model_url, dir);
    let company = CompanyId::new("acme");
    let manifest_agent = ManifestAgent {
        global: false,
        id: "ceo".to_string(),
        role: "Chief Executive".to_string(),
        description: None,
        tier: None,
        tools: Vec::new(),
        delegates_to: Vec::new(),
        context: None,
        budget_usd_daily,
        prompt: None,
        prompt_files: Vec::new(),
        prompt_files_resolved: Vec::new(),
        classes: Vec::new(),
        ledgers: None,
        can_declare_ledgers: true,
    };
    // The manifest default. `file_read` reaches nothing outside the sandbox, so
    // it is auto-approved here — the point is that the turn is gated by the real
    // policy, not that the policy is switched off for the test.
    let policy = ApprovalPolicy::new(&Policy::default(), None);
    let agent = build_agent(
        &company,
        "Acme",
        &manifest_agent,
        policy,
        &deps,
        &["docs".to_string()],
        &[],
        &[],
        false,
    )
    .expect("agent builds");

    let workspace = agent_workspace(&deps.workspace_root, &company, "ceo");
    std::fs::create_dir_all(&workspace).expect("workspace");
    for i in 0..notes {
        std::fs::write(
            workspace.join(format!("note-{i:02}.md")),
            format!("Note {i}.\n"),
        )
        .expect("seed note");
    }

    CompanyAgent {
        agent_id: "ceo".to_string(),
        role: "Chief Executive".to_string(),
        budget_usd_daily,
        agent: tokio::sync::Mutex::new(agent),
    }
}

/// Did the just-finished turn pause at the tool-iteration cap?
///
/// openhuman's own answer, read off the same session the turn ran on. This is
/// the distinction Part 1 of #926 surfaces to operators, and the reason the
/// budget halt below has to be measured against it rather than against a
/// substring of some reply.
async fn hit_cap(agent: &CompanyAgent) -> bool {
    agent.agent.lock().await.last_turn_hit_cap()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The in-turn brake arms **only** when the teammate declares a daily cap — and
/// a malformed manifest value is ignored rather than forwarded.
///
/// This is deliberate and matches upstream: openhuman constructs
/// `BudgetStopHook` nowhere and applies only an opt-in token-based goal hook, so
/// this crate, like upstream, refuses to invent a blanket per-turn number no
/// operator can see or change. A teammate with no declared budget is not
/// hard-stopped mid-turn. Forwarding a malformed value would be worse than
/// ignoring it: the vendored hook fails closed on a non-finite or non-positive
/// cap, so a zero would silently halt every turn that teammate ever ran at its
/// first iteration.
#[tokio::test]
async fn the_budget_brake_arms_only_when_a_daily_cap_is_declared() {
    let (model_url, _script) = spawn_script(vec![Turn::Say("hi")], 12).await;
    let dir = tempfile::tempdir().unwrap();
    let mut agent = company_agent(model_url, dir.path(), None, 0).await;

    // No declared budget → no hook armed.
    assert_eq!(agent.turn_spend_cap_usd(), None);

    // A declared daily cap arms the brake at exactly that value — one cap bounds
    // the worst-case overshoot rather than "one turn, of unknown size".
    agent.budget_usd_daily = Some(2.0);
    assert_eq!(agent.turn_spend_cap_usd(), Some(2.0));

    agent.budget_usd_daily = Some(500.0);
    assert_eq!(agent.turn_spend_cap_usd(), Some(500.0));

    // Malformed values arm nothing rather than a fail-closed hook: such a
    // teammate is already refused pre-dispatch (`spent >= cap` holds at zero
    // spend), so no hook is the safe and honest choice.
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        agent.budget_usd_daily = Some(bad);
        assert_eq!(
            agent.turn_spend_cap_usd(),
            None,
            "a manifest cap of {bad} must not reach the hook"
        );
    }
}

/// The headline proof, and the one that fails without the fix: a turn whose work
/// takes more than the inherited ten iterations — read the standards, read the
/// checklist, read the prior spec, … — now **delivers its answer** instead of
/// pausing at a checkpoint the operator has to resume.
///
/// Twelve reads is deliberately just past the old ceiling and well inside the
/// new one, so this measures the ceiling rather than the size of the script.
#[tokio::test]
async fn a_turn_past_the_old_ten_iteration_ceiling_now_finishes() {
    let reads = INHERITED_CAP + 2;
    let (model_url, script) = spawn_script(read_then_answer(reads, "Spec published."), 12).await;

    let dir = tempfile::tempdir().unwrap();
    let agent = company_agent(model_url, dir.path(), None, reads).await;

    let (outcome, _usages) = agent
        .run("Draft and publish the pricing spec.")
        .await
        .expect("the turn runs");

    assert!(
        outcome.reply.contains("Spec published."),
        "the turn did not deliver its answer — it paused instead: {}",
        outcome.reply
    );
    // The script's reads really happened: one model call per tool round plus the
    // final answer. Without this the assertion above could pass on a turn that
    // never looped at all.
    assert_eq!(
        model_calls(&script),
        reads + 1,
        "expected {reads} tool rounds plus a final answer"
    );
    assert!(
        !hit_cap(&agent).await,
        "a turn that finished must not report an iteration-cap pause"
    );
}

/// A turn that outruns its money is halted **inside** the turn — and the halt is
/// not an iteration-cap pause.
///
/// Both halves matter. Before #988 nothing in this crate could stop a running
/// turn except the iteration ceiling itself: the plan-level token ceiling and
/// the teammate's `budget_usd_daily` are both pre-dispatch, so a turn that
/// started under a cap could finish arbitrarily far over it. Raising the ceiling
/// without this hook would have removed the last brake rather than added one.
///
/// And the two stops must stay distinguishable. openhuman only reports
/// `hit_cap` when the run actually reached `max_tool_iterations` with no final
/// response, so a hook-driven halt that stops on the first iteration — nowhere
/// near the ceiling — reads as `false`. That is what Part 1 of #926 needs: it
/// renders the cap pause to the operator and must not label a budget halt as
/// one.
#[tokio::test]
async fn a_budget_halt_stops_the_turn_and_is_not_an_iteration_cap_pause() {
    // One model call reports a million prompt tokens. On the `chat-v1` tier
    // that estimates to ~$0.14, so the very first iteration crosses a $0.05 cap
    // and the hook halts a turn the script was willing to run for twelve more
    // rounds.
    let reads = INHERITED_CAP + 2;
    let (model_url, script) =
        spawn_script(read_then_answer(reads, "Spec published."), 1_000_000).await;

    let dir = tempfile::tempdir().unwrap();
    let agent = company_agent(model_url, dir.path(), Some(0.05), reads).await;

    let (outcome, _usages) = agent
        .run("Draft and publish the pricing spec.")
        .await
        .expect("the turn runs");

    assert!(
        !outcome.reply.contains("Spec published."),
        "the budget hook did not stop the turn — it ran to the script's answer: {}",
        outcome.reply
    );
    // Halted early, not merely slowed: the script offered `reads + 1` rounds and
    // the turn spent a small handful. (The exact count is left loose because the
    // vendored turn adds a closing wrap-up call after a partial run, and that
    // call is not the thing under test.)
    let calls = model_calls(&script);
    assert!(
        calls < reads,
        "expected the turn to halt well short of its {reads} scripted rounds, got {calls}"
    );
    assert!(
        !hit_cap(&agent).await,
        "a budget halt must NOT be reported as an iteration-cap pause — Part 1 of #926 \
         renders that pause to the operator and the two are different outcomes"
    );
}

/// The negative case the budget-halt test above needs to be meaningful: a
/// teammate who has declared **no** `budget_usd_daily` gets no in-turn brake at
/// all, so a turn that would have blown past any invented blanket figure still
/// finishes.
///
/// Same script as the budget-halt test — a million reported prompt tokens per
/// call, which would trip even a generous fixed ceiling on the very first
/// iteration — with the manifest budget omitted instead of set. If this test
/// fails, either a hook is being armed for a teammate who declared nothing, or
/// some other default crept back in.
#[tokio::test]
async fn a_turn_with_no_declared_budget_gets_no_in_turn_brake_at_any_cost() {
    let reads = INHERITED_CAP + 2;
    let (model_url, script) =
        spawn_script(read_then_answer(reads, "Spec published."), 1_000_000).await;

    let dir = tempfile::tempdir().unwrap();
    let agent = company_agent(model_url, dir.path(), None, reads).await;
    assert_eq!(
        agent.turn_spend_cap_usd(),
        None,
        "the fixture must actually be undeclared for this test to prove anything"
    );

    let (outcome, _usages) = agent
        .run("Draft and publish the pricing spec.")
        .await
        .expect("the turn runs");

    assert!(
        outcome.reply.contains("Spec published."),
        "a turn with no declared budget was halted anyway — a brake armed \
         without one being declared: {}",
        outcome.reply
    );
    assert_eq!(
        model_calls(&script),
        reads + 1,
        "expected every scripted round to run — nothing should have cut it short"
    );
    assert!(
        !hit_cap(&agent).await,
        "the script stayed well under the iteration ceiling; a cap pause here \
         would mean something other than the intended reply mechanism stopped \
         the turn"
    );
}

/// The contrast case, so the assertion above is a distinction rather than a
/// constant: a turn that really does exhaust [`MAX_TOOL_ITERATIONS`] **does**
/// report the cap.
///
/// Without this test `!hit_cap` in the budget case could hold because nothing
/// ever sets it.
#[tokio::test]
async fn exhausting_the_raised_cap_still_reports_an_iteration_cap_pause() {
    let reads = MAX_TOOL_ITERATIONS + 5;
    let (model_url, script) = spawn_script(read_then_answer(reads, "Spec published."), 12).await;

    let dir = tempfile::tempdir().unwrap();
    let agent = company_agent(model_url, dir.path(), None, reads).await;

    let (outcome, _usages) = agent
        .run("Draft and publish the pricing spec.")
        .await
        .expect("the turn runs");

    assert!(
        hit_cap(&agent).await,
        "a turn that never stopped calling tools must report the cap: {}",
        outcome.reply
    );
    // It got all the way to the raised ceiling before pausing — the cap moved
    // with the constant rather than staying at the inherited ten. `>=` rather
    // than `==` because the vendored turn adds a wrap-up call on top of the
    // loop's own rounds to compose the checkpoint.
    let calls = model_calls(&script);
    assert!(
        calls >= MAX_TOOL_ITERATIONS,
        "the turn paused after {calls} model calls, short of the stated \
         {MAX_TOOL_ITERATIONS} — the raised cap is not in effect"
    );
}
