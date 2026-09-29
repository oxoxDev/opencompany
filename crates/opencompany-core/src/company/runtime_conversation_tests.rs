//! Runtime tests: cross-desk conversation width limits and gated-node approval expiry.

#[cfg(feature = "openhuman")]
use super::CompanyEvent;
#[cfg(feature = "openhuman")]
use super::tests_approval::runtime_with_events;
#[cfg(feature = "openhuman")]
use crate::ports::tasks::TaskTitle;

/// Issue #1852 Part 1 — the discard bug and its fix, proven directly on
/// `run_dispatch_cycle` rather than on any one `Brain`'s output shape.
///
/// `RelayBrain` answers a `TaskDispatched` event with exactly the shape
/// `relay_reply` (`harness::built_in::lifecycle`) produces: a bubble whose
/// `reply_to` names the origin thread and whose `task_id` names the card
/// — without standing up a real harness or LLM. Before this fix,
/// `run_dispatch_cycle` discarded the `CycleReport` carrying it (`let
/// Err(err) = self.run_cycle(...).await else { return; }`), which is the
/// generic bug underneath #1852, independent of which `Brain` produced
/// the relay: reverting `run_dispatch_cycle` to that shape reproduces the
/// failure this test now guards — zero `AgentReply` events land in the
/// origin thread, because nothing ever journals the discarded report.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn a_dispatched_cards_relay_is_journaled_into_its_origin_thread() {
    use std::sync::Arc;

    use crate::ports::Brain;
    use crate::ports::TaskRecord;
    use crate::ports::brain::CycleHost;
    use crate::ports::tasks::COLUMN_IN_PROGRESS;
    use crate::ports::types::{CycleRequest, CycleResult, OutboundMessage, ReplyTo, TokenUsage};

    /// Answers a `TaskDispatched { task_id: "t-1" }` with a
    /// `relay_reply`-shaped bubble; silent on everything else, mirroring
    /// `EchoBrain`'s silence on `TaskDispatched`.
    struct RelayBrain;

    #[async_trait::async_trait]
    impl Brain for RelayBrain {
        async fn run_cycle(
            &self,
            req: CycleRequest,
            _host: &dyn CycleHost,
        ) -> crate::Result<CycleResult> {
            let mut channel_responses = Vec::new();
            for event in &req.events {
                if let CompanyEvent::TaskDispatched { task_id, .. } = event
                    && task_id == "t-1"
                {
                    channel_responses.push(OutboundMessage {
                        message_id: None,
                        task_id: Some("t-1".to_string()),
                        outputs: Vec::new(),
                        channel: "ceo".to_string(),
                        agent: None,
                        text: "\"Ship it\" is ready for review (ceo ran it).".to_string(),
                        mentions: Vec::new(),
                        reply_to: Some(ReplyTo {
                            chat_id: "strategy".to_string(),
                        }),
                        steps: Vec::new(),
                    });
                }
            }
            Ok(CycleResult {
                channel_responses,
                new_traces: Vec::new(),
                ledger_deltas: Vec::new(),
                token_usage: TokenUsage::default(),
            })
        }
    }

    let home_dir = tempfile::Builder::new()
        .prefix("opencompany-relay-journal-")
        .tempdir()
        .expect("tempdir");
    let manifest: crate::company::CompanyManifest = toml::from_str(
        "[company]\nname = \"Acme\"\n[[agent]]\nid = \"ceo\"\nrole = \"Chief\"\n[policy]\nmode = \"full\"\n",
    )
    .expect("manifest");
    let id = crate::ports::types::CompanyId::new("acme");
    let runtime = Arc::new(
        crate::runtime::RuntimeBuilder::new(home_dir.path().to_path_buf(), manifest)
            .with_id(id.clone())
            .with_brain(Arc::new(RelayBrain))
            .build()
            .await
            .expect("runtime"),
    );

    let card = TaskRecord {
        id: "t-1".to_string(),
        title: TaskTitle::authored("Ship it"),
        note: None,
        column: COLUMN_IN_PROGRESS.to_string(),
        priority: "medium".to_string(),
        assignee: "ceo".to_string(),
        updated_at_millis: 0,
        // The field the whole bug turns on: without an origin thread,
        // `relay_reply` is never called at all (a board-created card).
        origin: crate::ports::TaskOrigin::new(Some("strategy".to_string()), None),
        parent_task_id: None,
        output: None,
        plan: None,
        planning_attempts: Vec::new(),
        deliverable: crate::ports::tasks::TaskDeliverable::Once,
        workflow_proposal: None,
        origin_run_id: None,
        origin_workflow_id: None,
        origin_message_seq: None,
        bounced: None,
    };

    let run_id = runtime.open_run(&card).await;
    Arc::clone(&runtime)
        .run_dispatch_cycle(card.id.clone(), run_id)
        .await;

    let events = runtime
        .events
        .read_from(&id, crate::ports::types::EventSeq::new(0), usize::MAX)
        .await
        .expect("read journal");
    let relays: Vec<_> = events
        .iter()
        .filter_map(|stored| match &stored.event {
            CompanyEvent::AgentReply { chat_id, .. } if chat_id == "strategy" => {
                Some(&stored.event)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        relays.len(),
        1,
        "exactly one relay must land in the origin thread, found {relays:?}"
    );
    let CompanyEvent::AgentReply {
        agent_id, task_id, ..
    } = relays[0]
    else {
        unreachable!()
    };
    assert_eq!(
        agent_id, "ceo",
        "the orchestrator answers for its own roster (issue #885 fallback)"
    );
    assert_eq!(
        task_id, &None,
        "the settle already has its own card link — `DeskTaskCompleted`'s \
         \"finished → …\" pill (issue #377) — so this bubble must not carry \
         its own \"Card opened\" chip alongside it"
    );
    assert!(
        crate::server::chat_history::owns("strategy", "Strategy", relays[0]),
        "the origin desk's own history read must pick this reply up"
    );
}

/// A dispatched card whose origin is a teammate's **private DM** relays as
/// that teammate, so the orchestrator never authors a second voice in a
/// one-to-one thread — while a shared desk keeps the orchestrator's voice.
///
/// The relay is produced the way `HarnessBrain::run_task` produces it:
/// through [`relay_speaker`](crate::harness::built_in::brain::relay_speaker)
/// + `relay_reply`, so a regression that let the orchestrator reclaim the DM
/// voice would flip the journaled author and fail this test.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn a_private_dm_relay_is_authored_by_the_dm_agent_not_the_orchestrator() {
    use std::collections::HashMap;
    use std::sync::Arc;

    use crate::harness::built_in::brain::relay_speaker;
    use crate::harness::built_in::lifecycle::relay_reply;
    use crate::ports::Brain;
    use crate::ports::TaskRecord;
    use crate::ports::brain::CycleHost;
    use crate::ports::tasks::COLUMN_IN_REVIEW;
    use crate::ports::types::{CycleRequest, CycleResult, EventSeq, OutboundMessage, TokenUsage};

    /// Replays a pre-built relay for each `TaskDispatched` it recognises.
    struct RelayBrain {
        replies: HashMap<String, OutboundMessage>,
    }

    #[async_trait::async_trait]
    impl Brain for RelayBrain {
        async fn run_cycle(
            &self,
            req: CycleRequest,
            _host: &dyn CycleHost,
        ) -> crate::Result<CycleResult> {
            let mut channel_responses = Vec::new();
            for event in &req.events {
                if let CompanyEvent::TaskDispatched { task_id, .. } = event
                    && let Some(reply) = self.replies.get(task_id)
                {
                    channel_responses.push(reply.clone());
                }
            }
            Ok(CycleResult {
                channel_responses,
                new_traces: Vec::new(),
                ledger_deltas: Vec::new(),
                token_usage: TokenUsage::default(),
            })
        }
    }

    let manifest_toml = "[company]\nname = \"Acme\"\n\
         [policy]\nmode = \"full\"\n\
         [[agent]]\nid = \"ceo\"\nrole = \"Chief\"\n\
         [[agent]]\nid = \"writer\"\nrole = \"Writer\"\n\
         [[group_chat]]\nid = \"strategy\"\nname = \"Strategy\"\nmembers = [\"ceo\"]\n";
    let manifest: crate::company::CompanyManifest =
        toml::from_str(manifest_toml).expect("manifest");

    // A record standing for the same roster, to compute the relay authorship
    // exactly as `HarnessBrain` would. Journaling never reads it; it only
    // drives `relay_speaker`.
    let record = crate::ports::types::CompanyRecord {
        general_channel: Default::default(),
        overlay_desk_hive: Vec::new(),
        overlay_retired_agents: Vec::new(),
        overlay_agent_edits: Vec::new(),
        id: crate::ports::types::CompanyId::new("acme"),
        manifest: manifest.clone(),
        ledger: Vec::new(),
        lifecycle: "running".to_string(),
        overlay_agents: Vec::new(),
        overlay_desk_members: Vec::new(),
        overlay_desk_order: Vec::new(),
        overlay_desks: Vec::new(),
        overlay_workflows: Vec::new(),
        overlay_budgets: Vec::new(),
        overlay_policy: None,
        overlay_tool_grants: None,
        overlay_desk_tools: Default::default(),
        disabled_workflows: Vec::new(),
        template_provenance: None,
        setup: None,
        name_confirmed: false,
        activation_completed_at: None,
        created_at_millis: None,
    };

    let orchestrator = "ceo";
    let card = |id: &str, origin: &str| TaskRecord {
        id: id.to_string(),
        title: TaskTitle::authored("Ship it"),
        note: None,
        column: COLUMN_IN_REVIEW.to_string(),
        priority: "medium".to_string(),
        assignee: origin.to_string(),
        updated_at_millis: 0,
        origin: crate::ports::TaskOrigin::new(Some(origin.to_string()), None),
        parent_task_id: None,
        output: None,
        plan: None,
        planning_attempts: Vec::new(),
        deliverable: crate::ports::tasks::TaskDeliverable::Once,
        workflow_proposal: None,
        origin_run_id: None,
        origin_workflow_id: None,
        origin_message_seq: None,
        bounced: None,
    };
    let dm_card = card("t-dm", "writer");
    let desk_card = card("t-desk", "strategy");

    // Built the way `run_task` builds them: the speaker is the origin DM's
    // own agent, or the orchestrator for a shared surface.
    let relay = |c: &TaskRecord| {
        let origin = c.origin_chat_id().map(str::to_string).expect("origin");
        let speaker = relay_speaker(&record, &origin, orchestrator);
        relay_reply(c, orchestrator, &speaker, origin, &[])
    };
    let replies = HashMap::from([
        ("t-dm".to_string(), relay(&dm_card)),
        ("t-desk".to_string(), relay(&desk_card)),
    ]);

    let home_dir = tempfile::Builder::new()
        .prefix("opencompany-private-dm-relay-")
        .tempdir()
        .expect("tempdir");
    let id = crate::ports::types::CompanyId::new("acme");
    let runtime = Arc::new(
        crate::runtime::RuntimeBuilder::new(home_dir.path().to_path_buf(), manifest)
            .with_id(id.clone())
            .with_brain(Arc::new(RelayBrain { replies }))
            .build()
            .await
            .expect("runtime"),
    );

    for c in [&dm_card, &desk_card] {
        let run_id = runtime.open_run(c).await;
        Arc::clone(&runtime)
            .run_dispatch_cycle(c.id.clone(), run_id)
            .await;
    }

    let events = runtime
        .events
        .read_from(&id, EventSeq::new(0), usize::MAX)
        .await
        .expect("read journal");
    let author_in = |thread: &str| {
        events.iter().find_map(|stored| match &stored.event {
            CompanyEvent::AgentReply {
                chat_id, agent_id, ..
            } if chat_id == thread => Some(agent_id.clone()),
            _ => None,
        })
    };

    assert_eq!(
        author_in("writer").as_deref(),
        Some("writer"),
        "a relay into the writer's private DM must be authored by the writer"
    );
    assert_eq!(
        author_in("strategy").as_deref(),
        Some("ceo"),
        "a relay into a shared desk keeps the orchestrator's voice"
    );
}

/// Issue #1852: the gate that stops a dispatch relay from being posted
/// twice.
///
/// A response the ordinary chat-turn cycle already journals through
/// `journal_chat_replies` (`server::operator`) never carries `reply_to` —
/// [`relay_reply`](crate::harness::built_in::lifecycle::relay_reply) is
/// the only producer that sets it — so gating on that field structurally
/// cannot re-journal a bubble the inline work-card path already wrote.
/// The same absence covers a board-created card (no `origin_chat_id`):
/// `run_task`/`refuse_dispatch` return no relay for one at all, which is
/// this exact "no `reply_to`" shape.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn journal_dispatch_replies_only_touches_relay_shaped_responses() {
    use crate::CycleReport;
    use crate::ports::types::{EventSeq, OutboundMessage, ReplyTo};

    let (rt, _home_dir) = runtime_with_events().await;

    let report = CycleReport {
        responses: vec![
            // An ordinary chat-turn bubble: no `reply_to`, exactly what
            // `journal_chat_replies` already owns. Must not be touched
            // here, or the inline work-card path would double-post.
            OutboundMessage {
                message_id: None,
                task_id: None,
                outputs: Vec::new(),
                channel: "operator".to_string(),
                agent: Some("ceo".to_string()),
                text: "already handled elsewhere".to_string(),
                mentions: Vec::new(),
                reply_to: None,
                steps: Vec::new(),
            },
            // A legacy empty chat id is #general, so this must still journal.
            OutboundMessage {
                message_id: None,
                task_id: Some("t-2".to_string()),
                outputs: Vec::new(),
                channel: "ceo".to_string(),
                agent: None,
                text: "General-chat relay".to_string(),
                mentions: Vec::new(),
                reply_to: Some(ReplyTo {
                    chat_id: String::new(),
                }),
                steps: Vec::new(),
            },
            // The one shape `relay_reply` actually produces.
            OutboundMessage {
                message_id: None,
                task_id: Some("t-1".to_string()),
                outputs: Vec::new(),
                channel: "ceo".to_string(),
                agent: None,
                text: "\"Ship it\" is ready for review.".to_string(),
                mentions: Vec::new(),
                reply_to: Some(ReplyTo {
                    chat_id: "strategy".to_string(),
                }),
                steps: Vec::new(),
            },
        ],
        ..Default::default()
    };

    rt.journal_dispatch_replies(&report).await;

    let events = rt
        .events
        .read_from(&rt.id, EventSeq::new(0), usize::MAX)
        .await
        .expect("read journal");
    let relays: Vec<_> = events
        .iter()
        .filter_map(|stored| match &stored.event {
            CompanyEvent::AgentReply { .. } => Some(&stored.event),
            _ => None,
        })
        .collect();
    assert_eq!(
        relays.len(),
        2,
        "both reply_to-shaped responses must be journaled — an empty \
         chat_id is General, not absent — found {relays:?}"
    );
    let CompanyEvent::AgentReply {
        chat_id, task_id, ..
    } = relays
        .iter()
        .find(|event| matches!(event, CompanyEvent::AgentReply { chat_id, .. } if chat_id == "strategy"))
        .expect("the named-thread relay must be present")
    else {
        unreachable!()
    };
    assert_eq!(chat_id, "strategy");
    // Not `Some("t-1")`, even though the response itself carries it:
    // `journal_task_outcome` already marked "t-1" settled with its own
    // `DeskTaskCompleted` card link into this same thread, so this bubble
    // must not add a second one. See the drop site's own comment.
    assert_eq!(task_id, &None);

    let CompanyEvent::AgentReply { chat_id, .. } = relays
        .iter()
        .find(
            |event| matches!(event, CompanyEvent::AgentReply { chat_id, .. } if chat_id == "general"),
        )
        .expect("the empty-chat_id General relay must be present")
    else {
        unreachable!()
    };
    assert_eq!(
        chat_id, "general",
        "a legacy empty chat_id reads back as #general"
    );
}

/// Issue #1890: a relayed card reports back into the conversation that
/// raised it, not beside it.
///
/// Found by hand-testing, not by a suite. A delegated request produced the
/// orchestrator's answer inside its thread and then the delegate's reply
/// and the relay bubble loose in the channel — three bubbles for one ask,
/// two of them in the wrong place. Invisible while only hand-opened threads
/// existed; obvious the moment every exchange is one.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn a_relayed_card_answers_in_the_thread_that_raised_it() {
    let (rt, _home_dir) = runtime_with_events().await;
    let id = rt.id().clone();

    // The root has to be *in* the journal, not merely named by the card.
    // `journal_dispatch_replies` now guards its parent through
    // `resolvable_parent` (coderabbit on #1982), which is what stops a
    // pruned root turning the delegate's answer into a reply the console
    // silently drops. A fixture that names a sequence nothing was ever
    // written at is the pruned case, so it has to write one.
    let root = rt
        .events()
        .append(
            &id,
            CompanyEvent::OperatorMessage {
                mentions: Vec::new(),
                text: "Draft the launch email".to_string(),
                by: None,
                chat: Some("general".to_string()),
                parent: None,
                deliverable: None,
                attachments: Vec::new(),
            },
        )
        .await
        .expect("the root is journaled");

    let mut card = crate::ports::tasks::TaskRecord {
        id: "t-relay".to_string(),
        title: TaskTitle::authored("Draft the launch email"),
        note: None,
        column: crate::ports::tasks::COLUMN_IN_REVIEW.to_string(),
        priority: "medium".to_string(),
        assignee: "writer".to_string(),
        updated_at_millis: 0,
        origin: crate::ports::TaskOrigin::new(Some("general".to_string()), Some(root)),
        parent_task_id: None,
        output: None,
        plan: None,
        planning_attempts: Vec::new(),
        deliverable: crate::ports::tasks::TaskDeliverable::Once,
        workflow_proposal: None,
        origin_run_id: None,
        origin_workflow_id: None,
        origin_message_seq: None,
        bounced: None,
    };
    rt.tasks().upsert(&id, &card).await.unwrap();

    let relay = |task: Option<&str>| crate::ports::types::OutboundMessage {
        message_id: None,
        task_id: task.map(str::to_string),
        outputs: Vec::new(),
        channel: "ceo".to_string(),
        agent: None,
        text: "the delegate finished it".to_string(),
        mentions: Vec::new(),
        reply_to: Some(crate::ports::types::ReplyTo {
            chat_id: "general".to_string(),
        }),
        steps: Vec::new(),
    };

    let report = crate::runtime::types::CycleReport {
        responses: vec![relay(Some("t-relay"))],
        ..Default::default()
    };
    rt.journal_dispatch_replies(&report).await;

    let logged = rt
        .events()
        .read_from(&id, crate::ports::types::EventSeq::new(0), usize::MAX)
        .await
        .unwrap();
    let threaded = logged.iter().rev().find_map(|e| match &e.event {
        CompanyEvent::AgentReply { parent, .. } => Some(*parent),
        _ => None,
    });
    assert_eq!(
        threaded,
        Some(Some(root)),
        "the relay joins the thread the card recorded at raise time"
    );

    // And a card raised at channel level still relays flat — `None` is the
    // channel-level conversation, not a gap.
    card.id = "t-flat".to_string();
    card.origin = crate::ports::TaskOrigin::new(card.origin_chat_id().map(str::to_string), None);
    rt.tasks().upsert(&id, &card).await.unwrap();
    let report = crate::runtime::types::CycleReport {
        responses: vec![relay(Some("t-flat"))],
        ..Default::default()
    };
    rt.journal_dispatch_replies(&report).await;

    let logged = rt
        .events()
        .read_from(&id, crate::ports::types::EventSeq::new(0), usize::MAX)
        .await
        .unwrap();
    let last = logged.iter().rev().find_map(|e| match &e.event {
        CompanyEvent::AgentReply { parent, .. } => Some(*parent),
        _ => None,
    });
    assert_eq!(
        last,
        Some(None),
        "a channel-level card relays into the channel"
    );
}

// Issue #435: the guard that decides whether a remembered thread root is
// still usable, and the direction it fails in.
//
// Every arm here degrades to `None`, which means "answer in the channel".
// That is the issue's stated requirement and it is not merely tidy: the
// console drops a reply whose parent it cannot resolve in the channel
// rather than rendering it flat, so a stale root would make the
// continuation invisible — strictly worse than the bug being fixed, since
// today's answer at least reaches the channel.
