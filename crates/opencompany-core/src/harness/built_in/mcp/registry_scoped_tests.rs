//! Tests for [`OcMcpRegistryScopedTool`]: the gate on every entry point, the
//! shape of each refusal, and that the decorator is indistinguishable from the
//! tool it wraps.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use async_trait::async_trait;
use serde_json::{Value, json};

use tinytools::{PermissionLevel, Tool, ToolCallOptions, ToolResult};

use super::OcMcpRegistryScopedTool;
use crate::company::mcp_policy::{
    ApprovalMode, McpToolInventory, McpToolPolicies, ToolPolicy, ToolTier, blocked_refusal,
    registry_tool_inventory_key, registry_tool_policies_key,
};
use crate::error::Result;
use crate::ports::SecretStore;
use crate::ports::types::{CompanyId, SecretValue};

fn company() -> CompanyId {
    CompanyId::new("acme")
}

/// A secret store a test can seed, so the policy the decorator reads is one the
/// test wrote rather than one a fixture implies.
#[derive(Default)]
struct MemSecrets {
    map: StdMutex<HashMap<String, String>>,
}

#[async_trait]
impl SecretStore for MemSecrets {
    async fn get(&self, _c: &CompanyId, key: &str) -> Result<Option<SecretValue>> {
        Ok(self
            .map
            .lock()
            .unwrap()
            .get(key)
            .map(|v| SecretValue(v.clone())))
    }
    async fn set(&self, _c: &CompanyId, key: &str, value: SecretValue) -> Result<()> {
        self.map.lock().unwrap().insert(key.to_string(), value.0);
        Ok(())
    }
}

impl MemSecrets {
    fn put(&self, key: String, value: impl serde::Serialize) {
        self.map
            .lock()
            .unwrap()
            .insert(key, serde_json::to_string(&value).unwrap());
    }
}

/// An install's policy naming one tool's mode outright.
fn policy_with(tool: &str, mode: ApprovalMode) -> McpToolPolicies {
    let mut policies = McpToolPolicies::default();
    policies.overrides.insert(
        tool.to_string(),
        ToolPolicy {
            tier: None,
            mode: Some(mode),
        },
    );
    policies
}

fn grants(g: &[&str]) -> Vec<String> {
    g.iter().map(|s| s.to_string()).collect()
}

const INSTALL_A: &str = "0b8f4b0e-3c2a-4a1d-9e77-6d5a2f1c8e40";
const INSTALL_B: &str = "7f1c9d22-55ae-4f3b-8c10-2b9e4d6a3f51";

/// A shared handle onto the inner tool's call log, so a test can hold the
/// log while the tool itself is owned by the decorator.
type CallLog = Arc<StdMutex<Vec<Value>>>;

/// Records every delegated call so a test can assert the inner tool was
/// never reached, not merely that the outer result was an error.
#[derive(Default)]
struct Recording {
    calls: CallLog,
}

impl Recording {
    fn with_log(calls: CallLog) -> Self {
        Self { calls }
    }
}

fn logged(calls: &CallLog) -> Vec<Value> {
    calls.lock().expect("recorder").clone()
}

#[async_trait]
impl Tool for Recording {
    fn name(&self) -> &str {
        "mcp_registry_tool_call"
    }

    fn description(&self) -> &str {
        "fixture: invoke a tool on a connected MCP server"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "server_id": { "type": "string" } },
            "required": ["server_id"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        self.calls.lock().expect("recorder").push(args);
        Ok(ToolResult::success("delegated"))
    }

    // Overridden so the forwarding test can tell a real answer from the
    // trait's default.
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Execute
    }

    fn is_concurrency_safe(&self, _args: &Value) -> bool {
        true
    }
}

/// Builds the decorator over a fresh recorder, handing back the shared log.
fn wrap(grants_list: &[&str]) -> (CallLog, OcMcpRegistryScopedTool) {
    let calls: CallLog = CallLog::default();
    let tool = OcMcpRegistryScopedTool::new(
        Box::new(Recording::with_log(Arc::clone(&calls))),
        grants(grants_list),
        company(),
        None,
    );
    (calls, tool)
}

/// The same decorator over a seeded store, handing the store back so a test can
/// keep writing to it after the tool is built.
fn wrap_with_secrets(
    grants_list: &[&str],
    secrets: Arc<MemSecrets>,
) -> (CallLog, OcMcpRegistryScopedTool) {
    let calls: CallLog = CallLog::default();
    let tool = OcMcpRegistryScopedTool::new(
        Box::new(Recording::with_log(Arc::clone(&calls))),
        grants(grants_list),
        company(),
        Some(secrets),
    );
    (calls, tool)
}

/// Back-compat at the enforcement layer: a bare grant still reaches an
/// arbitrary install, so upgrading changes nothing for today's companies.
#[tokio::test]
async fn bare_grant_delegates() {
    let (calls, tool) = wrap(&["mcp_registry"]);
    let result = tool
        .execute(json!({ "server_id": INSTALL_A, "tool_name": "echo" }))
        .await
        .expect("bare grant delegates");
    assert!(!result.is_error, "bare grant must reach the inner tool");
    assert_eq!(logged(&calls).len(), 1, "the inner tool must have run");
}

#[tokio::test]
async fn scoped_grant_delegates_for_its_own_install() {
    let (calls, tool) = wrap(&[&format!("mcp_registry.{INSTALL_A}")]);
    let result = tool
        .execute(json!({ "server_id": INSTALL_A, "tool_name": "echo" }))
        .await
        .expect("scoped grant delegates");
    assert!(!result.is_error, "the granted install must be reachable");
    assert_eq!(logged(&calls).len(), 1, "the inner tool must have run");
}

/// The narrowing, proved at the tool: the inner tool is never reached, and
/// the refusal names the grant that would allow the call.
#[tokio::test]
async fn scoped_grant_refuses_another_install_without_delegating() {
    let (calls, tool) = wrap(&[&format!("mcp_registry.{INSTALL_A}")]);

    let result = tool
        .execute(json!({ "server_id": INSTALL_B, "tool_name": "echo" }))
        .await
        .expect("a scope miss is a result, not an Err");

    assert!(result.is_error, "an ungranted install must refuse");
    assert!(
        logged(&calls).is_empty(),
        "the inner tool must not run for an ungranted install"
    );
    let text = serde_json::to_string(&result).expect("serialize refusal");
    assert!(
        text.contains(&format!("mcp_registry.{INSTALL_B}")),
        "the refusal must name the missing grant verbatim: {text}"
    );
}

/// A malformed `server_id` fails closed, and says something different from
/// a grant miss so the agent does not ask for a grant it already has.
#[tokio::test]
async fn unusable_server_id_refuses_without_delegating() {
    for args in [
        json!({ "tool_name": "echo" }),
        json!({ "server_id": Value::Null, "tool_name": "echo" }),
        json!({ "server_id": 7, "tool_name": "echo" }),
        json!({ "server_id": "   ", "tool_name": "echo" }),
    ] {
        let (calls, tool) = wrap(&["mcp_registry"]);

        let result = tool
            .execute(args.clone())
            .await
            .expect("a bad argument is a result, not an Err");

        assert!(result.is_error, "must refuse on {args}");
        assert!(
            logged(&calls).is_empty(),
            "the inner tool must not run on {args}"
        );
        let text = serde_json::to_string(&result).expect("serialize refusal");
        assert!(
            !text.contains("Add the tool grant"),
            "a malformed argument must not read as a grant problem: {text}"
        );
    }
}

/// The trait chains its three entry points, so a caller taking any of them
/// must hit the gate. Without this, gating only `execute` leaves two open
/// doors.
#[tokio::test]
async fn every_entry_point_gates() {
    let ungranted = json!({ "server_id": INSTALL_B, "tool_name": "echo" });

    let (calls, tool) = wrap(&[&format!("mcp_registry.{INSTALL_A}")]);
    assert!(
        tool.execute(ungranted.clone()).await.unwrap().is_error,
        "execute must gate"
    );
    assert!(
        tool.execute_with_options(ungranted.clone(), ToolCallOptions::default())
            .await
            .unwrap()
            .is_error,
        "execute_with_options must gate"
    );
    assert!(
        tool.execute_with_context(ungranted, ToolCallOptions::default(), None)
            .await
            .unwrap()
            .is_error,
        "execute_with_context must gate"
    );
    assert!(
        logged(&calls).is_empty(),
        "no entry point may reach the inner tool for an ungranted install"
    );
}

/// The decorator must be indistinguishable from the tool it wraps. Name in
/// particular is load-bearing: the consequence tables, `is_mcp_bridge_tool`
/// and `mcp_call_pair` all key on it.
#[test]
fn wrapper_forwards_the_inner_tools_identity() {
    let inner = Recording::default();
    let expected_name = inner.name().to_string();
    let expected_description = inner.description().to_string();
    let expected_schema = inner.parameters_schema();
    let expected_permission = inner.permission_level();
    let expected_concurrency = inner.is_concurrency_safe(&json!({}));
    let expected_spec = inner.spec();

    let tool =
        OcMcpRegistryScopedTool::new(Box::new(inner), grants(&["mcp_registry"]), company(), None);

    assert_eq!(tool.name(), expected_name);
    assert_eq!(tool.description(), expected_description);
    assert_eq!(tool.parameters_schema(), expected_schema);
    assert_eq!(tool.permission_level(), expected_permission);
    assert_eq!(tool.is_concurrency_safe(&json!({})), expected_concurrency);
    assert_eq!(tool.spec().name, expected_spec.name);
    assert_eq!(tool.spec().description, expected_spec.description);
}

// ---- per-tool policy (issue #2373) ------------------------------------

/// A blocked tool is refused before the install is dialled, and the refusal is
/// the one the declared-server bridge words — an agent cannot tell from the
/// message which of the two paths it took.
#[tokio::test]
async fn a_blocked_tool_is_refused_without_delegating() {
    let secrets = Arc::new(MemSecrets::default());
    secrets.put(
        registry_tool_policies_key(INSTALL_A),
        policy_with("delete_page", ApprovalMode::Blocked),
    );
    let (calls, tool) = wrap_with_secrets(&["mcp_registry"], secrets);

    let result = tool
        .execute(json!({ "server_id": INSTALL_A, "tool_name": "delete_page" }))
        .await
        .unwrap();

    assert!(result.is_error);
    assert_eq!(result.text(), blocked_refusal(INSTALL_A, "delete_page"));
    assert!(
        calls.lock().unwrap().is_empty(),
        "a blocked call reached the install"
    );
}

/// The block is per tool, not per install: the same document leaves every
/// other tool on that server reachable.
#[tokio::test]
async fn another_tool_on_a_blocked_installs_document_still_delegates() {
    let secrets = Arc::new(MemSecrets::default());
    secrets.put(
        registry_tool_policies_key(INSTALL_A),
        policy_with("delete_page", ApprovalMode::Blocked),
    );
    let (calls, tool) = wrap_with_secrets(&["mcp_registry"], secrets);

    let result = tool
        .execute(json!({ "server_id": INSTALL_A, "tool_name": "read_page" }))
        .await
        .unwrap();

    assert!(!result.is_error, "{}", result.text());
    assert_eq!(calls.lock().unwrap().len(), 1);
}

/// The block is keyed by `server_id`: blocking a tool name on one install says
/// nothing about the same name on another.
#[tokio::test]
async fn a_block_on_one_install_does_not_reach_another() {
    let secrets = Arc::new(MemSecrets::default());
    secrets.put(
        registry_tool_policies_key(INSTALL_A),
        policy_with("delete_page", ApprovalMode::Blocked),
    );
    let (calls, tool) = wrap_with_secrets(&["mcp_registry"], secrets);

    let result = tool
        .execute(json!({ "server_id": INSTALL_B, "tool_name": "delete_page" }))
        .await
        .unwrap();

    assert!(!result.is_error, "{}", result.text());
    assert_eq!(calls.lock().unwrap().len(), 1);
}

/// A tier default blocks the tools discovery actually found on that install —
/// an install carries no `read_only_tools`, so the inventory is the only thing
/// that can say which tier a tool is in.
#[tokio::test]
async fn a_blocked_tier_default_reaches_an_inventoried_tool() {
    let secrets = Arc::new(MemSecrets::default());
    let mut policies = McpToolPolicies::default();
    policies
        .tier_defaults
        .insert(ToolTier::WriteDelete, ApprovalMode::Blocked);
    secrets.put(registry_tool_policies_key(INSTALL_A), policies);
    let mut inventory = McpToolInventory::default();
    inventory
        .tools
        .insert("delete_page".to_string(), ToolTier::WriteDelete);
    inventory
        .tools
        .insert("read_page".to_string(), ToolTier::ReadOnly);
    secrets.put(registry_tool_inventory_key(INSTALL_A), inventory);
    let (calls, tool) = wrap_with_secrets(&["mcp_registry"], secrets);

    let blocked = tool
        .execute(json!({ "server_id": INSTALL_A, "tool_name": "delete_page" }))
        .await
        .unwrap();
    assert!(blocked.is_error);
    assert_eq!(blocked.text(), blocked_refusal(INSTALL_A, "delete_page"));

    let allowed = tool
        .execute(json!({ "server_id": INSTALL_A, "tool_name": "read_page" }))
        .await
        .unwrap();
    assert!(!allowed.is_error, "{}", allowed.text());
    assert_eq!(calls.lock().unwrap().len(), 1);
}

/// An inventory on its own blocks nothing. A suggested tier is a proposal the
/// console renders; only a stored decision refuses a call.
#[tokio::test]
async fn an_inventory_alone_blocks_nothing() {
    let secrets = Arc::new(MemSecrets::default());
    let mut inventory = McpToolInventory::default();
    inventory
        .tools
        .insert("delete_page".to_string(), ToolTier::WriteDelete);
    secrets.put(registry_tool_inventory_key(INSTALL_A), inventory);
    let (calls, tool) = wrap_with_secrets(&["mcp_registry"], secrets);

    let result = tool
        .execute(json!({ "server_id": INSTALL_A, "tool_name": "delete_page" }))
        .await
        .unwrap();

    assert!(!result.is_error, "{}", result.text());
    assert_eq!(calls.lock().unwrap().len(), 1);
}

/// Every entry point consults the policy, not only the one a given caller
/// happens to take — the same property the grant check already has.
#[tokio::test]
async fn every_entry_point_consults_the_policy() {
    let secrets = Arc::new(MemSecrets::default());
    secrets.put(
        registry_tool_policies_key(INSTALL_A),
        policy_with("delete_page", ApprovalMode::Blocked),
    );
    let (calls, tool) = wrap_with_secrets(&["mcp_registry"], secrets);
    let args = json!({ "server_id": INSTALL_A, "tool_name": "delete_page" });

    for result in [
        tool.execute(args.clone()).await.unwrap(),
        tool.execute_with_options(args.clone(), ToolCallOptions::default())
            .await
            .unwrap(),
        tool.execute_with_context(args.clone(), ToolCallOptions::default(), None)
            .await
            .unwrap(),
    ] {
        assert!(result.is_error);
        assert_eq!(result.text(), blocked_refusal(INSTALL_A, "delete_page"));
    }
    assert!(calls.lock().unwrap().is_empty());
}

/// A deployment with no secret store cannot read a policy, and does not invent
/// one: the grant remains the whole gate.
#[tokio::test]
async fn without_a_store_the_grant_is_the_whole_gate() {
    let (calls, tool) = wrap(&["mcp_registry"]);

    let result = tool
        .execute(json!({ "server_id": INSTALL_A, "tool_name": "delete_page" }))
        .await
        .unwrap();

    assert!(!result.is_error, "{}", result.text());
    assert_eq!(calls.lock().unwrap().len(), 1);
}
