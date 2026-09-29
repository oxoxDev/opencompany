//! Cross-cutting tests for the GraphQL read plane: a four-case suite per query
//! and a committed SDL snapshot that freezes the read contract for WS7.

use crate::company::CompanyManifest;
use crate::ports::CompanyStore;
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::runtime::RuntimeBuilder;
use crate::store::FsCompanyStore;
use crate::{AppConfig, AppState};
use std::sync::Arc;

/// The workflow summaries a company itself has: the global baseline is listed
/// in every company, and these tests are about the company's own graphs.
///
/// This is an **id heuristic**, not provenance: `WorkflowSummary` carries no
/// `global` flag over GraphQL, so a row is classified as "the baseline's" by
/// whether its id matches one of `crate::globals::workflows()`. A company
/// definition of the *same* id supersedes the global one (see
/// `crate::company::list_workflows_with_globals`) and would be wrongly
/// excluded here — none of the fixtures below give a company workflow an id
/// that collides with a global, so that gap does not fire in this suite, but
/// see `graphql_lists_a_company_override_of_a_global_id_by_its_own_content`
/// for the same-id case asserted directly, without this helper.
pub(crate) fn own_workflows(value: &serde_json::Value) -> Vec<&serde_json::Value> {
    value
        .as_array()
        .expect("summaries")
        .iter()
        .filter(|row| {
            let id = row["id"].as_str().unwrap_or_default();
            !crate::globals::workflows().iter().any(|w| w.id == id)
        })
        .collect()
}

/// The skills a company itself has: the global baseline is installed in every
/// company, and these tests are about what the company adds to it.
///
/// An **id heuristic**, like [`own_workflows`]: a `Skill` row carries no
/// baseline flag, so a row is classified as the baseline's by whether its slug
/// is one of `crate::globals::skills()`. A company bundle or delta of the same
/// slug supersedes the global and would be wrongly excluded — the fixtures that
/// exercise that case assert on the row directly instead.
pub(crate) fn own_skills(value: &serde_json::Value) -> Vec<&serde_json::Value> {
    value
        .as_array()
        .expect("skills")
        .iter()
        .filter(|row| {
            let id = row["id"].as_str().unwrap_or_default();
            !crate::globals::skills().iter().any(|doc| doc.slug == id)
        })
        .collect()
}

pub(crate) fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("opencompany-gql-")
        .tempdir()
        .expect("tempdir")
}

pub(crate) fn manifest() -> CompanyManifest {
    toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n").unwrap()
}

pub(crate) async fn state_with_company(home: &std::path::Path) -> AppState {
    state_with_manifest(home, manifest()).await
}

/// [`state_with_company`] over an explicit manifest, for a test that needs the
/// company configured differently from [`manifest`] and should not have to
/// restate the whole record to get there.
pub(crate) async fn state_with_manifest(
    home: &std::path::Path,
    manifest: CompanyManifest,
) -> AppState {
    state_with_builder(home, manifest, |builder| builder).await
}

/// [`state_with_manifest`] with a runtime-builder override, for a test that
/// swaps a store the runtime owns — e.g. a counting deep-trace store.
pub(super) async fn state_with_builder(
    home: &std::path::Path,
    manifest: CompanyManifest,
    override_runtime: impl FnOnce(RuntimeBuilder) -> RuntimeBuilder,
) -> AppState {
    let store = FsCompanyStore::new(home.to_path_buf());
    let id = CompanyId::new("acme");
    store
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_desk_hive: Vec::new(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            id: id.clone(),
            manifest: manifest.clone(),
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
        })
        .await
        .unwrap();
    let runtime =
        override_runtime(RuntimeBuilder::new(home.to_path_buf(), manifest).with_id(id.clone()))
            .build()
            .await
            .unwrap();
    let state = AppState::new(AppConfig::default()).with_home(home.to_path_buf());
    state.registry().insert(id, Arc::new(runtime));
    // Every route needs a principal now; the harness signs in as an admin so
    // tests can keep asserting resolver behavior rather than auth.
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;
    state
}

// ---------------------------------------------------------------------------
// The approval tier over GraphQL (issue #1070)
// ---------------------------------------------------------------------------

/// A company whose always-ask list is **not** empty.
///
/// The shared [`manifest`] leaves `always_approve` on its default, which is
/// `[]` — and two empty lists are indistinguishable however they are wired, so
/// a suite driven off it could not tell `alwaysApprove` from
/// `manifestAlwaysApprove`, nor either from a resolver that answered `[]`
/// unconditionally. The values are the same pair the REST suite uses.
pub(super) fn policy_manifest() -> CompanyManifest {
    toml::from_str(
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
         always_approve = [\"payment.send\", \"filing.submit\"]\n",
    )
    .unwrap()
}

// ---------------------------------------------------------------------------
// Manifest-derived + store-backed reads, over a fuller company.
// ---------------------------------------------------------------------------
fn rich_manifest() -> CompanyManifest {
    toml::from_str(
        r#"
[company]
name = "Acme"
[policy]
mode = "full"
[[agent]]
id = "maya"
role = "Marketing Lead"
description = "Runs campaigns."
[[group_chat]]
id = "general"
name = "General"
description = "Company-wide desk."
members = ["maya"]
[[connection]]
provider = "slack"
reason = "Post updates."
"#,
    )
    .unwrap()
}

pub(super) async fn state_with_rich_company(home: &std::path::Path) -> AppState {
    let store = FsCompanyStore::new(home.to_path_buf());
    let id = CompanyId::new("acme");
    store
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_desk_hive: Vec::new(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            id: id.clone(),
            manifest: rich_manifest(),
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
        })
        .await
        .unwrap();
    let runtime = RuntimeBuilder::new(home.to_path_buf(), rich_manifest())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let state = AppState::new(AppConfig::default()).with_home(home.to_path_buf());
    state.registry().insert(id, Arc::new(runtime));
    // Every route needs a principal now; the harness signs in as an admin so
    // tests can keep asserting resolver behavior rather than auth.
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;
    state
}

// ---------------------------------------------------------------------------
// A binary node is not a note, on either read surface (issue #669)
// ---------------------------------------------------------------------------

/// Mints a real binary node in the store, the way an upload or a publish does.
pub(super) async fn given_a_binary_node(
    state: &AppState,
    name: &str,
    mime: &str,
    bytes: &[u8],
) -> String {
    let id = CompanyId::new("acme");
    let workspace = state.registry().get(&id).unwrap().workspace().clone();
    let node = crate::ports::workspace::WorkspaceNode {
        id: crate::ports::generate_id(),
        name: name.to_string(),
        kind: crate::ports::workspace::NodeKind::File,
        parent_id: None,
        updated_at_millis: 1_700_000_000_000,
        created_by: crate::ports::workspace::WorkspaceOrigin::Operator,
        updated_by: crate::ports::workspace::WorkspaceOrigin::Operator,
        mime: Some(mime.to_string()),
        size: None,
        sha256: None,
        adopted: false,
    };
    workspace.create_binary(&id, &node, bytes).await.unwrap();
    node.id
}

// ---------------------------------------------------------------------------
// Run observability: what a company's agents actually did
// ---------------------------------------------------------------------------

/// Seeds one workflow-node attempt with a two-step trace and a deep half.
pub(super) async fn given_a_workflow_node_attempt(state: &AppState) {
    use crate::ports::deep_trace::{RunStepDetailRecord, TurnStepDetail};
    use crate::ports::runs::{NewRun, RunStepRecord};
    use crate::ports::types::{TurnStep, TurnStepFailure, TurnStepKind, TurnStepStatus};

    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).expect("runtime");
    let runs = runtime.runs();

    let row = runs
        .create_run(
            &id,
            NewRun::for_workflow_node("att-1", "wr-1", "solve", "programmer"),
        )
        .await
        .unwrap();
    runs.begin_run_untriggered(&id, &row.id).await.unwrap();

    for (seq, kind, label) in [
        (0u32, TurnStepKind::Thinking, "Thinking"),
        (1, TurnStepKind::ToolCall, "Shell"),
    ] {
        runs.append_run_step(
            &id,
            &RunStepRecord {
                run_id: "att-1".to_string(),
                step_seq: seq,
                at_millis: 100 + seq as u64,
                step: TurnStep {
                    kind,
                    status: TurnStepStatus::Ok,
                    label: label.to_string(),
                    failure: (seq == 1).then_some(TurnStepFailure::BlockedByPolicy),
                    result: (seq == 1).then(|| "1 line".to_string()),
                    ..TurnStep::default()
                },
            },
        )
        .await
        .unwrap();
    }

    runtime
        .deep_trace()
        .append_step_detail(
            &id,
            &RunStepDetailRecord {
                run_id: "att-1".to_string(),
                step_seq: 0,
                at_millis: 100,
                detail: TurnStepDetail {
                    reasoning: Some("Collatz — memoise the chain".to_string()),
                    ..TurnStepDetail::default()
                },
            },
        )
        .await
        .unwrap();
}
