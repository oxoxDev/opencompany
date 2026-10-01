//! Drift as the reads actually reach it: over one entry of
//! [`skill_effective::resolve`], not over a hand-assembled pair of documents.
//!
//! `skill_provenance_tests.rs` covers [`drift`] itself. These cover the adapter
//! and the resolution around it — which entries carry a pin at all, which lose
//! one, and what a reader gets for an entry that was never pinned.

use super::*;
use crate::company::skill_effective::{EffectiveSkill, resolve};
use crate::ports::skills_state::{SkillInstall, SkillSource, SkillState};

fn library_doc(version: &str, body: &str) -> SkillDoc {
    SkillDoc {
        slug: "web-research".to_string(),
        name: "Web research".to_string(),
        description: "research a topic on the web".to_string(),
        category: Some("research".to_string()),
        version: Some(version.to_string()),
        body: body.to_string(),
        extra_frontmatter: Vec::new(),
    }
}

/// An install pin over `doc`, exactly as the install route records one.
fn pin(doc: &SkillDoc) -> SkillInstall {
    SkillInstall {
        digest: skill_digest(&render_skill_md(doc)),
        version: doc.version.clone(),
        installed_by: None,
        installed_at_millis: 1_700_000_000_000,
    }
}

/// A registry delta holding `stored` and pinned to `install`.
fn delta(stored: &str, install: Option<SkillInstall>) -> SkillState {
    SkillState {
        slug: "web-research".to_string(),
        enabled: true,
        source: SkillSource::Registry,
        custom_doc: Some(stored.to_string()),
        updated_at_millis: Some(1_700_000_000_000),
        install,
    }
}

fn entry<'a>(set: &'a [EffectiveSkill], slug: &str) -> &'a EffectiveSkill {
    set.iter()
        .find(|skill| skill.slug == slug)
        .unwrap_or_else(|| panic!("no `{slug}` in the effective set"))
}

/// The library moved on under an untouched install. The read must offer the
/// update and name both revisions.
#[test]
fn a_resolved_install_whose_library_moved_offers_the_update() {
    let installed = library_doc("1.0.0", "Step one.\n");
    let moved = library_doc("2.0.0", "Rewritten upstream.\n");
    let stored = render_skill_md(&installed);
    let registry = vec![moved];

    let set = resolve(None, &registry, &[delta(&stored, Some(pin(&installed)))]).unwrap();
    let drifted = effective_drift(entry(&set, "web-research"), &registry)
        .expect("a pinned install answers the question");

    assert_eq!(
        drifted.update_available,
        Some(VersionChange {
            from: Some("1.0.0".to_string()),
            to: Some("2.0.0".to_string()),
        })
    );
    assert!(!drifted.modified);
    assert!(drifted.update_allowed());
}

/// The stored copy was edited after it was pinned — by an upload over the slug,
/// or by anything that reached the store directly. The read must say so, and
/// must not offer an update that would discard the edit.
#[test]
fn a_resolved_install_edited_out_of_band_reads_as_modified() {
    let installed = library_doc("1.0.0", "Step one.\n");
    let registry = vec![installed.clone()];
    let edited = render_skill_md(&installed).replace("Step one.", "Do something else.");

    let set = resolve(None, &registry, &[delta(&edited, Some(pin(&installed)))]).unwrap();
    let drifted = effective_drift(entry(&set, "web-research"), &registry)
        .expect("a pinned install answers the question");

    assert!(drifted.modified);
    assert_eq!(drifted.update_available, None);
    assert!(!drifted.update_allowed());
}

/// A baseline entry carries no pin, so the read gets no drift object at all.
///
/// The distinction is the point: an empty [`SkillDrift`] would serialize as
/// "checked, and clean", which is a claim nothing here is entitled to make.
#[test]
fn an_unpinned_entry_answers_nothing_rather_than_clean() {
    let set = resolve(None, &[], &[]).unwrap();
    let baseline = crate::globals::skills()
        .first()
        .expect("the baseline ships at least one skill");

    let entry = entry(&set, &baseline.slug);
    assert_eq!(entry.install, None);
    assert_eq!(effective_drift(entry, &[]), None);
}

/// A healed row is serving the library's live document, not its own snapshot,
/// so the pin no longer describes what is being read. Leaving it in place would
/// make every healed install read as `modified` — a locally edited copy — which
/// is the opposite of what happened.
#[test]
fn healing_a_degenerate_snapshot_drops_the_pin_it_can_no_longer_check() {
    let live = library_doc("2.0.0", "The whole procedure.\n");
    let registry = vec![live.clone()];
    // The pre-fix install path wrote the description as the body, which is what
    // `is_registry_stub` recognises.
    let mut stub = live.clone();
    stub.body = stub.description.clone();
    let stored = render_skill_md(&stub);

    let set = resolve(None, &registry, &[delta(&stored, Some(pin(&stub)))]).unwrap();

    let entry = entry(&set, "web-research");
    assert_eq!(
        entry.doc().map(|doc| doc.body.trim()),
        Some("The whole procedure."),
        "the heal served the library's document"
    );
    assert_eq!(
        entry.install, None,
        "a healed row keeps a pin it can no longer check"
    );
    assert_eq!(effective_drift(entry, &registry), None);
}

/// `[globals].disable` synthesizes a disabling delta that pins nothing. It must
/// not be read as the slug's pin having been dropped.
#[test]
fn a_synthesized_disable_does_not_erase_the_pin_beneath_it() {
    let installed = library_doc("1.0.0", "Step one.\n");
    let registry = vec![installed.clone()];
    let stored = render_skill_md(&installed);
    let mut deltas = vec![delta(&stored, Some(pin(&installed)))];
    deltas.extend(crate::company::skill_effective::globals_skill_disables(&[
        "skill:web-research".to_string(),
    ]));

    let set = resolve(None, &registry, &deltas).unwrap();

    let entry = entry(&set, "web-research");
    assert!(!entry.enabled, "the disable still wins");
    assert_eq!(
        entry.install.as_ref().map(|install| install.digest.clone()),
        Some(pin(&installed).digest),
        "the disable erased the pin the install wrote"
    );
}
/// The console decides whether to enable its Update action with its own
/// predicate. Two copies of one rule drift, and the failure is silent — the
/// menu offers an update the route then refuses, or hides one it would accept.
///
/// Read out of the console's source so a divergence fails here.
#[test]
fn the_console_mirrors_update_allowed_rather_than_inventing_its_own_rule() {
    const CONSOLE_LIB: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../frontend/src/lib/skills-list.ts"
    ));
    assert!(
        CONSOLE_LIB.contains("!!skill.updateAvailable && !skill.modified"),
        "frontend/src/lib/skills-list.ts no longer expresses `canUpdateSkill` as \
         `!!updateAvailable && !modified`, which is what `SkillDrift::update_allowed` decides. \
         Change both together."
    );

    for (update_available, modified, allowed) in [
        (false, false, false),
        (true, false, true),
        (false, true, false),
        (true, true, false),
    ] {
        let drifted = SkillDrift {
            update_available: update_available.then(|| VersionChange {
                from: Some("1.0.0".to_string()),
                to: Some("2.0.0".to_string()),
            }),
            modified,
        };
        assert_eq!(
            drifted.update_allowed(),
            allowed,
            "update_available={update_available}, modified={modified}"
        );
    }
}
