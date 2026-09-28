//! `operator` unit tests, group 18 of N — split out of `operator_test_group_11.rs`
//! once it passed the 750-line cap (issue tracked alongside the rest of the
//! `operator.rs` test split). No topical grouping: the flat inline module
//! this came from had none either.

use super::*;

use super::operator_test_support_3::*;

/// A park with no conversation behind it omits the channel entirely, so a
/// console filtering by thread matches it nowhere and it stays on the
/// Approvals page (#379).
#[test]
fn projects_approval_parked_without_a_channel_when_no_thread_produced_it() {
    let v = super::project_event(&stored(CompanyEvent::ApprovalParked {
        approval_id: ApprovalId::new("appr-cron"),
        effect_kind: "email.send".into(),
        thread: None,
    }))
    .expect("a parked approval is an attention signal");
    assert_eq!(v["type"], "approval_parked");
    assert!(
        v.get("chatId").is_none(),
        "a page-only approval must carry no channel: {v}",
    );
}

#[test]
fn projects_agent_reply_omits_empty_steps() {
    let v = super::project_event(&stored(CompanyEvent::AgentReply {
        audience: Vec::new(),
        mentions: Vec::new(),
        mention_depth: 0,
        parent: None,
        task_id: None,
        outputs: Vec::new(),
        chat_id: "General".into(),
        agent_id: "ceo".into(),
        text: "hi".into(),
        steps: Vec::new(),
        episode: None,
    }))
    .expect("agent_reply is an attention signal");
    // A tool-less reply keeps the legacy wire shape — no `steps` key.
    assert!(v.get("steps").is_none());
    // …and an uncorrelated reply carries no `taskId` either, so the
    // pre-#185 wire shape is byte-for-byte what it was.
    assert!(v.get("taskId").is_none());
}

/// A crossing put to a person is a two-way exchange, and the frame says so.
#[test]
fn a_direct_crossing_names_both_sides_of_the_exchange() {
    let v = super::project_event(&stored(CompanyEvent::ReferralEnqueued {
        conversation: Some("dm:cancellations+amendments".into()),
        answers: None,
        from_desk: "order_ops".into(),
        from_desk_name: "Order Operations".into(),
        asker: "cancellations".into(),
        asker_label: "cancellations".into(),
        trigger_sequence: 12,
        to_desk: "order_ops".into(),
        target: "amendments".into(),
        returning: false,
        rows: None,
        episode_id: None,
        to_episode_id: None,
        hop: 0,
    }))
    .expect("a direct crossing is projected");

    assert_eq!(v["direct"], true, "a person was asked, not a desk");
    assert_eq!(v["target"], "amendments");
    assert_eq!(v["asker"], "cancellations");
    assert!(
        v.get("lines").is_none(),
        "and still no crossing content: {v}"
    );
}
