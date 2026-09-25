//! Which surface a chat resolves to, and the rows a takeover leaves behind.
//!
//! Extracted from `dispatch.rs`: an inline `#[cfg(test)] mod` is rejected by
//! `scripts/ci/assert-rs-source-layout.sh`, and it also put every item after
//! it -- `announce_takeover` and `hand_off_notice` -- behind a test module,
//! which `clippy::items_after_test_module` denies.

use super::*;
use crate::hive::test_support::{TWO_DESKS, record};

/// A DM routes to its hive, and to the pooled turn when it has none.
///
/// Both halves matter. The first is what makes an operator DM an episode
/// at all; the second is the flag being off -- `dm_hives` builds nothing
/// then, and an absent entry has to mean "take the path you always took"
/// rather than "this chat has no home".
#[test]
fn a_dm_is_a_room_when_it_has_a_hive_and_a_single_turn_when_it_does_not() {
    let record = record(TWO_DESKS);
    let empty: HashMap<String, Arc<crate::hive::graph::DeskHive>> = HashMap::new();

    assert!(
        matches!(surface_of(&record, &empty, Some("dm:ceo")), Surface::Single),
        "with no DM hive the pooled turn still answers"
    );

    // `resolve_desk_id` cannot answer for a DM -- it is not a desk in the
    // manifest -- so without the explicit branch this would stay `Single`
    // however many hives exist.
    assert!(
        matches!(
            surface_of(&record, &empty, Some("engineering")),
            Surface::Single
        ),
        "a desk with no hive is unchanged"
    );
}

/// General never becomes a DM room, whatever it is called.
#[test]
fn the_general_line_is_never_a_dm() {
    let record = record(TWO_DESKS);
    let empty: HashMap<String, Arc<crate::hive::graph::DeskHive>> = HashMap::new();
    assert!(matches!(
        surface_of(&record, &empty, Some("general")),
        Surface::Single
    ));
}

/// The flag is **on** unless something switches it off, and says so plainly.
///
/// Inverted deliberately: a DM running as an episode is the behaviour this
/// work exists for, so an instance that says nothing gets it. The escape
/// hatch stays exact — only an explicit off word restores the pooled turn,
/// and junk is not one, because a typo silently reverting the main operator
/// surface is the failure worth guarding against.
#[test]
fn dm_episodes_are_on_unless_switched_off() {
    struct Env(Option<&'static str>);
    impl crate::app::config::EnvSource for Env {
        fn get_os(&self, _key: &str) -> Option<std::ffi::OsString> {
            self.0.map(Into::into)
        }
    }
    use crate::hive::graph::dm_episodes_enabled;
    assert!(dm_episodes_enabled(&Env(None)), "unset is on");
    assert!(
        dm_episodes_enabled(&Env(Some("maybe"))),
        "junk is not an off word, so it does not silently revert the surface"
    );
    for off in ["0", "false", "no", "off", " off "] {
        assert!(!dm_episodes_enabled(&Env(Some(off))), "`{off}` is off");
    }
    for on in ["1", "true", "yes", "on", " true "] {
        assert!(dm_episodes_enabled(&Env(Some(on))), "`{on}` is still on");
    }
}

/// The console's own address for a DM reaches that DM's hive.
///
/// This is the defect the routing arm exists for. `dmThreadId`
/// (`frontend/src/views/room/channels.ts`) posts an ordinary teammate's DM
/// under the **bare** teammate id -- issue #364 re-keyed DMs onto it
/// deliberately -- and only a teammate whose id is a General spelling is
/// addressed `dm:<id>`. Keyed on the prefixed form alone, every DM the console
/// sent fell through to `Single`, so DM episodes were unreachable from the
/// operator console however `OPENCOMPANY_DM_EPISODES` was set: a live run with
/// the flag on built twelve DM hives, opened no episode, and the teammate --
/// having no seat, and so no `ask` -- wrote the consultation it had been asked
/// to hold into the operator's own line instead.
///
/// Both spellings answer, and they answer with the **same** desk id, because
/// that id is also the key every row of the episode is journaled under
/// (`hive::conducted`'s `chat:`). Two answers here would be two transcripts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dm_addressed_the_way_the_console_addresses_it_is_still_a_room() {
    use crate::harness::openhuman_runtime::{RuntimeBoot, global};
    use openhuman_embed::AgentSpec;

    let runtime = global(RuntimeBoot::ephemeral()).await.expect("runtime");
    let salt = uuid::Uuid::new_v4().simple().to_string();
    let agents: HashMap<String, openhuman_embed::Agent> = ["ceo", "engineer", "writer"]
        .into_iter()
        .map(|id| {
            (
                id.to_string(),
                runtime
                    .agent(AgentSpec::new(format!("hive-surface-{id}-{}", &salt[..8])))
                    .expect("agent"),
            )
        })
        .collect();
    let record = record(TWO_DESKS);
    let (dms, errors) = crate::hive::graph::dm_hives(&record, 7, &|id| agents.get(id).cloned());
    assert!(errors.is_empty(), "{errors:?}");

    let bare = surface_of(&record, &dms, Some("engineer"));
    assert!(
        matches!(&bare, Surface::Room { desk_id } if desk_id == "dm:engineer"),
        "the bare teammate id is how the console addresses a DM: {bare:?}"
    );

    let prefixed = surface_of(&record, &dms, Some("dm:engineer"));
    assert!(
        matches!(&prefixed, Surface::Room { desk_id } if desk_id == "dm:engineer"),
        "and the prefixed form still answers, unchanged: {prefixed:?}"
    );

    assert!(
        matches!(surface_of(&record, &dms, Some("nobody")), Surface::Single),
        "a key naming no teammate and no desk is still the pooled turn"
    );
}

/// A declared desk keeps a key it shares with a teammate.
///
/// A blueprint may name both a desk and a teammate the same thing -- manifest
/// validation does not forbid it (issue #1743) -- and the bare key belongs to
/// the desk, which is why the roster is asked only after `resolve_desk_id` has
/// declined. The teammate is still reachable, by the prefixed address that
/// exists for exactly this collision.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_desk_outranks_a_teammate_that_shares_its_id() {
    use crate::harness::openhuman_runtime::{RuntimeBoot, global};
    use openhuman_embed::AgentSpec;

    const COLLIDING: &str = r#"
[company]
name = "Acme"

[[agent]]
id = "ceo"
role = "Chief Executive"

[[agent]]
id = "engineering"
role = "Engineer"

[[group_chat]]
id = "engineering"
name = "Engineering desk"
description = "How things are built."
members = ["engineering", "ceo"]
"#;

    let runtime = global(RuntimeBoot::ephemeral()).await.expect("runtime");
    let salt = uuid::Uuid::new_v4().simple().to_string();
    let agents: HashMap<String, openhuman_embed::Agent> = ["ceo", "engineering"]
        .into_iter()
        .map(|id| {
            (
                id.to_string(),
                runtime
                    .agent(AgentSpec::new(format!("hive-clash-{id}-{}", &salt[..8])))
                    .expect("agent"),
            )
        })
        .collect();
    let record = record(COLLIDING);
    let bind = |id: &str| agents.get(id).cloned();
    let (mut hives, _) = crate::hive::graph::desk_hives(&record, 7, &bind);
    let (dms, _) = crate::hive::graph::dm_hives(&record, 7, &bind);
    hives.extend(dms);

    let bare = surface_of(&record, &hives, Some("engineering"));
    assert!(
        matches!(&bare, Surface::Room { desk_id } if desk_id == "engineering"),
        "the desk owns the bare key, so its transcript is not merged with a DM: {bare:?}"
    );

    let prefixed = surface_of(&record, &hives, Some("dm:engineering"));
    assert!(
        matches!(&prefixed, Surface::Room { desk_id } if desk_id == "dm:engineering"),
        "and the teammate is still reachable, prefixed: {prefixed:?}"
    );

    // **The desk keeps its key even with no hive of its own.**
    //
    // `desk_hives` builds nothing for a desk with nobody to deliberate with,
    // and skips one whose members will not bind. With only the DM hives in the
    // map, the desk arm finds nothing -- and must still answer `Single` rather
    // than falling through to the teammate that shares the id, which would run
    // a desk's message as that teammate's private episode.
    let (dms_only, _) = crate::hive::graph::dm_hives(&record, 7, &bind);
    let hiveless = surface_of(&record, &dms_only, Some("engineering"));
    assert!(
        matches!(hiveless, Surface::Single),
        "a declared desk with no hive is a pooled turn, never the teammate's DM: {hiveless:?}"
    );
}
