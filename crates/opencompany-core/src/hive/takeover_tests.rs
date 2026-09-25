//! What `take_over` must hold: the room decides first, and the operator is
//! told only when it said yes.

use std::sync::{Arc, Mutex};

use super::*;
use crate::hive::test_support::{MemoryLog, TWO_DESKS, record};
use crate::ports::events::EventLog;

/// A stand-in for the room's `complete_episode`, recording what it was passed.
///
/// The real one lives in `tinyhivemind-tools` behind an `EpisodeTools` that
/// needs a registered turn; what this file is about is the *wrapper's* rule --
/// conclude first, announce second, announce never on a refusal -- so the
/// thing being wrapped is the part worth faking.
struct FakeComplete {
    outcome: Box<dyn Fn() -> ToolResult + Send + Sync>,
    seen: Arc<Mutex<Vec<serde_json::Value>>>,
}

#[async_trait::async_trait]
impl Tool for FakeComplete {
    fn name(&self) -> &str {
        "desk_complete_episode"
    }
    fn description(&self) -> &str {
        "fake"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        self.seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(args);
        Ok((self.outcome)())
    }
}

fn wrapped(
    outcome: impl Fn() -> ToolResult + Send + Sync + 'static,
) -> (
    TakeOverTool,
    Arc<MemoryLog>,
    Arc<Mutex<Vec<serde_json::Value>>>,
) {
    let log = Arc::new(MemoryLog::default());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let complete = Box::new(FakeComplete {
        outcome: Box::new(outcome),
        seen: Arc::clone(&seen),
    }) as Box<dyn Tool>;
    let loan = TakeoverLoan {
        events: Arc::clone(&log) as Arc<dyn EventLog>,
        company: record(TWO_DESKS).id,
        agent: "engineer".into(),
    };
    (TakeOverTool::new(complete, loan, "desk_"), log, seen)
}

fn args() -> serde_json::Value {
    serde_json::json!({
        "message": "I have the webauthn estimate, I will follow up here.",
        "chat": "dm:copywriter",
        "parent": null
    })
}

fn text(result: &ToolResult) -> String {
    serde_json::to_string(&result.content).expect("content serialises")
}

/// The happy path: the conversation concludes, then the operator is told.
///
/// The ordering is the whole point. `Ledger::answered` is reached only by the
/// conductor's `Dm`, minted when a conversation ends -- so the conclusion is
/// what releases the teammate that asked. An announcement without it is a
/// teammate stuck awaiting an answer that is never coming.
#[tokio::test]
async fn concluding_first_is_what_earns_the_announcement() {
    let (tool, log, seen) =
        wrapped(|| ToolResult::success("recorded: your assignment is complete"));

    let result = tool.execute(args()).await.expect("the call runs");

    assert!(!result.is_error, "{}", text(&result));
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "the room's own tool decided it"
    );
    assert_eq!(
        seen.lock().unwrap()[0],
        args(),
        "and was passed the arguments unchanged, so its refusals stay its own"
    );

    let told = log.replies("dm:engineer");
    assert_eq!(
        told.len(),
        1,
        "one row, in the claimer's own line: {told:?}"
    );
    assert_eq!(told[0].0, "engineer");
    assert!(
        told[0].1.contains("webauthn"),
        "carrying what it said: {told:?}"
    );
    assert!(
        log.replies("dm:copywriter").is_empty(),
        "and nothing in the asker's line, which no longer holds the work"
    );
    assert!(
        text(&result).contains("dm:engineer"),
        "the seat is told where the operator will reach it: {}",
        text(&result)
    );
}

/// A refused conclusion announces nothing at all.
///
/// This is the failure the wrapper exists to make unreachable. A live run
/// showed a seat naming the wrong `chat` and being refused; if the
/// announcement had gone out anyway, the operator would have been told a
/// teammate owned work that the room never transferred.
#[tokio::test]
async fn a_room_that_refuses_leaves_the_operator_untold() {
    let (tool, log, _seen) = wrapped(|| {
        let mut refused = ToolResult::success(
            "this turn is in chat `dm:copywriter` with parent null; name exactly those",
        );
        refused.is_error = true;
        refused
    });

    let result = tool.execute(args()).await.expect("the call runs");

    assert!(result.is_error, "the refusal is passed straight back");
    assert!(
        text(&result).contains("name exactly those"),
        "in the room's own words, not reworded: {}",
        text(&result)
    );
    assert!(
        log.rows().is_empty(),
        "and nothing was announced: {:?}",
        log.rows().len()
    );
}

/// An empty `message` is refused before the room is asked.
///
/// The announcement is the message; there is nothing to write, and concluding
/// first would leave a settled conversation behind a call that then fails.
#[tokio::test]
async fn an_empty_message_never_reaches_the_room() {
    let (tool, log, seen) = wrapped(|| ToolResult::success("recorded"));

    let refused = tool
        .execute(serde_json::json!({"message": "   ", "chat": "dm:copywriter", "parent": null}))
        .await;

    assert!(refused.is_err(), "the call is rejected");
    assert!(seen.lock().unwrap().is_empty(), "the room was not asked");
    assert!(log.rows().is_empty(), "and nothing was announced");
}

/// Who gets the verb at all.
#[test]
fn only_a_guest_in_an_operator_dm_is_one() {
    assert!(
        is_guest_seat("dm:copywriter", "engineer"),
        "a teammate bound into somebody else's line can claim the work"
    );
    assert!(
        !is_guest_seat("dm:engineer", "engineer"),
        "the owner is already the operator's correspondent there"
    );
    assert!(
        !is_guest_seat("engineering", "engineer"),
        "a desk has no operator line to announce into"
    );
}

/// The note names the tool by the name the seat can actually see.
///
/// A live run watched a guest agree in words to own the work and then call
/// `complete_episode`: the note said `take_over`, its belt said
/// `desk_take_over`, and it reached for the only verb whose name matched
/// something it had.
#[test]
fn the_guest_note_names_the_prefixed_tool() {
    let note = guest_persona_note("desk_");
    assert!(
        note.contains("`desk_take_over`"),
        "the belt's own name, prefix and all: {note}"
    );
    assert!(
        !note.contains("`take_over`"),
        "and never the bare one, which is on no belt: {note}"
    );
}
