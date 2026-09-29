use super::tests_core::*;

/// The allowlist refusal must name what the member CAN reach: the model has
/// no other way to learn its own `delegates_to`.
#[test]
fn an_out_of_allowlist_desk_is_refused_with_the_permitted_set() {
    let record = record();
    let allowed = vec!["content".to_string()];
    let message =
        reject_out_of_allowlist_target(&record, &allowed, "engineering").expect("rejected");
    assert!(message.contains("engineering"), "{message}");
    assert!(message.contains("content"), "{message}");
    // The permitted desk itself passes, by id and by display name.
    assert_eq!(
        reject_out_of_allowlist_target(&record, &allowed, "content"),
        None
    );
    assert_eq!(
        reject_out_of_allowlist_target(&record, &allowed, "Content desk"),
        None
    );
    // …and an allowlist written with display names admits the id.
    let by_name = vec!["Content desk".to_string()];
    assert_eq!(
        reject_out_of_allowlist_target(&record, &by_name, "content"),
        None
    );
}

/// `"*"` admits every desk, and so does an empty allowlist — the default for
/// an agent whose manifest entry says nothing, which now carries the tool.
#[test]
fn the_wildcard_and_an_empty_allowlist_both_admit_every_desk() {
    let record = record();
    for allowed in [vec!["*".to_string()], Vec::new()] {
        for desk in ["engineering", "content", "legal"] {
            assert_eq!(
                reject_out_of_allowlist_target(&record, &allowed, desk),
                None,
                "{allowed:?} must admit {desk}"
            );
        }
    }
}

#[test]
fn delegate_args_require_desk_and_instruction() {
    assert_eq!(DelegateArgs::parse(&json!({ "desk": "eng" })), None);
    assert_eq!(
        DelegateArgs::parse(&json!({ "instruction": "do it" })),
        None
    );
    let parsed = DelegateArgs::parse(&json!({
        "desk": " engineering ",
        "instruction": " build the thing "
    }))
    .expect("valid");
    assert_eq!(parsed.desk, "engineering");
    assert_eq!(parsed.instruction, "build the thing");
}

/// Issue #1835: an `auto` channel answers the three routing questions
/// three different ways, and each is load-bearing. `desk_lead` is `None`
/// — no lead exists, so no crown, no badge, no `delegate_to_desk` target.
/// `desk_default_responder` is the first roster member — the deterministic
/// fallback wherever per-message selection cannot run. `chat_responder`
/// answers that fallback, so the small-talk fast path and the default
/// build route exactly as a lead desk would have.
#[test]
fn an_auto_channel_has_no_lead_but_a_deterministic_responder() {
    let mut record = record();
    record.overlay_desks.push(crate::ports::types::OverlayDesk {
        id: "launch".to_string(),
        name: "Launch week".to_string(),
        description: None,
        members: vec!["ceo".to_string(), "writer".to_string()],
        responder: crate::ports::types::ResponderMode::Auto,
        hive: Default::default(),
    });
    assert_eq!(
        desk_lead(&record, "launch"),
        None,
        "auto channels have no lead"
    );
    assert_eq!(
        desk_default_responder(&record, "launch").as_deref(),
        Some("ceo"),
        "the deterministic fallback is the first roster member"
    );
    assert_eq!(
        chat_responder(&record, "launch").as_deref(),
        Some("ceo"),
        "the shared seam answers the fallback, not None"
    );
    // An overlay desk that never states a mode is lead-routed — the
    // pre-#1835 behaviour, byte-for-byte.
    record.overlay_desks.push(crate::ports::types::OverlayDesk {
        id: "growth".to_string(),
        name: "Growth".to_string(),
        description: None,
        members: vec!["writer".to_string()],
        responder: crate::ports::types::ResponderMode::default(),
        hive: Default::default(),
    });
    assert_eq!(desk_lead(&record, "growth").as_deref(), Some("writer"));
}

/// Issue #1835: a hand-off to an `auto` channel is refused with the real
/// reason — no lead exists by design — never with the leadless-desk arm's
/// "no member on the roster", which would be a lie about a staffed
/// channel. The valid-target list must also exclude the channel itself.
#[test]
fn delegating_to_an_auto_channel_is_refused_naming_the_selection() {
    let mut record = record();
    record.overlay_desks.push(crate::ports::types::OverlayDesk {
        id: "launch".to_string(),
        name: "Launch week".to_string(),
        description: None,
        members: vec!["ceo".to_string(), "writer".to_string()],
        responder: crate::ports::types::ResponderMode::Auto,
        hive: Default::default(),
    });
    let message = reject_desk_target(&record, "launch").expect("refused");
    assert!(message.contains("picked per message"), "{message}");
    assert!(
        !message.contains("no member on the roster"),
        "a staffed channel must not be reported empty: {message}"
    );
    // Desks that can take work are still offered — and never the channel.
    assert!(message.contains("engineering"), "{message}");
    assert!(
        !message.contains("Desks that can take work: launch"),
        "{message}"
    );
}
