//! Tests for the per-desk hive graph.

use std::collections::HashMap;

use openhuman_embed::AgentSpec;

use super::*;
use crate::harness::openhuman_runtime::{RuntimeBoot, global};
use crate::hive::test_support::{TWO_DESKS, record};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shared_seat_is_the_same_agent_in_both_hives_and_a_desk_of_one_gets_none() {
    let runtime = global(RuntimeBoot::ephemeral()).await.expect("runtime");
    let salt = uuid::Uuid::new_v4().simple().to_string();
    let agents: HashMap<String, openhuman_embed::Agent> = ["ceo", "engineer", "writer"]
        .into_iter()
        .map(|id| {
            (
                id.to_string(),
                runtime
                    .agent(AgentSpec::new(format!("hive-graph-{id}-{}", &salt[..8])))
                    .expect("agent"),
            )
        })
        .collect();
    let mut record = record(TWO_DESKS);
    // A third desk of one, and one whose only other seat is unbound.
    record.manifest.group_chats.push(crate::company::GroupChat {
        id: "solo".into(),
        name: "Solo".into(),
        description: None,
        members: vec!["writer".into()],
        tools: Vec::new(),
        hive: Default::default(),
    });
    let (hives, errors) = desk_hives(&record, 7, &|id| agents.get(id).cloned());
    assert!(errors.is_empty(), "{errors:?}");
    let mut ids: Vec<&String> = hives.keys().collect();
    ids.sort();
    assert_eq!(ids, vec!["content", "engineering"]);
    let engineering = &hives["engineering"];
    let content = &hives["content"];
    assert_eq!(engineering.members(), vec!["engineer", "ceo"]);
    assert_eq!(engineering.lead().as_deref(), Some("engineer"));
    assert_eq!(engineering.roster_version, 7);
    assert_eq!(engineering.desk_name, "Engineering desk");
    let shared_here = engineering.hive.bound_agent("ceo").expect("ceo bound");
    let shared_there = content.hive.bound_agent("ceo").expect("ceo bound");
    assert_eq!(
        shared_here.0.id(),
        shared_there.0.id(),
        "one runtime agent, two hives"
    );
    let candidate = engineering
        .hive
        .graph()
        .candidates
        .iter()
        .find(|candidate| candidate.id == "engineer")
        .expect("candidate");
    assert_eq!(candidate.role.as_deref(), Some("Engineer"));
    assert!(candidate.available);

    // An unbound member leaves the desk with one seat: no hive.
    let (hives, errors) = desk_hives(&record, 8, &|id| (id != "ceo").then(|| agents[id].clone()));
    assert!(errors.is_empty(), "{errors:?}");
    assert!(hives.is_empty(), "{:?}", hives.keys().collect::<Vec<_>>());
}

/// A DM is a room of the whole roster, led by whose DM it is.
///
/// The membership is what makes `ask` possible at all: its target is resolved
/// against the bound members, so a teammate alone in its own DM would carry
/// the tool with nobody it could legally name. The *lead* is what keeps that
/// membership from changing who answers the operator -- a message in `dm:ceo`
/// is the CEO's, and the rest of the roster is there to be asked, not to reply.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dm_is_the_whole_roster_led_by_the_teammate_it_belongs_to() {
    let runtime = global(RuntimeBoot::ephemeral()).await.expect("runtime");
    let salt = uuid::Uuid::new_v4().simple().to_string();
    let agents: HashMap<String, openhuman_embed::Agent> = ["ceo", "engineer", "writer"]
        .into_iter()
        .map(|id| {
            (
                id.to_string(),
                runtime
                    .agent(AgentSpec::new(format!("hive-dm-{id}-{}", &salt[..8])))
                    .expect("agent"),
            )
        })
        .collect();
    let record = record(TWO_DESKS);

    let (dms, errors) = dm_hives(&record, 7, &|id| agents.get(id).cloned());
    assert!(errors.is_empty(), "{errors:?}");

    let mut ids: Vec<&String> = dms.keys().collect();
    ids.sort();
    assert_eq!(
        ids,
        vec!["dm:ceo", "dm:engineer", "dm:writer"],
        "one DM per roster teammate, keyed by the chat id `surface_of` looks up"
    );

    let ceo = dms.get("dm:ceo").expect("the CEO's DM");
    assert_eq!(
        ceo.lead().as_deref(),
        Some("ceo"),
        "the lead is whose DM it is, so the operator is never answered by someone else"
    );
    let mut members = ceo.members();
    members.sort();
    assert_eq!(
        members,
        vec!["ceo", "engineer", "writer"],
        "everyone is bound, so `ask` has somewhere to land"
    );

    // The same teammate leads its own DM and is merely present in the others.
    let writer = dms.get("dm:writer").expect("the writer's DM");
    assert_eq!(writer.lead().as_deref(), Some("writer"));
    assert!(
        writer.members().contains(&"ceo".to_string()),
        "a teammate is askable from a DM that is not its own"
    );
}

/// A teammate the pool cannot bind gets no DM, and is in nobody else's.
///
/// An unbound member would be a name `ask` could reach for and the runner
/// could not seat -- the refusal arriving a turn later, from the driver,
/// rather than here where it can simply not be offered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unbound_teammate_is_in_no_dm_at_all() {
    let runtime = global(RuntimeBoot::ephemeral()).await.expect("runtime");
    let salt = uuid::Uuid::new_v4().simple().to_string();
    // `writer` is deliberately absent from the pool.
    let agents: HashMap<String, openhuman_embed::Agent> = ["ceo", "engineer"]
        .into_iter()
        .map(|id| {
            (
                id.to_string(),
                runtime
                    .agent(AgentSpec::new(format!(
                        "hive-dm-unbound-{id}-{}",
                        &salt[..8]
                    )))
                    .expect("agent"),
            )
        })
        .collect();
    let record = record(TWO_DESKS);

    let (dms, errors) = dm_hives(&record, 7, &|id| agents.get(id).cloned());
    assert!(errors.is_empty(), "{errors:?}");

    let mut ids: Vec<&String> = dms.keys().collect();
    ids.sort();
    assert_eq!(
        ids,
        vec!["dm:ceo", "dm:engineer"],
        "no DM for an unbound seat"
    );
    assert!(
        !dms["dm:ceo"].members().contains(&"writer".to_string()),
        "and it is not askable from anyone else's"
    );
}

/// An operator DM is answered by whose DM it is, never by a router's pick.
///
/// This is the decision that binding the roster makes dangerous. Every
/// teammate is a member so `ask` has somewhere to land -- which also makes
/// every teammate a candidate the router could choose. If routing ran here, a
/// message to your PM could be answered by whoever a ranker preferred, and
/// with a TinyHumans key that ranker is a model.
#[test]
fn a_dm_is_answered_by_its_owner_and_a_desk_is_still_routed() {
    use crate::hive::conducted::dm_opening;

    let (starters, plan) = dm_opening("dm:ceo", "ceo", None).expect("a DM pins its own responder");
    assert_eq!(starters, vec!["ceo".to_string()]);
    assert!(
        matches!(plan, crate::hive::routing::RoutingPlanDto::One { primary_id } if primary_id == "ceo"),
        "one recipient, named -- not a plan for the router to fill in"
    );

    // Naming someone in your own DM is an instruction, not an ambiguity.
    let (starters, _) = dm_opening("dm:ceo", "ceo", Some("engineer"))
        .expect("a DM still pins, even when a mention redirects it");
    assert_eq!(
        starters,
        vec!["engineer".to_string()],
        "an explicit mention wins over the owner"
    );

    assert!(
        dm_opening("engineering", "ceo", None).is_none(),
        "a desk is routed -- that is what a desk is for"
    );
}
