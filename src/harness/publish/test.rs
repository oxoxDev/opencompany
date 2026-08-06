//! Unit tests for [`crate::harness::publish`].
//!
//! These pin the tool's *own* behaviour — validation, kind inference, capture,
//! queue semantics, scan bounds and the nudge's wording. Whether the tool is
//! reachable from a real model-driven turn is a different question, and it is
//! answered by `publish_turn_test.rs`.

use super::*;
use serde_json::json;

/// A workspace with the given `path → contents` files written into it.
fn workspace(files: &[(&str, &[u8])]) -> tempfile::TempDir {
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

async fn run(tool: &PublishArtifactTool, args: serde_json::Value) -> ToolResult {
    tool.execute(args).await.expect("the tool never propagates")
}

/// A queue claimed for `destination`, with the live claim (issue #445).
///
/// Both halves must be bound by the caller: the claim releases on drop, so
/// `let (queue, _) = claimed(..)` would un-claim it immediately and every
/// publish would then be refused. That is the guard doing its job, but it makes
/// for a confusing test failure, hence the name `_claim` at each call site.
fn claimed(destination: PublishDestination) -> (PendingPublishQueue, PublishClaim) {
    let queue = PendingPublishQueue::default();
    let claim = queue.claim(destination);
    (queue, claim)
}

fn text_of(result: &ToolResult) -> String {
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

// ── Path validation ───────────────────────────────────────────────────────

/// The headline: a file the agent wrote resolves, and its `source` is the
/// normalized workspace-relative path that becomes half the artifact's
/// identity.
#[test]
fn a_workspace_file_resolves_to_its_relative_source() {
    let dir = workspace(&[("specs/launch.md", b"# Spec")]);
    let (file, source) = resolve_in_workspace(dir.path(), "specs/launch.md").unwrap();
    assert!(file.is_file());
    assert_eq!(source, "specs/launch.md");
}

/// Identity must not depend on how the agent spelled the path, or a re-run that
/// wrote `./specs/launch.md` would open a second lineage for one file.
#[test]
fn an_equivalent_spelling_produces_the_same_identity() {
    let dir = workspace(&[("specs/launch.md", b"# Spec")]);
    let (_, direct) = resolve_in_workspace(dir.path(), "specs/launch.md").unwrap();
    let (_, roundabout) = resolve_in_workspace(dir.path(), "./specs/../specs/launch.md").unwrap();
    assert_eq!(direct, roundabout);
}

#[test]
fn traversal_and_absolute_paths_are_refused() {
    let dir = workspace(&[("specs/launch.md", b"# Spec")]);
    // Climbing out, in the obvious shape…
    assert_eq!(
        resolve_in_workspace(dir.path(), "../outside.md"),
        Err(PublishPathError::Missing),
        "nothing is there, and it would be outside if it were"
    );
    // …and where the target genuinely exists outside the workspace.
    let sibling = dir.path().parent().unwrap().join("outside.md");
    std::fs::write(&sibling, b"secret").unwrap();
    assert_eq!(
        resolve_in_workspace(dir.path(), "../outside.md"),
        Err(PublishPathError::Outside)
    );
    let _ = std::fs::remove_file(&sibling);

    assert_eq!(
        resolve_in_workspace(dir.path(), "/etc/hosts"),
        Err(PublishPathError::Outside)
    );
    assert_eq!(
        resolve_in_workspace(dir.path(), "  "),
        Err(PublishPathError::Empty)
    );
}

/// The reason containment is a canonicalize-then-prefix check and not a `..`
/// scan: a symlink inside the workspace has no `..` in it at all.
#[cfg(unix)]
#[test]
fn a_symlink_out_of_the_workspace_is_refused() {
    let dir = workspace(&[("specs/launch.md", b"# Spec")]);
    let outside = dir.path().parent().unwrap().join("escape-target.md");
    std::fs::write(&outside, b"not yours").unwrap();
    std::os::unix::fs::symlink(&outside, dir.path().join("escape.md")).unwrap();

    assert_eq!(
        resolve_in_workspace(dir.path(), "escape.md"),
        Err(PublishPathError::Outside),
        "a symlink is a path that contains no `..` and still leaves the sandbox"
    );
    let _ = std::fs::remove_file(&outside);
}

#[test]
fn a_missing_file_and_a_directory_are_different_mistakes() {
    let dir = workspace(&[("specs/launch.md", b"# Spec")]);
    assert_eq!(
        resolve_in_workspace(dir.path(), "specs/nope.md"),
        Err(PublishPathError::Missing)
    );
    assert_eq!(
        resolve_in_workspace(dir.path(), "specs"),
        Err(PublishPathError::NotAFile)
    );
}

/// Every refusal has to tell the agent what to do next — a tool error that only
/// says "no" costs a whole turn to recover from.
#[test]
fn every_path_error_names_a_next_step() {
    for err in [
        PublishPathError::Empty,
        PublishPathError::Outside,
        PublishPathError::Missing,
        PublishPathError::NotAFile,
    ] {
        let message = err.message("specs/launch.md");
        assert!(message.len() > 40, "{err:?}: {message}");
        assert!(
            message.contains("publish") || message.contains("Publish") || message.contains("path"),
            "{err:?}: {message}"
        );
    }
}

// ── Kind + capture ────────────────────────────────────────────────────────

#[test]
fn kind_is_inferred_from_the_extension() {
    use std::path::Path;
    assert_eq!(
        kind_for_extension(Path::new("a/launch.md")),
        ArtifactKind::Markdown
    );
    assert_eq!(
        kind_for_extension(Path::new("a/notes.txt")),
        ArtifactKind::Text
    );
    assert_eq!(
        kind_for_extension(Path::new("a/chart.png")),
        ArtifactKind::Image
    );
    assert_eq!(
        kind_for_extension(Path::new("a/data.parquet")),
        ArtifactKind::File
    );
    // No extension at all is a file, not a guess at prose.
    assert_eq!(
        kind_for_extension(Path::new("a/Makefile")),
        ArtifactKind::File
    );
    // Case does not decide anything.
    assert_eq!(
        kind_for_extension(Path::new("a/READ.MD")),
        ArtifactKind::Markdown
    );
}

#[test]
fn text_at_or_under_the_cap_is_stored_whole() {
    let body = "x".repeat(MAX_ARTIFACT_BODY_BYTES);
    let dir = workspace(&[("big.txt", body.as_bytes())]);
    let captured =
        capture_body(&dir.path().join("big.txt"), "big.txt", ArtifactKind::Text).unwrap();
    assert!(
        !captured.is_reference,
        "exactly at the cap must still inline"
    );
    assert_eq!(captured.body.len(), MAX_ARTIFACT_BODY_BYTES);
    assert_eq!(captured.forced_kind, None);
}

/// One byte over the cap flips to a reference. The boundary is asserted from
/// both sides because an off-by-one here silently truncates a deliverable.
#[test]
fn one_byte_over_the_cap_becomes_a_reference() {
    let body = "x".repeat(MAX_ARTIFACT_BODY_BYTES + 1);
    let dir = workspace(&[("big.txt", body.as_bytes())]);
    let captured =
        capture_body(&dir.path().join("big.txt"), "big.txt", ArtifactKind::Text).unwrap();
    assert!(captured.is_reference);
    assert_eq!(
        captured.forced_kind,
        Some(ArtifactKind::File),
        "a reference must not be filed under a kind the console renders as prose"
    );
    assert!(captured.body.contains("path: big.txt"), "{}", captured.body);
    assert!(
        captured
            .body
            .contains(&format!("bytes: {}", MAX_ARTIFACT_BODY_BYTES + 1)),
        "{}",
        captured.body
    );
    assert!(captured.body.contains("sha256: "), "{}", captured.body);
    // Never silently-truncated content presented as complete.
    assert!(
        !captured.body.contains(&"x".repeat(100)),
        "the reference must not carry a slice of the content"
    );
}

#[test]
fn a_non_utf8_file_becomes_a_reference_whatever_its_size() {
    let dir = workspace(&[("logo.png", &[0x89, 0x50, 0x4e, 0x47, 0xff, 0xfe])]);
    let captured = capture_body(
        &dir.path().join("logo.png"),
        "logo.png",
        ArtifactKind::Image,
    )
    .unwrap();
    assert!(captured.is_reference);
    assert_eq!(
        captured.forced_kind,
        Some(ArtifactKind::Image),
        "an image reference stays an image so the console picks the right renderer"
    );
    assert!(captured.body.contains("not UTF-8"), "{}", captured.body);
}

/// The digest is of the bytes, so two publishes of the same content agree and a
/// changed file does not.
#[test]
fn the_reference_digest_tracks_the_bytes() {
    let dir = workspace(&[("a.bin", &[0xff, 0x00]), ("b.bin", &[0xff, 0x00])]);
    let a = capture_body(&dir.path().join("a.bin"), "a.bin", ArtifactKind::File).unwrap();
    let b = capture_body(&dir.path().join("b.bin"), "b.bin", ArtifactKind::File).unwrap();
    let sha = |body: &str| {
        body.lines()
            .find_map(|l| l.strip_prefix("sha256: "))
            .unwrap()
            .to_string()
    };
    assert_eq!(sha(&a.body), sha(&b.body));

    std::fs::write(dir.path().join("b.bin"), [0xff, 0x01]).unwrap();
    let changed = capture_body(&dir.path().join("b.bin"), "b.bin", ArtifactKind::File).unwrap();
    assert_ne!(sha(&a.body), sha(&changed.body));
}

// ── The tool ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn publishing_stages_the_file_and_reports_what_was_captured() {
    let dir = workspace(&[("specs/launch.md", b"# Spec\nShip it.")]);
    let (queue, _claim) = claimed(PublishDestination::Task);
    let tool = PublishArtifactTool::new(dir.path(), queue.clone());

    let result = run(&tool, json!({ "path": "specs/launch.md" })).await;
    assert!(!result.is_error, "{}", text_of(&result));

    let staged = queue.drain();
    assert_eq!(staged.len(), 1);
    assert_eq!(staged[0].source, "specs/launch.md");
    assert_eq!(staged[0].kind, ArtifactKind::Markdown);
    assert_eq!(staged[0].body, "# Spec\nShip it.");
    // Title defaults to the file name, not the whole path.
    assert_eq!(staged[0].title, "launch.md");
    assert_eq!(staged[0].note, None);
}

#[tokio::test]
async fn an_explicit_title_kind_and_note_are_carried_through() {
    let dir = workspace(&[("out.dat", b"plain text really")]);
    let (queue, _claim) = claimed(PublishDestination::Task);
    let tool = PublishArtifactTool::new(dir.path(), queue.clone());

    run(
        &tool,
        json!({
            "path": "out.dat",
            "title": "Q3 export",
            "kind": "text",
            "note": "rewrote the pricing section"
        }),
    )
    .await;

    let staged = queue.drain();
    assert_eq!(staged[0].title, "Q3 export");
    assert_eq!(
        staged[0].kind,
        ArtifactKind::Text,
        "an explicit kind beats the extension"
    );
    assert_eq!(
        staged[0].note.as_deref(),
        Some("rewrote the pricing section")
    );
}

/// The body is read at publish time, so a later shell step cannot retroactively
/// change what the operator is told was published.
#[tokio::test]
async fn the_body_is_captured_at_publish_time_not_at_drain_time() {
    let dir = workspace(&[("spec.md", b"# The version I published")]);
    let (queue, _claim) = claimed(PublishDestination::Task);
    let tool = PublishArtifactTool::new(dir.path(), queue.clone());

    run(&tool, json!({ "path": "spec.md" })).await;
    // The agent's next step scribbles over the file.
    std::fs::write(dir.path().join("spec.md"), b"# clobbered afterwards").unwrap();

    let staged = queue.drain();
    assert_eq!(staged[0].body, "# The version I published");
}

#[tokio::test]
async fn a_bad_path_is_a_truthful_tool_error_and_stages_nothing() {
    let dir = workspace(&[("spec.md", b"# Spec")]);
    let (queue, _claim) = claimed(PublishDestination::Task);
    let tool = PublishArtifactTool::new(dir.path(), queue.clone());

    for path in ["../escape.md", "/etc/hosts", "nope.md", ""] {
        let result = run(&tool, json!({ "path": path })).await;
        assert!(result.is_error, "`{path}` was accepted");
    }
    // A missing `path` argument entirely.
    assert!(run(&tool, json!({})).await.is_error);
    assert_eq!(queue.queued(), 0, "a refused publish must stage nothing");
}

#[tokio::test]
async fn an_unknown_kind_is_refused_by_name() {
    let dir = workspace(&[("spec.md", b"# Spec")]);
    let (queue, _claim) = claimed(PublishDestination::Task);
    let tool = PublishArtifactTool::new(dir.path(), queue.clone());

    let result = run(&tool, json!({ "path": "spec.md", "kind": "spreadsheet" })).await;
    assert!(result.is_error);
    let message = text_of(&result);
    assert!(message.contains("markdown"), "{message}");
    assert_eq!(queue.queued(), 0);
}

// ── Queue semantics ───────────────────────────────────────────────────────

#[test]
fn the_queue_drains_fifo_and_empties() {
    let queue = PendingPublishQueue::default();
    let publish = |source: &str| PendingPublish {
        source: source.to_string(),
        title: source.to_string(),
        kind: ArtifactKind::Text,
        note: None,
        body: "b".to_string(),
    };
    queue.push(publish("a.md"));
    queue.push(publish("b.md"));
    assert_eq!(queue.sources(), ["a.md", "b.md"]);
    assert_eq!(queue.queued(), 2);

    let drained = queue.drain();
    assert_eq!(
        drained
            .iter()
            .map(|p| p.source.as_str())
            .collect::<Vec<_>>(),
        ["a.md", "b.md"]
    );
    assert_eq!(queue.queued(), 0, "drain empties");
    assert!(queue.drain().is_empty(), "a second drain yields nothing");
}

/// `clear` is what stops an operator chat turn earlier in the same cycle — or
/// an abandoned redirect re-run — from having its staged file attributed to
/// this card.
#[test]
fn clear_drops_what_a_prior_turn_staged() {
    let queue = PendingPublishQueue::default();
    queue.push(PendingPublish {
        source: "leftover.md".to_string(),
        title: "leftover".to_string(),
        kind: ArtifactKind::Text,
        note: None,
        body: "b".to_string(),
    });
    queue.clear();
    assert_eq!(queue.queued(), 0);
    assert!(queue.sources().is_empty());
}

/// The queue handle is shared, not copied — the tool built into the agent and
/// the brain that drains it must see one queue.
///
/// Since #445 that sharing has a second half: the **destination** travels with
/// the clone too. `build_agent` hands the tool one clone and nothing else, so a
/// destination that did not survive cloning would leave every built tool
/// permanently unclaimed and unable to publish at all.
#[tokio::test]
async fn a_cloned_handle_sees_the_same_queue() {
    let dir = workspace(&[("spec.md", b"# Spec")]);
    let queue = PendingPublishQueue::default();
    let tool = PublishArtifactTool::new(dir.path(), queue.clone());

    let _claim = queue.claim(PublishDestination::Task);
    run(&tool, json!({ "path": "spec.md" })).await;

    assert_eq!(queue.queued(), 1, "the brain's handle sees the tool's push");
}

// ── The scan ──────────────────────────────────────────────────────────────

#[test]
fn the_scan_sees_new_and_modified_files_but_not_deletions() {
    let dir = workspace(&[("keep.md", b"one"), ("gone.md", b"two")]);
    let before = WorkspaceSnapshot::take(dir.path());
    assert_eq!(before.len(), 2);

    std::fs::write(dir.path().join("keep.md"), b"one, revised").unwrap();
    std::fs::write(dir.path().join("fresh.md"), b"new").unwrap();
    std::fs::remove_file(dir.path().join("gone.md")).unwrap();

    let changed = before.changed_since(dir.path()).files;
    assert_eq!(
        changed,
        ["fresh.md", "keep.md"],
        "a deleted file is not a deliverable somebody forgot to publish"
    );
}

/// A same-timestamp rewrite still counts, because size is compared too. Coarse
/// filesystem clocks are common enough that mtime alone would miss real edits.
#[test]
fn a_same_instant_rewrite_of_a_different_length_is_still_a_change() {
    let dir = workspace(&[("spec.md", b"short")]);
    let before = WorkspaceSnapshot::take(dir.path());
    let path = dir.path().join("spec.md");
    let stat = std::fs::metadata(&path).unwrap();
    std::fs::write(&path, b"a considerably longer body").unwrap();
    // Force the mtime back so only the size differs.
    let file = std::fs::File::options().write(true).open(&path).unwrap();
    file.set_modified(stat.modified().unwrap()).unwrap();
    drop(file);

    assert_eq!(before.changed_since(dir.path()).files, ["spec.md"]);
}

#[test]
fn the_scan_skips_the_directories_an_exec_sandbox_fills() {
    let dir = workspace(&[
        ("spec.md", b"one"),
        (".git/objects/ab/cdef", b"blob"),
        ("node_modules/left-pad/index.js", b"module"),
        ("target/debug/build.log", b"log"),
    ]);
    let snapshot = WorkspaceSnapshot::take(dir.path());
    assert_eq!(snapshot.len(), 1, "only the agent's own file");
    assert!(!snapshot.truncated());

    // …and they are skipped on the diff side too, so a build never nudges.
    let before = WorkspaceSnapshot::take(dir.path());
    std::fs::write(dir.path().join("target/debug/build.log"), b"rebuilt").unwrap();
    assert!(before.changed_since(dir.path()).files.is_empty());
}

/// **The false-positive test that matters most.** The agent's `workspace_dir`
/// is also where OpenHuman writes its own session transcripts, audit trail and
/// checkpoints — on *every* run, by the harness rather than the agent. If the
/// scan counted them, the nudge would fire after every single dispatch, asking
/// an agent whether its own transcript is a deliverable.
///
/// Found the hard way: before these exclusions, every existing dispatch test
/// grew a second model turn.
#[test]
fn the_scan_ignores_what_the_runtime_itself_writes() {
    let dir = workspace(&[("spec.md", b"one")]);
    let before = WorkspaceSnapshot::take(dir.path());

    // Exactly what a real run leaves behind beside the agent's own work.
    for path in [
        "sessions/2026_08_05/1785952277_chief.md",
        "session_raw/1785952277_chief.jsonl",
        "artifacts/some-id/content",
        "checkpoints/state.json",
        "tinyagents_store/journal/session.1785953147_ceo.messages.jsonl",
        ".openhuman/subagent_checkpoints/a.json",
        ".runs/run-1.json",
        "audit.log",
        ".env",
    ] {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, b"runtime bookkeeping").unwrap();
    }

    assert!(
        before.changed_since(dir.path()).files.is_empty(),
        "the runtime's own files must never look like unpublished agent work"
    );

    // The agent's actual file is still seen, so the exclusions did not blind it.
    std::fs::write(dir.path().join("spec.md"), b"one, revised").unwrap();
    assert_eq!(before.changed_since(dir.path()).files, ["spec.md"]);
}

/// The entry cap. A truncated scan may only under-report — it feeds a warning,
/// never a promotion, so missing something is the acceptable failure.
#[test]
fn the_scan_stops_at_its_entry_cap() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..(MAX_SCAN_ENTRIES + 50) {
        std::fs::write(dir.path().join(format!("f{i}.txt")), b"x").unwrap();
    }
    let snapshot = WorkspaceSnapshot::take(dir.path());
    assert!(snapshot.truncated());
    assert!(snapshot.len() <= MAX_SCAN_ENTRIES);
}

#[test]
fn a_workspace_that_does_not_exist_yet_has_changed_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let never = dir.path().join("no-such-agent/workspace");
    let snapshot = WorkspaceSnapshot::take(&never);
    assert!(snapshot.is_empty());
    assert!(snapshot.changed_since(&never).files.is_empty());
}

#[test]
fn unpublished_is_changed_minus_staged() {
    let changed = vec!["a.md".to_string(), "b.md".to_string(), "c.md".to_string()];
    assert_eq!(
        unpublished(&changed, &["b.md".to_string()]),
        ["a.md", "c.md"]
    );
    assert!(unpublished(&changed, &changed).is_empty(), "all published");
    assert!(unpublished(&[], &[]).is_empty(), "nothing written");
}

#[test]
fn a_long_file_list_is_bounded_and_says_so() {
    let many: Vec<String> = (0..MAX_NAMED_FILES + 7)
        .map(|i| format!("f{i}.txt"))
        .collect();
    let rendered = name_files(&many);
    assert!(rendered.contains("and 7 more"), "{rendered}");
    assert!(!rendered.contains(&format!("f{}.txt", MAX_NAMED_FILES + 1)));
    // Under the bound, nothing is added.
    assert_eq!(name_files(&["a.md".to_string()]), "a.md");
}

// ── The nudge's words ─────────────────────────────────────────────────────

/// The nudge has to stand alone: turns share no conversation context, so the
/// brief, the reply and the files all have to be inside it.
#[test]
fn the_nudge_carries_its_own_context() {
    let instruction = nudge_instruction(
        "Draft the launch spec.",
        "Done — I've written it up.",
        &["specs/launch.md".to_string(), "scratch.txt".to_string()],
        false,
    );
    assert!(
        instruction.contains("Draft the launch spec."),
        "{instruction}"
    );
    assert!(
        instruction.contains("Done — I've written it up."),
        "{instruction}"
    );
    assert!(instruction.contains("specs/launch.md"), "{instruction}");
    assert!(instruction.contains("scratch.txt"), "{instruction}");
    assert!(instruction.contains(PUBLISH_ARTIFACT_TOOL), "{instruction}");
}

/// **The non-coercion test.** A nudge that reads as an instruction produces
/// published build logs. It must offer the decline in the same breath, and must
/// never claim publishing is required.
#[test]
fn the_nudge_offers_the_decline_and_never_demands_a_publish() {
    let instruction = nudge_instruction("Draft it.", "Done.", &["scratch.txt".to_string()], false);
    let lower = instruction.to_lowercase();

    assert!(
        lower.contains("declining is a normal answer"),
        "the decline must be affirmed, not merely permitted: {instruction}"
    );
    assert!(
        lower.contains("say briefly why not"),
        "there must be a stated way to decline: {instruction}"
    );
    assert!(
        lower.contains("scratch files"),
        "the legitimate reasons to decline must be named: {instruction}"
    );
    for coercion in [
        "you must",
        "you should",
        "required",
        "make sure you publish",
    ] {
        assert!(
            !lower.contains(coercion),
            "the nudge reads as a demand (`{coercion}`): {instruction}"
        );
    }
    // And it must be clear the already-sent answer is not at stake.
    assert!(
        lower.contains("already been sent"),
        "the agent must know its reply is safe: {instruction}"
    );
}

#[test]
fn a_decline_is_recorded_with_both_the_files_and_the_reason() {
    let note = declined_note(
        &["scratch.txt".to_string()],
        "  Those were intermediate notes, not the deliverable.  ",
    );
    assert_eq!(
        note,
        "unpublished: scratch.txt — agent: Those were intermediate notes, not the deliverable."
    );
}

// ── Issue #445: no success receipt for a publish nothing will record ───────

/// **The headline test.** With no claimed destination the tool must refuse,
/// because nothing is listening for what it would stage.
///
/// This is the exact shape of #445: the file resolved, it was readable, and
/// every check the tool used to make passed — so the old tool staged it, said
/// "published", and the item was cleared unread. A green assertion on the
/// receipt string would have passed then too, which is why this asserts on the
/// **queue** as well as on `is_error`.
#[tokio::test]
async fn an_unclaimed_queue_refuses_to_publish_and_stages_nothing() {
    let dir = workspace(&[("specs/launch.md", b"# Spec")]);
    let queue = PendingPublishQueue::default();
    let tool = PublishArtifactTool::new(dir.path(), queue.clone());

    let result = run(&tool, json!({ "path": "specs/launch.md" })).await;

    assert!(
        result.is_error,
        "a publish nothing will drain must not report success: {}",
        text_of(&result)
    );
    assert_eq!(
        queue.queued(),
        0,
        "a refused publish must not leave anything staged"
    );
    let message = text_of(&result);
    // The agent must be told not to claim delivery — the failure #445 describes
    // is laundered through the agent into a confident lie to the operator, and
    // only the tool's own words can stop that.
    assert!(
        message.contains("NOT published"),
        "the refusal must be unambiguous: {message}"
    );
    assert!(
        message.to_lowercase().contains("do not retry"),
        "an unclaimed queue is not a transient fault: {message}"
    );
    assert!(
        message.contains("sandbox"),
        "the agent must be told where the file actually still is: {message}"
    );
}

/// `Unclaimed` is the [`Default`], which is what makes the guarantee hold for
/// call sites that do not know this module exists.
#[test]
fn a_fresh_queue_is_unclaimed_by_default() {
    assert_eq!(
        PendingPublishQueue::default().destination(),
        PublishDestination::Unclaimed
    );
    assert_eq!(PublishDestination::default(), PublishDestination::Unclaimed);
}

/// The claim is a scope, not a flag: when it ends, publishing is off again.
///
/// This is what protects a turn that runs *after* a drain site returns — an
/// approval re-dispatch, a workflow node, anything reusing the same shared deps
/// — from inheriting a promise that has already been settled.
#[tokio::test]
async fn dropping_the_claim_stops_publishing_again() {
    let dir = workspace(&[("spec.md", b"# Spec")]);
    let queue = PendingPublishQueue::default();
    let tool = PublishArtifactTool::new(dir.path(), queue.clone());

    let claim = queue.claim(PublishDestination::Task);
    assert!(!run(&tool, json!({ "path": "spec.md" })).await.is_error);
    drop(claim);

    assert_eq!(
        queue.destination(),
        PublishDestination::Unclaimed,
        "the claim must release on drop"
    );
    assert_eq!(
        queue.queued(),
        0,
        "releasing must also clear, so nothing leaks into the next caller"
    );
    assert!(
        run(&tool, json!({ "path": "spec.md" })).await.is_error,
        "publishing must be refused once the claim has ended"
    );
}

/// Claiming clears, so one caller can never be handed the previous caller's
/// staged file. This is the invariant `run_task`'s hand-written `clear()` used
/// to carry, now enforced by the claim itself.
#[test]
fn claiming_clears_whatever_a_previous_caller_left_staged() {
    let queue = PendingPublishQueue::default();
    let claim = queue.claim(PublishDestination::Task);
    queue.push(PendingPublish {
        source: "stale.md".to_string(),
        title: "stale".to_string(),
        kind: ArtifactKind::Text,
        note: None,
        body: "old".to_string(),
    });
    drop(claim);

    let _claim = queue.claim(PublishDestination::Conversation);
    assert_eq!(queue.queued(), 0);
    assert_eq!(queue.destination(), PublishDestination::Conversation);
}

/// The receipt must describe **this** caller's destination. One sentence written
/// for the task case and reused everywhere is what told a chat turn its file
/// would appear on a run that did not exist.
#[tokio::test]
async fn the_receipt_names_the_destination_the_caller_actually_has() {
    let dir = workspace(&[("spec.md", b"# Spec")]);

    let (task_queue, _task_claim) = claimed(PublishDestination::Task);
    let task_tool = PublishArtifactTool::new(dir.path(), task_queue);
    let task_receipt = text_of(&run(&task_tool, json!({ "path": "spec.md" })).await);

    let (chat_queue, _chat_claim) = claimed(PublishDestination::Conversation);
    let chat_tool = PublishArtifactTool::new(dir.path(), chat_queue);
    let chat_receipt = text_of(&run(&chat_tool, json!({ "path": "spec.md" })).await);

    assert!(
        task_receipt.contains("this task's Artifacts tab"),
        "the task receipt is unchanged by #445: {task_receipt}"
    );
    assert!(
        chat_receipt.contains("card"),
        "a conversation's file lands on a minted card, and must say so: {chat_receipt}"
    );
    assert!(
        !chat_receipt.contains("this task's"),
        "a chat turn has no task; the receipt must not name one: {chat_receipt}"
    );
    assert_ne!(
        task_receipt, chat_receipt,
        "two destinations must not share one sentence"
    );
}

// ── Issue #445: the card a conversation's publish mints ───────────────────

#[test]
fn a_minted_card_is_titled_from_what_was_published() {
    let publish = |title: &str, source: &str| PendingPublish {
        source: source.to_string(),
        title: title.to_string(),
        kind: ArtifactKind::Markdown,
        note: None,
        body: "body".to_string(),
    };

    assert_eq!(
        conversation_card_title(&[publish("Launch spec", "specs/launch.md")]),
        "Launch spec",
        "one file gives the card its own title"
    );
    assert_eq!(
        conversation_card_title(&[
            publish("Launch spec", "specs/launch.md"),
            publish("Pricing", "pricing.md"),
            publish("FAQ", "faq.md"),
        ]),
        "Launch spec (+2 more)",
        "several files stay one fixed-width title"
    );
    // Never panics on the case that cannot happen.
    assert!(!conversation_card_title(&[]).is_empty());
}

/// A card nobody asked for has to explain itself, or the honest fix for a
/// silent drop introduces its own small mystery on the board.
#[test]
fn a_minted_card_explains_why_it_exists() {
    let note = conversation_card_note(
        "ceo",
        &[PendingPublish {
            source: "specs/launch.md".to_string(),
            title: "Launch spec".to_string(),
            kind: ArtifactKind::Markdown,
            note: None,
            body: "body".to_string(),
        }],
    );
    assert!(note.contains("ceo"), "{note}");
    assert!(note.contains("specs/launch.md"), "{note}");
    assert!(note.contains("conversation"), "{note}");
}

/// If a publish is accepted and then cannot be recorded, the operator hears
/// about it — in the conversation, where the wrong claim was made.
#[test]
fn a_recording_failure_is_stated_in_the_operators_own_reply() {
    let one = recording_failed_notice(1);
    assert!(one.contains("1 file was"), "{one}");
    assert!(
        one.contains("NOT") && one.to_lowercase().contains("incorrect"),
        "the operator must be told the delivery claim is wrong: {one}"
    );
    let many = recording_failed_notice(3);
    assert!(many.contains("3 files were"), "{many}");
}

// ── Issue #445: sandbox is not the company workspace ──────────────────────

/// The naming collision that sent an operator looking in the wrong place.
///
/// `publish_brief` and `workspace_brief` can sit in the same system prompt, and
/// both used to say "your workspace" about different directories. The brief must
/// now name the sandbox as the sandbox and say outright that it is not the
/// company workspace.
#[test]
fn the_brief_distinguishes_the_sandbox_from_the_company_workspace() {
    let brief = publish_brief();
    let lower = brief.to_lowercase();

    assert!(lower.contains("sandbox"), "{brief}");
    assert!(
        lower.contains("not the company workspace"),
        "the two places must be told apart explicitly: {brief}"
    );
    assert!(
        lower.contains("cannot see"),
        "the agent must know a written file is invisible to the operator: {brief}"
    );
    assert!(
        !lower.contains("in your workspace"),
        "the sandbox must never be called `your workspace` again: {brief}"
    );
    // The non-coercive contract from #244 must survive the rewrite.
    assert!(
        lower.contains("normal outcome"),
        "publishing nothing must stay a fine answer: {brief}"
    );
}

/// The agent-facing refusals must not reintroduce the collision either.
#[test]
fn path_errors_call_the_sandbox_a_sandbox() {
    for err in [PublishPathError::Outside, PublishPathError::Missing] {
        let message = err.message("specs/launch.md");
        assert!(message.contains("sandbox"), "{err:?}: {message}");
        assert!(
            !message.contains("your workspace"),
            "{err:?} still says `your workspace`: {message}"
        );
    }
}

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
