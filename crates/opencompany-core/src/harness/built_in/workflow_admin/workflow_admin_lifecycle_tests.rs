use std::sync::Arc;

use serde_json::json;

use super::workflow_admin_fixtures_tests::*;
use super::*;
use crate::company::{CompanyManifest, update_company_workflow};
use crate::ports::types::{CompanyEvent, CompanyRecord};

// ---------------------------------------------------------------------------
// 5. delete
// ---------------------------------------------------------------------------

/// A delete takes the body, the enabled id and the revision history, journals
/// what went, and names it.
#[tokio::test]
async fn delete_removes_the_body_the_enabled_id_and_the_history() {
    let fx = Fixture::new();
    let store: Arc<dyn CompanyStore> = fx.store.clone();
    let revisions: Arc<dyn WorkflowRevisionStore> = fx.revisions.clone();
    let draft = crate::company::RawWorkflow::try_from(
        serde_json::from_value::<CreateWorkflowArgs>(graph_args("greeter", "Greeter", "Worker"))
            .unwrap(),
    )
    .unwrap();
    crate::company::create_company_workflow(
        &fx.company,
        Some(fx.source_dir()),
        &store,
        None,
        draft,
        None,
        None,
    )
    .await
    .expect("creates");
    // Give it a history to cascade.
    let edited = crate::company::RawWorkflow::try_from(
        serde_json::from_value::<CreateWorkflowArgs>(graph_args("greeter", "Greeter", "Second"))
            .unwrap(),
    )
    .unwrap();
    update_company_workflow(
        &fx.company,
        Some(fx.source_dir()),
        &store,
        &revisions,
        None,
        edited,
        None,
        None,
    )
    .await
    .expect("edits");
    assert_eq!(
        fx.revisions
            .list_revisions(&fx.company, "greeter")
            .await
            .unwrap()
            .len(),
        1
    );

    let result = DeleteWorkflowTool::new(fx.admin())
        .execute(json!({ "id": "greeter" }))
        .await
        .unwrap();
    assert!(!result.is_error, "{}", err_text(&result));
    assert!(
        md(&result).contains("Greeter"),
        "names what went: {}",
        md(&result)
    );
    assert!(md(&result).contains("cannot be undone"), "{}", md(&result));

    assert!(fx.overlays().await.is_empty(), "body gone");
    assert!(
        !fx.enabled().await.contains(&"greeter".to_string()),
        "enabled id gone"
    );
    assert!(
        fx.revisions
            .list_revisions(&fx.company, "greeter")
            .await
            .unwrap()
            .is_empty(),
        "history cascaded"
    );
    assert!(
        fx.events()
            .iter()
            .any(|e| matches!(e, CompanyEvent::WorkflowDeleted { name, .. } if name == "Greeter")),
        "{:?}",
        fx.events()
    );
}

// ---------------------------------------------------------------------------
// 6. unknown / bodiless / corrupt ids
// ---------------------------------------------------------------------------

/// An id nobody answers for gets `run_workflow`'s steer, not a raw error.
#[tokio::test]
async fn an_unknown_id_steers_to_the_workflows_list() {
    let fx = Fixture::new();
    let read = ReadWorkflowTool::new(fx.admin())
        .execute(json!({ "id": "nope" }))
        .await
        .unwrap();
    assert!(
        err_text(&read).contains("Check the workflows list"),
        "{}",
        err_text(&read)
    );

    let deleted = DeleteWorkflowTool::new(fx.admin())
        .execute(json!({ "id": "nope" }))
        .await
        .unwrap();
    assert!(
        err_text(&deleted).contains("check the workflows list"),
        "{}",
        err_text(&deleted)
    );
}

/// A stored body that no longer parses still answers a read — with its token
/// and its editable flag — so an unreadable workflow does not become an
/// unremovable one.
#[tokio::test]
async fn a_corrupt_body_still_reads_a_version_and_still_deletes() {
    let fx = Fixture::new();
    fx.put_overlay("broken", "this is not = valid toml [[[")
        .await;

    let read = ReadWorkflowTool::new(fx.admin())
        .execute(json!({ "id": "broken" }))
        .await
        .unwrap();
    let payload = data(&read);
    assert_eq!(payload["readable"], json!(false));
    assert_eq!(payload["editable"], json!(true));
    assert!(payload["version"].is_string(), "{payload}");
    assert!(md(&read).contains(DELETE_WORKFLOW_TOOL), "{}", md(&read));

    let deleted = DeleteWorkflowTool::new(fx.admin())
        .execute(json!({ "id": "broken", "expected_version": payload["version"] }))
        .await
        .unwrap();
    assert!(!deleted.is_error, "{}", err_text(&deleted));
    assert!(fx.overlays().await.is_empty());
}

// ---------------------------------------------------------------------------
// 7. the #682 non-weakening pin — THE test that matters most
// ---------------------------------------------------------------------------

/// An agent edit is held to the console's own author-time validation (#682).
///
/// This is the constraint the whole issue is about: a second authoring surface
/// that skipped per-kind config enforcement would reopen exactly the hole #661
/// exists to close. Both shapes below are ones the pre-#682 code accepted and
/// that fail silently at run time.
#[tokio::test]
async fn an_agent_edit_cannot_bypass_the_per_kind_config_rules() {
    let fx = Fixture::new();
    let store: Arc<dyn CompanyStore> = fx.store.clone();
    let draft = crate::company::RawWorkflow::try_from(
        serde_json::from_value::<CreateWorkflowArgs>(graph_args("greeter", "Greeter", "Worker"))
            .unwrap(),
    )
    .unwrap();
    crate::company::create_company_workflow(
        &fx.company,
        Some(fx.source_dir()),
        &store,
        None,
        draft,
        None,
        None,
    )
    .await
    .expect("creates");
    let version = crate::company::workflow_version(&fx.overlays().await[0].toml);

    // A `condition` node with no `field` — the shape 19 shipped seeds carried
    // before #682 repaired them.
    let fieldless = json!({
        "id": "greeter",
        "name": "Greeter",
        "expected_version": version,
        "nodes": [
            { "id": "start", "kind": "trigger", "name": "Start" },
            { "id": "check", "kind": "condition", "name": "Check" },
            { "id": "done", "kind": "output", "name": "Report" }
        ],
        "edges": [
            { "from": "start", "to": "check" },
            { "from": "check", "to": "done", "label": "yes" }
        ]
    });
    let result = UpdateWorkflowTool::new(fx.admin())
        .execute(fieldless)
        .await
        .unwrap();
    let text = err_text(&result);
    assert!(text.contains("field"), "{text}");

    // A `tool_call` node with no `slug`.
    let slugless = json!({
        "id": "greeter",
        "name": "Greeter",
        "expected_version": version,
        "nodes": [
            { "id": "start", "kind": "trigger", "name": "Start" },
            { "id": "call", "kind": "tool_call", "name": "Call", "config": { "args": {} } },
            { "id": "done", "kind": "output", "name": "Report" }
        ],
        "edges": [
            { "from": "start", "to": "call" },
            { "from": "call", "to": "done" }
        ]
    });
    let result = UpdateWorkflowTool::new(fx.admin())
        .execute(slugless)
        .await
        .unwrap();
    assert!(err_text(&result).contains("slug"), "{}", err_text(&result));

    // Neither reached the record.
    let reloaded =
        crate::company::load_workflow_union(Some(fx.source_dir()), &fx.overlays().await, "greeter")
            .unwrap()
            .unwrap();
    assert_eq!(reloaded.nodes[1].name, "Worker", "the original survived");
}

// ---------------------------------------------------------------------------
// 8. deployment without a revision store
// ---------------------------------------------------------------------------

/// With no revision store, the writes refuse rather than write with no undo.
#[tokio::test]
async fn without_a_revision_store_the_writes_refuse_rather_than_lose_the_prior_body() {
    let fx = Fixture::new();
    fx.put_overlay("greeter", SEED_TOML).await;

    let mut graph = graph_args("greeter", "Greeter", "Worker");
    graph["expected_version"] = json!("whatever");
    let updated = UpdateWorkflowTool::new(fx.admin_without_revisions())
        .execute(graph)
        .await
        .unwrap();
    assert!(
        err_text(&updated).contains("isn't available on this deployment"),
        "{}",
        err_text(&updated)
    );

    let deleted = DeleteWorkflowTool::new(fx.admin_without_revisions())
        .execute(json!({ "id": "greeter" }))
        .await
        .unwrap();
    assert!(err_text(&deleted).contains("isn't available on this deployment"));
    assert_eq!(fx.overlays().await.len(), 1, "nothing was touched");
}

// ---------------------------------------------------------------------------
// 9. policy
// ---------------------------------------------------------------------------

/// The gating split, asserted at the classifier rather than read off the table.
#[test]
fn only_the_delete_parks_and_none_of_the_three_is_ever_grantable() {
    use crate::policy::consequence::consequence_of;
    let args = json!({});

    let read = consequence_of(READ_WORKFLOW_TOOL, &args);
    let update = consequence_of(UPDATE_WORKFLOW_TOOL, &args);
    let delete = consequence_of(DELETE_WORKFLOW_TOOL, &args);

    // Reads and edits run; the removal parks, under BOTH tiers.
    assert!(!read.reach.parks_under_supervision());
    assert!(!update.reach.parks_under_supervision());
    assert!(delete.reach.parks_under_supervision());
    assert!(delete.parks_under_auto());
    assert!(!read.parks_under_auto());
    assert!(!update.parks_under_auto());

    // `readonly` denies the removal and permits the other two.
    assert!(delete.reach.denied_under_readonly());
    assert!(!read.reach.denied_under_readonly());
    assert!(!update.reach.denied_under_readonly());

    // None of the three may be granted standing: a week-long licence to delete
    // workflows is not a sentence an operator can consent to, and the other two
    // never park so a standing grant on them would be unobservable.
    for tool in [
        READ_WORKFLOW_TOOL,
        UPDATE_WORKFLOW_TOOL,
        DELETE_WORKFLOW_TOOL,
    ] {
        assert!(
            !consequence_of(tool, &args).standing.is_grantable(),
            "{tool} must not be grantable"
        );
    }
}

// ---------------------------------------------------------------------------
// 10. the descriptions carry the routing
// ---------------------------------------------------------------------------

/// The three descriptions are the only place a model learns the read-first
/// contract and the permanence of the delete, so the words are pinned.
#[test]
fn the_descriptions_route_the_model_and_name_the_contract() {
    let fx = Fixture::new();
    let read = ReadWorkflowTool::new(fx.admin());
    let update = UpdateWorkflowTool::new(fx.admin());
    let delete = DeleteWorkflowTool::new(fx.admin());

    // The read is routed away from the two tools it is easily confused with.
    assert!(read.description().contains("query_company"));
    assert!(read.description().contains("run_workflow"));
    assert!(read.description().contains("update_workflow"));

    // The update names the read-first contract and the required token.
    assert!(update.description().contains("read_workflow"));
    assert!(update.description().contains("expected_version"));
    assert!(update.description().contains("REQUIRED"));
    assert!(update.description().contains("full replacement"));

    // The delete leads with permanence, and points fixes at the update.
    assert!(delete.description().contains("CANNOT be undone"));
    assert!(delete.description().contains("update_workflow"));

    // The update advertises the same graph shape it deserializes.
    let schema = update.parameters_schema();
    let required: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(required.contains(&"expected_version"), "{schema}");
    assert!(schema["properties"]["nodes"].is_object(), "{schema}");
    assert!(schema["properties"]["edges"].is_object(), "{schema}");
}

// ---------------------------------------------------------------------------
// 11. one record load, not two (globals opt-out)
// ---------------------------------------------------------------------------

/// A company that disabled a global workflow must never see it through
/// `read_workflow`, even when a *second* read of the record — which the tool
/// must not perform — would fail.
///
/// Before the fix, `read_workflow`'s fallback path re-loaded the record just
/// to fetch `[globals].disable`, and turned a failed second load into an
/// empty (i.e. nothing-disabled) list. With
/// [`FailsAfterFirstLoadStore`] failing every load after the first, that bug
/// would surface the disabled global's graph as a success; the fix reads the
/// overlay body and the disable list from one load, so only one `load` call
/// ever happens and the global stays hidden.
#[tokio::test]
async fn a_disabled_global_stays_hidden_even_if_a_second_read_would_fail() {
    let dropped = crate::globals::workflows()[0].id.clone();
    let company = CompanyId::new("acme");
    let manifest: CompanyManifest = toml::from_str(&format!(
        "[company]\nname = \"Acme\"\n[[agent]]\nid = \"assistant\"\nrole = \"Assistant\"\n\n\
         [globals]\ndisable = [\"workflow:{dropped}\"]\n"
    ))
    .expect("valid manifest");
    let record = CompanyRecord {
        general_channel: Default::default(),
        overlay_desk_hive: Vec::new(),
        overlay_retired_agents: Vec::new(),
        overlay_agent_edits: Vec::new(),
        id: company.clone(),
        manifest,
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
    let store: Arc<dyn CompanyStore> = Arc::new(FailsAfterFirstLoadStore::seeded(record));
    let admin = WorkflowAdmin::new(company, None, store, None, None);
    let read = ReadWorkflowTool::new(admin);

    let result = read
        .execute(json!({ "id": dropped }))
        .await
        .expect("tool call succeeds");
    assert!(
        result.is_error,
        "a company-disabled global must stay hidden, not succeed with its graph: {result:?}"
    );
    assert!(
        err_text(&result).contains("No workflow"),
        "{}",
        err_text(&result)
    );
}

/// A graph well under [`GRAPH_RENDER_BUDGET_BYTES`] renders pretty-printed —
/// the cheap, common case.
#[test]
fn render_graph_pretty_prints_a_small_graph() {
    let spec = json!({"nodes": [{"id": "n1"}], "edges": []});
    let rendered = render_graph(&spec);
    assert!(rendered.starts_with("```json\n"));
    assert!(rendered.contains("\"nodes\""));
    assert!(rendered.contains('\n'), "pretty output must be multi-line");
}

/// A graph over the pretty-printed budget but under the compact one falls
/// back to compact JSON rather than refusing.
#[test]
fn render_graph_falls_back_to_compact_before_refusing() {
    // Many short keys: pretty-printing's per-field newline/indent overhead
    // pushes this over budget while the compact form (no whitespace) stays
    // under it.
    let mut nodes = Vec::new();
    for i in 0..(GRAPH_RENDER_BUDGET_BYTES / 20) {
        nodes.push(json!({"id": format!("n{i}")}));
    }
    let spec = json!({ "nodes": nodes });
    let pretty_len = serde_json::to_string_pretty(&spec).unwrap().len();
    let compact_len = spec.to_string().len();
    assert!(
        pretty_len > GRAPH_RENDER_BUDGET_BYTES,
        "fixture must actually exceed the pretty budget: {pretty_len}"
    );
    assert!(
        compact_len <= GRAPH_RENDER_BUDGET_BYTES,
        "fixture must fit compact for this test to prove the fallback: {compact_len}"
    );

    let rendered = render_graph(&spec);
    assert!(rendered.starts_with("```json\n"));
    assert!(
        !rendered.contains("  "),
        "the compact fallback must carry no pretty-printer indentation"
    );
}

/// Past both budgets, `render_graph` refuses rather than quoting a
/// half-truncated fence the agent would hand back as an "edit".
#[test]
fn render_graph_refuses_a_graph_that_exceeds_both_budgets() {
    let huge_id = "n".repeat(GRAPH_RENDER_BUDGET_BYTES + 1_000);
    let spec = json!({ "nodes": [{"id": huge_id}] });
    let compact_len = spec.to_string().len();
    assert!(
        compact_len > GRAPH_RENDER_BUDGET_BYTES,
        "fixture must exceed even the compact budget: {compact_len}"
    );

    let rendered = render_graph(&spec);
    assert!(
        !rendered.starts_with("```json"),
        "an over-budget graph must not be quoted at all: {rendered}"
    );
    assert!(rendered.contains("too large"));
    assert!(rendered.contains("console"));
}
