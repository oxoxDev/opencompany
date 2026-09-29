use std::sync::Arc;

use serde_json::{Value, json};

use super::tools::{
    AcceptedCell, CheckWorkflowTool, CopilotContext, DiagCell, ListEffectiveToolsTool,
    ProposeWorkflowTool,
};
use super::workflow_build_fixtures_tests::*;
use super::workflow_build_shared_tests::*;
use super::*;
use tinytools::Tool;

// ---------------------------------------------------------------------------
// Create-time copilot (issue #753)
// ---------------------------------------------------------------------------

/// A drafter answer whose graph carries a model-chosen id and per-node approval
/// gating — both of which the host overrides — plus a real roster agent.
pub(super) const DESC_GRAPH: &str = r#"{"automatable":true,"summary":"email the weekly digest",
    "workflow":{"id":"the-model-should-not-pick-this","name":"Weekly digest",
        "nodes":[{"id":"start","kind":"trigger","name":"Every Monday","schedule":"0 9 * * 1","requires_approval":true},
                 {"id":"draft","kind":"agent","name":"Draft","agent":"maya","requires_approval":false}],
        "edges":[{"from":"start","to":"draft"}]}}"#;

// Seeds a real overlay workflow through the create path, so the drafter's host

// ---------------------------------------------------------------------------
// The three copilot tools — unit tier (issue #840)
// ---------------------------------------------------------------------------

/// Builds a shared [`CopilotContext`] over the fixture company for the tool unit
/// tests: the gathered evidence, the operator's description, and explicit
/// effective / granted-but-unwired slug sets.
async fn copilot_ctx(
    runtime: &Arc<CompanyRuntime>,
    description: &str,
    effective: &[&str],
    unwired: &[&str],
) -> Arc<CopilotContext> {
    let company = gather_company_evidence(runtime).await.expect("evidence");
    Arc::new(CopilotContext {
        company,
        description: description.to_string(),
        effective_slugs: effective.iter().map(|s| s.to_string()).collect(),
        unwired_slugs: unwired.iter().map(|s| s.to_string()).collect(),
        fixing: None,
    })
}

/// A minimal runtime just to gather company evidence for the tool units — a plain
/// tool-less model is fine, the tools never call it.
async fn evidence_runtime() -> (tempfile::TempDir, Arc<CompanyRuntime>) {
    runtime_with(ScriptedModel::replying(VALID_GRAPH)).await
}

/// `list_effective_tools` names the roster ids, each wired slug with its honest
/// capability + required args, and the granted-but-unwired ones under a "do not
/// author these" heading — PR-1's data, straight from the shared context.
#[tokio::test]
async fn list_effective_tools_reports_roster_wired_and_unwired() {
    let (_home, runtime) = evidence_runtime().await;
    let ctx = copilot_ctx(&runtime, "do the thing", &["web_fetch"], &["csv_export"]).await;
    let tool = ListEffectiveToolsTool::new(ctx);
    let out = tool.execute(json!({})).await.expect("runs").text();

    assert!(out.contains("`maya`"), "the roster id is named: {out}");
    assert!(
        out.contains("`web_fetch`"),
        "the wired slug is named: {out}"
    );
    assert!(
        out.contains("(args: url)"),
        "web_fetch's required arg is named: {out}"
    );
    assert!(
        out.contains("granted but not wired") || out.contains("not wired"),
        "the unwired heading is present: {out}"
    );
    assert!(
        out.contains("csv_export"),
        "the unwired slug is named: {out}"
    );
}

/// `check_workflow` surfaces `gates::failures`: it flags an agent step reading an
/// upstream agent's output WITHOUT the `.json` envelope (resolves to null at run
/// time), and an agent step whose instruction is a `=`-expression (runs empty),
/// while a clean graph passes. Both problems it names are recorded for the
/// caller's fallback reason.
#[tokio::test]
async fn check_workflow_flags_gate_failures_and_passes_a_clean_graph() {
    let (_home, runtime) = evidence_runtime().await;
    let diag: DiagCell = Arc::new(StdMutex::new(Vec::new()));
    let ctx = copilot_ctx(&runtime, "summarize the week", &[], &[]).await;
    let tool = CheckWorkflowTool::new(ctx, diag.clone());

    // (1) An envelope-null binding: `b` reads `=nodes.a.item.summary` from agent
    // `a`, whose output is enveloped — the `.json` is missing, so it resolves to
    // null. `gates::failures` catches exactly this.
    let envelope_null = json!({
        "name": "Digest",
        "nodes": [
            { "id": "t", "kind": "trigger", "name": "Start" },
            { "id": "a", "kind": "agent", "name": "Draft", "agent": "maya" },
            { "id": "b", "kind": "agent", "name": "Polish", "agent": "maya",
              "config": { "input": "=nodes.a.item.summary" } }
        ],
        "edges": [{ "from": "t", "to": "a" }, { "from": "a", "to": "b" }]
    });
    let out = tool
        .execute(json!({ "workflow": envelope_null }))
        .await
        .expect("runs")
        .text();
    assert!(
        out.contains("null") && out.contains("json"),
        "the envelope-null binding is flagged: {out}"
    );
    assert!(
        !diag.lock().unwrap().is_empty(),
        "the problems are recorded for the caller's fallback"
    );

    // (2) A prose-as-expression prompt: the instruction reads as a `=`-jq program
    // and resolves to null, so the step runs with an empty prompt.
    let prose_prompt = json!({
        "name": "Digest",
        "nodes": [
            { "id": "t", "kind": "trigger", "name": "Start" },
            { "id": "a", "kind": "agent", "name": "Draft", "agent": "maya",
              "config": { "prompt": "=Summarize the week and include the highlights" } }
        ],
        "edges": [{ "from": "t", "to": "a" }]
    });
    let out = tool
        .execute(json!({ "workflow": prose_prompt }))
        .await
        .expect("runs")
        .text();
    assert!(
        out.contains("empty prompt") || out.contains("resolves to null"),
        "the prose-as-expression prompt is flagged: {out}"
    );

    // (3) A clean graph passes.
    let clean = json!({
        "name": "Digest",
        "nodes": [
            { "id": "t", "kind": "trigger", "name": "Start" },
            { "id": "a", "kind": "agent", "name": "Draft", "agent": "maya" }
        ],
        "edges": [{ "from": "t", "to": "a" }]
    });
    let out = tool
        .execute(json!({ "workflow": clean }))
        .await
        .expect("runs")
        .text();
    assert!(out.contains("passes"), "a clean graph passes: {out}");
    assert!(
        diag.lock().unwrap().is_empty(),
        "a passing check clears the recorded problems"
    );
}

// NOTE ON SCOPE: the `gates::failures` "bad code language" arm is deliberately
// NOT exercised here — OpenCompany's authoring model (`WorkflowNodeKind`) has no
// `code` node kind, so a spec can never translate into a tinyflows `Code` node
// through OC's create pipeline. The two reachable gate classes above
// (envelope-null bindings, prose-as-expression prompts) prove `check_workflow`
// runs `gates::failures` and surfaces its findings.

/// `propose_company_workflow` accepts a good spec — running the SAME host authority the
/// old inline path did (a host-minted id, name dedup, stripped approval gating) —
/// and stashes `(summary, spec, notes)` in the shared cell.
#[tokio::test]
async fn propose_company_workflow_accepts_a_good_spec_under_host_authority() {
    let (_home, runtime) = evidence_runtime().await;
    seed_workflow(&runtime, "weekly-digest", "Weekly digest").await;
    let ctx = copilot_ctx(&runtime, "email the weekly digest every Monday", &[], &[]).await;
    let accepted: AcceptedCell = Arc::new(StdMutex::new(None));
    let diag: DiagCell = Arc::new(StdMutex::new(Vec::new()));
    let tool = ProposeWorkflowTool::new(ctx, accepted.clone(), diag.clone());

    let good = json!({
        "summary": "email the weekly digest",
        "workflow": {
            "name": "Weekly digest",
            "description": "Draft and send the weekly digest.",
            "nodes": [
                { "id": "start", "kind": "trigger", "name": "Every Monday", "schedule": "0 9 * * 1", "requires_approval": true },
                { "id": "draft", "kind": "agent", "name": "Draft", "agent": "maya", "requires_approval": false }
            ],
            "edges": [{ "from": "start", "to": "draft" }]
        }
    });
    let out = tool.execute(good).await.expect("runs").text();
    assert!(
        out.contains("Accepted"),
        "the tool reports acceptance: {out}"
    );

    let proposal = accepted.lock().unwrap().take().expect("a proposal landed");
    assert_eq!(proposal.summary, "email the weekly digest");
    // The host mints the id and dedups it (+ the name) against the seed.
    assert_eq!(proposal.spec.id, "weekly-digest-2");
    assert_eq!(proposal.spec.name, "Weekly digest 2");
    // Approval gating is the host's — both `true` and `false` are stripped.
    assert!(
        proposal
            .spec
            .nodes
            .iter()
            .all(|n| n.requires_approval.is_none())
    );
    // The schedule the model authored survives.
    assert_eq!(
        proposal.spec.nodes[0].schedule.as_deref(),
        Some("0 9 * * 1")
    );
    assert!(diag.lock().unwrap().is_empty());
}

/// `propose_company_workflow` rejects via each of the three host gates — the node-kind
/// refusal, `ground_and_validate` (an unknown agent), and `courtesy_validate_draft`
/// (an ungranted `tool_call`) — never stashing a proposal, and recording the
/// sentence for the caller's fallback.
#[tokio::test]
async fn propose_company_workflow_rejects_via_each_host_gate() {
    let (_home, runtime) = evidence_runtime().await;

    async fn propose(runtime: &Arc<CompanyRuntime>, description: &str, workflow: Value) -> String {
        let ctx = copilot_ctx(runtime, description, &[], &[]).await;
        let accepted: AcceptedCell = Arc::new(StdMutex::new(None));
        let diag: DiagCell = Arc::new(StdMutex::new(Vec::new()));
        let tool = ProposeWorkflowTool::new(ctx, accepted.clone(), diag.clone());
        let out = tool
            .execute(json!({ "summary": "x", "workflow": workflow }))
            .await
            .expect("runs")
            .text();
        assert!(
            accepted.lock().unwrap().is_none(),
            "a rejected proposal is never stashed"
        );
        assert!(
            !diag.lock().unwrap().is_empty(),
            "the gate sentence is recorded for the fallback"
        );
        out
    }

    // (1) Node-kind gate: `http_request` is outside DESCRIPTION_NODE_KINDS.
    let out = propose(
        &runtime,
        "call a url",
        json!({
            "name": "Reach out",
            "nodes": [
                { "id": "start", "kind": "trigger", "name": "Start" },
                { "id": "call", "kind": "http_request", "name": "Call",
                  "config": { "url": "http://x.example/y", "method": "GET" } }
            ],
            "edges": [{ "from": "start", "to": "call" }]
        }),
    )
    .await;
    assert!(
        out.contains("http_request"),
        "the kind gate names it: {out}"
    );

    // (2) Grounding gate: an `agent` node naming a teammate not on the roster.
    let out = propose(
        &runtime,
        "do it",
        json!({
            "name": "Ghosted",
            "nodes": [
                { "id": "start", "kind": "trigger", "name": "Start" },
                { "id": "a", "kind": "agent", "name": "Do", "agent": "ghost" }
            ],
            "edges": [{ "from": "start", "to": "a" }]
        }),
    )
    .await;
    assert!(
        out.contains("maya"),
        "the grounding gate names the roster: {out}"
    );

    // (3) Courtesy gate: a `tool_call` whose namespace the fixture never granted
    // (`code` — the manifest grants `docs`/`web`), refused by courtesy validation.
    let out = propose(
        &runtime,
        "export the rows",
        json!({
            "name": "Exporter",
            "nodes": [
                { "id": "start", "kind": "trigger", "name": "Start" },
                { "id": "exp", "kind": "tool_call", "name": "Export", "config": { "slug": "csv_export" } }
            ],
            "edges": [{ "from": "start", "to": "exp" }]
        }),
    )
    .await;
    assert!(
        out.contains("does not grant") || out.contains("csv_export"),
        "the courtesy gate refuses the ungranted tool: {out}"
    );
}
