//! A roster rebuilt under a resumed session. An embedded agent's own tools
//! reach the model natively, on their own schema, so a rebuild that moves the
//! belt owes the session no catalogue brief; the MCP rebrief text itself stays
//! readable for a served catalogue.

use super::built_in_test_fixtures::*;
use super::built_in_test_fixtures_2::*;
use super::*;
use crate::harness::build::{opencompany_mcp_rebrief, tools_named_in_mcp_brief};

/// `rec` with `namespace` on the company allow-list — a seed edit, which the
/// grant axis reads off the record `ensure` is handed.
fn granting(rec: &CompanyRecord, namespace: &str) -> CompanyRecord {
    let mut granted = rec.clone();
    granted.manifest.tools.allow.push(namespace.to_string());
    granted
}

#[tokio::test]
async fn a_rebuild_that_moves_the_belt_owes_the_session_no_brief() {
    let dir = tempfile::tempdir().unwrap();
    let context = Arc::new(MockContext::default());
    let mut rec = capped_record();
    rec.manifest.tools.allow = vec!["*".to_string()];
    let mut deps = deps_with_plan(dir.path(), context, None, None);
    deps.workspace = Some(Arc::new(crate::store::FsOps::new(dir.path().to_path_buf())));
    let on_belt = |agent: &CompanyAgent, name: &str| agent.tools().iter().any(|t| t.name() == name);

    let pool = HarnessPool::new();
    pool.ensure(&rec, &deps).await.expect("first ensure");
    let first = pool.agent(&rec.id, "engineer").await.expect("engineer");
    assert!(!first.catalogue_brief_pending());
    assert!(
        !on_belt(&first, "workspace_create"),
        "precondition: `*` confers no explicit workspace write"
    );

    pool.ensure(&rec, &deps).await.expect("redundant ensure");
    let same = pool.agent(&rec.id, "engineer").await.expect("engineer");
    assert!(
        Arc::ptr_eq(&first, &same),
        "an unchanged roster is not rebuilt"
    );

    let with_workspace = granting(&rec, "workspace.write");
    pool.ensure(&with_workspace, &deps)
        .await
        .expect("post-grant ensure");
    let granted = pool.agent(&rec.id, "engineer").await.expect("engineer");
    assert!(!Arc::ptr_eq(&first, &granted), "the grant must rebuild");
    assert!(
        on_belt(&granted, "workspace_create"),
        "the grant wires the workspace write tools onto the belt"
    );
    assert!(granted.served_catalogue().is_empty());
    assert!(
        !granted.catalogue_brief_pending(),
        "a native tool carries its own schema, so the session is owed no brief"
    );
}

#[test]
fn the_rebrief_is_read_back_like_the_prompt_brief_and_keeps_the_turn_text() {
    let tools = vec!["post".to_string(), "composio_execute".to_string()];
    let text = opencompany_mcp_rebrief(&tools, "[conversation: engineering]\nsend it");
    assert_eq!(tools_named_in_mcp_brief(&text), tools);
    assert!(
        text.ends_with("[conversation: engineering]\nsend it"),
        "the turn text follows the brief untouched: {text}"
    );
}
