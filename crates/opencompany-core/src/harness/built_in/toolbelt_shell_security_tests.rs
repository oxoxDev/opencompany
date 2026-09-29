use super::toolbelt_test_helpers_tests::*;
use super::*;
use serde_json::json;
#[test]
fn exec_security_shape_is_workspace_scoped_and_hardened() {
    let ws = Path::new("/tmp/oc-toolbelt-policy");

    let supervised = exec_security(ws, PolicyMode::Supervised);
    assert!(supervised.workspace_only, "must be workspace-only");
    assert_eq!(supervised.workspace_dir, ws);
    assert_eq!(supervised.action_dir, ws);
    assert!(
        supervised.block_high_risk_commands,
        "high-risk must be blocked"
    );
    assert!(
        !supervised.allow_tool_install,
        "tool install must be denied"
    );
    assert!(
        !supervised.auto_approve_all,
        "blanket auto-approve must be off"
    );
    assert_eq!(supervised.autonomy, AutonomyLevel::Supervised);
    assert!(
        supervised.require_approval_for_medium_risk,
        "supervised must approve medium-risk"
    );

    let readonly = exec_security(ws, PolicyMode::Readonly);
    assert_eq!(readonly.autonomy, AutonomyLevel::ReadOnly);
    assert!(!readonly.require_approval_for_medium_risk);

    let full = exec_security(ws, PolicyMode::Full);
    assert_eq!(full.autonomy, AutonomyLevel::Full);
    assert!(!full.require_approval_for_medium_risk);
}

/// `workspace_only` is a field on the policy this module builds, but the
/// enforcement lives in the vendored `SecurityPolicy::validate_path`. This
/// drives the real vendored check, not a stub, so a traversal or symlink
/// escape is actually refused rather than merely configured.
#[tokio::test]
async fn exec_security_refuses_a_traversal_and_a_symlink_escape() {
    let root = tempfile::tempdir().expect("tempdir");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace dir");
    std::fs::write(workspace.join("inside.txt"), b"ok").expect("seed file");

    let outside = root.path().join("outside");
    std::fs::create_dir_all(&outside).expect("outside dir");
    std::fs::write(outside.join("secret.txt"), b"nope").expect("seed secret");

    let policy = exec_security(&workspace, PolicyMode::Full);

    let traversal = policy.validate_path("../outside/secret.txt").await;
    assert!(
        traversal.is_err(),
        "a `..` component must be refused before any resolve: {traversal:?}"
    );

    #[cfg(unix)]
    {
        let link = workspace.join("escape-link");
        std::os::unix::fs::symlink(&outside, &link).expect("symlink");
        let via_symlink = policy.validate_path("escape-link/secret.txt").await;
        assert!(
            via_symlink.is_err(),
            "a symlink resolving outside the workspace must be refused: {via_symlink:?}"
        );
    }

    // Sanity: a real file inside the workspace is still reachable, so the
    // refusals above are workspace_only doing its job, not a broken policy.
    let inside = policy.validate_path("inside.txt").await;
    assert!(
        inside.is_ok(),
        "a file inside the workspace must resolve: {inside:?}"
    );
}

/// `auto` must not loosen shell execution (issue #560).
///
/// This is the test for the decision argued on [`autonomy_for`], and it
/// guards a hole with no other guard: a workflow `tool_call` node has **no**
/// `ApprovalPolicy` above it, so this policy is the entire tier there.
/// `auto` is more permissive than `supervised` at the approval gate, and the
/// tempting mapping — matching that feel with `AutonomyLevel::Full` — would
/// silently drop the medium-risk shell gate for every workflow node on an
/// `auto` company, because upstream's approval arm fires only when
/// `autonomy == Supervised`.
///
/// The second assertion is the subtler half. With autonomy mapped to
/// `Supervised`, `require_approval_for_medium_risk` becomes load-bearing —
/// and it was written as `mode == PolicyMode::Supervised`, an expression
/// that was exhaustive by accident and answers `false` for a variant added
/// beside it. Getting the mapping right and leaving that expression alone
/// would have reopened the same hole from the other side.
#[test]
fn auto_borrows_supervised_exec_security_rather_than_full() {
    let ws = Path::new("/tmp/oc-toolbelt-policy-auto");
    let auto = exec_security(ws, PolicyMode::Auto);

    assert_eq!(
        auto.autonomy,
        AutonomyLevel::Supervised,
        "auto must not inherit Full's exec autonomy — a workflow tool_call node has no \
         approval gate above this policy"
    );
    assert!(
        auto.require_approval_for_medium_risk,
        "the medium-risk gate is inert unless autonomy is Supervised, so auto must opt in \
         explicitly or the mapping above buys nothing"
    );

    // The rest of the hardening is tier-independent and stays so.
    assert!(auto.block_high_risk_commands);
    assert!(!auto.allow_tool_install);
    assert!(!auto.auto_approve_all);
    assert!(auto.workspace_only);
}

/// The mapping above, proven where it actually bites: at openhuman's own
/// command gate, on the class that separates the two candidate mappings.
///
/// `auto_borrows_supervised_exec_security_rather_than_full` pins the two
/// fields; this pins what they *do*.
///
/// # Which class actually distinguishes the mappings
///
/// Worth stating, because the intuitive example is the wrong one. At
/// `gate_decision`, `Destructive` prompts under `Supervised` **and** under
/// `Full` — so `rm -rf /` cannot tell the two mappings apart, and a test
/// written around it would pass whichever mapping `autonomy_for` chose.
/// (`block_high_risk_commands` is a separate, unconditional refusal on the
/// `validate_command` path; it is not what `gate_decision` reports.)
///
/// The one class `Full` actually loosens is `Write`: `Supervised` prompts,
/// `Full` allows. That makes `Write` the whole of the difference here, and
/// it is the ordinary case rather than an exotic one — an unrecognised
/// command is classified `Write` by fail-closed default. So mapping `auto`
/// to `Full` would have let routine state-changing shell commands run
/// unprompted in workflow `tool_call` nodes, which is exactly the tier
/// inversion `autonomy_for` argues against.
///
/// Asserted on `CommandClass` directly rather than through
/// `classify_command`, so this pins the tier decision and not the
/// classifier's string heuristics.
#[test]
fn auto_gates_write_class_commands_exactly_as_supervised_does() {
    use oh::security::{CommandClass, GateDecision};
    let ws = Path::new("/tmp/oc-toolbelt-policy-auto-cmd");

    let auto = exec_security(ws, PolicyMode::Auto);
    let supervised = exec_security(ws, PolicyMode::Supervised);
    let full = exec_security(ws, PolicyMode::Full);

    // The load-bearing assertion: the class the two mappings disagree about.
    assert_eq!(
        auto.gate_decision(CommandClass::Write),
        GateDecision::Prompt,
        "a write-class command must still ask on an auto desk — mapping auto to Full would \
         let it run unprompted in a workflow tool_call node, which has no approval gate above \
         this policy"
    );
    assert_eq!(
        full.gate_decision(CommandClass::Write),
        GateDecision::Allow,
        "guard for the assertion above: if Full ever stops allowing Write, this test no \
         longer distinguishes the two mappings and must be rewritten"
    );

    // Everything else `auto` decides, it decides identically to `supervised`.
    for class in [
        CommandClass::Read,
        CommandClass::Write,
        CommandClass::Network,
        CommandClass::Install,
        CommandClass::Destructive,
    ] {
        assert_eq!(
            auto.gate_decision(class),
            supervised.gate_decision(class),
            "auto must gate {class:?} exactly as supervised does"
        );
    }

    // And it is not readonly either — an auto desk can still act.
    let readonly = exec_security(ws, PolicyMode::Readonly);
    assert_eq!(
        readonly.gate_decision(CommandClass::Write),
        GateDecision::Block
    );
}

#[tokio::test]
async fn shell_rm_rf_denied_under_readonly_parked_under_supervised() {
    use oh::security::GateDecision;
    let ws = std::env::temp_dir();

    // Readonly: a destructive command is hard-blocked by the policy — proven
    // both at the decision layer and end-to-end (the tool refuses before it
    // spawns; the harmless nonexistent path is a belt-and-braces target).
    let readonly = test_security(&ws, PolicyMode::Readonly);
    assert_eq!(
        readonly.gate_decision(readonly.classify_command("rm -rf /")),
        GateDecision::Block,
        "readonly must hard-block destructive commands"
    );
    let tool = ShellTool::new(readonly, native_runtime(), AuditLogger::disabled());
    let result = tool
        .execute(json!({ "command": "rm -rf /tmp/oc-toolbelt-nonexistent-xyz" }))
        .await
        .unwrap();
    assert!(
        result.is_error,
        "readonly rm -rf must error: {}",
        result.output()
    );
    assert!(result.output().to_lowercase().contains("read-only"));

    // Supervised: the destructive command is *parked* (requires approval),
    // never auto-allowed. The park→resolve step is opencompany's own
    // `ApprovalPolicy` gate layered above; here we prove OpenHuman's policy
    // classifies it as approval-required rather than allowed.
    let supervised = test_security(&ws, PolicyMode::Supervised);
    assert_eq!(
        supervised.gate_decision(supervised.classify_command("rm -rf /")),
        GateDecision::Prompt,
        "supervised must park (require approval for) destructive commands"
    );
}

/// High-risk commands are refused independently of the autonomy tier.
#[tokio::test]
async fn block_high_risk_commands_refuses_a_destructive_command_even_under_full_autonomy() {
    let ws = std::env::temp_dir();
    let full = test_security(&ws, PolicyMode::Full);
    let tool = ShellTool::new(full, native_runtime(), AuditLogger::disabled());
    let result = tool
        .execute(json!({ "command": "rm -rf /tmp/oc-toolbelt-conf002-nonexistent-xyz" }))
        .await
        .unwrap();
    assert!(
        result.is_error,
        "block_high_risk_commands=true must refuse a destructive command even under Full \
         autonomy, independent of the autonomy-tier gate: {}",
        result.output()
    );
}

#[tokio::test]
async fn shell_factory_blocks_high_risk_commands_on_every_execution_path() {
    let ws = tempfile::Builder::new()
        .prefix("oc-shell-guard-")
        .tempdir_in("/tmp")
        .unwrap();
    let target = ws.path().join("protected");
    std::fs::create_dir(&target).unwrap();
    let tools = shell_tools(
        test_security(ws.path(), PolicyMode::Full),
        native_runtime(),
        Some(ShellAudit::disabled()),
        ws.path(),
    );
    let tool = tools.iter().find(|tool| tool.name() == "shell").unwrap();
    let args = json!({ "command": format!("rm -rf {}", target.display()) });
    for result in [
        tool.execute(args.clone()).await.unwrap(),
        tool.execute_with_options(args.clone(), ToolCallOptions::default())
            .await
            .unwrap(),
        tool.execute_with_context(args, ToolCallOptions::default(), None)
            .await
            .unwrap(),
    ] {
        assert!(result.is_error, "{}", result.output());
        assert!(result.output().contains("high-risk"), "{}", result.output());
    }
    assert!(target.is_dir());
    let result = tool
        .execute(json!({ "command": "printf safe-command" }))
        .await
        .unwrap();
    assert!(!result.is_error, "{}", result.output());
    assert!(result.output().contains("safe-command"));
    assert_eq!(tool.permission_level(), PermissionLevel::Execute);
    assert_eq!(tool.max_result_size_chars(), Some(30_000));
    assert_eq!(
        tool.timeout_policy(&json!({ "timeout_secs": 17 })),
        ToolTimeout::Millis(17_000)
    );

    let audit_dir = tempfile::tempdir().unwrap();
    let audit = shell_audit(audit_dir.path()).unwrap();
    let sink = audit.sink.clone();
    let tools = shell_tools(
        test_security(ws.path(), PolicyMode::Readonly),
        native_runtime(),
        Some(audit),
        ws.path(),
    );
    let tool = tools.iter().find(|tool| tool.name() == "shell").unwrap();
    let command = format!("rm -rf {}", target.display());
    let result = tool.execute(json!({ "command": command })).await.unwrap();
    assert!(result.is_error, "{}", result.output());
    assert!(result.output().contains("read-only"), "{}", result.output());
    assert!(std::fs::read_to_string(sink).unwrap().contains(&command));
    assert!(target.is_dir());
}

#[test]
fn high_risk_guard_respects_the_flag_without_blocking_ordinary_commands() {
    let ws = tempfile::tempdir().unwrap();
    let enabled = HighRiskCommands(test_security(ws.path(), PolicyMode::Full));
    for command in [
        "printf safe",
        "touch note.txt",
        "curl https://example.invalid",
    ] {
        assert!(enabled.refusal(&json!({ "command": command })).is_none());
    }
    assert!(enabled.refusal(&json!({ "command": "sudo id" })).is_some());
    let mut security = exec_security(ws.path(), PolicyMode::Full);
    security.block_high_risk_commands = false;
    let disabled = HighRiskCommands(Arc::new(security));
    assert!(disabled.refusal(&json!({ "command": "sudo id" })).is_none());
}

#[test]
fn shell_timeout_policy_honors_its_schema_fallback_claim() {
    use tinytools::ToolTimeout;

    let ws = std::env::temp_dir();
    let security = test_security(&ws, PolicyMode::Full);
    let tool = ShellTool::new(security, native_runtime(), AuditLogger::disabled());

    let schema = tool.parameters_schema();
    let description = schema["properties"]["timeout_secs"]["description"]
        .as_str()
        .expect("timeout_secs has a description");
    assert!(
        description.contains("falls back to the configured tool timeout"),
        "the schema must describe the configured fallback: {description}"
    );

    assert_eq!(
        tool.timeout_policy(&json!({})),
        ToolTimeout::Inherit,
        "an omitted timeout must inherit the configured deadline"
    );
    assert_eq!(
        tool.timeout_policy(&json!({ "timeout_secs": 0 })),
        ToolTimeout::Inherit,
        "an invalid zero timeout must inherit the configured deadline"
    );
}

#[test]
fn shell_factory_preserves_explicit_deadlines_and_inherits_for_invalid_values() {
    let ws = tempfile::tempdir().unwrap();
    let tools = shell_tools(
        test_security(ws.path(), PolicyMode::Full),
        native_runtime(),
        Some(ShellAudit::disabled()),
        ws.path(),
    );
    let shell = tools.iter().find(|tool| tool.name() == "shell").unwrap();
    for args in [
        json!({}),
        json!({"timeout_secs": null}),
        json!({"timeout_secs": 0}),
        json!({"timeout_secs": -1}),
        json!({"timeout_secs": 1.5}),
        json!({"timeout_secs": "17"}),
        json!({"timeout_secs": 3601}),
        json!({"timeout_secs": u64::MAX}),
    ] {
        assert_eq!(
            shell.timeout_policy(&args),
            ToolTimeout::Inherit,
            "invalid or absent deadline must inherit: {args}"
        );
        let (deadline, seconds) =
            oh::tools::timeout::resolve_tool_deadline(shell.timeout_policy(&args));
        assert_eq!(
            deadline,
            Some(std::time::Duration::from_secs(seconds)),
            "the execution adapter must resolve a finite inherited deadline"
        );
        assert!(seconds > 0);
    }
    for secs in [1, 17, 3600] {
        assert_eq!(
            shell.timeout_policy(&json!({"timeout_secs": secs})),
            ToolTimeout::Millis(secs * 1000),
            "valid explicit deadline must survive the audit wrapper"
        );
    }
}

#[tokio::test]
async fn web_tools_reject_ssrf_ip_literals() {
    let ws = std::env::temp_dir();
    let security = test_security(&ws, PolicyMode::Full);
    // Open-public allowlist: proves the IP guard fires independently of the
    // domain allowlist. Both are IP literals, so rejection short-circuits
    // before any DNS lookup or network I/O.
    let tools = web_tools(security, Vec::new(), &ws);
    for tool in &tools {
        // Only web_fetch / http_request take a plain `url` arg.
        if !matches!(tool.name(), "web_fetch" | "http_request") {
            continue;
        }
        for url in ["http://169.254.169.254/", "http://127.0.0.1:1/"] {
            let result = tool.execute(json!({ "url": url })).await.unwrap();
            assert!(
                result.is_error,
                "{} must reject SSRF target {url}: {}",
                tool.name(),
                result.output()
            );
        }
    }
}

#[tokio::test]
async fn apply_patch_denied_outside_workspace() {
    // A private workspace root, not a fixed `/tmp` name: the old fixed name
    // made two concurrent runs of this test tear down each other's
    // directory between the create and the assertion.
    let ws_dir = tempfile::Builder::new()
        .prefix("oc-toolbelt-escape-")
        .tempdir()
        .expect("tempdir");
    let ws = ws_dir.path();
    let security = test_security(ws, PolicyMode::Full);
    let tool = ApplyPatchTool::new(security);
    // A path escaping the workspace root must be refused by the policy.
    let result = tool
        .execute(json!({
            "edits": [
                { "path": "../outside.txt", "old_string": "x", "new_string": "y" }
            ]
        }))
        .await
        .unwrap();
    assert!(
        result.is_error,
        "workspace escape must be denied: {}",
        result.output()
    );
}
