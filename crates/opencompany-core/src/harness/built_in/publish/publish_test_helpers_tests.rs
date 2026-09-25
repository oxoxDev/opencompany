//! Unit tests for [`crate::harness::publish`].
//!
//! These pin the tool's *own* behaviour — validation, kind inference, capture,
//! queue semantics, scan bounds and the nudge's wording. Whether the tool is
//! reachable from a real model-driven turn is a different question, and it is

use super::*;
use openhuman_core as oh;

/// A workspace with the given `path → contents` files written into it.
pub(crate) fn workspace(files: &[(&str, &[u8])]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, body) in files {
        let full = dir.path().join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, body).unwrap();
    }
    dir
}

pub(crate) async fn run(tool: &PublishArtifactTool, args: serde_json::Value) -> ToolResult {
    tool.execute(args).await.expect("the tool never propagates")
}

/// A queue claimed for `destination`, with the live claim (issue #445).
///
/// Run the tool inside [`PublishClaim::scoped`]: outside it, and once the
/// claim drops, every publish is refused.
pub(crate) fn claimed(destination: PublishDestination) -> (PendingPublishQueue, PublishClaim) {
    let queue = PendingPublishQueue::default();
    let claim = queue.claim(destination);
    (queue, claim)
}

pub(crate) fn text_of(result: &ToolResult) -> String {
    result
        .content
        .iter()
        .map(|c| match c {
            oh::skills::types::ToolContent::Text { text } => text.clone(),
            other => format!("{other:?}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}
