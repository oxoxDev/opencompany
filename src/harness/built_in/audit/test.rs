//! Issue #775: the audit sink is out of the agent's reach, and a command that
//! could not be recorded does not run.
//!
//! Modelled on `crate::harness::repo`'s
//! `a_checkout_cannot_reach_the_mirror_it_came_from`: mount the attack with the
//! **real** tools an agent holds, then compare the protected bytes before and
//! after. An assertion that some path "is outside the workspace" would pass on a
//! broken sandbox too.
//!
//! # The honest limit of everything below
//!
//! These prove the **tool path**. Nothing here can prove the shell cannot delete
//! the host-side file, because it can — same uid, same filesystem, no sandbox
//! (`docs/spec/security/agent-isolation.md` §1). What moved is that the
//! *sanctioned* write paths now refuse the sink, and that a command which
//! destroys it has already had its intent line fsynced. Do not read a green run
//! here as tamper-evidence.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::json;

use oh::security::SecurityPolicy;
use oh::tools::{Tool, ToolResult};

use super::*;
use crate::harness::build::{file_tools, workspace_security};
use crate::harness::policy::PolicyMode;
use crate::harness::toolbelt::{exec_security, native_runtime, shell_audit, shell_tools};
use crate::ports::types::CompanyId;
use crate::store::DataLayout;

/// A pre-seeded audit line, so "the file is unchanged" is a claim about content
/// rather than about a file that was empty either way.
const SEEDED: &str = "{\"seeded\":\"pre-existing audit history\"}\n";

/// One tenant data root laid out the way a real instance is: `harness/` for
/// agent workspaces, `companies/` for the host-owned trees beside it.
struct Tenant {
    root: tempfile::TempDir,
    company: CompanyId,
    agent: String,
}

impl Tenant {
    fn new(agent: &str) -> Self {
        Self {
            root: tempfile::Builder::new()
                .prefix("oc-audit-")
                .tempdir()
                .expect("tempdir"),
            company: CompanyId::new("acme"),
            agent: agent.to_string(),
        }
    }

    fn data_root(&self) -> &Path {
        self.root.path()
    }

    fn workspace_root(&self) -> PathBuf {
        self.data_root().join("harness")
    }

    /// `harness/<company>/<agent>/workspace`, created — the file tools refuse
    /// relative paths without it.
    fn workspace(&self) -> PathBuf {
        let ws = crate::harness::build::agent_workspace(
            &self.workspace_root(),
            &self.company,
            &self.agent,
        );
        std::fs::create_dir_all(&ws).expect("workspace");
        ws
    }

    /// `companies/<slug>/audit/<agent>` — host-owned, outside every workspace.
    fn audit_dir(&self) -> PathBuf {
        crate::harness::build::agent_audit_dir(self.data_root(), &self.company, &self.agent)
    }

    fn audit_dir_for(&self, agent: &str) -> PathBuf {
        crate::harness::build::agent_audit_dir(self.data_root(), &self.company, agent)
    }
}

/// Seed `dir/audit.log` with a known line and hand back its path.
fn seed_sink(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).expect("audit dir");
    let sink = dir.join("audit.log");
    std::fs::write(&sink, SEEDED).expect("seed");
    sink
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// Look one tool up out of a built vector by name.
fn tool_named<'a>(tools: &'a [Box<dyn Tool>], name: &str) -> &'a dyn Tool {
    tools
        .iter()
        .find(|t| t.name() == name)
        .map(std::convert::AsRef::as_ref)
        .unwrap_or_else(|| panic!("no `{name}` tool in the built vector"))
}

/// Every write an agent can aim at one path through its granted file tools.
///
/// Returned as (label, tool name, args) so a failure names which shape got
/// through, and so the same table can be pointed at a permitted path to prove
/// the harness is not simply broken. `edit`'s runtime name is `edit`, not
/// `edit_file` — the type is `EditFileTool` but the tool the model calls is not.
fn write_attempts(target: &str) -> Vec<(&'static str, &'static str, serde_json::Value)> {
    vec![
        (
            "file_write",
            "file_write",
            json!({ "path": target, "content": "OWNED\n" }),
        ),
        (
            "edit",
            "edit",
            json!({ "path": target, "old_string": "seeded", "new_string": "OWNED" }),
        ),
    ]
}

/// Drive one attempt and return the result.
async fn attempt(tools: &[Box<dyn Tool>], name: &str, args: serde_json::Value) -> ToolResult {
    tool_named(tools, name)
        .execute(args)
        .await
        .expect("the tool must answer rather than panic")
}

// ---------------------------------------------------------------------------
// T1 — the headline: the agent's own file tools cannot reach the sink
// ---------------------------------------------------------------------------

/// **The attack this whole change exists to prevent.**
///
/// The agent workspace is the `workspace_only` `SecurityPolicy` root the file
/// tools enforce, so while `audit.log` lived inside it, rewriting the record of
/// an agent's own commands was a **policy-permitted** write — not something the
/// agent had to reach for `shell` to do. Every shape an agent can aim at a path
/// through those tools is mounted here, and then the sink's bytes are compared.
///
/// # Read the three attempts separately — they are refused for different reasons
///
/// Being precise about this is the difference between a test and a talisman:
///
/// * The **relative** write (`audit.log`, resolved against the workspace) is the
///   material one. It is *fully permitted* — the policy does not refuse it, and
///   the assertion is not that it fails but that it lands somewhere harmless.
///   Before #775 that exact call rewrote the audit trail. It is also the control:
///   it proves the tools work and the harness is not simply refusing everything.
/// * The **absolute** write at the sink is refused, but by `workspace_only`'s
///   blanket "no absolute paths" rule — it would be refused for any absolute
///   path, including one inside the workspace. Worth asserting, not worth
///   crediting to this change.
/// * The **`../` traversal** is refused by the parent-component check, likewise
///   independent of where the sink is.
///
/// So the boundary this change actually moved is the first bullet, and
/// `the_old_in_workspace_sink_is_rewritable_which_is_why_it_moved` is the proof
/// that it moved.
#[tokio::test]
async fn an_agent_cannot_rewrite_its_own_audit_log_through_its_file_tools() {
    let tenant = Tenant::new("ceo");
    let workspace = tenant.workspace();
    let sink = seed_sink(&tenant.audit_dir());
    let before = read(&sink);

    let tools = file_tools(&workspace);

    // (i) THE MATERIAL ONE, and the control. The natural, fully-permitted call —
    //     the very call that used to hit the audit trail. It must succeed, and
    //     it must land on a file that is not the sink.
    let result = attempt(
        &tools,
        "file_write",
        json!({ "path": "audit.log", "content": "OWNED\n" }),
    )
    .await;
    assert!(
        !result.is_error,
        "control: the relative in-workspace write must SUCCEED — if it were refused, \
         the refusals below would be a broken harness rather than a boundary: {}",
        result.output()
    );
    assert_eq!(
        read(&workspace.join("audit.log")),
        b"OWNED\n",
        "control: the permitted write must actually land inside the workspace"
    );

    // (ii) The absolute path at the sink, through both write-capable tools.
    for (label, name, args) in write_attempts(&sink.to_string_lossy()) {
        let result = attempt(&tools, name, args).await;
        assert!(
            result.is_error,
            "absolute `{label}` at the audit sink must be refused: {}",
            result.output()
        );
    }

    // (iii) The determined attempt: climb out of the workspace by relative
    //       path. `harness/<company>/<agent>/workspace` → the data root is four
    //       levels up, then down into the host-owned tree.
    let traversal = format!(
        "../../../../companies/{}/audit/{}/audit.log",
        tenant.company.as_ref(),
        tenant.agent,
    );
    for (label, name, args) in write_attempts(&traversal) {
        let result = attempt(&tools, name, args).await;
        assert!(
            result.is_error,
            "`{label}` via `../` traversal must be refused: {}",
            result.output()
        );
    }

    // What actually matters: not one byte moved.
    assert_eq!(
        before,
        read(&sink),
        "the audit sink changed — one of the attempts above got through"
    );
}

/// The teeth of the test above, proven by re-arming the bug.
///
/// Put the sink back where it used to live — inside the agent workspace, which
/// is what `shell_audit(&workspace)` builds — and the *same* permitted call with
/// the *same* tools truncates the real audit trail.
///
/// This is what makes the sibling test mean something. Without it, "the relative
/// write landed somewhere harmless" is a statement about a filename; with it,
/// that filename is demonstrably the one that used to be load-bearing.
#[tokio::test]
async fn the_old_in_workspace_sink_is_rewritable_which_is_why_it_moved() {
    let tenant = Tenant::new("ceo");
    let workspace = tenant.workspace();

    // The pre-#775 layout, built through the real factory: the sink IS the
    // workspace's own `audit.log`.
    let audit = shell_audit(&workspace).expect("a workspace-rooted sink builds");
    assert_eq!(
        audit.sink,
        workspace.join("audit.log"),
        "this test only means anything if it reconstructs the OLD layout"
    );
    let tool = audited_shell(
        Arc::new(exec_security(&workspace, PolicyMode::Full)),
        native_runtime(),
        audit.clone(),
    );
    tool.execute(json!({ "command": "echo history-worth-hiding" }))
        .await
        .expect("tool answers");
    let recorded = String::from_utf8(read(&audit.sink)).expect("utf-8");
    assert!(
        recorded.contains("history-worth-hiding"),
        "precondition: the command must be in the legacy sink: {recorded}"
    );

    // One ordinary, permitted file-tool call — no traversal, no absolute path,
    // no `shell`.
    let tools = file_tools(&workspace);
    let result = attempt(
        &tools,
        "file_write",
        json!({ "path": "audit.log", "content": "" }),
    )
    .await;
    assert!(
        !result.is_error,
        "the pre-#775 in-workspace sink must be truncatable — if this ever starts \
         failing, the boundary moved somewhere else and the sibling test no longer \
         proves what it claims: {}",
        result.output()
    );
    assert_eq!(
        read(&audit.sink),
        b"",
        "the legacy sink was truncated by a POLICY-PERMITTED write: this is exactly \
         the bug #775 fixed by moving the sink out of the workspace"
    );
}

/// The path-shape half, stated once so a reader does not have to infer it from
/// the attack: no spelling of the audit sink lands inside the workspace tree.
#[test]
fn the_audit_sink_never_lands_under_the_workspace_tree() {
    let tenant = Tenant::new("ceo");
    let workspace_root = tenant.workspace_root();
    let audit = tenant.audit_dir();
    assert!(
        !audit.starts_with(&workspace_root),
        "{} is inside the agent-workspace tree {}",
        audit.display(),
        workspace_root.display(),
    );
    // And it is exactly the layout's own answer — the harness does not
    // transcribe the path a second time.
    assert_eq!(
        audit,
        DataLayout::new(tenant.data_root()).agent_audit_dir(tenant.company.as_ref(), &tenant.agent),
    );
}

// ---------------------------------------------------------------------------
// T2 — a real shell call writes where we say it does, and nowhere else
// ---------------------------------------------------------------------------

/// One real command through the wired `shell` toolbelt: the record lands at
/// `companies/<slug>/audit/<agent>/audit.log`, and no `audit.log` appears
/// anywhere under the agent workspace.
///
/// The negative half is the one that would have caught the original bug — an
/// implementation that wrote to *both* places would satisfy the positive
/// assertion on its own.
#[tokio::test]
async fn a_real_shell_call_records_only_into_the_host_owned_sink() {
    let tenant = Tenant::new("ceo");
    let workspace = tenant.workspace();
    let audit_dir = tenant.audit_dir();

    let tools = shell_tools(
        Arc::new(exec_security(&workspace, PolicyMode::Full)),
        native_runtime(),
        shell_audit(&audit_dir),
        &workspace,
    );
    assert!(
        !tools.is_empty(),
        "the shell namespace must be wired when the sink is writable"
    );

    let result = attempt(&tools, "shell", json!({ "command": "echo audited" })).await;
    assert!(
        !result.is_error,
        "the command must run: {}",
        result.output()
    );

    let sink = audit_dir.join("audit.log");
    let recorded = String::from_utf8(read(&sink)).expect("utf-8 audit log");
    assert!(
        recorded.contains("echo audited"),
        "the command must be recorded in {}: {recorded}",
        sink.display(),
    );

    assert!(
        !workspace.join("audit.log").exists(),
        "an audit.log appeared in the agent workspace — the sink did not move"
    );
    assert!(
        find_audit_logs(&tenant.workspace_root()).is_empty(),
        "audit logs found under the workspace tree: {:?}",
        find_audit_logs(&tenant.workspace_root()),
    );
}

/// Every `audit.log` under `dir`, recursively. Small trees only — this is a test
/// helper, not the bounded production scan.
fn find_audit_logs(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().and_then(|n| n.to_str()) == Some("audit.log") {
                found.push(p);
            }
        }
    }
    found
}

// ---------------------------------------------------------------------------
// T3 — an unwritable sink refuses the command, and the command does not run
// ---------------------------------------------------------------------------

/// Make the sink path unappendable **without** relying on file permissions: put
/// a directory where the log file goes, so `OpenOptions::append().open()` fails
/// with `EISDIR`.
///
/// `chmod` was the obvious mechanism and is the wrong one: CI or a container may
/// run as root, for whom a read-only directory is not read-only, and the test
/// would pass vacuously by executing the command it was supposed to refuse. This
/// failure mode is uid-independent. The *cause* is incidental anyway — what is
/// under test is "append failed ⇒ refuse", not any particular reason it failed.
fn wedge_sink(audit_dir: &Path) -> PathBuf {
    std::fs::create_dir_all(audit_dir).expect("audit dir");
    let sink = audit_dir.join("audit.log");
    std::fs::create_dir(&sink).expect("wedge the sink path with a directory");
    sink
}

/// A sink that cannot be appended to **refuses the command**, and the command
/// does not run.
///
/// This is the second half of issue #775, and the half the init-time gate never
/// covered: upstream's `emit_audit` is post-execution and warn-and-continue by
/// design, so before this a sink that broke mid-session yielded commands running
/// with zero record — and an attacker holding `shell` can break it deliberately
/// by filling the volume. The full-disk-becomes-a-shell-outage trade is accepted
/// on purpose: an outage is the safe direction, and it is the same choice boot
/// makes on an unwritable journal root.
///
/// "Did not run" is asserted through a side effect, not through the returned
/// message — a refusal that still spawned the command would produce the same
/// text.
#[tokio::test]
async fn an_unwritable_sink_refuses_the_command_and_it_never_runs() {
    let tenant = Tenant::new("ceo");
    let workspace = tenant.workspace();
    let audit_dir = tenant.audit_dir();
    let sink = wedge_sink(&audit_dir);

    // The marker is the proof. If it exists afterwards, the command ran.
    let marker = workspace.join("it-ran.txt");
    let command = format!("echo ran > {}", marker.display());

    let tool = audited_shell(
        Arc::new(exec_security(&workspace, PolicyMode::Full)),
        native_runtime(),
        crate::harness::toolbelt::ShellAudit {
            logger: oh::security::get_or_create_workspace_audit_logger(
                oh::config::AuditConfig::default(),
                audit_dir.to_path_buf(),
            )
            .expect("the logger builds; it is the APPEND that fails"),
            sink: sink.clone(),
        },
    );

    let result = tool
        .execute(json!({ "command": command }))
        .await
        .expect("the tool answers rather than panics");

    assert!(result.is_error, "the call must fail: {}", result.output());
    assert!(
        result.output().contains(&sink.display().to_string()),
        "the refusal must NAME the sink it could not write — that path is the whole \
         diagnosis for an operator staring at a dead shell: {}",
        result.output(),
    );
    assert!(
        !marker.exists(),
        "the command RAN despite the refusal — this is precisely the unaudited \
         shell the fail-closed gate exists to prevent"
    );
}

/// The writable counterpart, which is what makes the refusal above meaningful:
/// the same tool, the same command, a sink that works — the command runs, and
/// the **intent line precedes the result line** in the file.
///
/// Ordering is the load-bearing property. A post-execution-only record cannot
/// survive a command that destroys the sink or kills the process; an intent line
/// fsynced first can.
#[tokio::test]
async fn a_writable_sink_records_intent_before_the_result() {
    let tenant = Tenant::new("ceo");
    let workspace = tenant.workspace();
    let audit_dir = tenant.audit_dir();

    let tool = audited_shell(
        Arc::new(exec_security(&workspace, PolicyMode::Full)),
        native_runtime(),
        shell_audit(&audit_dir).expect("a writable sink builds a logger"),
    );

    let marker = workspace.join("it-ran.txt");
    let result = tool
        .execute(json!({ "command": format!("echo ran > {}", marker.display()) }))
        .await
        .expect("tool answers");
    assert!(
        !result.is_error,
        "the command must run: {}",
        result.output()
    );
    assert!(marker.exists(), "the command must have actually run");

    let log = String::from_utf8(read(&audit_dir.join("audit.log"))).expect("utf-8");
    let intent_line = log
        .lines()
        .position(|line| line.contains(INTENT_CHANNEL))
        .expect("an intent line must be present");
    // The vendored result line is the one carrying a populated `result` object.
    // `"result":null` is NOT a discriminator — the field is serialized either
    // way, so the intent line contains the key too; only the value differs.
    let result_line = log
        .lines()
        .position(|line| line.contains("\"result\":{"))
        .expect("a post-execution result line must be present");
    assert!(
        intent_line < result_line,
        "the intent line must be written BEFORE the result line — a record that only \
         exists after execution cannot survive the command that destroys it.\n{log}"
    );
    assert!(
        log.lines()
            .nth(intent_line)
            .is_some_and(|l| l.contains("\"result\":null")),
        "the intent line must carry no populated result — that absence is how a reader \
         tells the two phases apart.\n{log}"
    );
}

// ---------------------------------------------------------------------------
// T4 — one directory per agent, because the vendored registry is first-config-wins
// ---------------------------------------------------------------------------

/// Two agents in one company get two distinct files; the same agent asked twice
/// gets the same logger and its appends accumulate.
///
/// This pins a trap rather than a preference. OpenHuman's
/// `get_or_create_workspace_audit_logger` caches one logger per **directory**
/// and the *first* caller's config wins, so a design that put both agents'
/// per-agent log files in one shared directory would silently hand the second
/// agent the first agent's file — one company's whole shell history in one blob,
/// attributed to whoever built first.
#[tokio::test]
async fn each_agent_gets_its_own_sink_and_one_agent_gets_one_logger() {
    let tenant = Tenant::new("ceo");
    let workspace = tenant.workspace();
    let security = Arc::new(exec_security(&workspace, PolicyMode::Full));

    let ceo_dir = tenant.audit_dir_for("ceo");
    let cto_dir = tenant.audit_dir_for("cto");
    assert_ne!(ceo_dir, cto_dir, "two agents must not share a directory");

    let ceo = audited_shell(
        security.clone(),
        native_runtime(),
        shell_audit(&ceo_dir).expect("ceo sink"),
    );
    let cto = audited_shell(
        security.clone(),
        native_runtime(),
        shell_audit(&cto_dir).expect("cto sink"),
    );

    ceo.execute(json!({ "command": "echo ceo-one" }))
        .await
        .expect("ceo runs");
    cto.execute(json!({ "command": "echo cto-only" }))
        .await
        .expect("cto runs");

    let ceo_log = String::from_utf8(read(&ceo_dir.join("audit.log"))).expect("utf-8");
    let cto_log = String::from_utf8(read(&cto_dir.join("audit.log"))).expect("utf-8");
    assert!(ceo_log.contains("echo ceo-one"), "{ceo_log}");
    assert!(
        !ceo_log.contains("echo cto-only"),
        "one agent's commands leaked into another's sink: {ceo_log}"
    );
    assert!(cto_log.contains("echo cto-only"), "{cto_log}");
    assert!(
        !cto_log.contains("echo ceo-one"),
        "one agent's commands leaked into another's sink: {cto_log}"
    );

    // The same agent, resolved twice: one shared logger, appends accumulate.
    let ceo_again = audited_shell(
        security,
        native_runtime(),
        shell_audit(&ceo_dir).expect("ceo sink again"),
    );
    ceo_again
        .execute(json!({ "command": "echo ceo-two" }))
        .await
        .expect("ceo runs again");
    let ceo_log = String::from_utf8(read(&ceo_dir.join("audit.log"))).expect("utf-8");
    assert!(
        ceo_log.contains("echo ceo-one") && ceo_log.contains("echo ceo-two"),
        "a second resolution of one agent's sink must append, not replace: {ceo_log}"
    );
}

// ---------------------------------------------------------------------------
// The wrapper is transparent
// ---------------------------------------------------------------------------

/// The wrapper must be indistinguishable from a bare `ShellTool` in everything
/// except the intent append.
///
/// `name()` in particular is load-bearing well beyond cosmetics:
/// [`namespace_of`](crate::harness::toolbelt::namespace_of) keys the whole grant
/// gate on the literal `"shell"`, and a workflow `tool_call` node's slug is
/// looked up by the same string. A wrapper that renamed the tool would silently
/// make it ungateable *and* unreachable.
#[test]
fn the_wrapper_delegates_the_whole_advertised_surface() {
    let workspace = std::env::temp_dir();
    let security = Arc::new(exec_security(&workspace, PolicyMode::Supervised));
    let bare = oh::tools::ShellTool::new(
        security.clone(),
        native_runtime(),
        oh::security::AuditLogger::disabled(),
    );
    let wrapped = audited_shell(
        security,
        native_runtime(),
        crate::harness::toolbelt::ShellAudit::disabled(),
    );

    assert_eq!(wrapped.name(), bare.name());
    assert_eq!(
        wrapped.name(),
        "shell",
        "the grant gate keys on this literal"
    );
    assert_eq!(
        crate::harness::toolbelt::namespace_of(wrapped.name()),
        Some("shell"),
    );
    assert_eq!(wrapped.description(), bare.description());
    assert_eq!(wrapped.parameters_schema(), bare.parameters_schema());
    assert_eq!(wrapped.permission_level(), bare.permission_level());
    assert_eq!(
        wrapped.max_result_size_chars(),
        bare.max_result_size_chars()
    );

    let args = json!({ "command": "rm -rf /", "timeout_secs": 12 });
    assert_eq!(wrapped.timeout_policy(&args), bare.timeout_policy(&args));
    assert_eq!(
        wrapped.external_effect_with_args(&args),
        bare.external_effect_with_args(&args),
        "the approval gate reads this — a wrapper that answered `false` would route \
         a destructive command around the human"
    );
    assert_eq!(
        wrapped.permission_level_with_args(&args),
        bare.permission_level_with_args(&args),
    );
    assert_eq!(wrapped.spec().name, bare.spec().name);
}

/// The policy still refuses what it always refused: wrapping does not turn a
/// readonly desk's destructive command into an allowed one. The intent line is
/// written first either way — a refused command is still an attempted command,
/// and that is worth having on disk.
#[tokio::test]
async fn wrapping_does_not_loosen_the_security_policy() {
    let tenant = Tenant::new("ceo");
    let workspace = tenant.workspace();
    let audit_dir = tenant.audit_dir();

    let tool = audited_shell(
        Arc::new(exec_security(&workspace, PolicyMode::Readonly)),
        native_runtime(),
        shell_audit(&audit_dir).expect("sink"),
    );
    let result = tool
        .execute(json!({ "command": "rm -rf /tmp/oc-audit-nonexistent-xyz" }))
        .await
        .expect("tool answers");
    assert!(
        result.is_error,
        "readonly must still refuse a destructive command: {}",
        result.output()
    );
    assert!(result.output().to_lowercase().contains("read-only"));

    let log = String::from_utf8(read(&audit_dir.join("audit.log"))).expect("utf-8");
    assert!(
        log.contains(INTENT_CHANNEL) && log.contains("oc-audit-nonexistent-xyz"),
        "an attempted-but-refused command must still be recorded: {log}"
    );
}

/// A guard for the helper the whole suite leans on: `workspace_security` and
/// `exec_security` must both root at the workspace, or "outside the workspace"
/// means nothing in the tests above.
#[test]
fn both_policies_root_at_the_agent_workspace() {
    let tenant = Tenant::new("ceo");
    let workspace = tenant.workspace();

    let files: SecurityPolicy = workspace_security(&workspace);
    assert!(files.workspace_only);
    assert_eq!(files.action_dir, workspace);

    let exec = exec_security(&workspace, PolicyMode::Full);
    assert!(exec.workspace_only);
    assert_eq!(exec.action_dir, workspace);
}
