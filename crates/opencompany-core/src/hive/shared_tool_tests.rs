//! What the wrapper must not change about the tool it wraps.
//!
//! Twenty-two of `Tool`'s methods carry defaults, so a wrapper that forgot one
//! would compile and answer for its inner tool with the wrong value. These pin
//! the ones that change *behaviour* — what a gate admits, what a model is
//! shown, how a result is bounded — rather than presentation.

use super::*;
use tinytools::{PermissionLevel, ToolExposure};

/// A tool whose every interesting answer differs from the trait default, so a
/// wrapper that dropped a method would report the default and fail here.
#[derive(Debug)]
struct Opinionated;

#[async_trait]
impl Tool for Opinionated {
    fn name(&self) -> &str {
        "opinionated"
    }

    fn description(&self) -> &str {
        "answers nothing by default"
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({ "type": "object" })
    }

    async fn execute(&self, _args: Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::success("ran"))
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Write
    }

    fn exposure(&self) -> ToolExposure {
        ToolExposure::Hidden
    }

    fn external_effect(&self) -> bool {
        true
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(17)
    }

    fn return_direct(&self) -> bool {
        true
    }

    fn supports_markdown(&self) -> bool {
        true
    }
}

fn wrapped() -> SharedTool {
    SharedTool::new(Arc::new(Opinionated))
}

/// An admission gate reads these. A wrapper that answered the default would
/// quietly widen what a tool is allowed to do.
#[test]
fn the_wrapper_does_not_soften_what_a_gate_reads() {
    let tool = wrapped();
    assert_eq!(tool.permission_level(), PermissionLevel::Write);
    assert!(tool.external_effect());
}

/// The catalogue reads these. A hidden tool that advertised itself is the
/// failure this crate spent a session chasing from the other direction.
#[test]
fn the_wrapper_does_not_reveal_a_hidden_tool() {
    assert_eq!(wrapped().exposure(), ToolExposure::Hidden);
}

/// Result handling reads these.
#[test]
fn the_wrapper_keeps_the_result_bounds_and_directness() {
    let tool = wrapped();
    assert_eq!(tool.max_result_size_chars(), Some(17));
    assert!(tool.return_direct());
    assert!(tool.supports_markdown());
}

/// The identity half — the part a model actually sees.
#[test]
fn the_wrapper_is_the_tool_it_wraps() {
    let tool = wrapped();
    assert_eq!(tool.name(), "opinionated");
    assert_eq!(tool.description(), "answers nothing by default");
    assert_eq!(tool.spec().name, "opinionated");
}

/// The whole point: one shared instance, many owned handles. A belt minted
/// twice must not build the tool twice.
#[tokio::test]
async fn an_owned_belt_delegates_to_the_one_shared_instance() {
    let shared: Vec<Arc<dyn Tool>> = vec![Arc::new(Opinionated)];
    let first = owned_belt(&shared);
    let second = owned_belt(&shared);

    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert_eq!(
        Arc::strong_count(&shared[0]),
        3,
        "two handles plus the source"
    );
    for belt in [first, second] {
        let out = belt[0].execute(serde_json::json!({})).await.unwrap();
        assert!(format!("{out:?}").contains("ran"));
    }
}
