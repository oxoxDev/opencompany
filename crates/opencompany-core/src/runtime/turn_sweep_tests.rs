use super::*;
use crate::store::FsEventLog;

fn log() -> (tempfile::TempDir, Arc<dyn EventLog>) {
    let dir = tempfile::Builder::new()
        .prefix("oc-turn-sweep-")
        .tempdir()
        .expect("tempdir");
    let events: Arc<dyn EventLog> = Arc::new(FsEventLog::new(dir.path()));
    (dir, events)
}

async fn started(events: &Arc<dyn EventLog>, company: &CompanyId, turn_id: &str) {
    events
        .append(
            company,
            CompanyEvent::TurnStarted {
                turn_id: turn_id.to_string(),
                chat_id: "general".to_string(),
                parent: None,
                by: None,
                agent_id: None,
                episode_id: None,
                round_revision: None,
            },
        )
        .await
        .expect("append");
}

async fn failures(events: &Arc<dyn EventLog>, company: &CompanyId) -> Vec<(String, String)> {
    events
        .read_from(company, EventSeq::new(0), usize::MAX)
        .await
        .expect("read")
        .into_iter()
        .filter_map(|s| match s.event {
            CompanyEvent::TurnFailed { turn_id, error, .. } => Some((turn_id, error)),
            _ => None,
        })
        .collect()
}

/// The case the sweep exists for: the host died holding a turn, so the
/// operator's question sits in the transcript with nothing after it.
#[tokio::test]
async fn an_unterminated_turn_is_settled_at_boot() {
    let (_home, events) = log();
    let company = CompanyId::new("acme");
    started(&events, &company, "turn-dead").await;

    sweep_interrupted_turns(&events, &company).await;

    assert_eq!(
        failures(&events, &company).await,
        vec![(
            "turn-dead".to_string(),
            TURN_INTERRUPTED_BY_RESTART.to_string()
        )]
    );
}

/// A turn that settled itself is left alone, and the sweep is idempotent —
/// its own synthetic failure closes the bracket, so a second boot after an
/// unclean one does not stack a second line onto the same turn.
#[tokio::test]
async fn a_settled_turn_is_never_swept_twice() {
    let (_home, events) = log();
    let company = CompanyId::new("acme");
    started(&events, &company, "turn-ok").await;
    events
        .append(
            &company,
            CompanyEvent::TurnFailed {
                turn_id: "turn-ok".to_string(),
                error: "the brain refused".to_string(),
                agent_id: None,
                chat_id: None,
                episode_id: None,
                round_revision: None,
                outcome: None,
            },
        )
        .await
        .expect("append");
    started(&events, &company, "turn-dead").await;

    sweep_interrupted_turns(&events, &company).await;
    sweep_interrupted_turns(&events, &company).await;

    assert_eq!(
        failures(&events, &company).await,
        vec![
            ("turn-ok".to_string(), "the brain refused".to_string()),
            (
                "turn-dead".to_string(),
                TURN_INTERRUPTED_BY_RESTART.to_string()
            ),
        ],
        "the sweep re-settled a turn it had already settled"
    );
}

/// A company with nothing open appends nothing at all — the sweep must not
/// leave a trace of having run on every boot of every company.
#[tokio::test]
async fn a_quiet_company_is_untouched() {
    let (_home, events) = log();
    let company = CompanyId::new("acme");
    sweep_interrupted_turns(&events, &company).await;
    assert!(
        events
            .read_from(&company, EventSeq::new(0), usize::MAX)
            .await
            .expect("read")
            .is_empty()
    );
}

/// One company's dead turn is not another's.
#[tokio::test]
async fn the_sweep_is_scoped_to_one_company() {
    let (_home, events) = log();
    let acme = CompanyId::new("acme");
    let other = CompanyId::new("other");
    started(&events, &acme, "turn-a").await;
    started(&events, &other, "turn-b").await;

    sweep_interrupted_turns(&events, &acme).await;

    assert_eq!(failures(&events, &acme).await.len(), 1);
    assert!(
        failures(&events, &other).await.is_empty(),
        "another company's live turn was settled"
    );
}
