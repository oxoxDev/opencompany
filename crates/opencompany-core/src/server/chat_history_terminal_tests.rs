use super::tests_reactions::{at, labels};
use super::*;
use crate::ports::tasks::{
    COLUMN_DONE, COLUMN_IN_PROGRESS, COLUMN_IN_REVIEW, COLUMN_PAUSED, COLUMN_PLANNING, COLUMN_TODO,
};

/// A settled dispatch, as the harness journals it. `desk` is deliberately
/// an agent id (`engineer`) and never a channel id (`engineering`) — that
/// difference is the whole reason the origin has to be carried.
fn desk_task_completed(origin: Option<&str>, column: &str) -> CompanyEvent {
    threaded_desk_task_completed(origin, None, column)
}

/// The same settle, for a card raised inside a thread (#1890 B).
fn threaded_desk_task_completed(
    origin: Option<&str>,
    origin_parent: Option<u64>,
    column: &str,
) -> CompanyEvent {
    CompanyEvent::DeskTaskCompleted {
        task_id: "t-1".to_string(),
        desk: "engineer".to_string(),
        output: "the run's prose".to_string(),
        column: column.to_string(),
        artifact_ids: Vec::new(),
        origin_chat_id: origin.map(str::to_string),
        origin_parent: origin_parent.map(EventSeq::new),
    }
}

/// The terminal routes by the origin the card recorded, on exactly the same
/// terms a reply does: the desk's id or its name, and nothing else.
#[test]
fn a_terminal_belongs_to_the_channel_its_card_was_raised_in() {
    let event = desk_task_completed(Some("engineering"), COLUMN_IN_REVIEW);
    assert!(owns("engineering", "Engineering desk", &event));
    // …and by the desk's *name*, for a card whose origin was journaled
    // under it — the same either-spelling rule a reply routes by.
    let by_name = desk_task_completed(Some("Engineering desk"), COLUMN_IN_REVIEW);
    assert!(owns("engineering", "Engineering desk", &by_name));
    // …and nowhere else. A settle in one channel must not surface in
    // another, which is what would make the marker worse than no marker.
    assert!(!owns("strategy", "Strategy desk", &event));
    assert!(!owns(GENERAL_CHANNEL_ID, GENERAL_CHANNEL_NAME, &event));
    // The responder is not the channel — matching on it would file every
    // settle under a desk whose id happens to equal an agent's.
    assert!(!owns("engineer", "engineer", &event));
}

/// **The most bug-prone line in `owns`.** A card no conversation raised
/// belongs to no conversation's history — General emphatically included.
///
#[test]
fn a_terminal_with_no_origin_belongs_to_nobody_not_to_general() {
    let event = desk_task_completed(None, COLUMN_IN_REVIEW);
    assert!(
        !owns(GENERAL_CHANNEL_ID, GENERAL_CHANNEL_NAME, &event),
        "an origin-less terminal must not land in #general",
    );
    assert!(!owns("", "", &event));
    assert!(!owns("engineering", "Engineering desk", &event));
}

/// A terminal journaled under a legacy General spelling decodes to #general
/// on read, so #general owns it and no named desk does.
#[test]
fn a_legacy_general_terminal_decodes_to_general() {
    for origin in ["General", "main", ""] {
        let journaled = serde_json::to_value(desk_task_completed(Some(origin), COLUMN_PAUSED))
            .expect("serialize");
        let event: CompanyEvent = serde_json::from_value(journaled).expect("decode");
        assert!(
            owns(GENERAL_CHANNEL_ID, GENERAL_CHANNEL_NAME, &event),
            "a terminal stored as `{origin}` belongs to #general",
        );
        assert!(!owns("strategy", "Strategy desk", &event));
    }
}

/// The marker's wording, pinned per column. The console holds the same
/// literals (`dispatchMarkerText`, `frontend/src/lib/chat.ts`) because the
/// live frame carries the raw column id; these two tests are what couple
/// them.
#[test]
fn the_marker_names_where_the_card_landed() {
    assert_eq!(
        dispatch_marker_text(COLUMN_IN_REVIEW),
        "finished → In review"
    );
    assert_eq!(dispatch_marker_text(COLUMN_PAUSED), "finished → Paused");
    assert_eq!(dispatch_marker_text(COLUMN_TODO), "finished → To-do");
    assert_eq!(dispatch_marker_text(COLUMN_DONE), "finished → Done");
    assert_eq!(dispatch_marker_text(COLUMN_PLANNING), "finished → Planning");
    assert_eq!(
        dispatch_marker_text(COLUMN_IN_PROGRESS),
        "finished → In progress"
    );
}

/// A column this build has not heard of reads a little raw rather than
/// rendering blank — the same fallback `relay_text` takes, and the reason a
/// newer host cannot produce an empty pill here.
#[test]
fn an_unknown_column_passes_through_verbatim() {
    assert_eq!(
        dispatch_marker_text("shipped_to_orbit"),
        "finished → shipped_to_orbit"
    );
}

/// The terminal projects as a system line carrying its card — not as the
/// `Debug` dump the defensive fallback would have rendered into a person's
/// transcript.
#[test]
fn project_renders_a_terminal_as_a_card_linked_system_marker() {
    let view = MessageView::project(
        at(
            21,
            desk_task_completed(Some("engineering"), COLUMN_IN_REVIEW),
        ),
        &Viewer::Operator,
        &labels(),
    );
    assert_eq!(view.author, "system");
    assert_eq!(view.channel, "system");
    assert_eq!(view.text, "finished → In review");
    assert_eq!(
        view.task_id.as_deref(),
        Some("t-1"),
        "the pill links the card"
    );
    assert!(!view.mine);
    assert!(view.steps.is_empty(), "a marker is not a turn");
    assert!(
        view.parent_id.is_none(),
        "a card raised at channel level settles flat in the channel",
    );
    assert_eq!(view.id, "21", "the host id the console dedupes a reload on");
}

/// Issue #1890 B — the whole of what this sub-issue repairs.
///
/// A card raised inside a thread used to settle flat in the channel, so the
/// thread that asked for the work never showed it finishing. The marker
/// carries the root now, in the same field and the same rendering an
/// operator message's parent takes, so the console files it into the thread
/// with no renderer change at all.
#[test]
fn a_terminal_raised_in_a_thread_projects_into_that_thread() {
    let view = MessageView::project(
        at(
            50,
            threaded_desk_task_completed(Some("engineering"), Some(41), COLUMN_IN_REVIEW),
        ),
        &Viewer::Operator,
        &labels(),
    );
    assert_eq!(
        view.parent_id.as_deref(),
        Some("41"),
        "the marker hangs off the root the card recorded",
    );
    // The channel half is unchanged: routing still runs through `owns` on
    // the origin channel, and the thread only narrows within it. A marker
    // that threaded but stopped belonging to its channel would vanish.
    assert!(owns(
        "engineering",
        "Engineering desk",
        &threaded_desk_task_completed(Some("engineering"), Some(41), COLUMN_IN_REVIEW),
    ));
}

/// The run's prose stays out of the marker. It already reaches this same
/// channel as the orchestrator's relay bubble (#151); repeating it here
/// would put one run's words into one conversation twice.
#[test]
fn the_marker_does_not_repeat_the_runs_prose() {
    let view = MessageView::project(
        at(22, desk_task_completed(Some("engineering"), COLUMN_PAUSED)),
        &Viewer::Operator,
        &labels(),
    );
    assert!(!view.text.contains("the run's prose"), "{}", view.text);
    assert_eq!(view.text, "finished → Paused");
}
