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

// ---- the path a company agent actually takes ---------------------------

/// What `AgentSpec::mcp` will carry, read back the only way the type allows:
/// its redacting `Debug`, which prints `disallowed_tools` verbatim. Asserting
/// on the attachment itself rather than on a helper's return value is the
/// point — the question is what the spec receives.
fn attachment(server: McpServerDecl) -> String {
    let attached = crate::harness::mcp::embed_servers_for_agent(&[server], &grants(&["mcp:*"]));
    assert_eq!(attached.len(), 1);
    format!("{attached:?}")
}

/// A company agent reaches a declared server through OpenHuman's own native
/// `mcp_call_tool` over the servers `AgentSpec::mcp` carries, not through
/// [`OcMcpCallTool`] — see `embed_servers_for_agent`'s doc comment. The refusal
/// above is therefore not the enforcement on that path; the attached server's
/// deny list is, and the transport's own filter puts deny above allow.
#[test]
fn a_blocked_tool_is_denied_on_the_attached_server() {
    let debug = attachment(blocked_server("notion", "delete_page"));

    assert!(
        debug.contains(r#"disallowed_tools: ["delete_page"]"#),
        "a blocked tool must not be reachable natively: {debug}"
    );
}

/// The declaration's own deny list survives: the policy adds to it rather than
/// replacing it, or an operator's `disallowed_tools` would be dropped the
/// moment they blocked something else.
#[test]
fn the_declarations_own_deny_list_is_kept() {
    let mut server = blocked_server("notion", "delete_page");
    server.disallowed_tools = vec!["debug_dump".to_string()];

    let debug = attachment(server);

    assert!(
        debug.contains(r#"disallowed_tools: ["debug_dump", "delete_page"]"#),
        "{debug}"
    );
}

/// A server nobody has blocked anything on is attached exactly as it was.
#[test]
fn an_unblocked_server_is_attached_unchanged() {
    let debug = attachment(decl("notion", DEAD_ENDPOINT));

    assert!(debug.contains("disallowed_tools: []"), "{debug}");
}

/// A tool the operator left at `needs_approval` is not denied — the deny list
/// is the enforcement for `blocked` alone, and denying anything else would take
/// away a tool the approval gate exists to let through.
#[test]
fn a_tool_that_merely_parks_is_not_denied() {
    let mut server = decl("notion", DEAD_ENDPOINT);
    let mut policies = McpToolPolicies::default();
    policies.overrides.insert(
        "update_page".to_string(),
        ToolPolicy {
            tier: None,
            mode: Some(ApprovalMode::NeedsApproval),
        },
    );
    server.tool_policies = policies;

    let debug = attachment(server);

    assert!(debug.contains("disallowed_tools: []"), "{debug}");
}

/// A tier default blocks the tools discovery found, not only the ones an
/// operator has already named — the widening the persisted inventory exists
/// for, asserted where it has to hold: the attachment.
#[test]
fn a_blocked_tier_default_denies_the_inventoried_tools() {
    let mut server = decl("notion", DEAD_ENDPOINT);
    let mut policies = McpToolPolicies::default();
    policies.tier_defaults.insert(
        crate::company::mcp_policy::ToolTier::WriteDelete,
        ApprovalMode::Blocked,
    );
    server.tool_policies = policies;
    let mut inventory = crate::company::mcp_policy::McpToolInventory::default();
    inventory.tools.insert(
        "delete_page".to_string(),
        crate::company::mcp_policy::ToolTier::WriteDelete,
    );
    inventory.tools.insert(
        "read_page".to_string(),
        crate::company::mcp_policy::ToolTier::ReadOnly,
    );
    server.tool_inventory = inventory;

    let debug = attachment(server);

    assert!(
        debug.contains(r#"disallowed_tools: ["delete_page"]"#),
        "{debug}"
    );
}
