use super::*;
use crate::ports::types::CompanyRecord;
use crate::ports::types::EventSeq;
use crate::ports::workspace::{NodeKind, WorkspaceNode, WorkspaceOrigin};
use crate::runtime::RuntimeBuilder;
use crate::store::FsCompanyStore;
use crate::{AppConfig, AppState};
use axum::body::Body;
use axum::http::Request;

use super::operator_test_support_1::*;

pub(super) fn attachment_folder_node(id: &str, name: &str) -> WorkspaceNode {
    WorkspaceNode {
        id: id.to_string(),
        name: name.to_string(),
        kind: NodeKind::Folder,
        parent_id: None,
        updated_at_millis: 1_700_000_000_000,
        created_by: WorkspaceOrigin::Operator,
        updated_by: WorkspaceOrigin::Operator,
        mime: None,
        size: None,
        sha256: None,
        adopted: false,
    }
}

pub(super) fn attachment_note_node(id: &str, name: &str) -> WorkspaceNode {
    WorkspaceNode {
        kind: NodeKind::File,
        ..attachment_folder_node(id, name)
    }
}

/// Two companies on one host, each with its own signed-in admin — the
/// shape a cross-company (IDOR) question needs, since a single-company
/// host cannot tell "refused for crossing a boundary" from "there was
/// nothing else to reach". Mirrors
/// `graphql::bridge_scope_test::state_with_two_companies`.
pub(super) async fn state_with_two_companies(home: &std::path::Path) -> AppState {
    use crate::ports::store::CompanyStore;
    let store = FsCompanyStore::new(home.to_path_buf());
    let state = AppState::new(AppConfig::default());
    for name in ["acme", "globex"] {
        let id = CompanyId::new(name);
        let m = manifest();
        store
            .save(&CompanyRecord {
                general_channel: Default::default(),
                overlay_desk_hive: Vec::new(),
                overlay_retired_agents: Vec::new(),
                overlay_agent_edits: Vec::new(),
                id: id.clone(),
                manifest: m.clone(),
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
        let runtime = RuntimeBuilder::new(home.to_path_buf(), m)
            .with_id(id.clone())
            .build()
            .await
            .unwrap();
        state.registry().insert(id, Arc::new(runtime));
        crate::server::test_support::seed_fixed_admin(&state, name).await;
    }
    state
}

pub(super) fn chat_with_attachments(company: &str, attachments: Vec<String>) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/api/v1/companies/{company}/chat"))
        .header("cookie", crate::server::test_support::fixed_cookie(company))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "text": "see attached", "attachments": attachments }).to_string(),
        ))
        .unwrap()
}

pub(super) async fn last_operator_message_attachments(
    runtime: &Arc<CompanyRuntime>,
    company: &CompanyId,
) -> Vec<Attachment> {
    let events = runtime
        .events()
        .read_from(company, EventSeq::new(0), usize::MAX)
        .await
        .unwrap();
    events
        .into_iter()
        .rev()
        .find_map(|stored| match stored.event {
            CompanyEvent::OperatorMessage { attachments, .. } => Some(attachments),
            _ => None,
        })
        .expect("an operator message was journaled")
}
