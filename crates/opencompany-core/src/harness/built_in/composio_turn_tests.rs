//! Issue #410 — end-to-end proof that an agent with a connected provider can
//! *discover* the action it needs and *call* it, unaided, on more than one
//! toolkit.
//!
//! The acceptance in #410 is behavioural, and the unit tests in
//! [`composio_catalog`](crate::harness::composio_catalog) structurally cannot
//! reach it: they pin how a listing renders, not whether the rendering ever
//! reaches a model's context intact. The failure being fixed happened *on the
//! way out of the tool* — a successful result was passed through the MCP
//! **message** sanitiser, whose 300-byte cap left the model the first action,
//! half of its schema, and a bare `…`. Run against the pre-fix code these tests
//! fail with exactly that fragment.
//!
//! So this drives the **real** harness — real `HarnessPool`, real `build_agent`,
//! real `HostedProvider` on the native tool-calling path, real `ApprovalPolicy`,
//! real `ComposioClient` — and stubs exactly two things, both at a network
//! boundary and neither of which we own:
//!
//! * the **model's choices**, via a scripted OpenAI-compatible endpoint on
//!   loopback (the shape [`search_turn_test`](super::search_turn_test)
//!   established); and
//! * the **Composio backend**, via a second loopback endpoint serving the real
//!   `/agent-integrations/composio/tools` and `/execute` routes with a
//!   *synthesised* catalogue of 120 GitHub and 140 Notion actions. **No Composio
//!   credential exists here** — the tools talk to the managed backend, so
//!   stubbing the managed backend stubs the whole provider chain, and
//!   synthesising the catalogue is the only honest way to prove a generic fix
//!   without two live hundred-action connections.
//!
//! The load-bearing assertions are the ones a unit test cannot make:
//!
//! 1. every Composio tool result the model sees stays under the harness's
//!    16 KiB shared budget, so **our** self-describing cut is the cut, not an
//!    anonymous downstream byte slice;
//! 2. a genuinely oversized listing says it was cut, by how much, and which
//!    argument makes it smaller;
//! 3. the agent gets from "I need to list issues" to a real
//!    `composio_execute(GITHUB_LIST_ISSUES)` with no human supplying the slug —
//!    and does the same on a second, larger, non-GitHub toolkit.
//!
//! Gated on the `composio` feature, which CI builds (`--all-features`) but never
//! *runs*; the narrowing and truncation logic these tests exercise therefore
//! also carries its own tests in [`composio_catalog`], which the
//! `--features openhuman,tinymemory` test lane does run.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::Query;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::company::CompanyManifest;
use crate::company::credentials::Credential;
use crate::harness::composio::TenantComposio;
use crate::harness::mcp_probe::McpFailureQueue;
use crate::harness::orchestrator::{DelegationQueue, WorkflowRunnerHandle};
use crate::harness::policy::ApprovalRequestQueue;
use crate::harness::provider::{HostedProvider, HostedProviderConfig};
use crate::harness::{HarnessDeps, HarnessPool};
use crate::ports::types::CompanyRecord;
use crate::store::{FsCompanyStore, FsContextStore};

/// The harness's shared per-tool-result byte budget
/// (`openhuman::context::DEFAULT_TOOL_RESULT_BUDGET_BYTES`). A Composio result
/// at or above this is cut by machinery that neither counts what it dropped nor
/// names an argument to narrow with — which is the whole bug.
const HARNESS_TOOL_RESULT_BUDGET_BYTES: usize = 16 * 1024;

// ---------------------------------------------------------------------------
// The scripted model
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Turn {
    Call { tool: &'static str, args: Value },
    Say(&'static str),
}

struct Script {
    turns: Mutex<Vec<Turn>>,
    seen: Mutex<Vec<Value>>,
}

fn tool_call_message(tool: &str, args: &Value) -> Value {
    // Plan hive-desks Phase 3: this crate's tools are served over the
    // `opencompany` MCP server, so a scripted model reaches one exactly as a
    // real one does — through `mcp_call_tool`. A native tool is unchanged.
    let (tool, args) = crate::hive::tools::via_opencompany_mcp(tool, args.clone());
    let tool = tool.as_str();
    let args = &args;
    json!({
        "role": "assistant",
        "content": null,
        "tool_calls": [{
            "id": format!("call-{tool}-{}", args.to_string().len()),
            "type": "function",
            "function": { "name": tool, "arguments": args.to_string() }
        }]
    })
}

async fn spawn_script(turns: Vec<Turn>) -> (String, Arc<Script>) {
    let script = Arc::new(Script {
        turns: Mutex::new(turns),
        seen: Mutex::new(Vec::new()),
    });
    let handle = Arc::clone(&script);
    let app = Router::new().route(
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
                let next = next.unwrap_or(Turn::Say("done"));
                let message = match next {
                    Turn::Say(text) => json!({ "role": "assistant", "content": text }),
                    Turn::Call { tool, args } => tool_call_message(tool, &args),
                };
                Json(json!({
                    "choices": [{ "index": 0, "message": message }],
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
// The stubbed Composio backend
// ---------------------------------------------------------------------------

/// A synthesised action, shaped like a real Composio function schema: a long
/// upstream description and a parameter schema with per-property prose. Sizes
/// are deliberately realistic — this is what makes the catalogue genuinely
/// oversized rather than artificially so.
fn action(toolkit: &str, slug: &str, summary: &str) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": slug,
            "description": format!(
                "{summary} This action operates on the connected {toolkit} account. \
                 {}",
                "Upstream publishes several sentences of prose for every action, which is \
                 exactly why a whole toolkit's catalogue does not fit in one tool result. "
                    .repeat(2)
            ),
            "parameters": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "o".repeat(180) },
                    "target": { "type": "string", "description": "t".repeat(180) },
                    "state": { "type": "string", "enum": ["open", "closed", "all"] }
                },
                "required": ["owner"]
            }
        }
    })
}

/// 120 GitHub actions, one of which is the one an agent asked to "list the open
/// issues" actually needs. The needle sorts nowhere near first — the pre-fix
/// cut kept whatever sorted first, so a needle at index 97 is the honest case.
fn github_catalogue() -> Vec<Value> {
    let mut out: Vec<Value> = (0..120)
        .map(|i| {
            action(
                "github",
                &format!("GITHUB_ACTION_{i:03}"),
                &format!("Performs repository operation number {i}."),
            )
        })
        .collect();
    out[97] = action(
        "github",
        "GITHUB_LIST_REPOSITORY_ISSUES",
        "List the issues on a repository, optionally filtered by state.",
    );
    out
}

/// 140 Notion actions — the "at least one large toolkit that is not GitHub"
/// half of the acceptance criteria.
fn notion_catalogue() -> Vec<Value> {
    let mut out: Vec<Value> = (0..140)
        .map(|i| {
            action(
                "notion",
                &format!("NOTION_ACTION_{i:03}"),
                &format!("Performs workspace operation number {i}."),
            )
        })
        .collect();
    out[131] = action(
        "notion",
        "NOTION_SEARCH_NOTION_PAGE",
        "Search the pages in a workspace by query text.",
    );
    out
}

/// What the stub observed, so the tests can assert on the wire rather than on
/// the model's narration.
#[derive(Default)]
struct ComposioStub {
    /// Every `toolkits=` query string the tool sent to `/tools`.
    tool_queries: Mutex<Vec<Option<String>>>,
    /// Every action slug `/execute` was asked to run.
    executed: Mutex<Vec<String>>,
    /// How many times `/tools` was called at all.
    list_calls: AtomicUsize,
}

async fn spawn_composio_backend() -> (String, Arc<ComposioStub>) {
    let stub = Arc::new(ComposioStub::default());

    let tools_stub = Arc::clone(&stub);
    let execute_stub = Arc::clone(&stub);
    let app = Router::new()
        .route(
            "/agent-integrations/composio/tools",
            get(move |Query(params): Query<Vec<(String, String)>>| {
                let stub = Arc::clone(&tools_stub);
                async move {
                    stub.list_calls.fetch_add(1, Ordering::SeqCst);
                    let requested = params
                        .iter()
                        .find(|(k, _)| k == "toolkits")
                        .map(|(_, v)| v.clone());
                    stub.tool_queries.lock().unwrap().push(requested.clone());
                    // Mirror the real backend: `toolkits=` narrows server-side,
                    // its absence returns every enabled toolkit's actions.
                    let mut tools: Vec<Value> = Vec::new();
                    let wants = |name: &str| {
                        requested
                            .as_deref()
                            .map(|r| r.split(',').any(|t| t.eq_ignore_ascii_case(name)))
                            .unwrap_or(true)
                    };
                    if wants("github") {
                        tools.extend(github_catalogue());
                    }
                    if wants("notion") {
                        tools.extend(notion_catalogue());
                    }
                    Json(json!({ "success": true, "data": { "tools": tools } }))
                }
            }),
        )
        .route(
            "/agent-integrations/composio/execute",
            post(move |Json(body): Json<Value>| {
                let stub = Arc::clone(&execute_stub);
                async move {
                    let tool = body
                        .get("tool")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    stub.executed.lock().unwrap().push(tool.clone());
                    Json(json!({
                        "success": true,
                        "data": {
                            "data": { "ran": tool, "items": ["#1 flaky test", "#2 docs typo"] },
                            "successful": true,
                            "costUsd": 0.0
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
// The harness under test
// ---------------------------------------------------------------------------

/// A one-agent company that explicitly grants `composio` (the catch-all `*`
/// deliberately does not) and runs in `full` mode. The scripted actions use
/// the provider's curated read slugs, so the policy can classify and run them
/// rather than conservatively parking an unknown action mid-test.
fn manifest() -> CompanyManifest {
    toml::from_str(
        r#"
[company]
name = "Acme"

[policy]
mode = "full"

[tools]
allow = ["composio"]

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
    composio_url: String,
    dir: &std::path::Path,
) -> (HarnessPool, HarnessDeps, CompanyRecord) {
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
        meter: None,
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
        // An empty toolkit allowlist is "defer to the backend" (open mode) —
        // the worst case for catalogue size, and the case a newly-connected
        // provider lands in.
        #[cfg(feature = "chargebee")]
        chargebee: None,
        #[cfg(feature = "paypal")]
        paypal: None,
        hosting: None,
        composio: Some(TenantComposio::new(
            composio_url,
            Credential::from_value("stub-tenant-token"),
            Vec::new(),
        )),
        steer: crate::company::steer::InflightRegistry::default(),
        run_supervisor: crate::runtime::RunSupervisor::default(),
        delivery: None,
        search: None,
        tenant_search: None,
        workspace: None,
        workflow_runs: None,
        deep_trace: None,
    };

    let record = CompanyRecord {
        overlay_desk_hive: Vec::new(),
        overlay_retired_agents: Vec::new(),
        overlay_agent_edits: Vec::new(),
        // A fresh id per test, for the reason `workspace_turn_helpers_tests`
        // gives: every turn test in this binary runs on the one process-wide
        // OpenHuman runtime, which pins a session's system prompt at its
        // first committed turn — two fixtures naming `acme`/`ceo` resume each
        // other's session, and the second reads a prompt that never named
        // the Composio route.
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
    (pool, deps, record)
}

/// Every tool *result* the harness fed back to the model, in order.
fn tool_results(script: &Script) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for body in script.seen.lock().unwrap().iter() {
        for message in body
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            if message.get("role").and_then(Value::as_str) != Some("tool") {
                continue;
            }
            if let Some(content) = message.get("content").and_then(Value::as_str)
                && !seen.iter().any(|s| s == content)
            {
                seen.push(content.to_string());
            }
        }
    }
    seen
}

fn advertised_tools(script: &Script) -> Vec<String> {
    let mut names: Vec<String> = script
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|body| body.get("tools").and_then(Value::as_array).cloned())
        .flatten()
        .filter_map(|tool| {
            tool.get("function")?
                .get("name")?
                .as_str()
                .map(str::to_string)
        })
        .collect();
    // Plan hive-desks Phase 3 (matching `search_turn_tests::advertised_tools`):
    // this crate's own tools — `composio_list_tools`/`composio_execute` among
    // them — no longer reach the model as direct function tools. They reach it
    // as the `opencompany` MCP catalogue, named in the system prompt's MCP
    // brief and called through `mcp_call_tool`/`mcp_list_tools`, so
    // "advertised" has to read both halves or every non-native tool looks
    // unreachable.
    names.extend(
        script
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter_map(|body| body.get("messages").and_then(Value::as_array).cloned())
            .flatten()
            .filter(|message| message.get("role").and_then(Value::as_str) == Some("system"))
            .filter_map(|message| {
                message
                    .get("content")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .flat_map(|prompt| crate::harness::build::tools_named_in_mcp_brief(&prompt)),
    );
    names.sort();
    names.dedup();
    names
}

/// Every distinct `system` message the harness actually sent to the model —
/// the composed system prompt as it went over the wire, so a test can assert
/// what an agent was really told rather than what a brief function returns in
/// isolation.
fn system_prompts(script: &Script) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for body in script.seen.lock().unwrap().iter() {
        for message in body
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            if message.get("role").and_then(Value::as_str) != Some("system") {
                continue;
            }
            if let Some(content) = message.get("content").and_then(Value::as_str)
                && !seen.iter().any(|s| s == content)
            {
                seen.push(content.to_string());
            }
        }
    }
    seen
}

// ---------------------------------------------------------------------------
// Tests, split by topic (this file would otherwise exceed 750 lines).
// ---------------------------------------------------------------------------

#[path = "composio_turn_tests_discovery.rs"]
mod tests_discovery;
#[path = "composio_turn_tests_narrowing.rs"]
mod tests_narrowing;
