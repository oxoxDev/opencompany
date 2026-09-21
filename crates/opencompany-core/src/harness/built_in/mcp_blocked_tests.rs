//! A blocked tool is refused before anything is dialled.

use super::*;

use std::sync::Arc;

use serde_json::json;

use crate::company::mcp_policy::{ApprovalMode, McpToolPolicies, ToolPolicy};
use crate::harness::mcp::{McpFailureQueue, McpMetering};
use crate::harness::mcp::{OcMcpCallTool, granted_policies, registry_for_agent};

use super::tests::{decl, grants};

/// An endpoint nothing listens on: a call that reaches the transport fails
/// loudly, so "was it dialled" is observable without a live server.
const DEAD_ENDPOINT: &str = "http://127.0.0.1:1/mcp";

fn blocked_server(name: &str, tool: &str) -> McpServerDecl {
    let mut server = decl(name, DEAD_ENDPOINT);
    let mut policies = McpToolPolicies::default();
    policies.overrides.insert(
        tool.to_string(),
        ToolPolicy {
            tier: None,
            mode: Some(ApprovalMode::Blocked),
        },
    );
    server.tool_policies = policies;
    server
}

fn call_tool(servers: &[McpServerDecl], queue: McpFailureQueue) -> OcMcpCallTool {
    let grants = grants(&["mcp:*"]);
    let registry = registry_for_agent(servers, &grants).expect("registry");
    OcMcpCallTool::new(
        registry,
        Arc::new(SecurityPolicy::default()),
        Vec::new(),
        queue,
        McpMetering::off(),
        granted_policies(servers, &grants),
    )
}

fn args(server: &str, tool: &str) -> serde_json::Value {
    json!({ "server": server, "tool": tool, "arguments": {} })
}

#[tokio::test]
async fn a_blocked_tool_refuses_without_dialling() {
    let servers = vec![blocked_server("fixture", "delete_page")];
    let queue = McpFailureQueue::default();
    let tool = call_tool(&servers, queue.clone());

    let result = tool
        .execute(args("fixture", "delete_page"))
        .await
        .expect("mcp_call_tool");

    assert!(result.is_error);
    let text = result.output();
    assert!(text.contains("delete_page"), "{text}");
    assert!(text.contains("fixture"), "{text}");
    assert!(text.contains("blocked"), "{text}");
    assert!(text.contains("do not retry"), "{text}");
    // A call that reached the dead endpoint would have recorded a failure.
    assert!(
        queue.drain().is_empty(),
        "a blocked call must not reach the transport"
    );
}

/// The control: the same server, a tool nobody blocked. It dials, fails against
/// the dead endpoint, and records that failure — which is what makes the
/// assertion above about an empty queue mean something.
#[tokio::test]
async fn an_unblocked_tool_on_the_same_server_still_dials() {
    let servers = vec![blocked_server("fixture", "delete_page")];
    let queue = McpFailureQueue::default();
    let tool = call_tool(&servers, queue.clone());

    let result = tool
        .execute(args("fixture", "search_pages"))
        .await
        .expect("mcp_call_tool");

    assert!(result.is_error);
    assert!(!result.output().contains("blocked"), "{}", result.output());
    assert_eq!(queue.drain().len(), 1);
}

/// All three entry points refuse. The trait chains `execute` and
/// `execute_with_context` into `execute_with_options` by default, so today one
/// guard covers all three — this fails the day someone overrides one of the
/// other two and forgets the check.
#[tokio::test]
async fn every_entry_point_refuses_a_blocked_tool() {
    let servers = vec![blocked_server("fixture", "delete_page")];
    let queue = McpFailureQueue::default();
    let tool = call_tool(&servers, queue.clone());

    let direct = tool
        .execute(args("fixture", "delete_page"))
        .await
        .expect("execute");
    let with_options = tool
        .execute_with_options(args("fixture", "delete_page"), ToolCallOptions::default())
        .await
        .expect("execute_with_options");
    let with_context = tool
        .execute_with_context(
            args("fixture", "delete_page"),
            ToolCallOptions::default(),
            None,
        )
        .await
        .expect("execute_with_context");

    for result in [direct, with_options, with_context] {
        assert!(result.is_error);
        assert!(result.output().contains("blocked"), "{}", result.output());
    }
    assert!(queue.drain().is_empty());
}

/// The block is resolved through the same cleaned name the registry would be
/// handed, so wrapping the tool name in the markdown a model routinely emits
/// cannot slip past it.
#[tokio::test]
async fn markdown_wrapping_does_not_evade_the_block() {
    let servers = vec![blocked_server("fixture", "delete_page")];
    let queue = McpFailureQueue::default();
    let tool = call_tool(&servers, queue.clone());

    let result = tool
        .execute(args("`fixture`", "`delete_page`"))
        .await
        .expect("mcp_call_tool");

    assert!(result.output().contains("blocked"), "{}", result.output());
    assert!(queue.drain().is_empty());
}

/// A server the agent's grants do not reach contributes no policy, so the
/// refusal cannot become a way to learn that an ungranted server exists.
#[test]
fn an_ungranted_server_contributes_no_policy() {
    let servers = vec![blocked_server("fixture", "delete_page")];
    let narrowed = granted_policies(&servers, &grants(&["mcp:other"]));
    assert!(!narrowed.is_blocked("fixture", "delete_page"));
    assert!(granted_policies(&servers, &grants(&["mcp:*"])).is_blocked("fixture", "delete_page"));
}

/// A disabled server hands out no tool at all, so its policy is not consulted.
#[test]
fn a_disabled_server_contributes_no_policy() {
    let mut server = blocked_server("fixture", "delete_page");
    server.enabled = false;
    let policies = granted_policies(std::slice::from_ref(&server), &grants(&["mcp:*"]));
    assert!(!policies.is_blocked("fixture", "delete_page"));
}
