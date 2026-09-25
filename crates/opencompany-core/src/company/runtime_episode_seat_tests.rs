//! Runtime tests: approvals a hive episode seat parked under its own turn key.

use crate::ports::types::{ApprovalId, Effect, EffectGroup};
use crate::runtime::approval_park::{ApprovalParker, ParkSite};
use crate::runtime::episode_resume::turn_key;
use crate::runtime::journal::{ApprovalConversation, TaskLink};

use super::tests_approval::runtime_with_events;

pub(super) fn seat_effect(kind: &str, agent: &str, payload: serde_json::Value) -> Effect {
    Effect {
        kind: kind.to_owned(),
        group: EffectGroup::Other,
        amount_usd: None,
        established_thread: false,
        first_time_counterparty: false,
        payload,
        agent: Some(agent.to_owned()),
        run_id: None,
    }
}

pub(super) async fn park_for_seat(
    rt: &crate::company::runtime::CompanyRuntime,
    episode: &str,
    seat: &str,
    effect: Effect,
) -> ApprovalId {
    ApprovalParker::new(
        rt.approvals.clone(),
        rt.journal.clone(),
        rt.grants.clone(),
        rt.continuations.clone(),
        rt.events.clone(),
    )
    .park(
        &rt.id,
        effect,
        ParkSite {
            task: TaskLink::Unlinked,
            conversation: ApprovalConversation {
                thread: Some("engineering".to_owned()),
                parent: None,
            },
            turn: Some(turn_key(episode, seat)),
        },
    )
    .await
    .expect("parked")
}

#[tokio::test]
async fn a_seat_approval_names_its_episode_and_seat() {
    let (rt, _home) = runtime_with_events().await;
    park_for_seat(
        &rt,
        "ep1",
        "ceo",
        seat_effect("shell", "ceo", serde_json::json!({ "cmd": "ls" })),
    )
    .await;
    let summary = &rt.pending_approvals()[0];
    let episode = summary
        .episode
        .as_ref()
        .expect("an episode seat's approval");
    assert_eq!(episode.id, "ep1");
    assert_eq!(episode.seat, "ceo");
    assert_eq!(summary.thread.as_deref(), Some("engineering"));
    let wire = serde_json::to_value(summary).unwrap();
    assert_eq!(
        wire["episode"],
        serde_json::json!({ "id": "ep1", "seat": "ceo" })
    );
}

#[tokio::test]
async fn an_ordinary_approval_names_no_episode() {
    let (rt, _home) = runtime_with_events().await;
    super::tests_approval::seed_parked(&rt, "plain", 5_000).await;
    let summary = &rt.pending_approvals()[0];
    assert!(summary.episode.is_none());
    assert!(
        serde_json::to_value(summary)
            .unwrap()
            .get("episode")
            .is_none()
    );
}

use std::sync::{Arc, Mutex};

use crate::ports::types::{Actor, ActorKind, CompanyEvent, Verdict};
use crate::runtime::episode_resume::{SeatAsk, SeatVerdict};

/// A brain that records the episodes it is asked to resume and fails any
/// cycle, so a seat's decision that fell through to a chat turn shows up.
#[derive(Default)]
struct ResumeRecorder {
    resumed: Mutex<Vec<String>>,
    cycles: Mutex<usize>,
}

#[async_trait::async_trait]
impl crate::ports::Brain for ResumeRecorder {
    async fn run_cycle(
        &self,
        _req: crate::ports::types::CycleRequest,
        _host: &dyn crate::ports::brain::CycleHost,
    ) -> crate::Result<crate::ports::types::CycleResult> {
        *self.cycles.lock().unwrap() += 1;
        Err(crate::error::OpenCompanyError::InvalidRequest(
            "an episode seat's decision must not run a chat cycle".into(),
        ))
    }

    async fn resume_episode(&self, episode_id: &str) -> bool {
        self.resumed.lock().unwrap().push(episode_id.to_owned());
        true
    }
}

async fn runtime_with_brain(
    brain: Arc<ResumeRecorder>,
) -> (
    Arc<crate::company::runtime::CompanyRuntime>,
    tempfile::TempDir,
) {
    let home = tempfile::tempdir().expect("tempdir");
    let manifest: crate::company::types::CompanyManifest = toml::from_str(
        r#"
        [company]
        name = "Acme"

        [[agent]]
        id = "ceo"
        role = "Chief"

        [policy]
        mode = "supervised"
        "#,
    )
    .expect("manifest");
    let rt = crate::runtime::RuntimeBuilder::new(home.path().to_path_buf(), manifest)
        .with_brain(brain)
        .build()
        .await
        .expect("runtime");
    (Arc::new(rt), home)
}

fn operator() -> Actor {
    Actor {
        kind: ActorKind::Operator,
        id: "owner".into(),
    }
}

async fn resolved_events(rt: &crate::company::runtime::CompanyRuntime) -> Vec<Verdict> {
    rt.events
        .read_from(&rt.id, crate::ports::types::EventSeq::new(0), usize::MAX)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|stored| match stored.event {
            CompanyEvent::ApprovalResolved { verdict, .. } => Some(verdict),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn an_approved_seat_call_goes_to_its_running_episode_not_a_chat_cycle() {
    let brain = Arc::new(ResumeRecorder::default());
    let (rt, _home) = runtime_with_brain(brain.clone()).await;
    let releases = rt.grants.episode_releases();
    releases.start("ep1");
    let id = park_for_seat(
        &rt,
        "ep1",
        "ceo",
        seat_effect("send_email", "ceo", serde_json::json!({ "to": "a@b.c" })),
    )
    .await;
    rt.resolve_approval(&id, Verdict::Approve, operator())
        .await
        .expect("resolved");
    let released = releases.take("ep1", &["ceo".to_owned()]);
    let decision = &released["ceo"][0];
    assert_eq!(decision.verdict, SeatVerdict::Approved);
    assert_eq!(
        decision.ask,
        SeatAsk::Call {
            tool: "send_email".into(),
            args: serde_json::json!({ "to": "a@b.c" }),
        }
    );
    assert!(
        rt.grants.peek(&id).is_some(),
        "the seat redeems the single-use grant itself"
    );
    assert_eq!(*brain.cycles.lock().unwrap(), 0, "no chat cycle ran");
    assert!(brain.resumed.lock().unwrap().is_empty());
    assert_eq!(resolved_events(&rt).await, vec![Verdict::Approve]);
}

#[tokio::test]
async fn a_decision_for_an_episode_not_running_resumes_it() {
    let brain = Arc::new(ResumeRecorder::default());
    let (rt, _home) = runtime_with_brain(brain.clone()).await;
    let id = park_for_seat(
        &rt,
        "ep1",
        "ceo",
        seat_effect("send_email", "ceo", serde_json::json!({})),
    )
    .await;
    rt.resolve_approval(&id, Verdict::Deny, operator())
        .await
        .expect("resolved");
    assert_eq!(*brain.resumed.lock().unwrap(), vec!["ep1".to_owned()]);
    assert_eq!(*brain.cycles.lock().unwrap(), 0);
    let banked = rt
        .grants
        .episode_releases()
        .take("ep1", &["ceo".to_owned()]);
    assert_eq!(banked["ceo"][0].verdict, SeatVerdict::Denied);
}

#[tokio::test]
async fn a_seat_waits_for_every_decision_before_it_is_released() {
    let brain = Arc::new(ResumeRecorder::default());
    let (rt, _home) = runtime_with_brain(brain.clone()).await;
    let releases = rt.grants.episode_releases();
    releases.start("ep1");
    let first = park_for_seat(
        &rt,
        "ep1",
        "ceo",
        seat_effect("send_email", "ceo", serde_json::json!({ "n": 1 })),
    )
    .await;
    let second = park_for_seat(
        &rt,
        "ep1",
        "ceo",
        seat_effect("send_email", "ceo", serde_json::json!({ "n": 2 })),
    )
    .await;
    rt.resolve_approval(&first, Verdict::Approve, operator())
        .await
        .unwrap();
    assert!(releases.take("ep1", &["ceo".to_owned()]).is_empty());
    rt.resolve_approval(&second, Verdict::Deny, operator())
        .await
        .unwrap();
    assert_eq!(releases.take("ep1", &["ceo".to_owned()])["ceo"].len(), 2);
}

#[tokio::test]
async fn an_explicit_request_decision_retires_its_continuation() {
    let brain = Arc::new(ResumeRecorder::default());
    let (rt, _home) = runtime_with_brain(brain.clone()).await;
    let releases = rt.grants.episode_releases();
    releases.start("ep1");
    let id = park_for_seat(
        &rt,
        "ep1",
        "ceo",
        seat_effect(
            crate::ports::types::REQUEST_APPROVAL_EFFECT_KIND,
            "ceo",
            serde_json::json!({ "title": "email the client" }),
        ),
    )
    .await;
    rt.resolve_approval(&id, Verdict::Approve, operator())
        .await
        .unwrap();
    let released = releases.take("ep1", &["ceo".to_owned()]);
    assert_eq!(
        released["ceo"][0].ask,
        SeatAsk::Request {
            title: "email the client".into()
        }
    );
    assert!(
        rt.grants.peek_continuation(&id).is_none(),
        "the continuation is retired, so nothing replays it as a chat turn"
    );
    assert!(rt.journal.replayed_approval_continuations().is_empty());
    assert_eq!(*brain.cycles.lock().unwrap(), 0);
}

#[tokio::test]
async fn an_expired_seat_approval_releases_the_seat_as_denied() {
    let brain = Arc::new(ResumeRecorder::default());
    let (rt, _home) = runtime_with_brain(brain.clone()).await;
    let releases = rt.grants.episode_releases();
    releases.start("ep1");
    let id = park_for_seat(
        &rt,
        "ep1",
        "ceo",
        seat_effect("send_email", "ceo", serde_json::json!({})),
    )
    .await;
    rt.retire_approval(
        &id,
        crate::runtime::journal::ExpiryReason::Ttl,
        crate::ports::now_millis(),
    )
    .await
    .expect("retired");
    let released = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        releases.released("ep1", &["ceo".to_owned()]),
    )
    .await
    .expect("an expiry releases the seat");
    assert_eq!(released["ceo"][0].verdict, SeatVerdict::Denied);
    assert_eq!(
        resolved_events(&rt).await,
        vec![Verdict::Deny],
        "the expiry is journaled once"
    );
    assert_eq!(*brain.cycles.lock().unwrap(), 0);
}

#[cfg(feature = "openhuman")]
#[tokio::test]
async fn an_escalation_answer_goes_to_the_episode_not_a_blocker_resume() {
    use crate::ports::blockers::{BlockerKind, BlockerPayload, BlockerSource, BlockerVerdict};
    let brain = Arc::new(ResumeRecorder::default());
    let (rt, _home) = runtime_with_brain(brain.clone()).await;
    let releases = rt.grants.episode_releases();
    releases.start("ep1");
    let payload = BlockerPayload {
        kind: BlockerKind::Information,
        source: BlockerSource::AgentQuestion,
        step: None,
        reason: "two regions are configured".into(),
        needed: "which region to deploy to".into(),
        group_key: None,
    };
    let id = park_for_seat(
        &rt,
        "ep1",
        "ceo",
        seat_effect(
            &payload.effect_kind(),
            "ceo",
            serde_json::to_value(&payload).unwrap(),
        ),
    )
    .await;
    let (_, follow_up) = rt
        .apply_blocker_reply_spawned(
            std::slice::from_ref(&id),
            &id,
            BlockerVerdict::Amend,
            "eu-west",
            None,
        )
        .await
        .expect("answered");
    super::join_follow_up(follow_up).await.expect("followed up");
    let released = releases.take("ep1", &["ceo".to_owned()]);
    let decision = &released["ceo"][0];
    assert_eq!(decision.verdict, SeatVerdict::Approved);
    assert_eq!(decision.answer, "eu-west");
    assert!(matches!(decision.ask, SeatAsk::Question { .. }));
    assert!(
        rt.journal.replayed_blocker_resolutions().is_empty(),
        "the answer is retired, so no boot replays it into a blocker resume"
    );
    assert_eq!(*brain.cycles.lock().unwrap(), 0);
}
