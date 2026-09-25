use super::publish_test_helpers_tests::*;
use super::*;
use serde_json::json;

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
    assert_eq!(
        captured,
        PublishPayload::Text(body),
        "exactly at the cap must still be stored as prose"
    );
    assert_eq!(captured.forced_kind(ArtifactKind::Text), ArtifactKind::Text);
}

/// One byte over the cap is stored as **bytes**, not as a reference.
///
/// Issue #553 removed the reference branch entirely: the workspace tree can
/// hold bytes on every backend, so there is nothing for a fallback to fall back
/// to. The boundary is asserted from both sides because an off-by-one here
/// changes how a deliverable is stored.
#[test]
fn one_byte_over_the_cap_is_stored_as_bytes() {
    let body = "x".repeat(MAX_ARTIFACT_BODY_BYTES + 1);
    let dir = workspace(&[("big.txt", body.as_bytes())]);
    let captured =
        capture_body(&dir.path().join("big.txt"), "big.txt", ArtifactKind::Text).unwrap();
    match &captured {
        PublishPayload::Bytes { bytes, mime } => {
            assert_eq!(
                bytes.len(),
                MAX_ARTIFACT_BODY_BYTES + 1,
                "the whole file is carried, not a slice of it"
            );
            assert_eq!(mime, "text/plain");
        }
        other => panic!("expected bytes, got {other:?}"),
    }
    assert_eq!(
        captured.forced_kind(ArtifactKind::Text),
        ArtifactKind::File,
        "bytes must not be filed under a kind the console renders as prose"
    );
}

/// `capture_body` stats before it reads: a file over `MAX_CAPTURED_FILE_BYTES`
/// is refused before `std::fs::read` ever runs, so nothing this far past any
/// sane size for a single in-memory `Vec<u8>` allocation is ever buffered
/// whole just to be classified.
#[test]
fn capture_body_refuses_a_file_far_past_any_sane_single_read() {
    const UNREASONABLE_FOR_ONE_READ: usize = 64 * 1024 * 1024; // 64 MiB
    let body = vec![b'x'; UNREASONABLE_FOR_ONE_READ];
    let dir = workspace(&[("huge.bin", &body)]);
    let result = capture_body(&dir.path().join("huge.bin"), "huge.bin", ArtifactKind::File);
    assert!(
        result.is_err(),
        "a file this large must be refused before being read whole into memory"
    );
}

#[test]
fn a_non_utf8_file_is_stored_as_bytes_whatever_its_size() {
    let png = [0x89, 0x50, 0x4e, 0x47, 0xff, 0xfe];
    let dir = workspace(&[("logo.png", &png)]);
    let captured = capture_body(
        &dir.path().join("logo.png"),
        "logo.png",
        ArtifactKind::Image,
    )
    .unwrap();
    assert_eq!(
        captured,
        PublishPayload::Bytes {
            bytes: png.to_vec(),
            mime: "image/png".to_string(),
        }
    );
    assert_eq!(
        captured.forced_kind(ArtifactKind::Image),
        ArtifactKind::Image,
        "an image stays an image so the console picks the right renderer"
    );
}

/// The payoff of #553, stated as a test: **no publish can produce a reference
/// record any more.** The branch that emitted "the file lives in the agent's
/// own sandbox … the payload unreachable" is gone, so a paid image generation
/// cannot become a dangling digest pointing into a directory that gets wiped.
///
/// Asserted over every shape that used to take that branch — over-cap text,
/// non-UTF-8 bytes, and an empty file — because the guarantee is "none of
/// them", not "not the one I happened to check".
#[test]
fn no_publish_can_produce_a_reference_record() {
    let over_cap = "x".repeat(MAX_ARTIFACT_BODY_BYTES + 1);
    let dir = workspace(&[
        ("big.txt", over_cap.as_bytes()),
        ("logo.png", &[0x89, 0xff, 0xfe]),
        ("empty.bin", &[]),
        ("small.md", b"# fine"),
    ]);
    for name in ["big.txt", "logo.png", "empty.bin", "small.md"] {
        let captured = capture_body(
            &dir.path().join(name),
            name,
            kind_for_extension(Path::new(name)),
        )
        .unwrap();
        let recorded = captured.artifact_body();
        assert!(
            !recorded.contains("sandbox"),
            "{name} still points at the sandbox: {recorded}"
        );
        assert!(
            !recorded.contains("unreachable"),
            "{name} still claims its payload is unreachable: {recorded}"
        );
        assert!(
            !recorded.contains("sha256"),
            "{name} hashes on the publish path; the store computes the digest once: {recorded}"
        );
    }
}

/// A binary version records a pointer, not the bytes — issue #187's rule. The
/// artifact chain stays the version history and the workspace node holds the
/// content.
#[test]
fn a_binary_version_records_a_description_and_not_the_bytes() {
    let payload = PublishPayload::Bytes {
        bytes: vec![0u8; 4096],
        mime: "image/png".to_string(),
    };
    let body = payload.artifact_body();
    assert!(body.contains("image/png"), "{body}");
    assert!(body.contains("4096 bytes"), "{body}");
    assert!(body.contains("company workspace"), "{body}");
}

/// Issue #663. The body composed **before** the store is asked must not assert
/// that the file is there — that claim was unconditional, and it survived a
/// workspace refusal, leaving the record promising a file that does not exist.
#[test]
fn a_pending_binary_version_does_not_claim_the_file_is_stored() {
    let payload = PublishPayload::Bytes {
        bytes: vec![0u8; 16],
        mime: "image/png".to_string(),
    };
    let body = payload.artifact_body_for(PayloadStorage::Pending);
    assert!(
        !body.contains("stored as a file"),
        "nothing has been stored yet: {body}"
    );
    assert!(
        !body.contains("Open it there"),
        "and the operator must not be sent to look for it: {body}"
    );
}

/// Issue #668. A stored version carries the digest **the store** computed, which
/// is what lets a reader tell two versions apart and see whether a re-publish
/// changed anything.
#[test]
fn a_stored_binary_version_records_the_stores_digest() {
    let payload = PublishPayload::Bytes {
        bytes: vec![0u8; 16],
        mime: "image/png".to_string(),
    };
    let body = payload.artifact_body_for(PayloadStorage::Stored {
        sha256: Some("abc123"),
    });
    assert!(body.contains("sha256 abc123"), "{body}");
    assert!(body.contains("stored as a file"), "{body}");
}

/// The defect #668 describes in one assertion: two versions of one binary that
/// coincide in mime and length used to be **literally equal strings**, so the
/// history could not say which was which. The digest is what separates them.
#[test]
fn two_binary_versions_of_the_same_length_differ_by_their_digest() {
    let payload = PublishPayload::Bytes {
        bytes: vec![0u8; 120_000],
        mime: "image/png".to_string(),
    };
    let v1 = payload.artifact_body_for(PayloadStorage::Stored {
        sha256: Some("1111111111111111"),
    });
    let v2 = payload.artifact_body_for(PayloadStorage::Stored {
        sha256: Some("2222222222222222"),
    });
    assert_ne!(
        v1, v2,
        "two versions of one deliverable must not be the same sentence"
    );

    // The control: without a digest they collapse back into one string, which
    // is exactly the state this issue is about.
    let bare = payload.artifact_body_for(PayloadStorage::Stored { sha256: None });
    assert_eq!(
        bare,
        payload.artifact_body_for(PayloadStorage::Stored { sha256: None }),
        "the no-digest body is the indistinguishable case, and it says so"
    );
    assert!(
        bare.contains("no digest recorded"),
        "a backend that recorded none must say so rather than imply identity: {bare}"
    );
}

/// Issue #663's other half: when the workspace refuses the file, the record
/// withdraws the claim instead of leaving it standing.
///
/// It must also NOT carry the store's error text — a version body is permanent
/// and a backend error can name host paths.
#[test]
fn a_refused_binary_version_withdraws_the_storage_claim() {
    let payload = PublishPayload::Bytes {
        bytes: vec![0u8; 16],
        mime: "image/png".to_string(),
    };
    let body = payload.artifact_body_for(PayloadStorage::Refused);
    assert!(body.contains("NOT stored"), "{body}");
    assert!(
        !body.contains("Open it there"),
        "the operator must not be sent to a file that is not there: {body}"
    );
}

/// Prose is unaffected by any of this: for text the version IS the content, so
/// it is complete whatever the tree did.
#[test]
fn a_prose_version_is_its_content_whatever_the_store_did() {
    let payload = PublishPayload::Text("# Spec".to_string());
    for storage in [
        PayloadStorage::Pending,
        PayloadStorage::Stored { sha256: None },
        PayloadStorage::Refused,
    ] {
        assert_eq!(payload.artifact_body_for(storage), "# Spec");
    }
}

// ── The tool ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn publishing_stages_the_file_and_reports_what_was_captured() {
    let dir = workspace(&[("specs/launch.md", b"# Spec\nShip it.")]);
    let (queue, claim) = claimed(PublishDestination::Task);
    let tool = PublishArtifactTool::new(dir.path(), "maya", queue.clone());

    let result = claim
        .scoped(run(&tool, json!({ "path": "specs/launch.md" })))
        .await;
    assert!(!result.is_error, "{}", text_of(&result));

    let staged = claim.drain();
    assert_eq!(staged.len(), 1);
    assert_eq!(staged[0].source, "specs/launch.md");
    assert_eq!(staged[0].kind, ArtifactKind::Markdown);
    assert_eq!(
        staged[0].payload,
        PublishPayload::Text("# Spec\nShip it.".to_string())
    );
    // Title defaults to the file name, not the whole path.
    assert_eq!(staged[0].title, "launch.md");
    assert_eq!(staged[0].note, None);
}

/// Issue #463: the staged item names **who published it**.
///
/// The queue is shared by every turn a cycle runs, so the drain site cannot
/// answer this — an operator message answered by the orchestrator and handed to
/// a desk stages a file from whichever of them reached for the tool. Without the
/// stamp the card and the artifact were filed under the turn's responder, so a
/// deliverable the writer produced was recorded as the orchestrator's.
#[tokio::test]
async fn a_staged_publish_names_the_agent_that_called_the_tool() {
    let dir = workspace(&[("memo.md", b"# Memo")]);
    let (queue, claim) = claimed(PublishDestination::Conversation);
    let tool = PublishArtifactTool::new(dir.path(), "writer", queue.clone());

    claim.scoped(run(&tool, json!({ "path": "memo.md" }))).await;

    let staged = claim.drain();
    assert_eq!(staged[0].agent, "writer");
}

#[tokio::test]
async fn an_explicit_title_kind_and_note_are_carried_through() {
    let dir = workspace(&[("out.dat", b"plain text really")]);
    let (queue, claim) = claimed(PublishDestination::Task);
    let tool = PublishArtifactTool::new(dir.path(), "maya", queue.clone());

    claim
        .scoped(run(
            &tool,
            json!({
                "path": "out.dat",
                "title": "Q3 export",
                "kind": "text",
                "note": "rewrote the pricing section"
            }),
        ))
        .await;

    let staged = claim.drain();
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
    let (queue, claim) = claimed(PublishDestination::Task);
    let tool = PublishArtifactTool::new(dir.path(), "maya", queue.clone());

    claim.scoped(run(&tool, json!({ "path": "spec.md" }))).await;
    // The agent's next step scribbles over the file.
    std::fs::write(dir.path().join("spec.md"), b"# clobbered afterwards").unwrap();

    let staged = claim.drain();
    assert_eq!(
        staged[0].payload,
        PublishPayload::Text("# The version I published".to_string())
    );
}

#[tokio::test]
async fn a_bad_path_is_a_truthful_tool_error_and_stages_nothing() {
    let dir = workspace(&[("spec.md", b"# Spec")]);
    let (queue, claim) = claimed(PublishDestination::Task);
    let tool = PublishArtifactTool::new(dir.path(), "maya", queue.clone());

    for path in ["../escape.md", "/etc/hosts", "nope.md", ""] {
        let result = claim.scoped(run(&tool, json!({ "path": path }))).await;
        assert!(result.is_error, "`{path}` was accepted");
    }
    // A missing `path` argument entirely.
    assert!(claim.scoped(run(&tool, json!({}))).await.is_error);
    assert!(
        claim.sources().is_empty(),
        "a refused publish must stage nothing"
    );
}

#[tokio::test]
async fn an_unknown_kind_is_refused_by_name() {
    let dir = workspace(&[("spec.md", b"# Spec")]);
    let (queue, claim) = claimed(PublishDestination::Task);
    let tool = PublishArtifactTool::new(dir.path(), "maya", queue.clone());

    let result = claim
        .scoped(run(
            &tool,
            json!({ "path": "spec.md", "kind": "spreadsheet" }),
        ))
        .await;
    assert!(result.is_error);
    let message = text_of(&result);
    assert!(message.contains("markdown"), "{message}");
    assert!(claim.sources().is_empty());
}
