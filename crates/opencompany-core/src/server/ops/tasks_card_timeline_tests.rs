use super::*;
use crate::ports::types::{CompanyId, StoredEvent};
use crate::runtime::{CHANGE_OPENED, CHANGE_UPDATED};

fn card_changed(seq: u64, id: &str, change: &str, column: Option<&str>) -> StoredEvent {
    StoredEvent {
        seq: EventSeq::new(seq),
        company: CompanyId::new("acme"),
        event: CompanyEvent::TaskCardChanged {
            task_id: id.to_string(),
            change: change.to_string(),
            column: column.map(str::to_string),
        },
        at_millis: 1_700_000_000_000 + seq,
    }
}

fn run_fold(page: &[StoredEvent], task_id: &str) -> TaskFold {
    let discussion = DiscussionWindow {
        before_seq: None,
        first: 0,
    };
    let mut fold = TaskFold::default();
    fold_page(
        page,
        task_id,
        discussion,
        |_id| None,
        &std::collections::HashSet::new(),
        &mut fold,
    );
    fold
}

#[test]
fn a_card_changed_event_reaches_its_cards_timeline() {
    let page = [card_changed(1, "t-1", CHANGE_OPENED, Some("todo"))];
    let fold = run_fold(&page, "t-1");
    assert_eq!(
        fold.timeline.len(),
        1,
        "the card event never reached the timeline"
    );
    assert_eq!(fold.timeline[0].kind, "card");
    assert_eq!(fold.timeline[0].label, "Card opened → todo");
}

#[test]
fn a_card_changed_event_for_another_card_stays_off_this_timeline() {
    let page = [card_changed(1, "t-2", CHANGE_UPDATED, Some("in_progress"))];
    let fold = run_fold(&page, "t-1");
    assert!(
        fold.timeline.is_empty(),
        "an unrelated card's write leaked onto this task's timeline: {:?}",
        fold.timeline
    );
}
