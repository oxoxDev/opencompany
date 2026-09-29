use super::steps_fixtures_tests::*;
use super::*;

// #924: a missing path is not a missing app
// -----------------------------------------------------------------------

/// The bare operating-system `ENOENT` string, which is what both tools in
/// issue #924 actually returned. Upstream's classifier routes this to
/// `MissingApp` on text alone.
const ENOENT: &str = "No such file or directory (os error 2)";

/// **The two failures issue #924 reports**, verbatim in the shape their
/// producers emit, driven end to end through the fold.
///
/// `grep`'s comes from openhuman's `validate_path`, which joins the
/// caller's sub-path onto the agent's *own* workspace and canonicalizes it —
/// so a company note path like `agents/…`, which the sandboxed file tools
/// cannot see, fails here rather than anywhere more informative.
/// `read_skill_resource`'s comes from its `symlink_metadata` pre-check on a
/// `references/` file that the skill does not bundle.
///
/// Neither host has an app to install, which is what made "App unavailable"
/// unactionable on a server tenant.
#[test]
fn a_path_tools_missing_file_is_not_reported_as_a_missing_app() {
    for (tool, output) in [
        (
            "grep",
            format!("Failed to resolve path 'agents/Product Manager/notes': {ENOENT}"),
        ),
        (
            crate::harness::skills::READ_SKILL_RESOURCE_TOOL,
            format!(
                "read_skill_resource: failed to stat resource \
                 /data/companies/acme/skills/feature-spec/references/spec.md: {ENOENT}"
            ),
        ),
    ] {
        // Precondition: upstream really does call this a missing app, so
        // this test is exercising the re-read and not a changed upstream.
        assert!(
            matches!(
                oh::tools::status::classify(&output, false).class,
                ToolFailureClass::MissingApp
            ),
            "upstream no longer calls `{tool}`'s ENOENT a missing app; \
             the re-read in `refine_missing_app` may be obsolete"
        );

        let step = one(tool, false, &output, None);
        assert_eq!(
            step.failure,
            Some(TurnStepFailure::NotFound),
            "`{tool}` reads a path in this process; there is nothing to install: {step:?}"
        );
        let result = step.result.expect("a failed step states its cause");
        assert!(
            result.contains("does not exist"),
            "the cause must name the real problem: {result:?}"
        );
        assert!(
            !result.to_lowercase().contains("install"),
            "a server operator cannot install anything to fix a missing note: {result:?}"
        );
    }
}

/// The other half of the same `ENOENT`, and the reason this is keyed on the
/// tool rather than the message: `Command::new` on a binary that is not
/// installed yields the *same* string with none of upstream's
/// program-specific needles. `shell` can genuinely be missing an app, so its
/// verdict must survive untouched.
#[test]
fn a_missing_program_is_still_a_missing_app() {
    for tool in ["shell", "git_operations", "apply_patch"] {
        let step = one(
            tool,
            false,
            &format!("failed to spawn `git`: {ENOENT}"),
            None,
        );
        assert_eq!(
            step.failure,
            Some(TurnStepFailure::MissingApp),
            "`{tool}` can invoke an external program, so its ENOENT may well \
             be a missing app and must not be relabelled: {step:?}"
        );
    }
}

/// The wire value the console keys on.
///
/// `STEP_FAILURE_LABEL` in `frontend/src/api/types.ts` is a
/// `Record<TurnStepFailure, string>`, so TypeScript fails its own build if
/// the label is missing — but nothing checks that the *string* on each side
/// is the same one. This pins this side of that seam.
#[test]
fn not_found_serializes_as_the_snake_case_the_console_indexes_on() {
    assert_eq!(
        serde_json::to_value(TurnStepFailure::NotFound).expect("serializes"),
        serde_json::json!("not_found")
    );
}

/// **The drift guard.** [`PATH_ONLY_TOOLS`] is a `const`, so it is memory;
/// this derives the truth from the same constructor the belt uses
/// ([`crate::harness::build::file_tools`]) and fails when the belt grows a
/// path tool the list does not name.
///
/// Without it the list rots silently: a new sandboxed file tool would go on
/// reporting "App unavailable" for a missing file and nothing would say so.
#[test]
fn every_path_tool_on_the_belt_is_listed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing: Vec<String> = crate::harness::build::file_tools(dir.path(), None)
        .iter()
        .map(|t| t.name().to_string())
        .filter(|name| !PATH_ONLY_TOOLS.contains(&name.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "these sandboxed file tools resolve a path in this process but are not in \
         `PATH_ONLY_TOOLS`, so a missing file from them still renders as \
         \"App unavailable\": {missing:?}"
    );
    // Vacuity guard: an empty belt would satisfy the filter above.
    assert!(
        PATH_ONLY_TOOLS.contains(&"grep"),
        "`grep` is one of the two tools #924 is about"
    );
    assert!(
        PATH_ONLY_TOOLS.contains(&crate::harness::skills::READ_SKILL_RESOURCE_TOOL),
        "`read_skill_resource` is the other"
    );
}

/// An unauthorized call reads as unauthorized end to end — through the fold,
/// not just through the mapping function — and its raw body stays out.
#[test]
fn an_unauthorized_call_says_unauthorized() {
    let steps = fold_steps(vec![completed(
        "c1",
        "mcp_call_tool",
        false,
        &format!("401 unauthorized token={FAKE_SECRET}"),
        Some(serde_json::json!({ "server": "github", "tool": "list_issues" })),
        None,
    )]);
    assert_eq!(steps[0].status, TurnStepStatus::Error);
    assert_eq!(steps[0].failure, Some(TurnStepFailure::Unauthorized));
    assert!(
        !serde_json::to_string(&steps).unwrap().contains(FAKE_SECRET),
        "the 401 body must not ride along"
    );
}

/// A timeout reads as a timeout even when the harness attached no
/// classification of its own — the fallback runs the real classifier rather
/// than the coarse `tool: failed (…)` string the old code produced.
#[test]
fn a_timeout_says_timeout_even_without_a_supplied_classification() {
    let step = one(
        "mcp_call_tool",
        false,
        "the request timed out after 30s",
        None,
    );
    assert_eq!(step.failure, Some(TurnStepFailure::Timeout));
    assert_eq!(
        step.result.as_deref(),
        Some("The action took too long and was stopped.")
    );
}

#[test]
fn error_uses_cause_plain_when_present() {
    let steps = fold_steps(vec![completed(
        "c1",
        "mcp_call_tool",
        false,
        "HTTP 503 upstream exploded at https://x.test?token=SECRET",
        Some(serde_json::json!({"server": "brave", "tool": "search"})),
        Some(classified(
            ToolFailureClass::ServiceUnavailable,
            "The search service was temporarily unavailable.",
        )),
    )]);
    assert_eq!(steps[0].status, TurnStepStatus::Error);
    assert_eq!(steps[0].failure, Some(TurnStepFailure::Unavailable));
    assert_eq!(
        steps[0].result.as_deref(),
        Some("The search service was temporarily unavailable.")
    );
}

/// The workflow-create error-masking fix, carried forward: an intrinsic
/// OpenCompany tool's failure surfaces its OWN message — the actionable
/// reason — even when the classifier only offers the generic cause. It now
/// lands in `result` ("what came back") rather than in `detail`.
#[test]
fn intrinsic_tool_failure_surfaces_oc_authored_reason() {
    let reason = "Couldn't create the workflow: a workflow needs exactly one trigger";
    let steps = fold_steps(vec![
        started("c1", "create_workflow", Some("Create Workflow")),
        completed(
            "c1",
            "create_workflow",
            false,
            reason,
            None,
            Some(classified(
                ToolFailureClass::Unknown,
                "Something went wrong",
            )),
        ),
    ]);
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].status, TurnStepStatus::Error);
    assert_eq!(
        steps[0].result.as_deref(),
        Some(reason),
        "the intrinsic tool's own message must win over the generic cause"
    );
}

// -----------------------------------------------------------------------
// #411: what the step was doing
// -----------------------------------------------------------------------

/// The acceptance criterion, stated exactly as the issue does: two calls to
/// the same tool must be distinguishable. Before #411 both of these rendered
/// as the bare word "Read file".
#[test]
fn two_calls_to_the_same_tool_are_distinguishable() {
    let steps = fold_steps(vec![
        completed(
            "c1",
            "read_file",
            true,
            "…",
            Some(serde_json::json!({ "path": "docs/spec/README.md" })),
            None,
        ),
        completed(
            "c2",
            "read_file",
            true,
            "…",
            Some(serde_json::json!({ "path": "src/lib.rs" })),
            None,
        ),
    ]);
    assert_eq!(steps[0].detail.as_deref(), Some("path=docs/spec/README.md"));
    assert_eq!(steps[1].detail.as_deref(), Some("path=src/lib.rs"));
    assert_ne!(steps[0].detail, steps[1].detail);
}

/// ...including two calls to the *same remote* tool, where the routing
/// fields are identical and only the nested arguments differ. Rendering
/// those flat would show both as `server · tool` and lose the distinction
/// entirely.
#[test]
fn two_mcp_calls_to_one_remote_tool_are_distinguishable() {
    let steps = fold_steps(vec![
        completed(
            "c1",
            "mcp_call_tool",
            true,
            "[]",
            Some(serde_json::json!({
                "server": "github", "tool": "list_issues",
                "arguments": { "repo": "opencompany", "state": "open" }
            })),
            None,
        ),
        completed(
            "c2",
            "mcp_call_tool",
            true,
            "[]",
            Some(serde_json::json!({
                "server": "github", "tool": "list_issues",
                "arguments": { "repo": "landing", "state": "closed" }
            })),
            None,
        ),
    ]);
    assert_eq!(
        steps[0].detail.as_deref(),
        Some("github · list_issues — repo=opencompany · state=open")
    );
    assert_eq!(
        steps[1].detail.as_deref(),
        Some("github · list_issues — repo=landing · state=closed")
    );
}

#[test]
fn arguments_render_for_any_tool_not_just_a_whitelist() {
    let step = one(
        "some_other_tool",
        true,
        "ok",
        Some(serde_json::json!({ "anything": "at all" })),
    );
    assert_eq!(step.detail.as_deref(), Some("anything=at all"));
}

#[test]
fn a_call_with_no_arguments_says_nothing() {
    assert!(one("spawn_task", true, "ok", None).detail.is_none());
    assert!(
        one("spawn_task", true, "ok", Some(serde_json::json!({})))
            .detail
            .is_none()
    );
}

/// Bounds, so one verbose field cannot crowd out the fields that carry the
/// distinction — and so a multi-line argument cannot break its row.
#[test]
fn argument_rendering_is_bounded_and_single_line() {
    let step = one(
        "spawn_task",
        true,
        "ok",
        Some(serde_json::json!({
            "title": "x".repeat(200),
            "note": "first line\nsecond line",
            "a": 1, "b": 2, "c": 3, "d": 4,
        })),
    );
    let detail = step.detail.as_deref().unwrap();
    assert!(detail.chars().count() <= DETAIL_MAX + 1, "{detail}");
    assert!(!detail.contains('\n'), "must stay one line: {detail}");
    assert!(detail.contains('…'), "the long value is cut: {detail}");
}

#[test]
fn deeply_nested_arguments_render_as_a_count_not_a_dump() {
    let step = one(
        "some_tool",
        true,
        "ok",
        Some(serde_json::json!({ "outer": { "inner": { "deep": "value" } } })),
    );
    assert_eq!(step.detail.as_deref(), Some("outer=1 field"));
}
