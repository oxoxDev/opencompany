use super::*;

// ---------------------------------------------------------------------------
// Issue #1725 — the greeting fast-path + context-isolation regression.
//
// The direct reproduction of the screenshot bug, end to end through the real
// `CompanyAgent::run_with_steer` turn path against the scripted model: a task
// that fetched content (the "sport story ranking" HTML) leaves the agent's
// in-memory history full, and a bare "hi" on a NEW chat used to run the whole
// agentic loop AND reply against the prior task's replayed content. The four
// unit tests cover the mechanisms individually (per-turn tool/memory/goal
// suppression, the greeting classifier, the chat-only hint, per-conversation
// history); this is the one test that exercises them together and asserts the
// observable symptoms the screenshot showed.
// ---------------------------------------------------------------------------

/// A live turn-stream routing context for `chat`, so the pool binds this
/// agent's history to that thread (issue #1725).
fn stream_for(chat: &str) -> crate::turn_stream::TurnStreamCtx {
    crate::turn_stream::TurnStreamCtx {
        company: CompanyId::new("acme"),
        agent_id: "ceo".to_string(),
        route: crate::turn_stream::LiveRoute::Chat {
            chat_id: chat.to_string(),
        },
        message_seq: None,
    }
}

/// A background task (`stream: None` — the same shape `run_background` and
/// `run_steered_background` hand `run_with_steer`, since neither carries a
/// chat thread) must not leave the agent bound to whichever chat happened to
/// stream last. Reverting the unthreaded-turn branch in `run_with_steer`
/// leaves `bound_chat` pointed at "sports" after the background turn runs, so
/// the operator's SECOND turn on that same chat reads `switched == false`,
/// skips the clear-and-reseed, and inherits the background task's fetched
/// content — the cross-context leak review found on #1725.
#[tokio::test]
async fn a_background_turn_does_not_leak_into_the_next_turn_on_its_bound_chat() {
    const FETCHED: &str = "BACKGROUND_TASK_MARKER_71B2";

    let (model_url, script) = spawn_script(
        vec![
            // Chat "sports", turn 1: a plain reply — binds bound_chat to "sports".
            Turn::Say("Sure, tracking the sports desk."),
            // The background task: a tool round, then an answer.
            Turn::Call {
                tool: "file_read".to_string(),
                args: json!({ "path": "note-00.md" }),
            },
            Turn::Say("Background task done."),
            // Chat "sports", turn 2 — the SAME chat id as turn 1.
            Turn::Say("Sounds good."),
        ],
        12,
    )
    .await;

    let dir = tempfile::tempdir().unwrap();
    let agent = company_agent(model_url, dir.path(), None, 1).await;
    // Overwrite the seeded note with the distinctive background-task body.
    let workspace = agent_workspace(dir.path(), &CompanyId::new("acme"), "ceo");
    std::fs::write(
        workspace.join("note-00.md"),
        format!("{FETCHED}\n<html>background task content</html>\n"),
    )
    .expect("seed the fetched note");

    // ── Chat "sports", turn 1: binds bound_chat to "sports". ──
    agent
        .run_with_steer(
            "hello from sports",
            None,
            Some(stream_for("sports")),
            None,
            ChatTarget::channel(Some("sports")),
        )
        .await
        .0
        .expect("chat turn 1 runs");

    // ── The background task: unthreaded — `stream: None`, same shared Agent. ──
    let (outcome_bg, _usage_bg) = agent
        .run_with_steer(
            "run the background task",
            None,
            None,
            None,
            // A background task names no conversation — the point of the case.
            ChatTarget::default(),
        )
        .await;
    let outcome_bg = outcome_bg.expect("background task runs");
    assert!(
        !outcome_bg.steps.is_empty(),
        "the background task must actually run a tool step (the fetch) — \
         otherwise the isolation below proves nothing"
    );
    {
        let seen = script.seen.lock().unwrap();
        assert!(
            seen.iter().any(|r| r.to_string().contains(FETCHED)),
            "the background task's fetched content must reach the model on \
             its own turn"
        );
    }

    let calls_before = model_calls(&script);

    // ── Chat "sports", turn 2 — same chat id the background task ran under
    //    no binding for, so this must be treated as a switch and re-seed. ──
    agent
        .run_with_steer(
            "still there?",
            None,
            Some(stream_for("sports")),
            None,
            ChatTarget::channel(Some("sports")),
        )
        .await
        .0
        .expect("chat turn 2 runs");

    assert_eq!(
        model_calls(&script) - calls_before,
        1,
        "chat turn 2 must be a single model call, not a loop"
    );

    let turn2_req = script
        .seen
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("chat turn 2 produced a model request");

    assert!(
        !turn2_req.to_string().contains(FETCHED),
        "the background task's fetched content must NOT leak into the next \
         turn on the chat it happened to be bound to before the background \
         task ran"
    );
}
