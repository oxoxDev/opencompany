use std::sync::Arc;

use async_trait::async_trait;

use super::*;
use crate::ports::CompanyStore;
use crate::ports::types::{CompanyId, CompanyRecord};

struct RecordStore(Option<CompanyRecord>);

#[async_trait]
impl CompanyStore for RecordStore {
    async fn load(&self, _id: &CompanyId) -> crate::Result<Option<CompanyRecord>> {
        Ok(self.0.clone())
    }
    async fn save(&self, _record: &CompanyRecord) -> crate::Result<()> {
        unreachable!("resolve only reads")
    }
    async fn list(&self) -> crate::Result<Vec<CompanySummary>> {
        unreachable!("resolve only reads")
    }
    async fn append_ledger(
        &self,
        _id: &CompanyId,
        _entry: crate::ports::types::LedgerEntry,
    ) -> crate::Result<()> {
        unreachable!("resolve only reads")
    }
}

use crate::ports::types::CompanySummary;

fn record_with_group_chat(id: &str, name: &str) -> CompanyRecord {
    let manifest = toml::from_str(&format!(
        r#"
[company]
name = "Acme"

[policy]
mode = "full"

[[agent]]
id = "ceo"
role = "Chief Executive"
description = "Sets direction."

[[group_chat]]
id = "{id}"
name = "{name}"
"#,
    ))
    .expect("valid manifest");
    CompanyRecord {
        overlay_desk_hive: Vec::new(),
        overlay_retired_agents: Vec::new(),
        overlay_agent_edits: Vec::new(),
        overlay_tool_grants: None,
        name_confirmed: false,
        activation_completed_at: None,
        created_at_millis: None,
        id: CompanyId::new("acme"),
        manifest,
        ledger: Vec::new(),
        lifecycle: "running".to_string(),
        setup: None,
        overlay_agents: Vec::new(),
        overlay_desk_members: Vec::new(),
        overlay_desk_order: Vec::new(),
        overlay_desks: Vec::new(),
        overlay_workflows: Vec::new(),
        overlay_budgets: Vec::new(),
        overlay_policy: None,
        overlay_desk_tools: Default::default(),
        disabled_workflows: Vec::new(),
        template_provenance: None,
    }
}

async fn resolve(store: RecordStore, chat_id: Option<&str>) -> (String, String) {
    let store: Arc<dyn CompanyStore> = Arc::new(store);
    resolve_seed_desk(&store, &CompanyId::new("acme"), chat_id).await
}

/// A desk created from the console is a desk.
///
/// It lives in `overlay_desks` and never in the manifest, so a lookup that
/// reads only `group_chats` fell through to the verbatim selector — and
/// every line journaled under the desk's *other* spelling was orphaned from
/// the thread index, `read_thread` and the seed alike (coderabbit + codex
/// on #1972).
#[test]
fn an_overlay_desk_resolves_by_either_spelling() {
    let mut record = record_with_group_chat("growth_desk", "Growth");
    record.overlay_desks.push(crate::ports::types::OverlayDesk {
        id: "ops_desk".to_string(),
        name: "Operations".to_string(),
        description: None,
        members: Vec::new(),
        responder: crate::ports::types::ResponderMode::default(),
        hive: Default::default(),
    });
    for spelling in ["ops_desk", "Operations"] {
        assert_eq!(
            desk_aliases(&record, Some(spelling)),
            ("ops_desk".to_string(), "Operations".to_string()),
            "{spelling:?} is the console-created desk"
        );
    }
}

/// An exact id beats another desk's display name.
///
/// Desk creation enforces unique ids but **not** unique names, so
/// `{id: "ops_desk", name: "sales"}` is valid and can sit ahead of
/// `{id: "sales", …}`. A single pass matching `id == key || name == key`
/// answers with whichever came first, so asking for the desk `sales` got
/// `ops_desk` — and since this returns a *pair*, the damage is worse than a
/// miss: `owns` would be handed one desk's id and another's name, merging
/// two conversations that have nothing to do with each other.
///
/// The precedence itself is `CompanyRecord::resolve_desk_id`'s, which this
/// now defers to rather than keeping a second, laxer copy of.
#[test]
fn an_exact_id_wins_over_an_earlier_desks_display_name() {
    let mut record = record_with_group_chat("ops_desk", "sales");
    record
        .manifest
        .group_chats
        .push(toml::from_str("id = \"sales\"\nname = \"Sales\"").expect("a desk"));
    assert_eq!(
        desk_aliases(&record, Some("sales")).0,
        "sales",
        "the desk whose id is `sales` owns that key"
    );
}

#[tokio::test]
async fn resolve_none_is_the_general_desk() {
    assert_eq!(
        resolve(RecordStore(None), None).await,
        (GENERAL_DESK.to_string(), GENERAL_DESK.to_string())
    );
}

#[tokio::test]
async fn resolve_general_spelling_short_circuits_without_a_store_read() {
    // The store would panic on `save`/`list`, but a General spelling must not
    // even reach `load` — it returns `(chat, chat)`, which owns folds.
    assert_eq!(
        resolve(RecordStore(None), Some("main")).await,
        ("main".to_string(), "main".to_string())
    );
}

#[tokio::test]
async fn resolve_named_desk_by_id_returns_the_manifest_name() {
    // Addressed by id; the seed must carry the name too, or a line journaled
    // under the name would be missed. This is the exact "looks fixed but seeds
    // nothing" trap the resolution guards against.
    let store = RecordStore(Some(record_with_group_chat("eng-123", "Engineering")));
    assert_eq!(
        resolve(store, Some("eng-123")).await,
        ("eng-123".to_string(), "Engineering".to_string())
    );
}

#[tokio::test]
async fn resolve_unmatched_selector_passes_through_verbatim() {
    let store = RecordStore(Some(record_with_group_chat("eng-123", "Engineering")));
    assert_eq!(
        resolve(store, Some("ad-hoc-thread")).await,
        ("ad-hoc-thread".to_string(), "ad-hoc-thread".to_string())
    );
}

/// A DM resolves to **both** spellings it is journaled under.
///
/// The two are not interchangeable in the journal and both are correct: the
/// console posts an ordinary teammate's DM under the bare id (`dmThreadId`),
/// while a DM hive keys its episode -- and so every row that episode journals
/// -- under `dm:<id>`. A reader handed one spelling and matching only it saw
/// half its own conversation, which is what made an episode's transcript
/// vanish on reload while the company stayed blocked on an approval raised in
/// it.
///
/// The sibling rides in the **name** slot because `owns` compares either slot
/// and renders neither.
#[tokio::test]
async fn a_dm_resolves_to_both_spellings_it_is_journaled_under() {
    let store = RecordStore(Some(record_with_group_chat("eng-123", "Engineering")));
    assert_eq!(
        resolve(store, Some("ceo")).await,
        ("ceo".to_string(), "dm:ceo".to_string()),
        "asked bare, it still owns the rows its episode journaled prefixed"
    );

    let store = RecordStore(Some(record_with_group_chat("eng-123", "Engineering")));
    assert_eq!(
        resolve(store, Some("dm:ceo")).await,
        ("dm:ceo".to_string(), "ceo".to_string()),
        "and asked prefixed, it still owns what the console posted bare"
    );
}

/// The sibling is a teammate's, and nobody else's.
#[test]
fn only_a_roster_teammate_has_a_sibling_spelling() {
    let record = record_with_group_chat("eng-123", "Engineering");

    assert_eq!(dm_sibling(&record, "ceo").as_deref(), Some("dm:ceo"));
    assert_eq!(dm_sibling(&record, "dm:ceo").as_deref(), Some("ceo"));
    assert_eq!(
        dm_sibling(&record, "ad-hoc-thread"),
        None,
        "an ad-hoc thread owns its exact string and nothing else"
    );
    assert_eq!(
        dm_sibling(&record, "eng-123"),
        None,
        "a desk is not a DM, so it grows no second key"
    );
    assert_eq!(
        dm_sibling(&record, "dm:nobody"),
        None,
        "and a prefixed key naming no teammate folds to nothing"
    );
}

/// A desk that shares a teammate's id keeps its own rows.
///
/// Both halves matter, and the second is the one that bit. A **manifest** desk
/// is declined by `dm_sibling` because `resolve_desk_id` claims it; an
/// **overlay** desk -- created from the console, absent from the manifest --
/// is claimed by `resolve_desk_id` too, but `operator::resolve_desk` matches
/// `manifest.group_chats` alone and so hands this an id that looks unmatched.
/// Without the guard inside `dm_sibling` that desk's transcript would have
/// taken the teammate's DM rows (tinysweeper on #2484).
#[test]
fn a_desk_sharing_a_teammates_id_grows_no_dm_sibling() {
    let mut record = record_with_group_chat("ceo", "Chief's desk");
    assert_eq!(
        dm_sibling(&record, "ceo"),
        None,
        "a manifest desk owns the key outright, teammate of the same name or not"
    );

    let mut overlay = record_with_group_chat("growth_desk", "Growth");
    overlay
        .overlay_desks
        .push(crate::ports::types::OverlayDesk {
            id: "ceo".to_string(),
            name: "Chief's overlay".to_string(),
            description: None,
            members: Vec::new(),
            responder: crate::ports::types::ResponderMode::default(),
            hive: Default::default(),
        });
    assert_eq!(
        dm_sibling(&overlay, "ceo"),
        None,
        "and so does a desk the console created, which the manifest never names"
    );

    record.overlay_desks.clear();
}

/// A teammate whose id carries the prefix is resolved exactly, not stripped.
///
/// Nothing forbids a teammate called `dm:ceo`, and this module already carries
/// the mirror case of one named for a General spelling. Stripping first would
/// answer that key with `ceo`'s sibling, handing one teammate's DM the other's
/// rows.
#[test]
fn an_exact_teammate_id_beats_the_prefix() {
    let manifest: crate::company::CompanyManifest = toml::from_str(
        r#"
[company]
name = "Acme"

[[agent]]
id = "ceo"
role = "Chief Executive"

[[agent]]
id = "dm:ceo"
role = "Impostor"
"#,
    )
    .expect("valid manifest");
    let mut record = record_with_group_chat("growth_desk", "Growth");
    record.manifest = manifest;

    assert_eq!(
        dm_sibling(&record, "dm:ceo").as_deref(),
        Some("dm:dm:ceo"),
        "the teammate literally called `dm:ceo` gets its OWN prefixed line, \
         not the one belonging to `ceo`"
    );
    assert_eq!(
        dm_sibling(&record, "ceo").as_deref(),
        Some("dm:ceo"),
        "and `ceo` still resolves to its own"
    );
}
