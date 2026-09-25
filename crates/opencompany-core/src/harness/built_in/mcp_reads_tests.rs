//! The MCP read set every teammate policy carries: one source, the resolved
//! per-server tool policy, whether the teammate answers in chat or sits in an
//! episode seat.

use super::built_in_test_fixtures::*;
use super::*;
use crate::company::McpServer;
use crate::company::mcp::{effective_mcp_servers, mcp_read_set};
use crate::company::mcp_policy::{ApprovalMode, ToolPolicy, mcp_allow_set};

/// A server whose manifest declaration and stored policy disagree both ways:
/// `search_pages` is declared read-only but the operator requires approval
/// for it, and `get_page` is undeclared but the operator always allows it.
fn divergent_servers() -> Vec<crate::company::mcp::McpServerDecl> {
    let server = McpServer {
        name: "notion".into(),
        endpoint: "https://notion.example/mcp".into(),
        description: None,
        command: None,
        allowed_tools: Vec::new(),
        disallowed_tools: Vec::new(),
        read_only_tools: vec!["search_pages".into()],
        timeout_secs: 30,
        enabled: true,
        auth_secret: None,
    };
    let mut decls = effective_mcp_servers(&[], &[server], &[]);
    let policies = &mut decls[0].tool_policies;
    policies.overrides.insert(
        "search_pages".into(),
        ToolPolicy {
            tier: None,
            mode: Some(ApprovalMode::NeedsApproval),
        },
    );
    policies.overrides.insert(
        "get_page".into(),
        ToolPolicy {
            tier: None,
            mode: Some(ApprovalMode::AlwaysAllow),
        },
    );
    decls
}

fn engineer(company: &CompanyRecord) -> ManifestAgent {
    company
        .effective_agents()
        .into_iter()
        .find(|agent| agent.id == "engineer")
        .expect("engineer is on the roster")
}

#[test]
fn a_seat_and_a_chat_agent_carry_the_same_mcp_read_set() {
    let mut fx = fixture();
    fx.deps.mcp_servers = divergent_servers();
    let company = record();
    let agent = engineer(&company);
    let grants = grants_for_policy(&company, &company.manifest.tools.allow, &agent);

    let seat = seat_policy(&company, &fx.deps, &agent, &grants);
    let chat = agent_policy_for(
        &company,
        &fx.deps,
        &agent,
        &company.effective_policy(),
        company.effective_budget(&agent.id),
        &grants,
    );

    assert_eq!(seat.mcp_reads(), chat.mcp_reads());
    assert_eq!(seat.mcp_reads(), &mcp_allow_set(&fx.deps.mcp_servers));
}

#[test]
fn the_read_set_follows_the_stored_policy_over_the_declaration() {
    let mut fx = fixture();
    fx.deps.mcp_servers = divergent_servers();
    let company = record();
    let agent = engineer(&company);
    let grants = grants_for_policy(&company, &company.manifest.tools.allow, &agent);

    let reads = seat_policy(&company, &fx.deps, &agent, &grants)
        .mcp_reads()
        .clone();

    assert!(
        !reads.contains("notion", "search_pages"),
        "an operator's approval requirement must win over the read-only declaration"
    );
    assert!(
        reads.contains("notion", "get_page"),
        "an operator's always-allow must reach the seat"
    );
    assert_ne!(reads, mcp_read_set(&fx.deps.mcp_servers));
}
