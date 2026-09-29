use super::*;
use crate::company::CompanyManifest;
use crate::ports::types::CompanyId;
use crate::runtime::RuntimeBuilder;

pub(super) fn manifest() -> CompanyManifest {
    toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n")
        .expect("parse manifest")
}

pub(super) async fn runtime(home: &std::path::Path) -> Arc<CompanyRuntime> {
    Arc::new(
        RuntimeBuilder::new(home.to_path_buf(), manifest())
            .with_id(CompanyId::new("acme"))
            .build()
            .await
            .expect("build a runtime"),
    )
}

/// Helper: the marker and the agent-authored line it caused, as one leg.
pub(super) fn referral_leg(
    from_desk: &str,
    from_desk_name: &str,
    asker: &str,
    to_desk: &str,
    target: &str,
    returning: bool,
    text: &str,
) -> [CompanyEvent; 2] {
    referral_leg_answering(
        from_desk,
        from_desk_name,
        asker,
        to_desk,
        target,
        returning,
        text,
        None,
    )
}

/// The same, with the forward this return answers named explicitly — the
/// pointer the host records so the projection need not scan for it.
#[expect(clippy::too_many_arguments, reason = "a journal event's own shape")]
pub(super) fn referral_leg_answering(
    from_desk: &str,
    from_desk_name: &str,
    asker: &str,
    to_desk: &str,
    target: &str,
    returning: bool,
    text: &str,
    answers: Option<u64>,
) -> [CompanyEvent; 2] {
    [
        CompanyEvent::ReferralEnqueued {
            // These fixtures are desk crossings, which run on the target's
            // own desk and name no pair conversation — and so name no rows
            // either: a range is what a tool-sent DM carries, because it
            // journals before the reply its marker folds onto.
            conversation: None,
            rows: None,
            answers,
            from_desk: from_desk.to_string(),
            from_desk_name: from_desk_name.to_string(),
            asker: asker.to_string(),
            asker_label: asker.to_string(),
            trigger_sequence: 1,
            to_desk: to_desk.to_string(),
            target: target.to_string(),
            returning,
            episode_id: None,
            to_episode_id: None,
            hop: 0,
        },
        CompanyEvent::OperatorMessage {
            text: text.to_string(),
            by: Some(Actor {
                kind: ActorKind::Agent,
                id: asker.to_string(),
            }),
            chat: Some(to_desk.to_string()),
            parent: None,
            deliverable: None,
            mentions: Vec::new(),
            attachments: Vec::new(),
        },
    ]
}
