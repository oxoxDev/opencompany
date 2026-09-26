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
    let (tool, log, seen, _queue) = wrapped_with_queue(outcome);
    (tool, log, seen)
}

/// [`wrapped`], keeping the queue the claim is staged onto.
fn wrapped_with_queue(
    outcome: impl Fn() -> ToolResult + Send + Sync + 'static,
) -> (
    TakeOverTool,
    Arc<MemoryLog>,
    Arc<Mutex<Vec<serde_json::Value>>>,
    crate::hive::takeover::TakeoverQueue,
) {
    let log = Arc::new(MemoryLog::default());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let complete = Box::new(FakeComplete {
        outcome: Box::new(outcome),
        seen: Arc::clone(&seen),
    }) as Box<dyn Tool>;
    let queue = crate::hive::takeover::TakeoverQueue::default();
    let loan = TakeoverLoan {
        episode: EPISODE.into(),
        events: Arc::clone(&log) as Arc<dyn EventLog>,
        company: record(TWO_DESKS).id,
        agent: "engineer".into(),
        queue: queue.clone(),
    };
    (TakeOverTool::new(complete, loan, "desk_"), log, seen, queue)
}

/// The episode the test's seat sits in, and the key its claims carry.
const EPISODE: &str = "ep-webauthn";

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

/// The note fires on **owning the work**, not on the shape of the reply.
///
/// Two live runs watched a guest hold the verb, be told its name, and not
/// reach for it. The second said the quiet part out loud -- "Yes, I'll own the
/// pricing launch campaign end to end. ... Tell the operator I've got it." --
/// and the operator's own line stayed empty.
///
/// The old wording triggered on "rather than hand back an answer", and from
/// that seat it *was* handing back an answer: it had been asked who should own
/// the work and answered that it would. So the one case the tool exists for
/// read as the case the note excluded. It now keys on becoming the owner,
/// however the sentence is shaped.
#[test]
fn the_note_fires_on_owning_the_work_not_on_refusing_to_answer() {
    let note = guest_persona_note("desk_");
    assert!(
        note.contains("own it"),
        "the trigger is ownership, which is what the seat knows about itself: {note}"
    );
    assert!(
        !note.contains("rather than hand back an answer"),
        "and not the shape of the reply, which excluded the very case it is for: {note}"
    );
}

/// And it closes the route the guest actually took instead.
///
/// Asked to own the campaign, the guest replied "Tell the operator I've got
/// it" -- delegating the announcement to the teammate that asked it, who
/// cannot make it: that teammate is finishing its own conversation, and the
/// operator never hears a word. A note that names the verb without closing
/// that door leaves the seat a plausible wrong move.
#[test]
fn the_note_says_no_one_else_can_tell_the_operator() {
    let note = guest_persona_note("desk_");
    assert!(
        note.contains("only way to reach the operator"),
        "the verb is the sole route, said plainly: {note}"
    );
    assert!(
        note.contains("pass it on"),
        "and relaying through the asker is named and refused: {note}"
    );
}

/// A claim is staged, so the work carries on in the claimer's own line.
///
/// Announcing alone left the handover a dead end: a live run watched a guest
/// say "I'm owning the pricing launch campaign end to end" and then stop --
/// no episode, no room, a line the operator had to prod to restart. The claim
/// is what the dispatcher opens that line from once the asker's episode ends.
///
/// Staged rather than opened here because the claimer is still a seat inside
/// that episode: opening its own line mid-turn would run one teammate twice.
#[tokio::test]
async fn a_claim_is_staged_for_the_claimers_own_line() {
    let (tool, log, _seen, queue) =
        wrapped_with_queue(|| ToolResult::success("recorded: your assignment is complete"));

    let result = tool.execute(args()).await.expect("the call runs");
    assert!(!result.is_error, "{}", text(&result));

    let staged = queue.drain(EPISODE);
    assert_eq!(staged.len(), 1, "one claim, for one takeover: {staged:?}");
    assert_eq!(staged[0].seat, "engineer", "the teammate that took it on");
    assert_eq!(
        staged[0].chat, "dm:engineer",
        "and its own line with the operator, which is where the work carries on"
    );
    assert!(
        staged[0].saying.contains("webauthn"),
        "carrying what it said, so the line opens on the claim: {staged:?}"
    );
    assert!(
        log.replies("dm:engineer").len() == 1,
        "the announcement still lands; staging is in addition to it, not instead"
    );
}

/// A refused conclusion stages nothing, for the same reason it announces
/// nothing: the room never transferred the work.
#[tokio::test]
async fn a_refused_conclusion_stages_no_claim() {
    let (tool, _log, _seen, queue) = wrapped_with_queue(|| {
        let mut refused = ToolResult::success("name exactly those");
        refused.is_error = true;
        refused
    });

    let refused = tool.execute(args()).await.expect("the call runs");

    assert!(refused.is_error);
    assert!(
        queue.drain(EPISODE).is_empty(),
        "nothing was claimed, so no line is opened"
    );
}

/// One episode's ending takes its own claims and leaves everyone else's.
///
/// The queue is reached through `HarnessDeps`, built once per company runtime
/// and shared by `Arc` through every clone a message makes, so every episode
/// in the company stages into the same `Vec`. An unkeyed drain handed the
/// first episode to finish the claims of episodes still running -- opening a
/// claimer's line while that teammate was still a live seat in the episode it
/// claimed in, which is the one thing staging exists to prevent.
#[test]
fn a_drain_takes_only_the_claims_of_the_episode_that_ended() {
    let queue = crate::hive::takeover::TakeoverQueue::default();
    let claim = |episode: &str, seat: &str| crate::hive::takeover::TakeoverClaim {
        episode: episode.into(),
        seat: seat.into(),
        chat: format!("dm:{seat}"),
        at: 7,
        saying: "I have this.".into(),
    };
    queue.stage(claim("ep-a", "engineer"));
    queue.stage(claim("ep-b", "designer"));
    queue.stage(claim("ep-a", "writer"));

    let ended = queue.drain("ep-a");
    assert_eq!(
        ended.iter().map(|c| c.seat.as_str()).collect::<Vec<_>>(),
        ["engineer", "writer"],
        "both of this episode's claims, in the order they were staged: {ended:?}"
    );

    let other = queue.drain("ep-b");
    assert_eq!(
        other.iter().map(|c| c.seat.as_str()).collect::<Vec<_>>(),
        ["designer"],
        "and the episode still running kept its own, to act on when it ends: {other:?}"
    );
    assert!(
        queue.drain("ep-a").is_empty() && queue.drain("ep-b").is_empty(),
        "a claim is acted on once; draining twice would open the same line twice"
    );
}
