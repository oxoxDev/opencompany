//! End-to-end proof that a tool call a model wrote as **text** is actually
//! executed by a real turn — not just parsed by a unit test.
//!
//! The unit tests in [`native_salvage`](crate::harness::native_salvage) pin the
//! parser. They cannot tell you whether the recovery is *reachable*: whether
//! `build_agent` really wraps the native dispatcher, whether a recovered call
//! survives the [`ApprovalPolicy`] gate the same way a native one does, whether
//! it dispatches through openhuman's tool loop, and — the subtle one — whether
//! its **synthesized id keeps the cycle paired** so the result reaches the
//! model's next request instead of being dropped on the way to the wire.
//!
//! Each of those is a place the wiring can be silently wrong while the parser is
//! perfect, and the last one fails in the most misleading way available: the
//! tool runs, the operator is billed for it, and the model never learns the
//! answer — so it re-narrates the same intention on the next iteration, which is
//! indistinguishable from the model simply being bad at its job.
//!
//! So this drives the **real** harness — real `HarnessPool`, real `build_agent`,
//! real `HostedProvider` (whose `tool_calling: true` profile is what puts the
//! turn on the native path this module wraps), real `ApprovalPolicy`, real
//! `UsageMeter` — and stubs exactly two things, both at a network boundary:
//! the model's choices, and the managed search backend.
//!
//! `web_search` is the tool under recovery because it is *observable at three
//! independent layers*: the backend counts its calls, the meter prices them, and
//! the results are distinctive text that either reaches the model's context or
//! does not. A recovery that parsed but never executed would still pass an
//! assertion about the parsed call; it cannot pass these.
//!
//! The fixture mirrors [`search_turn_test`](super::search_turn_test), which
//! established this shape.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::routing::post;
use serde_json::{Value, json};

use crate::company::CompanyManifest;
use crate::company::credentials::Credential;
use crate::harness::mcp_probe::McpFailureQueue;
use crate::harness::orchestrator::{DelegationQueue, WorkflowRunnerHandle};
use crate::harness::policy::ApprovalRequestQueue;
use crate::harness::provider::{HostedProvider, HostedProviderConfig};
use crate::harness::search::SearchBackend;
use crate::harness::{HarnessDeps, HarnessPool};
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::ports::usage::{SampleKind, UsageMeter, UsageSample};
use crate::store::{FsCompanyStore, FsContextStore};

// ---------------------------------------------------------------------------
// The scripted model
// ---------------------------------------------------------------------------

/// What the scripted model does on each successive call.
#[derive(Clone, Debug)]
enum Turn {
    /// Put `content` on the wire with **no** `tool_calls` array — the failing
    /// shape this module exists for. Whatever the model wrote is all there is.
    Text(&'static str),
}

/// A scripted OpenAI-compatible `/chat/completions` endpoint.
struct Script {
    turns: Mutex<Vec<Turn>>,
    /// Every request body the harness sent, for post-hoc assertions.
    seen: Mutex<Vec<Value>>,
}

/// Serve the script on loopback and return its base URL plus the shared handle.
async fn spawn_script(turns: Vec<Turn>) -> (String, Arc<Script>) {
    let script = Arc::new(Script {
        turns: Mutex::new(turns),
        seen: Mutex::new(Vec::new()),
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
                let next = next.unwrap_or(Turn::Text("done"));
                let Turn::Text(content) = next;
                Json(json!({
                    "choices": [{
                        "index": 0,
                        "message": { "role": "assistant", "content": content }
                    }],
                    "usage": { "prompt_tokens": 12, "completion_tokens": 4 }
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

// ---------------------------------------------------------------------------
// The stubbed search backend
// ---------------------------------------------------------------------------

/// A loopback stand-in for the managed search backend, counting what actually
/// reached it. No third-party search key exists here — the tool authenticates
/// to the managed platform, so the platform is the only thing to stub.
struct SearchStub {
    calls: AtomicUsize,
}

async fn spawn_search_backend() -> (String, Arc<SearchStub>) {
    let stub = Arc::new(SearchStub {
        calls: AtomicUsize::new(0),
    });
    let handle = Arc::clone(&stub);
    let app = axum::Router::new().route(
        "/agent-integrations/parallel/search",
        post(move |Json(_body): Json<Value>| {
            let stub = Arc::clone(&handle);
            async move {
                stub.calls.fetch_add(1, Ordering::SeqCst);
                Json(json!({
                    "success": true,
                    "data": {
                        "searchId": "search-1",
                        "provider": "Exa",
                        "costUsd": 0.013,
                        "results": [{
                            "url": "https://competitor.test/pricing",
                            "title": "Competitor pricing",
                            "publish_date": "2026-05-02",
                            "excerpts": ["Team plan is $29 per seat per month."]
                        }]
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
    (format!("http://{addr}"), stub)
}

// ---------------------------------------------------------------------------
// A recording meter
// ---------------------------------------------------------------------------

/// The real port, recording in memory — so "the console would show this" is an
/// assertion about the same trait the usage reads go through.
#[derive(Default)]
struct RecordingMeter {
    samples: Mutex<Vec<UsageSample>>,
}

#[async_trait::async_trait]
impl UsageMeter for RecordingMeter {
    async fn record(&self, _company: &CompanyId, sample: &UsageSample) -> crate::Result<()> {
        self.samples.lock().unwrap().push(sample.clone());
        Ok(())
    }
    async fn query(&self, _company: &CompanyId, _since: u64) -> crate::Result<Vec<UsageSample>> {
        Ok(self.samples.lock().unwrap().clone())
    }
}

/// How many `SearchCall` samples the meter holds.
///
/// Filtered rather than counting every sample: a real turn also records
/// `Inference` samples through the harness's own cost hook, so "the meter is
/// empty" would be false even when no search ran — exactly the kind of
/// assertion that passes for the wrong reason.
fn search_samples(meter: &RecordingMeter) -> usize {
    meter
        .samples
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.kind == SampleKind::SearchCall)
        .count()
}

/// Every tool *result* the harness fed back to the model.
///
/// Empty is the signature of a dropped cycle: `pair_tool_cycles` removes an
/// assistant opener whose id set does not match the results answering it, and
/// the tool message goes with it. So a non-empty list here is the direct proof
/// that the synthesized ids paired.
fn tool_results(script: &Script) -> Vec<String> {
    script
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|body| body.get("messages").and_then(Value::as_array).cloned())
        .flatten()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("tool"))
        .filter_map(|m| m.get("content").and_then(Value::as_str).map(str::to_string))
        .collect()
}

// ---------------------------------------------------------------------------
// The harness under test
// ---------------------------------------------------------------------------

fn manifest() -> CompanyManifest {
    toml::from_str(
        r#"
[company]
name = "Acme"

[policy]
mode = "supervised"

[tools]
allow = ["search"]
search_daily_calls = 50

[[agent]]
id = "ceo"
role = "Chief Executive"
tier = "orchestrator"
"#,
    )
    .expect("manifest parses")
}

async fn harness(
    model_url: String,
    search_url: String,
    dir: &std::path::Path,
) -> (HarnessPool, HarnessDeps, CompanyRecord, Arc<RecordingMeter>) {
    let meter = Arc::new(RecordingMeter::default());
    let deps = HarnessDeps {
        takeovers: Default::default(),
        emergency_gate: None,
        notifications: None,
        ledgers: None,
        ledger_registry: Default::default(),
        provider: Arc::new(HostedProvider::new(HostedProviderConfig {
            base_url: model_url,
            credential: Credential::from_value("stub-key"),
            extra_headers: Vec::new(),
        })),
        provider_slug: "managed".to_string(),
        serves: None,
        context: Arc::new(FsContextStore::new(dir)),
        store: Arc::new(FsCompanyStore::new(dir)),
        meter: Some(meter.clone()),
        workspace_root: dir.to_path_buf(),
        mcp_home: None,
        workspace_git_enabled: false,
        audit_root: dir.to_path_buf(),
        model_override: Some("stub-model".to_string()),
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
        approval_parker: None,
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
        search: Some(SearchBackend::new(
            search_url,
            Credential::from_value("stub-platform-token"),
            50,
        )),
        tenant_search: None,
        workspace: None,
        workflow_runs: None,
        deep_trace: None,
    };

    let record = CompanyRecord {
        overlay_desk_hive: Vec::new(),
        overlay_retired_agents: Vec::new(),
        overlay_agent_edits: Vec::new(),
        id: crate::test_support::per_test_company_id("acme"),
        manifest: manifest(),
        ledger: Vec::new(),
        lifecycle: "running".to_string(),
        overlay_agents: Vec::new(),
        overlay_desk_members: Vec::new(),
        overlay_desk_order: Vec::new(),
        overlay_desks: Vec::new(),
        overlay_workflows: Vec::new(),
        overlay_budgets: Vec::new(),
        overlay_policy: None,
        overlay_tool_grants: None,
        overlay_desk_tools: Default::default(),
        disabled_workflows: Vec::new(),
        template_provenance: None,
        setup: None,
        name_confirmed: false,
        activation_completed_at: None,
        created_at_millis: None,
    };

    let pool = HarnessPool::new();
    pool.ensure(&record, &deps).await.expect("pool ensures");
    (pool, deps, record, meter)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The headline proof, on the exact shape observed in the 1/9 QA round: a
/// lead-in sentence, a `function_call:` marker, and a `call` name key, with no
/// `tool_calls` array anywhere on the wire.
///
/// Pre-fix this whole message was narrative text: `web_search` never ran, the
/// backend was never called, nothing was metered, and the raw JSON was shown to
/// the operator as the agent's reply.
#[tokio::test]
async fn a_real_turn_executes_a_tool_call_the_model_wrote_as_text() {
    let (model_url, script) = spawn_script(vec![
        Turn::Text(
            "Let me look that up. function_call:{\"id\":\"call_3rY\",\
             \"call\":\"web_search\",\"arguments\":{\"query\":\"competitor pricing\"}}",
        ),
        Turn::Text("Their team plan is $29 per seat per month."),
    ])
    .await;
    let (search_url, backend) = spawn_search_backend().await;

    let dir = tempfile::tempdir().unwrap();
    let (pool, deps, record, meter) = harness(model_url, search_url, dir.path()).await;

    let outcome = pool
        .run(
            &record.id,
            "ceo",
            "What does our competitor charge?",
            &deps,
            crate::runtime::delegation::ChatTarget::default(),
        )
        .await
        .expect("turn runs");

    // 1. The tool really executed — counted at the backend, not inferred from a
    //    parsed call.
    assert_eq!(
        backend.calls.load(Ordering::SeqCst),
        1,
        "the recovered call never reached the search backend"
    );

    // 2. It went through the real metered path, so the console would show it.
    assert_eq!(
        search_samples(&meter),
        1,
        "the recovered call was not priced like a native one"
    );

    // 3. The result reached the MODEL's next request. This is the assertion
    //    that fails if the synthesized ids do not pair: the tool would still
    //    have run, and the model would still never have learned the answer.
    let joined = tool_results(&script).join("\n---\n");
    assert!(
        joined.contains("https://competitor.test/pricing"),
        "the tool result never reached the model — the cycle was dropped unpaired: {joined}"
    );

    // 4. The turn completed on the model's own follow-up, which it could only
    //    write having seen the result.
    assert!(
        outcome.reply.contains("$29 per seat"),
        "the turn did not complete: {}",
        outcome.reply
    );

    // 5. And the raw call never surfaced as something the operator reads.
    assert!(
        !outcome.reply.contains("function_call"),
        "the raw call leaked into the reply: {}",
        outcome.reply
    );
}

/// The control that keeps the headline test honest: the same text-shaped call,
/// naming a tool this agent does **not** have.
///
/// Checking the name against the agent's real belt is the entire licence for
/// recovering an object out of prose. Without this test, a regression that
/// dropped the check would leave every assertion above passing while the parser
/// dispatched anything object-shaped a model happened to write.
#[tokio::test]
async fn a_text_shaped_call_to_a_tool_the_agent_does_not_have_is_not_dispatched() {
    let (model_url, _script) = spawn_script(vec![Turn::Text(
        "Let me look that up. function_call:{\"call\":\"delete_production_db\",\
         \"arguments\":{\"confirm\":true}}",
    )])
    .await;
    let (search_url, backend) = spawn_search_backend().await;

    let dir = tempfile::tempdir().unwrap();
    let (pool, deps, record, meter) = harness(model_url, search_url, dir.path()).await;

    let outcome = pool
        .run(
            &record.id,
            "ceo",
            "Clean up the database.",
            &deps,
            crate::runtime::delegation::ChatTarget::default(),
        )
        .await
        .expect("turn runs");

    assert_eq!(
        backend.calls.load(Ordering::SeqCst),
        0,
        "an unknown tool name must never reach a backend"
    );
    assert_eq!(search_samples(&meter), 0, "and must never be metered");
    // Nothing was recovered, so the text stands as the model's answer rather
    // than being silently swallowed.
    assert!(
        outcome.reply.contains("delete_production_db"),
        "an unrecovered call must stay visible, not vanish: {}",
        outcome.reply
    );
}
