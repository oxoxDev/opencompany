use super::publish_test_helpers_tests::*;
use super::*;
use serde_json::json;

// ── Issue #420 item 3: a partial scan says it is partial ──────────────────

/// The flag existed and nothing read it, so the nudge presented an arbitrary
/// DFS prefix as the complete list of what the agent changed.
#[test]
fn a_truncated_scan_reports_that_its_diff_is_partial() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..(MAX_SCAN_ENTRIES + 50) {
        std::fs::write(dir.path().join(format!("f{i}.txt")), b"x").unwrap();
    }
    let before = WorkspaceSnapshot::take(dir.path());
    assert!(before.truncated(), "the fixture must actually truncate");

    let changed = before.changed_since(dir.path());
    assert!(
        changed.partial,
        "a diff of truncated walks must admit it is a subset"
    );
}

/// A scan that saw everything must NOT claim to be partial, or the caveat
/// becomes noise the agent learns to ignore.
#[test]
fn a_complete_scan_is_not_reported_as_partial() {
    let dir = workspace(&[("spec.md", b"one")]);
    let before = WorkspaceSnapshot::take(dir.path());
    std::fs::write(dir.path().join("spec.md"), b"one, revised").unwrap();

    let changed = before.changed_since(dir.path());
    assert_eq!(changed.files, ["spec.md"]);
    assert!(!changed.partial);
}

/// The agent has to be told, or it declines on behalf of files the scan never
/// reached — a completeness claim the scan cannot support.
#[test]
fn the_nudge_says_when_the_file_list_is_incomplete() {
    let files = ["a.md".to_string()];

    let complete = nudge_instruction("brief", "reply", &files, false);
    assert!(
        complete.contains("That is everything you changed."),
        "{complete}"
    );
    assert!(
        !complete.to_lowercase().contains("incomplete"),
        "{complete}"
    );

    let partial = nudge_instruction("brief", "reply", &files, true);
    assert!(
        partial.to_lowercase().contains("incomplete"),
        "a partial scan must say so: {partial}"
    );
    assert!(
        !partial.contains("That is everything you changed."),
        "a partial scan must not claim completeness: {partial}"
    );
    // The caveat must not turn the nudge coercive — #244's contract still holds.
    assert!(!partial.to_lowercase().contains("you must"), "{partial}");
}

/// A refusal that names a tool the agent does not have costs it a turn.
///
/// The listing tool is advertised as `list` (openhuman's `ListFilesTool::name`);
/// the message used to say `list_files`, which is the Rust type name and not
/// anything the model can call.
#[test]
fn the_missing_file_message_names_a_tool_that_exists() {
    let message = PublishPathError::Missing.message("specs/launch.md");
    assert!(message.contains("`list`"), "{message}");
    assert!(!message.contains("list_files"), "{message}");
}

// ── Refused publishes (issue #1192) ───────────────────────────────────────

/// **The incident, at the queue.** An unclaimed publish is refused *and*
/// recorded, so something other than the model's prose knows it happened.
///
/// Before #1192 the refusal existed in exactly two places: the sentence handed
/// to the model, and a `tracing::warn!`. Neither reaches an operator. A caller
/// that can reach one — a workflow run, which has no conversation to speak in —
/// had nothing structural to say, so the model's apology became the node output
/// and the run scored clean.
///
/// The second half of this test is the one that must not be "simplified away":
/// **`sources()` stays empty.** A refused file is not a staged file. It is the
/// file most at risk of being lost, and the #244 unpublished-work scan is
/// `changed − sources()` — so the moment a refusal counts as a source, the nudge
/// goes silent on precisely the file it exists to catch.
#[tokio::test]
async fn a_refused_publish_is_recorded_on_the_queue_not_only_told_to_the_agent() {
    let dir = workspace(&[("spec.md", b"# Spec")]);
    let queue = PendingPublishQueue::default();
    let tool = PublishArtifactTool::new(dir.path(), "maya", queue.clone());

    // No claim: `Unclaimed` is the default, which is the workflow-run shape.
    assert_eq!(queue.destination(), PublishDestination::Unclaimed);
    let result = run(&tool, json!({ "path": "spec.md" })).await;
    assert!(result.is_error, "an unclaimed publish is still refused");

    assert_eq!(
        queue.sources(),
        Vec::<String>::new(),
        "a refused file is NOT staged — it must still read as unpublished to the #244 scan"
    );
    assert_eq!(
        queue.queued(),
        0,
        "nothing was staged, so nothing can be drained as an artifact"
    );
    assert_eq!(
        queue.drain_refusals(),
        vec!["spec.md".to_string()],
        "…but the refusal itself is a fact the queue carries"
    );
    assert!(
        queue.drain_refusals().is_empty(),
        "draining empties the bucket, so one refusal is reported once"
    );
}

/// The guard on the new bucket: the chat and task paths must not start
/// producing refusals.
///
/// Both claim a destination before the turn, so `receipt_tail()` answers and the
/// refusal arm is never reached. Worth pinning because the queue handle is
/// *shared* — the same `PendingPublishQueue` clone backs every path — so a
/// refusal recorded on one path is visible to a drain on another. The reason
/// that is safe is exactly this: a claimed destination records none.
#[tokio::test]
async fn a_claimed_destination_records_no_refusal() {
    let dir = workspace(&[("spec.md", b"# Spec")]);

    for destination in [PublishDestination::Task, PublishDestination::Conversation] {
        let (queue, claim) = claimed(destination.clone());
        let tool = PublishArtifactTool::new(dir.path(), "maya", queue.clone());
        let result = claim.scoped(run(&tool, json!({ "path": "spec.md" }))).await;

        assert!(!result.is_error, "{destination:?}: the publish succeeds");
        assert_eq!(
            claim.sources().len(),
            1,
            "{destination:?}: and it is staged"
        );
        assert!(
            queue.drain_refusals().is_empty(),
            "{destination:?}: a claimed destination must never record a refusal"
        );
    }
}

/// A steer redirect abandons the previous turn's work, and `clear()` is how that
/// abandonment is expressed. It must take the refusals with it.
///
/// Otherwise a redirect re-runs from the original brief and the operator is told
/// about a refusal provoked by a turn that was thrown away — attributed to the
/// turn that replaced it, which never asked to publish anything.
#[tokio::test]
async fn clearing_the_queue_empties_both_buckets() {
    let queue = PendingPublishQueue::default();
    let claim = queue.claim(PublishDestination::Task);
    claim
        .scoped(async {
            assert!(queue.push(PendingPublish {
                agent: "maya".to_string(),
                source: "staged.md".to_string(),
                title: "staged".to_string(),
                kind: ArtifactKind::Text,
                note: None,
                payload: PublishPayload::Text("body".to_string()),
            }));
            queue.push_refusal("refused.md".to_string());
            assert_eq!(queue.queued(), 1);

            queue.clear();

            assert_eq!(queue.queued(), 0, "the staged bucket is emptied, as before");
            assert!(
                queue.drain_refusals().is_empty(),
                "an abandoned turn's refusal must not survive into the turn that replaces it"
            );
        })
        .await;
}
