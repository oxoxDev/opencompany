use super::*;

use crate::company::CompanyManifest;
use crate::error::Result as OcResult;
use crate::ports::types::{CompanyRecord, CompanySummary, LedgerEntry};

/// An in-memory `CompanyStore` holding one record, so the resolver's overlay
/// half can be seeded without a real backend.
struct MemStore(std::sync::Mutex<Option<CompanyRecord>>);

#[async_trait]
impl CompanyStore for MemStore {
    async fn load(&self, _id: &CompanyId) -> OcResult<Option<CompanyRecord>> {
        Ok(self.0.lock().unwrap().clone())
    }
    async fn save(&self, record: &CompanyRecord) -> OcResult<()> {
        *self.0.lock().unwrap() = Some(record.clone());
        Ok(())
    }
    async fn list(&self) -> OcResult<Vec<CompanySummary>> {
        Ok(Vec::new())
    }
    async fn append_ledger(&self, _id: &CompanyId, _entry: LedgerEntry) -> OcResult<()> {
        Ok(())
    }
}

/// A store whose record carries `overlays` and a `[globals].disable` list —
/// [`store_with`] with the disable list always empty.
fn store_with_globals_disable(
    overlays: Vec<OverlayWorkflow>,
    disable: Vec<String>,
) -> Arc<dyn CompanyStore> {
    let entries = disable
        .iter()
        .map(|d| format!("\"{d}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let manifest: CompanyManifest = toml::from_str(&format!(
        "[company]\nname = \"Acme\"\n\n[globals]\ndisable = [{entries}]\n"
    ))
    .expect("valid manifest");
    Arc::new(MemStore(std::sync::Mutex::new(Some(CompanyRecord {
        general_channel: Default::default(),
        overlay_desk_hive: Vec::new(),
        overlay_retired_agents: Vec::new(),
        overlay_agent_edits: Vec::new(),
        id: CompanyId::new("acme"),
        manifest,
        ledger: Vec::new(),
        lifecycle: "running".to_string(),
        overlay_agents: Vec::new(),
        overlay_desk_members: Vec::new(),
        overlay_desk_order: Vec::new(),
        overlay_desks: Vec::new(),
        overlay_workflows: overlays,
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
    }))))
}

/// A store whose record carries `overlays` as its runtime-authored graphs.
pub(super) fn store_with(overlays: Vec<OverlayWorkflow>) -> Arc<dyn CompanyStore> {
    let manifest: CompanyManifest =
        toml::from_str("[company]\nname = \"Acme\"\n").expect("valid manifest");
    Arc::new(MemStore(std::sync::Mutex::new(Some(CompanyRecord {
        general_channel: Default::default(),
        overlay_desk_hive: Vec::new(),
        overlay_retired_agents: Vec::new(),
        overlay_agent_edits: Vec::new(),
        id: CompanyId::new("acme"),
        manifest,
        ledger: Vec::new(),
        lifecycle: "running".to_string(),
        overlay_agents: Vec::new(),
        overlay_desk_members: Vec::new(),
        overlay_desk_order: Vec::new(),
        overlay_desks: Vec::new(),
        overlay_workflows: overlays,
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
    }))))
}

/// A resolver over a seed directory only (no runtime-authored graphs).
fn seed_resolver(dir: &std::path::Path, root_id: &str) -> StoreWorkflowResolver {
    StoreWorkflowResolver::new(
        Some(dir.to_path_buf()),
        store_with(Vec::new()),
        CompanyId::new("acme"),
        root_id.to_string(),
        None,
    )
}

/// A resolver with NO seed directory, serving only overlay bodies — the
/// hosted shape (issue #168).
pub(super) fn overlay_resolver(
    overlays: Vec<OverlayWorkflow>,
    root_id: &str,
) -> StoreWorkflowResolver {
    StoreWorkflowResolver::new(
        None,
        store_with(overlays),
        CompanyId::new("acme"),
        root_id.to_string(),
        None,
    )
}

pub(super) fn overlay(id: &str, toml: String) -> OverlayWorkflow {
    OverlayWorkflow {
        id: id.to_string(),
        toml,
    }
}

/// Writes `src` to `<dir>/workflows/<id>.toml`.
fn write_wf(dir: &std::path::Path, id: &str, src: &str) {
    let workflows = dir.join("workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    std::fs::write(workflows.join(format!("{id}.toml")), src).unwrap();
}

/// A minimal valid child graph body (trigger → output).
fn leaf(id: &str) -> String {
    format!(
        r#"
id = "{id}"
name = "{id}"
[[node]]
id = "start"
kind = "trigger"
name = "Start"
[[node]]
id = "done"
kind = "output"
name = "Done"
[[edge]]
from = "start"
to = "done"
"#
    )
}

/// A graph that runs `child_id` as a sub_workflow.
pub(super) fn parent_of(id: &str, child_id: &str) -> String {
    format!(
        r#"
id = "{id}"
name = "{id}"
[[node]]
id = "start"
kind = "trigger"
name = "Start"
[[node]]
id = "sub"
kind = "sub_workflow"
name = "Sub"
[node.config]
workflow_id = "{child_id}"
[[edge]]
from = "start"
to = "sub"
"#
    )
}

#[tokio::test]
async fn resolves_a_saved_child_into_a_compilable_graph() {
    let dir = tempfile::tempdir().unwrap();
    write_wf(dir.path(), "child", &leaf("child"));
    let resolver = seed_resolver(dir.path(), "root");

    let graph = resolver.resolve("child").await.expect("resolves");
    assert_eq!(graph.id.as_deref(), Some("child"));
    // The resolved child is a graph the engine accepts.
    tinyflows::compiler::compile(&graph).expect("resolved child compiles");
}

#[tokio::test]
async fn unknown_child_is_a_capability_error() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = seed_resolver(dir.path(), "root");
    let err = resolver.resolve("ghost").await.unwrap_err();
    assert!(err.to_string().contains("ghost"), "{err}");
}

/// A global workflow the company disabled via `[globals].disable` must
/// fail resolution as a `sub_workflow` child, exactly like an unknown id —
/// the same contract `crate::globals::test`'s
/// `a_disabled_global_workflow_neither_lists_nor_loads` pins at the
/// `load_workflow_with_globals` layer this resolver calls into.
#[tokio::test]
async fn a_company_disabled_global_child_fails_resolution() {
    let dropped = crate::globals::workflows()[0].id.clone();
    let store = store_with_globals_disable(Vec::new(), vec![format!("workflow:{dropped}")]);
    let resolver = StoreWorkflowResolver::new(
        None,
        store,
        CompanyId::new("acme"),
        "root".to_string(),
        None,
    );

    let err = resolver.resolve(&dropped).await.unwrap_err();
    assert!(
        err.to_string().contains(&dropped),
        "the error names the disabled child: {err}"
    );
    assert!(
        err.to_string().contains("not a saved workflow"),
        "a disabled global reads the same as an unknown id, not a cycle or a parse error: {err}"
    );
}

/// A disabled global is excluded from the cycle scan the same way an
/// unresolvable child is (per `guard_cycle`'s own doc comment): `middle`
/// runs the disabled global as a `sub_workflow`, and if the scan tried to
/// load it the same way it loads a live child, it would hit the same
/// company-disabled refusal `a_company_disabled_global_child_fails_resolution`
/// pins — instead the disabled id is skipped as unresolvable, so resolving
/// `middle` itself succeeds rather than failing with an unrelated
/// "workflow not found" surfaced through the cycle guard.
#[tokio::test]
async fn a_disabled_global_in_the_closure_is_skipped_not_treated_as_a_cycle() {
    let dropped = crate::globals::workflows()[0].id.clone();
    let dir = tempfile::tempdir().unwrap();
    write_wf(dir.path(), "middle", &parent_of("middle", &dropped));
    let store = store_with_globals_disable(Vec::new(), vec![format!("workflow:{dropped}")]);
    let resolver = StoreWorkflowResolver::new(
        Some(dir.path().to_path_buf()),
        store,
        CompanyId::new("acme"),
        "root".to_string(),
        None,
    );

    let graph = resolver
        .resolve("middle")
        .await
        .expect("the disabled global in the closure is skipped, not fatal");
    assert_eq!(graph.id.as_deref(), Some("middle"));
}

#[tokio::test]
async fn traversal_id_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = seed_resolver(dir.path(), "root");
    let err = resolver.resolve("../secrets").await.unwrap_err();
    assert!(err.to_string().contains("not a valid workflow id"), "{err}");
}

/// A→A: the child directly references the run root, closing a one-level loop.
#[tokio::test]
async fn root_self_loop_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    // `a` runs `b`; `b` runs `a` (the root). Resolving `b` (root = `a`) must
    // reject because `a` is in `b`'s closure.
    write_wf(dir.path(), "a", &parent_of("a", "b"));
    write_wf(dir.path(), "b", &parent_of("b", "a"));
    let resolver = seed_resolver(dir.path(), "a");
    let err = resolver.resolve("b").await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("cycle"), "{msg}");
    // The cycle message names the chain, not the depth backstop.
    assert!(
        !msg.contains("depth"),
        "should be the static cycle msg: {msg}"
    );
}

/// A→B→A: two on-disk workflows referencing each other hard-reject at the
/// first resolve of the second.
#[tokio::test]
async fn mutual_reference_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    write_wf(dir.path(), "flow_a", &parent_of("flow_a", "flow_b"));
    write_wf(dir.path(), "flow_b", &parent_of("flow_b", "flow_a"));
    // Root is flow_a; the engine resolves flow_b first.
    let resolver = seed_resolver(dir.path(), "flow_a");
    let err = resolver.resolve("flow_b").await.unwrap_err();
    assert!(err.to_string().contains("cycle"), "{err}");
}

/// A diamond (root → B and → C, both → D) is NOT a cycle: D is reached twice
/// but never loops back, so every resolve succeeds.
#[tokio::test]
async fn diamond_is_allowed() {
    let dir = tempfile::tempdir().unwrap();
    write_wf(dir.path(), "b", &parent_of("b", "d"));
    write_wf(dir.path(), "c", &parent_of("c", "d"));
    write_wf(dir.path(), "d", &leaf("d"));
    let resolver = seed_resolver(dir.path(), "root");
    resolver.resolve("b").await.expect("b resolves");
    resolver.resolve("c").await.expect("c resolves");
    resolver.resolve("d").await.expect("d resolves");
}

// --- #168: overlay-only (hosted) children --------------------------------

/// A `sub_workflow` child that exists ONLY as a runtime-authored body — the
/// hosted case — resolves into a compilable graph.
#[tokio::test]
async fn resolves_an_overlay_child_with_no_source_dir() {
    let resolver = overlay_resolver(vec![overlay("child", leaf("child"))], "root");
    let graph = resolver.resolve("child").await.expect("resolves");
    assert_eq!(graph.id.as_deref(), Some("child"));
    tinyflows::compiler::compile(&graph).expect("resolved overlay child compiles");
}

/// The static cycle scan must walk overlay children too — otherwise a cycle
/// formed entirely from console-created workflows would only be caught by the
/// engine's depth backstop, deep into the run.
#[tokio::test]
async fn cycle_through_overlay_children_is_rejected() {
    let resolver = overlay_resolver(
        vec![
            overlay("flow_a", parent_of("flow_a", "flow_b")),
            overlay("flow_b", parent_of("flow_b", "flow_a")),
        ],
        "flow_a",
    );
    let err = resolver.resolve("flow_b").await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("cycle"), "{msg}");
    assert!(
        !msg.contains("depth"),
        "should be the static cycle msg: {msg}"
    );
}

/// A seed file and an overlay body sharing one id: the seed wins, matching
/// `load_workflow_union`'s documented precedence.
#[tokio::test]
async fn a_seed_file_shadows_an_overlay_of_the_same_id() {
    let dir = tempfile::tempdir().unwrap();
    write_wf(dir.path(), "child", &leaf("child"));
    // The overlay body of the same id is a *parent* graph — if it won, the
    // resolved graph would carry a sub_workflow node.
    let resolver = StoreWorkflowResolver::new(
        Some(dir.path().to_path_buf()),
        store_with(vec![overlay("child", parent_of("child", "other"))]),
        CompanyId::new("acme"),
        "root".to_string(),
        None,
    );
    let graph = resolver.resolve("child").await.expect("resolves");
    assert_eq!(graph.id.as_deref(), Some("child"));
    assert_eq!(
        graph.nodes.len(),
        2,
        "the seed leaf must win over the overlay parent"
    );
}
