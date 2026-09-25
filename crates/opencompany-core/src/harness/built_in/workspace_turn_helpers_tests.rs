//! End-to-end proof that the workspace tools (issue #237) actually work when a
//! *model* drives them — not just when a unit test calls `execute()` directly.
//!
//! Unit tests in [`workspace_tools`](crate::harness::workspace_tools) pin the
//! tools' own behaviour. They cannot tell you whether a tool is reachable from a
//! real turn: whether it survives `build_agent`'s grant gates, is advertised on
//! the wire in the shape the provider emits, passes the [`ApprovalPolicy`] gate,
//! dispatches through openhuman's native tool loop, and hands its result back
//! into the model's context. Every one of those is a place the wiring can be
//! silently wrong while the tools themselves are perfect.
//!
//! So this drives the **real** harness — real `HarnessPool`, real
//! `build_agent`, real `HostedProvider` (which advertises `tool_calling: true`,
//! putting the turn on the production `NativeToolDispatcher` path), real
//! `ApprovalPolicy`, real `FsOps`-backed `WorkspaceStore` — and stubs only the
//! one thing that needs a credential: the model's *choices*. A scripted
//! OpenAI-compatible endpoint on loopback returns the `tool_calls` a model would
//! return.
//!
//! The load-bearing detail is that the stub reads the revision token **out of
//! the conversation it is sent**, exactly as a model would. Nothing hands it the
//! value out of band. If the read tool stopped emitting `rev=…`, or the tool
//! result stopped reaching the model's context, the scripted write would fail to
//! find a revision and the test would fail rather than quietly passing.

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
use crate::harness::{HarnessDeps, HarnessPool};
use crate::ports::types::CompanyRecord;
use crate::ports::workspace::{NodeKind, WorkspaceNode, WorkspaceOrigin, WorkspaceStore};
use crate::store::{FsCompanyStore, FsContextStore, FsOps};

/// What the scripted model does on each successive call.
#[derive(Clone, Debug)]
pub(crate) enum Turn {
    /// Emit a tool call with these literal arguments.
    Call { tool: &'static str, args: Value },
    /// Emit a tool call whose args are built from the revision the conversation
    /// has already carried back (`rev=<n>` in a tool result), proving the token
    /// really travels read → model → write.
    WriteWithObservedRev {
        path: &'static str,
        content: &'static str,
        /// Offset applied to the observed revision. `0` writes with the current
        /// revision (must land); a non-zero value fakes a stale read.
        delta: i64,
    },
    /// Finish the turn with plain assistant text.
    Say(&'static str),
}

/// A scripted OpenAI-compatible `/chat/completions` endpoint.
pub(crate) struct Script {
    turns: Mutex<Vec<Turn>>,
    /// Every request body the harness sent, for post-hoc assertions.
    seen: Mutex<Vec<Value>>,
}

/// Sent as `expected_updated_at` when [`observed_rev`] finds no `rev=` token at
/// all, i.e. the revision never reached the model's context.
///
/// Without it these tests pass for the wrong reason: a missing revision used to
/// fall back to `0`, and `0` is refused with the very "changed since you read
/// it" message the stale-write test asserts on — so the test stayed green even
/// when the read → model → write round trip it exists to prove was broken. No
/// real note can carry this revision, so asserting it never appears in a tool
/// result turns that silent pass into a failure.
pub(crate) const UNOBSERVED_REV: u64 = u64::MAX;

/// Pull the most recent `rev=<digits>` out of the conversation the stub was
/// sent. This is the model's-eye view: the revision is only available because
/// `workspace_read`'s result was fed back into the context.
pub(crate) fn observed_rev(body: &Value) -> Option<u64> {
    let messages = body.get("messages")?.as_array()?;
    let mut found = None;
    for message in messages {
        let Some(content) = message.get("content").and_then(Value::as_str) else {
            continue;
        };
        let mut rest = content;
        while let Some(at) = rest.find("rev=") {
            let digits: String = rest[at + 4..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if let Ok(value) = digits.parse::<u64>() {
                found = Some(value);
            }
            rest = &rest[at + 4..];
        }
    }
    found
}

/// Serve the script on loopback and return its base URL plus the shared handle.
pub(crate) async fn spawn_script(turns: Vec<Turn>) -> (String, Arc<Script>) {
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
                let next = next.unwrap_or(Turn::Say("done"));
                let message = match next {
                    Turn::Say(text) => json!({ "role": "assistant", "content": text }),
                    Turn::Call { tool, args } => tool_call_message(tool, &args),
                    Turn::WriteWithObservedRev {
                        path,
                        content,
                        delta,
                    } => {
                        let rev = match observed_rev(&body) {
                            Some(rev) => (rev as i64 + delta).max(0) as u64,
                            None => UNOBSERVED_REV,
                        };
                        tool_call_message(
                            "workspace_write",
                            &json!({
                                "path": path,
                                "content": content,
                                "expected_updated_at": rev,
                            }),
                        )
                    }
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

/// One assistant message carrying a native `tool_calls` array — the shape the
/// provider's `tool_calling: true` profile puts the turn loop on.
pub(crate) fn tool_call_message(tool: &str, args: &Value) -> Value {
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
            "id": format!("call-{tool}"),
            "type": "function",
            "function": { "name": tool, "arguments": args.to_string() }
        }]
    })
}

pub(crate) fn folder(id: &str, name: &str) -> WorkspaceNode {
    WorkspaceNode {
        id: id.to_string(),
        name: name.to_string(),
        kind: NodeKind::Folder,
        parent_id: None,
        updated_at_millis: crate::ports::now_millis(),
        created_by: WorkspaceOrigin::Operator,
        updated_by: WorkspaceOrigin::Operator,
        mime: None,
        size: None,
        sha256: None,
        adopted: false,
    }
}

pub(crate) fn note(id: &str, name: &str, parent: &str) -> WorkspaceNode {
    WorkspaceNode {
        id: id.to_string(),
        name: name.to_string(),
        kind: NodeKind::File,
        parent_id: Some(parent.to_string()),
        updated_at_millis: crate::ports::now_millis(),
        created_by: WorkspaceOrigin::Operator,
        updated_by: WorkspaceOrigin::Operator,
        mime: None,
        size: None,
        sha256: None,
        adopted: false,
    }
}

/// A one-agent company, with `grants` controlling the workspace surface.
pub(crate) fn manifest(grants: &str) -> CompanyManifest {
    // `full` so an ordinary turn is not parked; the write tool's own
    // compare-and-swap token is what guards the write in this mode.
    manifest_in_mode(grants, "full")
}

pub(crate) fn manifest_in_mode(grants: &str, mode: &str) -> CompanyManifest {
    toml::from_str(&format!(
        r#"
[company]
name = "Acme"

[policy]
mode = "{mode}"

[tools]
allow = [{grants}]

[[agent]]
id = "ceo"
role = "Chief Executive"
tier = "orchestrator"
"#
    ))
    .expect("manifest parses")
}

/// Wire a real harness against the scripted endpoint and a seeded workspace.
///
/// Returns the pool, deps, record and the live store so a test can read back
/// what the turn actually persisted.
pub(crate) async fn harness(
    base_url: String,
    grants: &str,
    dir: &std::path::Path,
) -> (
    HarnessPool,
    HarnessDeps,
    CompanyRecord,
    Arc<dyn WorkspaceStore>,
) {
    let store: Arc<dyn WorkspaceStore> = Arc::new(FsOps::new(dir));
    // A fresh id per test: every turn test in this binary runs on the one
    // process-wide OpenHuman runtime, and an agent's thread transcript is
    // keyed by `(company, agent)` — two fixtures naming `acme`/`ceo` would
    // resume each other's transcript, system prompt included. Per test rather
    // than per call so the supervised suite's second record agrees with it.
    let id = crate::test_support::per_test_company_id("acme");
    store
        .create(&id, &folder("f-std", "standards"), None)
        .await
        .unwrap();
    store
        .create(
            &id,
            &note("n-eng", "engineering-standards.md", "f-std"),
            Some("# Engineering\nReview every PR before merge."),
        )
        .await
        .unwrap();

    let deps = HarnessDeps {
        emergency_gate: None,
        notifications: None,
        ledgers: None,
        ledger_registry: Default::default(),
        provider: Arc::new(HostedProvider::new(HostedProviderConfig {
            base_url,
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
        composio: None,
        #[cfg(feature = "chargebee")]
        chargebee: None,
        #[cfg(feature = "paypal")]
        paypal: None,
        hosting: None,
        steer: crate::company::steer::InflightRegistry::default(),
        run_supervisor: crate::runtime::RunSupervisor::default(),
        delivery: None,
        workspace: Some(store.clone()),
        // Issue #238's metered search is off in this fixture: the turn under
        // test exercises the #237 workspace path only, and no managed search
        // backend is the fail-closed default outside the runtime builder.
        search: None,
        tenant_search: None,
        workflow_runs: None,
        deep_trace: None,
    };

    let record = CompanyRecord {
        overlay_desk_hive: Vec::new(),
        overlay_retired_agents: Vec::new(),
        overlay_agent_edits: Vec::new(),
        id,
        manifest: manifest(grants),
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
    (pool, deps, record, store)
}

/// Every tool name the scripted model was offered across the whole turn.
pub(crate) fn advertised_tools(script: &Script) -> Vec<String> {
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
    // Plan hive-desks Phase 3: this crate's own tools reach the model as the
    // `opencompany` MCP catalogue, named in the system prompt and called
    // through `mcp_call_tool`, so "advertised" reads both halves.
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

/// Every tool *result* the harness fed back to the model.
pub(crate) fn tool_results(script: &Script) -> Vec<String> {
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
