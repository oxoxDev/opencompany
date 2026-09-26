use super::*;
use oh::tools::status::FailureCategory;

/// An obvious fake, in the same shape `approval_display`'s tests use. Never
/// a credential pattern that could be mistaken for a real one in a diff.
pub(crate) const FAKE_SECRET: &str = "NOT-A-REAL-KEY-planted-for-tests";

/// The refusal OpenHuman hands the model when *our* approval policy parks a
/// call, reproduced in the shape `PolicyDenial::ApprovalRequired::render`
/// produces. `PolicyDenial` is crate-private upstream, so this is a copy —
/// which is exactly why
/// [`approval_needle_still_appears_in_the_vendored_denial_render`] pins the
/// needle against the real source.
pub(crate) fn approval_refusal(tool: &str) -> String {
    format!(
        "Blocked: Tool '{tool}' requires approval under policy '{POLICY_NAME}'. \
         Reason: '{tool}' has an external effect and this company runs supervised. \
         Workaround: Ask the user to approve this action, then retry. \
         Relay this to the user: explain what was blocked and why."
    )
}

/// The lockstep [`INTRINSIC_TOOLS`] claims, made mechanical. Every name it
/// carries is a `pub const` on the module that wires the tool, so drift
/// between the lists fails here instead of silently downgrading a tool's own
/// sentence to a bare failure class (which is how `assign_task` /
/// `review_task` sat missing from #186 until #461, and how the whole
/// workspace family sat missing until #887).
#[test]
fn intrinsic_tools_covers_every_oc_authored_tool() {
    use crate::harness::approval_tool::REQUEST_APPROVAL_TOOL;
    use crate::harness::orchestrator::{
        ADD_AGENT_TOOL, ASSIGN_TASK_TOOL, CREATE_WORKFLOW_TOOL, QUERY_COMPANY_TOOL,
        READ_RUN_OUTPUT_TOOL, REVIEW_TASK_TOOL, RUN_WORKFLOW_TOOL,
    };
    use crate::harness::workflow_admin::{
        DELETE_WORKFLOW_TOOL, READ_WORKFLOW_TOOL, UPDATE_WORKFLOW_TOOL,
    };
    use crate::harness::workspace_tools::{
        WORKSPACE_CREATE_TOOL, WORKSPACE_DELETE_TOOL, WORKSPACE_LIST_TOOL, WORKSPACE_READ_TOOL,
        WORKSPACE_RENAME_TOOL, WORKSPACE_SEARCH_TOOL, WORKSPACE_WRITE_TOOL,
    };
    use crate::runtime::delegation_tools::{
        DELEGATE_TO_DESK_TOOL, DELEGATE_TO_TEAMMATE_TOOL, SPAWN_TASK_TOOL,
    };

    let expected = [
        REQUEST_APPROVAL_TOOL,
        QUERY_COMPANY_TOOL,
        SPAWN_TASK_TOOL,
        DELEGATE_TO_DESK_TOOL,
        DELEGATE_TO_TEAMMATE_TOOL,
        RUN_WORKFLOW_TOOL,
        READ_RUN_OUTPUT_TOOL,
        CREATE_WORKFLOW_TOOL,
        READ_WORKFLOW_TOOL,
        UPDATE_WORKFLOW_TOOL,
        DELETE_WORKFLOW_TOOL,
        ADD_AGENT_TOOL,
        ASSIGN_TASK_TOOL,
        REVIEW_TASK_TOOL,
        // Issue #887. The whole family, because a tool's refusal is worth
        // exactly as much on a write as on a read — and because leaving
        // siblings out is how a list like this rots.
        WORKSPACE_LIST_TOOL,
        WORKSPACE_READ_TOOL,
        WORKSPACE_SEARCH_TOOL,
        WORKSPACE_CREATE_TOOL,
        WORKSPACE_WRITE_TOOL,
        WORKSPACE_RENAME_TOOL,
        WORKSPACE_DELETE_TOOL,
    ];
    for name in expected {
        assert!(
            INTRINSIC_TOOLS.contains(&name),
            "{name} is a wired OC-authored tool but is absent from INTRINSIC_TOOLS"
        );
    }
    // Exact, not just covering: a name here that no longer exists upstream
    // would surface a stale tool's output as OC-authored copy.
    assert_eq!(INTRINSIC_TOOLS.len(), expected.len(), "{INTRINSIC_TOOLS:?}");
}

/// The catch-all this issue is named after must still be reachable — for
/// the tools that genuinely have nothing OC-authored to say.
///
/// Without this, "surface the tool's message" could be implemented as
/// "surface every tool's message", which is the `mcp_call_tool` leak the
/// membership rule exists to prevent. So the same failing output is
/// asserted BOTH ways round: verbatim for a workspace tool, collapsed to
/// the class for a remote one.
#[test]
fn a_non_intrinsic_tools_output_is_still_collapsed_to_its_class() {
    let output = "Could not read `standards/engineering-standards.md`: the workspace store \
                  failed (store_io).";
    let classified = oh::tools::status::classify(output, false);

    let intrinsic = failure_result("workspace_read", output, &classified, None);
    assert_eq!(intrinsic.as_deref(), Some(output));

    let remote = failure_result("mcp_call_tool", output, &classified, None);
    assert_eq!(remote.as_deref(), Some(classified.cause_plain.trim()));
    assert_ne!(
        remote.as_deref(),
        Some(output),
        "a remote server's body must never leave this module as content"
    );
}

/// Reads a file out of the vendored OpenHuman checkout, for the tests that
/// couple a string needle to the source that produces it.
///
/// A missing file is reported as a **moved** file, because that is what it
/// almost always is. These paths point into a submodule that reorganises on
/// its own schedule — the #499 pin bump moved both of them in one step
/// (`openhuman/tinyagents/` → `openhuman/agent/tinyagents/`) — and a bare
/// "unreadable: No such file or directory" reads as a broken test rather
/// than as the vendored tree having been rearranged underneath it. The
/// needle these tests pin may well still exist; only its address changed.
/// **The module, not one file.** Upstream splits a large module into
/// `foo.rs` + `foo_part_NN.rs` as it grows, and the #5767-era pin moved both
/// needles below out of their parent and into a `_part_02`. The parent still
/// exists — it just declares the parts now — so reading it alone finds
/// nothing and the canary fires as though the *behaviour* changed. Reading
/// the siblings keeps the coupling honest across a split: what these tests
/// pin is the needle, not the file it lives in.
pub(crate) fn vendored(relative: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative);
    let mut all = read_vendored(&path, relative);
    if let (Some(dir), Some(stem)) = (path.parent(), path.file_stem()) {
        let prefix = format!("{}_part_", stem.to_string_lossy());
        let mut parts: Vec<_> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".rs"))
            })
            .collect();
        // Deterministic order, so a failure message is reproducible.
        parts.sort();
        for part in parts {
            all.push('\n');
            all.push_str(&std::fs::read_to_string(&part).unwrap_or_default());
        }
    }
    all
}

pub(crate) fn read_vendored(path: &std::path::Path, relative: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|err| {
        let basename = relative.rsplit('/').next().unwrap_or(relative);
        panic!(
            "vendored source {} is unreadable: {err}\n\
             If the vendored openhuman pin moved, this file most likely moved with it \
             rather than being deleted — the basenames survive reorgs, the parents do not. \
             Find its new address and update this path:\n    \
             git -C vendor/openhuman ls-files '*{basename}'\n\
             Do NOT relax the assertion: the needle is what couples us to that source.",
            path.display()
        )
    })
}

pub(crate) fn started(call_id: &str, tool: &str, label: Option<&str>) -> AgentProgress {
    AgentProgress::ToolCallStarted {
        call_id: call_id.to_string(),
        tool_name: tool.to_string(),
        // The tinyagents path sends Null here; mirror that.
        arguments: Value::Null,
        iteration: 1,
        display_label: label.map(str::to_string),
        display_detail: None,
    }
}

pub(crate) fn completed(
    call_id: &str,
    tool: &str,
    success: bool,
    output: &str,
    arguments: Option<Value>,
    failure: Option<ClassifiedFailure>,
) -> AgentProgress {
    AgentProgress::ToolCallCompleted {
        call_id: call_id.to_string(),
        tool_name: tool.to_string(),
        success,
        output_chars: output.chars().count(),
        output: output.to_string(),
        arguments,
        elapsed_ms: 42,
        iteration: 1,
        failure,
        display_label: None,
        display_detail: None,
        structured: None,
    }
}

pub(crate) fn thinking(delta: &str) -> AgentProgress {
    AgentProgress::ThinkingDelta {
        delta: delta.to_string(),
        iteration: 1,
    }
}

pub(crate) fn text(delta: &str) -> AgentProgress {
    AgentProgress::TextDelta {
        delta: delta.to_string(),
        iteration: 1,
    }
}

pub(crate) fn classified(class: ToolFailureClass, cause: &str) -> ClassifiedFailure {
    ClassifiedFailure {
        class,
        category: FailureCategory::Recoverable,
        cause_plain: cause.to_string(),
        next_action: "try again".to_string(),
        recoverable: true,
    }
}

/// Folds a single completed call and hands back its step.
pub(crate) fn one(tool: &str, success: bool, output: &str, arguments: Option<Value>) -> TurnStep {
    let steps = fold_steps(vec![completed(
        "c1", tool, success, output, arguments, None,
    )]);
    assert_eq!(steps.len(), 1, "expected exactly one step: {steps:?}");
    steps.into_iter().next().expect("a step")
}
