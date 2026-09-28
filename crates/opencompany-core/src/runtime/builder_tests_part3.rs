use super::tests_core::*;
use super::*;

/// **Issue #1059.** A runtime with no agent pool says so when a card is
/// dispatched, instead of leaving it inert in silence.
///
/// The silence was the whole bug: `dispatch_task` returned without minting a
/// run, journalling anything or logging, so a card dragged into In Progress
/// simply sat there. Everything upstream looked healthy — the write returned
/// 200 and the card moved — and there was nothing to grep for.
///
/// Asserted through a capturing subscriber rather than by reading the code,
/// because "it logs" is exactly the claim that rots: the warning could be
/// deleted, demoted to `debug!`, or moved behind a branch nothing reaches,
/// and every other test here would still pass.
///
/// The second dispatch pins the latch. An inert board with fifty cards has
/// one problem, not fifty, and a per-card warning is the kind of noise that
/// gets a useful line filtered out.
///
/// The remedy is asserted per build (issue #1059 review). "No agent pool"
/// has two causes with two different fixes — nobody called `with_harness`,
/// or the binary was built without the feature that compiles it — and a
/// message naming the wrong one is a dead end dressed as help. Each arm
/// pins the other's remedy *absent* as well as its own present, so a
/// message that hedged by carrying both would fail here.
#[tokio::test]
async fn an_inert_board_says_it_cannot_dispatch_once() {
    use std::sync::{Arc as StdArc, Mutex as StdMutex};

    /// A writer that keeps everything the subscriber emits.
    #[derive(Clone, Default)]
    struct Captured(StdArc<StdMutex<Vec<u8>>>);
    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Keeps each event's level alongside its rendered message.
    ///
    /// The captured text cannot stand in for the level: `with_max_level`
    /// names a *maximum verbosity*, so a `WARN` ceiling admits `ERROR` too,
    /// and a promotion would slip past an assertion that only reads the
    /// message. This reads `Metadata::level()` itself.
    #[derive(Clone, Default)]
    struct Levels(StdArc<StdMutex<Vec<(tracing::Level, String)>>>);
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Levels {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            struct Message(String);
            impl tracing::field::Visit for Message {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "message" {
                        self.0 = format!("{value:?}");
                    }
                }
            }
            let mut message = Message(String::new());
            event.record(&mut message);
            self.0
                .lock()
                .unwrap()
                .push((*event.metadata().level(), message.0));
        }
    }

    let home_dir = tmp_home("oc-inert-board-");
    let manifest = parse("[company]\nname=\"Acme\"\n[policy]\nmode=\"full\"\n");
    // No `with_harness`: the default shape ~200 callers use.
    let runtime = RuntimeBuilder::new(home_dir.path().to_path_buf(), manifest)
        .with_id(CompanyId::new("acme"))
        .build()
        .await
        .expect("builds");
    let runtime = Arc::new(runtime);

    let logs = Captured::default();
    let sink = logs.clone();
    let levels = Levels::default();
    let subscriber = {
        use tracing_subscriber::layer::SubscriberExt;
        tracing_subscriber::fmt()
            .with_writer(move || sink.clone())
            .with_max_level(tracing::Level::WARN)
            .finish()
            .with(levels.clone())
    };

    let card = |id: &str, column: &str| crate::ports::tasks::TaskRecord {
        id: id.to_string(),
        title: crate::ports::tasks::TaskTitle::authored("Do the thing"),
        note: None,
        column: column.to_string(),
        priority: "medium".to_string(),
        assignee: "ceo".to_string(),
        updated_at_millis: 1,
        origin: None,
        parent_task_id: None,
        output: None,
        plan: None,
        deliverable: crate::ports::tasks::TaskDeliverable::Once,
        workflow_proposal: None,
        origin_run_id: None,
        origin_workflow_id: None,
        origin_message_seq: None,
        bounced: None,
        planning_attempts: Vec::new(),
    };

    // Through `upsert_task`, the real entry point: it reads the To-do →
    // In Progress edge and calls `dispatch_task`, so this exercises the drag
    // an operator actually performs rather than the private hop beneath it.
    for id in ["card-1", "card-2"] {
        runtime
            .upsert_task(&card(id, crate::ports::tasks::COLUMN_TODO))
            .await
            .expect("seed the card in To-do");
    }
    let guard = tracing::subscriber::set_default(subscriber);
    for id in ["card-1", "card-2"] {
        runtime
            .upsert_task(&card(id, crate::ports::tasks::COLUMN_IN_PROGRESS))
            .await
            .expect("drag it into In Progress");
    }
    drop(guard);

    let text = String::from_utf8(logs.0.lock().unwrap().clone()).expect("utf-8");
    assert!(
        text.contains("no agent pool"),
        "an inert board must say why nothing will work the card: {text:?}"
    );
    // The remedy has to be the one that helps THIS build, and asserting
    // its presence is only half of that (issue #1059 review). A message
    // carrying both remedies would satisfy every "contains" assertion while
    // still telling a default-build operator to call a method that is not
    // in their binary — so each arm also pins the other's absence, which is
    // what makes this prove the split rather than tolerate it.
    #[cfg(feature = "openhuman")]
    {
        assert!(
            text.contains("with_harness"),
            "a build WITH the feature must name the call that wires a pool: {text:?}"
        );
        assert!(
            !text.contains("--features openhuman"),
            "the feature is already on; telling this operator to rebuild with it is \
             a remedy for a problem they do not have: {text:?}"
        );
    }
    #[cfg(not(feature = "openhuman"))]
    {
        assert!(
            text.contains("--features openhuman"),
            "a default-feature build has no harness to wire, so rebuilding with the \
             feature is the only thing that helps: {text:?}"
        );
        assert!(
            !text.contains("with_harness"),
            "`RuntimeBuilder::with_harness` is itself `#[cfg(feature = \"openhuman\")]`, \
             so naming it here sends the operator after a method that is not compiled \
             into their binary: {text:?}"
        );
    }
    assert_eq!(
        text.matches("no agent pool").count(),
        1,
        "the warning is latched per runtime, not raised per card: {text:?}"
    );

    // The level, read from the event rather than inferred from the text.
    // `warn!` is the whole point: demoted to `debug!` it restores the
    // silence this fixes, and promoted to `error!` it cries failure over a
    // documented default that ~200 callers build on purpose.
    let seen = levels.0.lock().unwrap().clone();
    let inert: Vec<_> = seen
        .iter()
        .filter(|(_, message)| message.contains("no agent pool"))
        .collect();
    assert_eq!(
        inert.len(),
        1,
        "exactly one inert-board event should reach the subscriber: {seen:?}"
    );
    assert_eq!(
        inert[0].0,
        tracing::Level::WARN,
        "the inert-board line must stay at WARN: {seen:?}"
    );
}

/// Issue #242: a run row left active by a dead host is reclaimed at the next
/// boot, and a parked one is not.
///
/// The store is the default fs backend over the same home, so the second
/// `build()` is a genuine restart of the same company — this asserts the
/// reaper is *wired into boot*, not merely that the port function works
/// (which the conformance suite covers for all three backends).
#[tokio::test]
async fn boot_reaps_runs_stranded_by_a_previous_host() {
    use crate::ports::runs::{NewRun, ORPHAN_ERROR, RunOutcome, RunStatus};

    let home_dir = tmp_home("oc-run-reap-");
    let home = home_dir.path().to_path_buf();
    let manifest = parse("[company]\nname=\"Acme\"\n[policy]\nmode=\"full\"\n");
    let id = CompanyId::new("acme");
    let spec = |run: &str, task: &str| NewRun::for_task(run, task, "ceo");

    let first_boot = RuntimeBuilder::new(home.clone(), manifest.clone())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let runs = first_boot.runs().clone();

    // Two attempts the host is "running", and one parked for a person.
    runs.create_run(&id, spec("pending", "card-a"))
        .await
        .unwrap();
    runs.create_run(&id, spec("running", "card-b"))
        .await
        .unwrap();
    runs.begin_run(&id, "running", crate::ports::types::EventSeq::new(1))
        .await
        .unwrap();
    runs.create_run(&id, spec("review", "card-c"))
        .await
        .unwrap();
    runs.begin_run(&id, "review", crate::ports::types::EventSeq::new(2))
        .await
        .unwrap();
    runs.finish_run(&id, "review", RunOutcome::new(RunStatus::WaitingApproval))
        .await
        .unwrap();

    // The host dies here — no settle, no journal entry, nothing.
    drop(first_boot);

    let second_boot = RuntimeBuilder::new(home.clone(), manifest)
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let runs = second_boot.runs();

    for stranded in ["pending", "running"] {
        let run = runs.get_run(&id, stranded).await.unwrap().unwrap();
        assert_eq!(
            run.status,
            RunStatus::Failed,
            "{stranded} outlived its process and must be reclaimed"
        );
        assert_eq!(run.error.as_deref(), Some(ORPHAN_ERROR));
        assert!(run.finished_at_millis.is_some());
    }

    // Parked is not orphaned: this one is waiting on a person, and a restart
    // must not throw that work away.
    let review = runs.get_run(&id, "review").await.unwrap().unwrap();
    assert_eq!(review.status, RunStatus::WaitingApproval);
    assert_eq!(review.error, None);

    assert!(runs.list_stale_active(&id).await.unwrap().is_empty());
}

/// Issue #983: a chat turn the host died under is reclaimed on **both**
/// halves — a `Failed` row carrying the orphan reason, and a `TurnFailed`
/// line closing the transcript bracket.
///
/// Both are needed and neither is derivable from the other. The row makes
/// `GET {scope}/runs` honest; the event makes the *conversation* honest,
/// and without it the operator's question sits there with no answer and no
/// explanation — which is what a message that never warranted a reply looks
/// like too. The turn names no card, which is exactly why the row sweep
/// alone leaves nothing an operator would ever find.
#[tokio::test]
async fn boot_reclaims_a_chat_turn_stranded_by_a_previous_host() {
    use crate::ports::runs::{NewRun, ORPHAN_ERROR, RunStatus};
    use crate::ports::types::{CompanyEvent, EventSeq};

    let home_dir = tmp_home("oc-turn-reap-");
    let home = home_dir.path().to_path_buf();
    let manifest = parse("[company]\nname=\"Acme\"\n[policy]\nmode=\"full\"\n");
    let id = CompanyId::new("acme");

    let first_boot = RuntimeBuilder::new(home.clone(), manifest.clone())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    first_boot
        .runs()
        .create_run(&id, NewRun::for_chat("turn-dead", "general", "general"))
        .await
        .unwrap();
    first_boot
        .runs()
        .begin_run(&id, "turn-dead", EventSeq::new(1))
        .await
        .unwrap();
    first_boot
        .events()
        .append(
            &id,
            CompanyEvent::TurnStarted {
                turn_id: "turn-dead".to_string(),
                chat_id: "general".to_string(),
                parent: None,
                by: None,
                agent_id: None,
                episode_id: None,
                round_revision: None,
            },
        )
        .await
        .unwrap();

    // The host dies here: no settle, no reply, no failure line.
    drop(first_boot);

    let second_boot = RuntimeBuilder::new(home.clone(), manifest)
        .with_id(id.clone())
        .build()
        .await
        .unwrap();

    let row = second_boot
        .runs()
        .get_run(&id, "turn-dead")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, RunStatus::Failed);
    assert_eq!(row.error.as_deref(), Some(ORPHAN_ERROR));
    assert_eq!(row.task_id, None, "a chat turn attempted no card");

    let swept: Vec<String> = second_boot
        .events()
        .read_from(&id, EventSeq::new(0), usize::MAX)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|s| match s.event {
            CompanyEvent::TurnFailed { turn_id, error, .. } if turn_id == "turn-dead" => {
                Some(error)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        swept,
        vec![crate::runtime::TURN_INTERRUPTED_BY_RESTART.to_string()],
        "the transcript bracket was left open by the boot sweep"
    );
}

/// **The negative half, and the one that matters most.** A live runtime
/// rebuild must sweep *neither* half of a chat turn.
///
/// This is the #290 lesson in its sharpest form. A rebuild happens in a
/// process that has been serving, so "nothing from this process can be in
/// flight" — the whole proof both sweeps rest on — is false. And a chat turn
/// is more exposed than a workflow run: `rebuild_company` quiesces and
/// drains the *cycle* lock, but the spawned turn task journals its replies
/// and settles its row **after** the cycle returns, so a turn is routinely
/// live at exactly the moment a rebuild reaches here. Sweeping would fail
/// the row out from under it — its own settle is then rejected by the
/// transition table — and tell the operator in the transcript that the turn
/// failed, moments before its answer arrives.
#[tokio::test]
async fn a_rebuild_sweeps_no_live_chat_turn() {
    use crate::ports::runs::{NewRun, RunStatus};
    use crate::ports::types::{CompanyEvent, EventSeq};

    let home_dir = tmp_home("oc-turn-rebuild-");
    let home = home_dir.path().to_path_buf();
    let manifest = parse("[company]\nname=\"Acme\"\n[policy]\nmode=\"full\"\n");
    let id = CompanyId::new("acme");

    let live = RuntimeBuilder::new(home.clone(), manifest.clone())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    live.runs()
        .create_run(&id, NewRun::for_chat("turn-live", "general", "general"))
        .await
        .unwrap();
    live.runs()
        .begin_run(&id, "turn-live", EventSeq::new(1))
        .await
        .unwrap();
    live.events()
        .append(
            &id,
            CompanyEvent::TurnStarted {
                turn_id: "turn-live".to_string(),
                chat_id: "general".to_string(),
                parent: None,
                by: None,
                agent_id: None,
                episode_id: None,
                round_revision: None,
            },
        )
        .await
        .unwrap();

    // The swap, as `rebuild_company` performs it: quiesce, hand over, build.
    live.quiesce().await;
    let successor = RuntimeBuilder::new(home, manifest)
        .with_id(id.clone())
        .with_handover(live.handover())
        .build()
        .await
        .unwrap();

    let row = successor
        .runs()
        .get_run(&id, "turn-live")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.status,
        RunStatus::Running,
        "a rebuild failed a turn that is still working"
    );
    assert!(
        !successor
            .events()
            .read_from(&id, EventSeq::new(0), usize::MAX)
            .await
            .unwrap()
            .iter()
            .any(|s| matches!(&s.event, CompanyEvent::TurnFailed { .. })),
        "a rebuild told the operator a live turn had failed"
    );

    // And the turn's own settle still lands, because nothing took the row
    // to a terminal state behind its back.
    successor
        .runs()
        .finish_run(
            &id,
            "turn-live",
            crate::ports::runs::RunOutcome::new(RunStatus::Succeeded),
        )
        .await
        .expect("the live turn can still settle itself");
}

/// Issue #337, the crash-truthfulness half: reaping the *row* is not enough
/// — the **card** has to leave In Progress too, or the board keeps claiming
/// work that provably is not being done and nothing will ever re-drive it
/// (`task_enters_in_progress` fires on the transition, which already
/// happened).
///
/// Three things at once, because they are one behaviour: the stranded card
/// returns to To-do with the reason readable on it, a card parked for a
/// person is untouched, and re-dispatching the returned card starts a
/// **new** attempt rather than resuming the dead one.
#[tokio::test]
async fn boot_returns_a_stranded_card_and_leaves_a_parked_one_alone() {
    use crate::ports::runs::{NewRun, ORPHAN_ERROR, RunOutcome, RunStatus};
    use crate::ports::tasks::{COLUMN_IN_PROGRESS, COLUMN_PAUSED, COLUMN_TODO, TaskRecord};

    let home_dir = tmp_home("oc-run-reap-cards-");
    let home = home_dir.path().to_path_buf();
    let manifest = parse("[company]\nname=\"Acme\"\n[policy]\nmode=\"full\"\n");
    let id = CompanyId::new("acme");
    let card = |task: &str, column: &str| TaskRecord {
        id: task.to_string(),
        title: crate::ports::tasks::TaskTitle::authored("Draft the spec"),
        note: Some("[maya] started".to_string()),
        column: column.to_string(),
        priority: "medium".to_string(),
        assignee: "ceo".to_string(),
        updated_at_millis: 1,
        origin: None,
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

    let first_boot = RuntimeBuilder::new(home.clone(), manifest.clone())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let runs = first_boot.runs().clone();
    let tasks = first_boot.tasks().clone();

    // `card-a` is being worked by an attempt that will die with the host.
    // `card-b` is parked for a person, and its run is parked with it.
    tasks
        .upsert(&id, &card("card-a", COLUMN_IN_PROGRESS))
        .await
        .unwrap();
    tasks
        .upsert(&id, &card("card-b", COLUMN_PAUSED))
        .await
        .unwrap();
    runs.create_run(&id, NewRun::for_task("run-a", "card-a", "ceo"))
        .await
        .unwrap();
    runs.begin_run(&id, "run-a", crate::ports::types::EventSeq::new(1))
        .await
        .unwrap();
    runs.create_run(&id, NewRun::for_task("run-b", "card-b", "ceo"))
        .await
        .unwrap();
    runs.begin_run(&id, "run-b", crate::ports::types::EventSeq::new(2))
        .await
        .unwrap();
    runs.finish_run(&id, "run-b", RunOutcome::new(RunStatus::Paused))
        .await
        .unwrap();

    // The host dies here — `kill -9`, no settle, no journal entry.
    drop(first_boot);

    let second_boot = RuntimeBuilder::new(home.clone(), manifest)
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let tasks = second_boot.tasks();
    let after = |task: &'static str| {
        let tasks = tasks.clone();
        let id = id.clone();
        async move {
            tasks
                .list(&id)
                .await
                .unwrap()
                .into_iter()
                .find(|t| t.id == task)
                .expect("card survives the restart")
        }
    };

    // The stranded card is back in To-do, and says why in words an operator
    // can act on rather than silently.
    let stranded = after("card-a").await;
    assert_eq!(stranded.column, COLUMN_TODO);
    let note = stranded.note.expect("note");
    assert!(note.contains(ORPHAN_ERROR), "{note}");
    assert!(
        note.contains("[maya] started"),
        "the note is append-only; what the run already said must survive: {note}"
    );

    // The parked card is exactly as it was. Its run was `Paused`, so the
    // reaper never saw it — and even if it had, the mover only ever leaves
    // In Progress.
    let parked = after("card-b").await;
    assert_eq!(parked.column, COLUMN_PAUSED);
    assert_eq!(parked.note.as_deref(), Some("[maya] started"));

    // Re-dispatching the returned card mints a **new** attempt. Nothing
    // resurrects `run-a`, which is terminal.
    let runs = second_boot.runs();
    assert_eq!(
        runs.get_run(&id, "run-a").await.unwrap().unwrap().status,
        RunStatus::Failed
    );
    let next = runs
        .create_run(&id, NewRun::for_task("run-a2", "card-a", "ceo"))
        .await
        .unwrap();
    assert_eq!(
        next.attempt, 2,
        "a card that came back to To-do is re-tried, not resumed"
    );
}
