//! `#general` driven end to end: `hives_for`, `surface_of`, the dispatcher and
//! real seats on the pool's own agents, with the model scripted on loopback.

use std::sync::Arc;

use crate::harness::HarnessPool;
use crate::hive::test_support::{MemoryLog, TWO_DESKS, record};
use crate::ports::events::EventLog;
use crate::ports::types::CompanyEvent;
use crate::workflows::gated_tool_turn_tests::{Turn, deps, spawn_script_recording};

const GENERAL: &str = crate::ports::general_channel::GENERAL_CHANNEL_ID;

fn advertised(body: &serde_json::Value) -> Vec<String> {
    body.get("tools")
        .and_then(|tools| tools.as_array())
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| {
                    tool.get("function")
                        .and_then(|function| function.get("name"))
                        .and_then(|name| name.as_str())
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_general_message_runs_an_episode_in_general() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let completing = || Turn::Call {
        tool: "desk_complete_episode",
        args: serde_json::json!({
            "message": "passkeys land next sprint",
            "chat": GENERAL,
            "parent": null
        }),
    };
    let (base_url, script) =
        spawn_script_recording(vec![completing(), completing(), completing(), completing()]).await;
    let (deps, _journal) = deps(base_url, dir.path());
    let record = record(TWO_DESKS);
    let pool = HarnessPool::new();
    pool.ensure(&record, &deps).await.expect("the roster boots");

    let hives = crate::hive::dispatch::hives_for(&record, &|id| {
        futures::executor::block_on(pool.agent(&record.id, id))
            .map(|agent| agent.runtime_agent().clone())
    });
    let surface = crate::hive::dispatch::surface_of(&record, &hives, Some(GENERAL));
    let crate::hive::dispatch::Surface::Room { desk_id } = surface else {
        panic!("#general with a hive is a room: {surface:?}");
    };
    assert_eq!(desk_id, GENERAL);
    assert_eq!(hives[GENERAL].lead().as_deref(), Some("ceo"));

    let log = Arc::new(MemoryLog::default());
    let events: Arc<dyn EventLog> = log.clone();
    let dispatcher = crate::hive::dispatch::dispatcher(
        Arc::new(record.clone()),
        Arc::clone(&events),
        hives,
        Arc::new(deps),
        Arc::new(pool),
        None,
    )
    .await;
    let seq = events
        .append(
            &record.id,
            crate::hive::test_support::operator_message(&desk_id, "when do passkeys ship?", None),
        )
        .await
        .expect("the operator's message is a real row");
    let report = dispatcher
        .run_desk_message(
            &desk_id,
            crate::hive::conducted::Trigger {
                seq,
                text: "when do passkeys ship?".to_owned(),
                parent: None,
                mentions: Vec::new(),
            },
        )
        .await
        .expect("the episode runs");
    assert!(report.turns > 0, "a seat took a turn: {report:?}");

    let rows = log.rows();
    let opened = rows
        .iter()
        .position(|row| {
            matches!(&row.event, CompanyEvent::EpisodeOpened { chat_id, .. } if chat_id == GENERAL)
        })
        .unwrap_or_else(|| panic!("an episode opened in #general: {:?}", log.kinds()));
    let replied = rows
        .iter()
        .position(|row| {
            matches!(
                &row.event,
                CompanyEvent::AgentReply { chat_id, agent_id, episode, .. }
                    if chat_id == GENERAL && agent_id != crate::ports::SYSTEM_AUTHOR && episode.is_some()
            )
        })
        .unwrap_or_else(|| panic!("a seat replied in #general: {:?}", log.kinds()));
    let completed = rows
        .iter()
        .position(|row| {
            matches!(&row.event, CompanyEvent::EpisodeCompleted { chat_id, .. } if chat_id == GENERAL)
        })
        .unwrap_or_else(|| panic!("the episode completed in #general: {:?}", log.kinds()));
    assert!(
        opened < replied && replied < completed,
        "opened {opened}, replied {replied}, completed {completed}"
    );

    let seen = script.seen.lock().unwrap().clone();
    assert!(!seen.is_empty(), "the seat reached the model");
    for body in &seen {
        let belt = advertised(body);
        for withheld in crate::harness::built_in::EPISODE_WITHHELD_TOOLS {
            assert!(
                !belt.iter().any(|name| name == withheld),
                "a #general seat is not offered `{withheld}`: {belt:?}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_with_no_chat_id_opens_no_episode() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let (base_url, _script) = spawn_script_recording(Vec::new()).await;
    let (deps, _journal) = deps(base_url, dir.path());
    let record = record(TWO_DESKS);
    let pool = HarnessPool::new();
    pool.ensure(&record, &deps).await.expect("the roster boots");

    let hives = crate::hive::dispatch::hives_for(&record, &|id| {
        futures::executor::block_on(pool.agent(&record.id, id))
            .map(|agent| agent.runtime_agent().clone())
    });
    assert!(hives.contains_key(GENERAL), "{:?}", hives.keys());
    assert_eq!(
        crate::hive::dispatch::surface_of(&record, &hives, None),
        crate::hive::dispatch::Surface::Single
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_addressed_the_legacy_id_reaches_the_general_room() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let (base_url, _script) = spawn_script_recording(Vec::new()).await;
    let (deps, _journal) = deps(base_url, dir.path());
    let record = record(TWO_DESKS);
    let pool = HarnessPool::new();
    pool.ensure(&record, &deps).await.expect("the roster boots");

    let hives = crate::hive::dispatch::hives_for(&record, &|id| {
        futures::executor::block_on(pool.agent(&record.id, id))
            .map(|agent| agent.runtime_agent().clone())
    });
    let addressed = crate::ports::general_channel::decode_general_chat_id("general".into());
    let surface = crate::hive::dispatch::surface_of(&record, &hives, Some(&addressed));
    assert!(
        matches!(&surface, crate::hive::dispatch::Surface::Room { desk_id } if desk_id == GENERAL),
        "{surface:?}"
    );
}
