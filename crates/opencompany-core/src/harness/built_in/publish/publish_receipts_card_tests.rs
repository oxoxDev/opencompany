use super::publish_test_helpers_tests::*;
use super::*;
use serde_json::json;

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
    let tool = PublishArtifactTool::new(dir.path(), "maya", queue.clone());

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
    let tool = PublishArtifactTool::new(dir.path(), "maya", queue.clone());

    let claim = queue.claim(PublishDestination::Task);
    let outcome = PUBLISH_SCOPE
        .scope(claim.scope, async {
            let published = !run(&tool, json!({ "path": "spec.md" })).await.is_error;
            drop(claim);
            (
                published,
                queue.destination(),
                queue.queued(),
                run(&tool, json!({ "path": "spec.md" })).await.is_error,
            )
        })
        .await;
    assert!(outcome.0, "the claimed publish succeeds");
    assert_eq!(
        outcome.1,
        PublishDestination::Unclaimed,
        "the claim must release on drop"
    );
    assert_eq!(
        outcome.2, 0,
        "releasing must also discard, so nothing leaks into the next caller"
    );
    assert!(
        outcome.3,
        "publishing must be refused once the claim has ended"
    );
}

/// A new claim is a fresh bucket, so one caller can never be handed the
/// previous caller's staged file.
#[tokio::test]
async fn a_new_claim_never_sees_what_a_previous_caller_left_staged() {
    let queue = PendingPublishQueue::default();
    let claim = queue.claim(PublishDestination::Task);
    claim
        .scoped(async {
            assert!(queue.push(PendingPublish {
                agent: "maya".to_string(),
                source: "stale.md".to_string(),
                title: "stale".to_string(),
                kind: ArtifactKind::Text,
                note: None,
                payload: PublishPayload::Text("old".to_string()),
            }));
        })
        .await;

    let next = queue.claim(PublishDestination::Conversation);
    assert!(next.sources().is_empty());
    assert_eq!(next.destination(), PublishDestination::Conversation);
    assert_eq!(
        claim.sources(),
        ["stale.md"],
        "the first claim keeps its own"
    );
}

/// The receipt must describe **this** caller's destination. One sentence written
/// for the task case and reused everywhere is what told a chat turn its file
/// would appear on a run that did not exist.
#[tokio::test]
async fn the_receipt_names_the_destination_the_caller_actually_has() {
    let dir = workspace(&[("spec.md", b"# Spec")]);

    let (task_queue, task_claim) = claimed(PublishDestination::Task);
    let task_tool = PublishArtifactTool::new(dir.path(), "maya", task_queue);
    let task_receipt = text_of(
        &task_claim
            .scoped(run(&task_tool, json!({ "path": "spec.md" })))
            .await,
    );

    let (chat_queue, chat_claim) = claimed(PublishDestination::Conversation);
    let chat_tool = PublishArtifactTool::new(dir.path(), "maya", chat_queue);
    let chat_receipt = text_of(
        &chat_claim
            .scoped(run(&chat_tool, json!({ "path": "spec.md" })))
            .await,
    );

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
        agent: "maya".to_string(),
        source: source.to_string(),
        title: title.to_string(),
        kind: ArtifactKind::Markdown,
        note: None,
        payload: PublishPayload::Text("body".to_string()),
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
            agent: "maya".to_string(),
            source: "specs/launch.md".to_string(),
            title: "Launch spec".to_string(),
            kind: ArtifactKind::Markdown,
            note: None,
            payload: PublishPayload::Text("body".to_string()),
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
