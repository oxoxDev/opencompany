use super::*;

#[tokio::test]
async fn operator_message_gets_an_agent_reply() {
    let dir = tempfile::tempdir().unwrap();
    let brain = brain_over_mock(dir.path());
    let result = brain
        .run_cycle(
            request(vec![CompanyEvent::OperatorMessage {
                mentions: Vec::new(),
                parent: None,
                text: "status?".into(),
                by: None,
                chat: None,
                deliverable: None,
                attachments: Vec::new(),
            }]),
            &NoopHost,
        )
        .await
        .expect("cycle runs");

    assert_eq!(result.channel_responses.len(), 1);
    assert_eq!(result.channel_responses[0].channel, "operator");
    // The mock provider prefixes the routed message, proving the turn ran
    // through the openhuman agent rather than an echo.
    assert!(
        result.channel_responses[0].text.contains("status?"),
        "{:?}",
        result.channel_responses[0].text
    );
    // The offline mock runs no tools and emits no progress, so the operator
    // bubble carries zero steps — the tell that distinguishes a tool-less
    // (here, memory/echo-style) answer from a tool-backed one.
    assert!(
        result.channel_responses[0].steps.is_empty(),
        "a tool-less turn carries no steps: {:?}",
        result.channel_responses[0].steps
    );
    assert_eq!(result.new_traces.len(), 1);
    // Single cost-accounting site: the cycle result carries no ledger delta.
    assert!(result.ledger_deltas.is_empty());
}

#[tokio::test]
async fn schedule_fired_gets_an_agent_reply() {
    // A cron tick (`ScheduleFired`) must drive a real turn and surface its
    // reply, not fall through the match and vanish — the same guarantee an
    // operator message gets. Without this arm a scheduled prompt ran to
    // nowhere: the turn produced an answer that was never journaled, so the
    // desk history had no record it fired.
    let dir = tempfile::tempdir().unwrap();
    let brain = brain_over_mock(dir.path());
    let result = brain
        .run_cycle(
            request(vec![CompanyEvent::ScheduleFired {
                cron: "0 9 * * *".into(),
                prompt: "daily standup".into(),
            }]),
            &NoopHost,
        )
        .await
        .expect("cycle runs");

    assert_eq!(result.channel_responses.len(), 1);
    // The mock provider prefixes the routed prompt, proving the turn ran
    // through the agent rather than falling through the match.
    assert!(
        result.channel_responses[0].text.contains("daily standup"),
        "{:?}",
        result.channel_responses[0].text
    );
    assert_eq!(result.new_traces.len(), 1);
}

/// A cron tick's reply is journaled onto the General desk under the
/// responder's name, and the returned bubble is stamped with the journaled
/// event's sequence — the same contract an operator reply's bubble carries
/// (issue #885: destination and author are separate facts).
#[tokio::test]
async fn schedule_fired_journals_an_agent_reply_on_the_general_desk() {
    use crate::ports::EventLog;
    use crate::ports::types::EventSeq;
    use crate::store::FsEventLog;

    let dir = tempfile::tempdir().unwrap();
    let log: Arc<dyn EventLog> = Arc::new(FsEventLog::new(dir.path()));
    let brain = brain_with_queue_and_events(dir.path(), Default::default(), log.clone());
    let result = brain
        .run_cycle(
            request(vec![CompanyEvent::ScheduleFired {
                cron: "0 9 * * *".into(),
                prompt: "daily standup".into(),
            }]),
            &NoopHost,
        )
        .await
        .expect("cycle runs");

    // The reply lands on the General desk, authored by the responder —
    // destination and author stay separate.
    assert_eq!(result.channel_responses.len(), 1);
    let bubble = &result.channel_responses[0];
    assert_eq!(
        bubble.channel,
        crate::server::ops::language::GENERAL_CHANNEL_ID
    );
    assert_eq!(bubble.agent.as_deref(), Some("ceo"));

    // The journal holds one AgentReply, on the General desk, attributed to
    // the responder — not to the channel the reply was routed over.
    let events = log
        .read_from(&CompanyId::new("acme"), EventSeq::new(0), usize::MAX)
        .await
        .expect("read events");
    let reply = events
        .iter()
        .find(|e| matches!(&e.event, CompanyEvent::AgentReply { .. }))
        .expect("a scheduled reply was journaled");
    match &reply.event {
        CompanyEvent::AgentReply {
            chat_id,
            agent_id,
            text,
            ..
        } => {
            assert_eq!(chat_id, crate::server::ops::language::GENERAL_CHANNEL_ID);
            assert_eq!(agent_id, "ceo");
            assert!(text.contains("daily standup"), "{text}");
        }
        _ => unreachable!(),
    }

    // The returned bubble carries the appended event's sequence as its
    // durable id.
    assert_eq!(bubble.message_id, Some(reply.seq.value().to_string()));
}

#[tokio::test]
async fn schedule_fired_journals_halt_notices() {
    use crate::ports::EventLog;
    use crate::ports::types::EventSeq;
    use crate::store::FsEventLog;

    let dir = tempfile::tempdir().unwrap();
    let log: Arc<dyn EventLog> = Arc::new(FsEventLog::new(dir.path()));
    let outcome = crate::harness::built_in::TurnOutcome {
        reply: "checkpoint".to_string(),
        steps: Vec::new(),
        hit_iteration_cap: true,
        // Test fixture, not the ACP fold (PR #1880 review).
        abnormal_stop: None,
        halted_for_spend: Some(crate::harness::SpendHalt {
            agent: "ceo".to_string(),
            spent_usd: 1.25,
            cap_usd: 1.0,
        }),
        // This fixture scripts a SPEND halt; a budget pause is the separate
        // signal added in issue #1846 and is not what it exercises.
        budget_paused: None,
    };
    let brain = brain_with_queue_and_events(dir.path(), Default::default(), log.clone())
        .with_default_engine(Some(Arc::new(FixedOutcomeTurn {
            outcome,
            approval_requests: None,
        })));
    let result = brain
        .run_cycle(
            request(vec![CompanyEvent::ScheduleFired {
                cron: "0 9 * * *".into(),
                prompt: "daily standup".into(),
            }]),
            &NoopHost,
        )
        .await
        .expect("cycle runs");

    assert_eq!(result.channel_responses.len(), 3);
    assert!(
        result.channel_responses[1]
            .text
            .contains("maximum number of steps")
    );
    assert!(result.channel_responses[2].text.contains("spend cap"));
    assert!(
        result
            .channel_responses
            .iter()
            .skip(1)
            .all(|response| response.agent.as_deref() == Some(crate::ports::SYSTEM_AUTHOR))
    );
    let events = log
        .read_from(&CompanyId::new("acme"), EventSeq::new(0), usize::MAX)
        .await
        .expect("read events");
    let replies: Vec<_> = events
        .iter()
        .filter(|event| matches!(event.event, CompanyEvent::AgentReply { .. }))
        .collect();
    assert_eq!(replies.len(), 3, "all scheduled notices are durable");
}

#[tokio::test]
async fn schedule_fired_journals_a_budget_pause_notice() {
    use crate::ports::EventLog;
    use crate::ports::types::EventSeq;
    use crate::store::FsEventLog;

    let dir = tempfile::tempdir().unwrap();
    let log: Arc<dyn EventLog> = Arc::new(FsEventLog::new(dir.path()));
    let outcome = crate::harness::built_in::TurnOutcome {
        reply: "checkpoint".to_string(),
        steps: Vec::new(),
        hit_iteration_cap: false,
        // Test fixture, not the ACP fold (PR #1880 review).
        abnormal_stop: None,
        // Issue #1906: this fixtures a BUDGET pause, not a spend halt — the
        // halt sibling is pinned by `schedule_fired_journals_halt_notices`.
        halted_for_spend: None,
        budget_paused: Some(crate::harness::BudgetPause {
            agent: "ceo".to_string(),
            summary: "the provider is exhausted".to_string(),
        }),
    };
    let brain = brain_with_queue_and_events(dir.path(), Default::default(), log.clone())
        .with_default_engine(Some(Arc::new(FixedOutcomeTurn {
            outcome,
            approval_requests: None,
        })));
    let result = brain
        .run_cycle(
            request(vec![CompanyEvent::ScheduleFired {
                cron: "0 9 * * *".into(),
                prompt: "daily standup".into(),
            }]),
            &NoopHost,
        )
        .await
        .expect("cycle runs");

    // Issue #1906: a scheduled tick that pauses for lack of credits must
    // not present the interrupted turn as a completed answer — the primary
    // bubble carries the pause placeholder and a system notice follows it.
    assert_eq!(result.channel_responses.len(), 2);
    assert_eq!(
        result.channel_responses[0].text,
        BUDGET_PAUSED_PLACEHOLDER_REPLY
    );
    assert!(
        result.channel_responses[1]
            .text
            .starts_with(BUDGET_PAUSE_NOTICE_PREFIX)
    );
    assert!(
        result
            .channel_responses
            .iter()
            .skip(1)
            .all(|response| response.agent.as_deref() == Some(crate::ports::SYSTEM_AUTHOR))
    );
    let events = log
        .read_from(&CompanyId::new("acme"), EventSeq::new(0), usize::MAX)
        .await
        .expect("read events");
    let replies: Vec<_> = events
        .iter()
        .filter(|event| matches!(event.event, CompanyEvent::AgentReply { .. }))
        .collect();
    assert_eq!(
        replies.len(),
        2,
        "the pause placeholder and notice are durable"
    );
}

#[tokio::test]
async fn schedule_fired_journals_approval_overflow_notice() {
    use crate::ports::EventLog;
    use crate::ports::types::EventSeq;
    use crate::store::FsEventLog;

    let dir = tempfile::tempdir().unwrap();
    let log: Arc<dyn EventLog> = Arc::new(FsEventLog::new(dir.path()));
    let requests = crate::harness::policy::ApprovalRequestQueue::default();
    let brain = brain_with_queue_and_events(dir.path(), requests.clone(), log.clone())
        .with_default_engine(Some(Arc::new(FixedOutcomeTurn {
            outcome: crate::harness::built_in::TurnOutcome {
                reply: "checkpoint".to_string(),
                steps: Vec::new(),
                hit_iteration_cap: false,
                // Test fixture, not the ACP fold (PR #1880 review).
                abnormal_stop: None,
                halted_for_spend: None,
                // Added by #1846 after these fixtures were written.
                budget_paused: None,
            },
            approval_requests: Some(requests.clone()),
        })));
    let result = brain
        .run_cycle(
            request(vec![CompanyEvent::ScheduleFired {
                cron: "0 9 * * *".into(),
                prompt: "daily standup".into(),
            }]),
            &NoopHost,
        )
        .await
        .expect("cycle runs");

    assert_eq!(result.channel_responses.len(), 2);
    let notice = &result.channel_responses[1];
    assert!(
        notice.text.contains("further gated tool call"),
        "{}",
        notice.text
    );
    assert_eq!(notice.agent.as_deref(), Some(crate::ports::SYSTEM_AUTHOR));
    assert_eq!(requests.queued(), 0);
    let events = log
        .read_from(&CompanyId::new("acme"), EventSeq::new(0), usize::MAX)
        .await
        .expect("read events");
    assert!(events.iter().any(|event| {
        matches!(&event.event, CompanyEvent::AgentReply { text, .. } if text.contains("further gated tool call"))
    }));
}

#[tokio::test]
async fn no_events_still_acknowledges() {
    let dir = tempfile::tempdir().unwrap();
    let brain = brain_over_mock(dir.path());
    let result = brain
        .run_cycle(request(Vec::new()), &NoopHost)
        .await
        .expect("cycle runs");
    assert_eq!(result.channel_responses.len(), 1);
    assert_eq!(result.channel_responses[0].text, "Acknowledged.");
    // Issue #966, asserted here rather than only on `system_notice`: this
    // drives the real cycle, so it pins that the fallback *calls* the
    // constructor. Asserting the constructor alone leaves the call site free
    // to go back to an inline bubble with no author, which is the shape that
    // caused the defect.
    assert_eq!(
        result.channel_responses[0].agent.as_deref(),
        Some(crate::ports::SYSTEM_AUTHOR),
        "the runtime's own fallback is authored by the runtime, not by its destination"
    );
}

#[test]
fn responder_defaults_to_first_roster_agent() {
    let dir = tempfile::tempdir().unwrap();
    let brain = brain_over_mock(dir.path());
    assert_eq!(brain.responder, "ceo");
    let brain = brain.with_responder("cfo");
    assert_eq!(brain.responder, "cfo");
}

/// **The regression.** Issue #1846 review (Codex #3864988168): `run_task`
/// never inspected `outcome.budget_paused` before this fix — a dispatched
/// card whose model call ran out of credits fell straight into the
/// `None => { ... None => settle(Completed) }` arm, since a budget pause
/// carries an `Ok(TurnOutcome)` with no delegation queued, and landed in
/// `in_review` looking like a finished, reviewable result instead of the
/// graceful pause the operator-chat path already gave the same failure.
#[tokio::test]
async fn a_dispatched_tasks_budget_exhaustion_pauses_rather_than_completes() {
    let dir = tempfile::tempdir().unwrap();
    let (brain, tasks) = brain_with_tasks_and_budget_exhausted_provider(dir.path());
    let company = CompanyId::new("acme");
    tasks
        .upsert(&company, &card("t-1", "engineer"))
        .await
        .expect("seed");
    // Driven directly, not through `run_cycle`, so the roster has to be
    // built explicitly — see
    // `dispatched_card_with_an_origin_stops_in_review_and_still_posts_back`.
    brain
        .pool
        .ensure(&brain.record(), &brain.deps)
        .await
        .expect("roster");

    brain.run_task("t-1", None).await.expect("run");

    let settled = only_card(&tasks).await;
    assert_eq!(
        settled.column, COLUMN_PAUSED,
        "a budget-exhausted model call is a graceful pause, not a completed result — \
         it must not read on the board as a finished, reviewable card"
    );
    let note = settled.note.expect("note");
    assert!(
        note.contains("add credits") || note.contains("Add credits"),
        "the note must carry the actionable ask a genuine budget pause gives, not just \
         an opaque dispatch failure: {note}"
    );
}

/// **Chain first, proven.** A re-publish whose artifact write fails must
/// leave the note holding the PREVIOUS body — the version was stored before
/// the tree was touched, so a refused version means an untouched tree.
///
/// The opposite ordering is what this rules out, and it is not a stylistic
/// difference: a note one version ahead of the chain shows the operator
/// content the version history has no record of, which makes
/// `human_edit_diff` quietly wrong rather than loudly broken — the same rot
/// the artifact port exists to prevent, arriving through the tree instead.
#[tokio::test]
async fn a_refused_republish_leaves_the_note_on_the_previous_body() {
    use crate::ports::artifacts::ArtifactStore;

    let dir = tempfile::tempdir().unwrap();
    let ops = Arc::new(FsOps::new(dir.path()));
    // v1 costs two upserts: the record, then the link once the node exists.
    let artifacts = FailingArtifacts::new(ops.clone(), 2);
    let (brain, _) =
        brain_with_injected_artifacts(dir.path(), ops.clone(), artifacts.clone(), true);
    let company = CompanyId::new("acme");
    let c = card("t-1", "maya");

    brain
        .record_published_artifacts(&c, "maya", vec![publish_of("launch.md", "v1")], None)
        .await
        .expect("the first publish lands");
    let (node_id, body) = note_in_tree(&ops, &company, "launch.md")
        .await
        .expect("v1 is in the tree");
    assert_eq!(body, "v1");

    // Now the artifact store refuses. The re-publish must fail *before*
    // reaching the tree.
    brain
        .record_published_artifacts(&c, "maya", vec![publish_of("launch.md", "v2")], None)
        .await
        .expect_err("a refused artifact write fails the publish");

    let (still, body) = note_in_tree(&ops, &company, "launch.md")
        .await
        .expect("the note is still there");
    assert_eq!(still, node_id, "no rival note was minted");
    assert_eq!(
        body, "v1",
        "the tree must not hold a body the version history never recorded"
    );
    // And the chain is unchanged too — one version, not a half-written two.
    let stored = ArtifactStore::list(&*ops, &company, Some("t-1"))
        .await
        .unwrap();
    assert_eq!(stored[0].versions.len(), 1);
    assert_eq!(stored[0].latest().unwrap().body, "v1");
}

/// The same ordering on a **fresh** publish: an artifact write that fails
/// creates nothing in the tree at all.
///
/// This is what makes the fresh path's residual an *orphan note* rather
/// than a lost deliverable — a node is only ever created for a deliverable
/// that is already recorded, so this path cannot leave a file in the tree
/// with no artifact behind it.
#[tokio::test]
async fn a_refused_first_publish_creates_nothing_in_the_tree() {
    use crate::ports::workspace::WorkspaceStore;

    let dir = tempfile::tempdir().unwrap();
    let ops = Arc::new(FsOps::new(dir.path()));
    let artifacts = FailingArtifacts::new(ops.clone(), 0);
    let (brain, _) = brain_with_injected_artifacts(dir.path(), ops.clone(), artifacts, true);
    let company = CompanyId::new("acme");

    brain
        .record_published_artifacts(
            &card("t-1", "maya"),
            "maya",
            vec![publish_of("launch.md", "v1")],
            None,
        )
        .await
        .expect_err("a refused artifact write fails the publish");

    assert!(
        note_in_tree(&ops, &company, "launch.md").await.is_none(),
        "no note may exist for a deliverable that was never recorded"
    );
    assert!(
        WorkspaceStore::tree(&*ops, &company)
            .await
            .unwrap()
            .is_empty(),
        "not even the agent's folder is minted for a publish that failed"
    );
}

/// The fresh path's one residual, and its repair.
///
/// A fresh publish has no node id to inherit, so v1 is stored unlinked and
/// a *second* artifact write stamps the link. If that second write fails,
/// both surfaces hold the body and only the pointer between them is
/// missing. That is deliberately warned-and-tolerated rather than fatal:
/// failing would discard the rest of the batch to report a link that the
/// next publish repairs.
///
/// The repair is the half worth proving. `materialize` find-or-creates by
/// path, so the next publish of the same source **re-adopts the very same
/// note** rather than duplicating it — which is what makes the orphan
/// self-healing rather than permanent.
#[tokio::test]
async fn an_unlinked_first_publish_is_repaired_by_the_next_one() {
    use crate::ports::artifacts::ArtifactStore;
    use crate::ports::workspace::WorkspaceStore;

    let dir = tempfile::tempdir().unwrap();
    let ops = Arc::new(FsOps::new(dir.path()));
    // Exactly one upsert succeeds: the record lands, the link does not.
    let artifacts = FailingArtifacts::new(ops.clone(), 1);
    let (brain, _) =
        brain_with_injected_artifacts(dir.path(), ops.clone(), artifacts.clone(), true);
    let company = CompanyId::new("acme");
    let c = card("t-1", "maya");

    brain
        .record_published_artifacts(&c, "maya", vec![publish_of("launch.md", "v1")], None)
        .await
        .expect("a missing link must not fail the publish");

    // Both surfaces hold the body; only the pointer is absent.
    let (orphan, body) = note_in_tree(&ops, &company, "launch.md")
        .await
        .expect("the note was still written");
    assert_eq!(body, "v1");
    let stored = ArtifactStore::list(&*ops, &company, Some("t-1"))
        .await
        .unwrap();
    assert_eq!(stored[0].latest().unwrap().body, "v1");
    assert_eq!(
        stored[0].workspace_node_id(),
        None,
        "this is the orphan: recorded and written, but not linked"
    );

    // The next publish of the same source repairs it.
    artifacts.heal();
    let nodes_before = WorkspaceStore::tree(&*ops, &company).await.unwrap().len();
    brain
        .record_published_artifacts(&c, "maya", vec![publish_of("launch.md", "v2")], None)
        .await
        .expect("the repairing publish lands");

    let stored = ArtifactStore::list(&*ops, &company, Some("t-1"))
        .await
        .unwrap();
    assert_eq!(stored[0].versions.len(), 2, "one record, extended");
    assert_eq!(
        stored[0].workspace_node_id(),
        Some(orphan.as_str()),
        "the very same note is re-adopted, which is what makes the orphan self-healing"
    );
    assert_eq!(
        WorkspaceStore::tree(&*ops, &company).await.unwrap().len(),
        nodes_before,
        "re-adoption, not duplication: no rival note beside the orphan"
    );
    assert_eq!(
        WorkspaceStore::read(&*ops, &company, &orphan)
            .await
            .unwrap()
            .unwrap()
            .1,
        "v2"
    );
}

/// The ordinary re-publish stores **once**, not twice. The second artifact
/// write exists only for a link that actually changed — a fresh publish, or
/// a note the operator deleted — and a re-publish that reuses its note has
/// nothing to restate.
#[tokio::test]
async fn an_ordinary_republish_writes_the_artifact_once() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Arc::new(FsOps::new(dir.path()));
    let artifacts = FailingArtifacts::new(ops.clone(), usize::MAX);
    let (brain, _) =
        brain_with_injected_artifacts(dir.path(), ops.clone(), artifacts.clone(), true);
    let c = card("t-1", "maya");

    brain
        .record_published_artifacts(&c, "maya", vec![publish_of("launch.md", "v1")], None)
        .await
        .unwrap();
    // v1: the record, then the link once the node id exists.
    assert_eq!(
        artifacts.seen.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "a fresh publish stores the record, then stamps the link"
    );

    brain
        .record_published_artifacts(&c, "maya", vec![publish_of("launch.md", "v2")], None)
        .await
        .unwrap();
    assert_eq!(
        artifacts.seen.load(std::sync::atomic::Ordering::SeqCst),
        3,
        "a re-publish inherits its note, so one store is enough"
    );
}
// -- issue #552: a published deliverable reaches the shared workspace -----

/// The headline of #552. A published file used to reach the artifact store
/// and stop, which left it visible only in the Artifacts tab of one card.
/// It must now also land in the shared tree, under the publishing agent's
/// own folder, attributed to that agent — and the version that wrote it
/// must carry the node id, which is the link the console's cross-link and
/// every later mirror depend on.
#[tokio::test]
async fn a_publish_lands_in_the_shared_workspace_and_the_version_names_the_node() {
    use crate::harness::publish::PendingPublish;
    use crate::ports::artifacts::ArtifactStore;
    use crate::ports::workspace::{WorkspaceOrigin, WorkspaceStore};

    let dir = tempfile::tempdir().unwrap();
    let (brain, ops) = brain_with_artifacts_and_workspace(dir.path());
    let company = CompanyId::new("acme");

    brain
        .record_published_artifacts(
            &card("t-1", "maya"),
            "maya",
            vec![PendingPublish {
                agent: "maya".to_string(),
                source: "specs/launch.md".to_string(),
                title: "Launch spec".to_string(),
                kind: crate::ports::artifacts::ArtifactKind::Markdown,
                note: None,
                payload: crate::harness::publish::PublishPayload::Text("the spec body".to_string()),
            }],
            Some("run-1"),
        )
        .await
        .expect("records");

    let listed = ArtifactStore::list(&*ops, &company, Some("t-1"))
        .await
        .unwrap();
    let node_id = listed[0]
        .workspace_node_id()
        .expect("the version must name the node its body was mirrored into");

    let (node, body) = WorkspaceStore::read(&*ops, &company, node_id)
        .await
        .unwrap()
        .expect("the node exists in the shared tree");
    assert_eq!(body, "the spec body");
    assert_eq!(node.name, "launch.md");
    assert_eq!(
        node.created_by,
        WorkspaceOrigin::Agent {
            id: "maya".to_string()
        },
        "the tree must say which teammate produced this"
    );
}
