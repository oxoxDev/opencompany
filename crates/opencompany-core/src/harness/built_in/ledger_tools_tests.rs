//! What the agent's five tools promise, and what they refuse.

use std::sync::Arc;

use serde_json::json;

use super::*;
use crate::ledger::Registry;
use crate::ports::types::CompanyId;
use crate::store::FsOps;

fn ctx(home: &tempfile::TempDir) -> Ledgers {
    let ops = Arc::new(FsOps::new(home.path().to_path_buf()));
    Ledgers::new(CompanyId::new("acme"), ops)
}

fn tools(ctx: &Ledgers) -> Vec<Box<dyn Tool>> {
    ledger_tools(ctx.clone(), "ceo".to_string(), None, true)
}

fn tool<'a>(tools: &'a [Box<dyn Tool>], name: &str) -> &'a dyn Tool {
    tools
        .iter()
        .find(|tool| tool.name() == name)
        .map(AsRef::as_ref)
        .unwrap_or_else(|| panic!("`{name}` is registered"))
}

fn risks() -> serde_json::Value {
    json!({
        "slug": "risks",
        "title": "Risks",
        "purpose": "What could go wrong.",
        "fields": [
            { "name": "id", "role": "id" },
            { "name": "risk", "role": "title" },
            { "name": "status", "role": "status" },
            { "name": "reason", "role": "prose" }
        ],
        "statuses": [
            { "name": "open" },
            { "name": "closed", "closed": true, "needs_reason": true }
        ]
    })
}

/// Five tools, whatever a company declares. The count must not grow per tenant:
/// the schema is built once, and a surface whose shape changes per company is
/// one no prompt can describe.
#[tokio::test]
async fn the_surface_is_five_tools_and_stays_five() {
    let home = tempfile::tempdir().unwrap();
    let ctx = ctx(&home);
    assert_eq!(tools(&ctx).len(), 5);
    ledgers::define(&ctx, &risks()).await.expect("declared");
    let built = tools(&ctx);
    let names: Vec<&str> = built.iter().map(|tool| tool.name()).collect();
    assert_eq!(names, LEDGER_TOOL_NAMES);
}

/// The one thing an agent may never do. Its absence is the enforcement — a tool
/// that is not registered cannot be reached by a model that guesses well.
#[tokio::test]
async fn there_is_no_delete_tool_and_no_retire_tool() {
    let home = tempfile::tempdir().unwrap();
    let built = tools(&ctx(&home));
    let names: Vec<&str> = built.iter().map(|tool| tool.name()).collect();
    for forbidden in [
        "delete_entry",
        "retire_ledger",
        "delete_ledger",
        "purge_ledger",
    ] {
        assert!(
            !names.contains(&forbidden),
            "`{forbidden}` must not be reachable from a turn"
        );
    }
}

#[tokio::test]
async fn listing_names_every_ledger_with_its_statuses() {
    let home = tempfile::tempdir().unwrap();
    let ctx = ctx(&home);
    let tools = tools(&ctx);
    let result = tool(&tools, LIST_LEDGERS_TOOL)
        .execute(json!({}))
        .await
        .unwrap();
    let text = format!("{result:?}");
    // The listing renders each ledger's phase vocabulary (spec.statuses), not
    // the internal stage names — since three-state ledgers these are the
    // contract a model reads, so pin them exactly.
    for (slug, statuses) in [
        ("tasks", "pending, working, done"),
        ("goals", "active, met, dropped"),
        ("decisions", "proposed, accepted, retired"),
    ] {
        assert!(text.contains(slug), "`{slug}` missing: {text}");
        assert!(
            text.contains(statuses),
            "`{slug}` statuses (`{statuses}`) are listed: {text}"
        );
    }
}

/// The discovery path a model actually follows: guess, and learn the real names
/// from the failure, in one turn, without having thought to list them first.
#[tokio::test]
async fn an_unknown_slug_answers_with_the_real_ones() {
    let home = tempfile::tempdir().unwrap();
    let ctx = ctx(&home);
    let tools = tools(&ctx);
    let result = tool(&tools, READ_LEDGER_TOOL)
        .execute(json!({ "ledger": "objectives" }))
        .await
        .unwrap();
    let text = format!("{result:?}");
    assert!(text.contains("objectives"), "{text}");
    assert!(text.contains("goals"), "{text}");
}

#[tokio::test]
async fn an_agent_declares_records_and_closes() {
    let home = tempfile::tempdir().unwrap();
    let ctx = ctx(&home);
    let tools = tools(&ctx);

    tool(&tools, DEFINE_LEDGER_TOOL)
        .execute(risks())
        .await
        .unwrap();

    tool(&tools, RECORD_ENTRY_TOOL)
        .execute(json!({
            "ledger": "risks",
            "id": "vendor-slip",
            "fields": { "risk": "the vendor misses the date", "status": "open" }
        }))
        .await
        .unwrap();

    let read = tool(&tools, READ_LEDGER_TOOL)
        .execute(json!({ "ledger": "risks" }))
        .await
        .unwrap();
    assert!(format!("{read:?}").contains("vendor-slip"));

    tool(&tools, CLOSE_ENTRY_TOOL)
        .execute(json!({
            "ledger": "risks",
            "id": "vendor-slip",
            "status": "closed",
            "reason": "they delivered on the 4th"
        }))
        .await
        .unwrap();

    // Closed, and still there with its reason. A closed row is an archive
    // entry, not a deletion.
    let read = tool(&tools, READ_LEDGER_TOOL)
        .execute(json!({ "ledger": "risks", "status": "closed" }))
        .await
        .unwrap();
    let text = format!("{read:?}");
    assert!(text.contains("vendor-slip"), "{text}");
    assert!(text.contains("delivered on the 4th"), "{text}");
}

/// The refusal has to say what is missing, or the turn spends itself guessing.
#[tokio::test]
async fn closing_without_a_reason_says_what_is_missing() {
    let home = tempfile::tempdir().unwrap();
    let ctx = ctx(&home);
    let tools = tools(&ctx);
    tool(&tools, DEFINE_LEDGER_TOOL)
        .execute(risks())
        .await
        .unwrap();
    let result = tool(&tools, RECORD_ENTRY_TOOL)
        .execute(json!({
            "ledger": "risks",
            "id": "r1",
            "fields": { "status": "closed" }
        }))
        .await
        .unwrap();
    assert!(format!("{result:?}").contains("reason"));
}

/// A JSON null clears a field — the one thing a merge cannot otherwise express.
#[tokio::test]
async fn a_null_field_clears_it() {
    let home = tempfile::tempdir().unwrap();
    let ctx = ctx(&home);
    let tools = tools(&ctx);
    tool(&tools, DEFINE_LEDGER_TOOL)
        .execute(risks())
        .await
        .unwrap();
    tool(&tools, RECORD_ENTRY_TOOL)
        .execute(json!({ "ledger": "risks", "id": "r1", "fields": { "risk": "a", "reason": "b" } }))
        .await
        .unwrap();
    tool(&tools, RECORD_ENTRY_TOOL)
        .execute(json!({ "ledger": "risks", "id": "r1", "fields": { "reason": null } }))
        .await
        .unwrap();
    let read = tool(&tools, READ_LEDGER_TOOL)
        .execute(json!({ "ledger": "risks", "entry": "r1" }))
        .await
        .unwrap();
    let text = format!("{read:?}");
    assert!(text.contains("risk: a"), "{text}");
    assert!(!text.contains("reason: b"), "{text}");
}

/// The board is readable through the surface and refuses a write, naming what
/// does write it — a refusal that sent the caller to `record_entry` would refuse
/// them a second time.
#[tokio::test]
async fn the_board_reads_here_and_refuses_a_write_with_a_usable_remedy() {
    let home = tempfile::tempdir().unwrap();
    let ctx = ctx(&home);
    let tools = tools(&ctx);
    tool(&tools, READ_LEDGER_TOOL)
        .execute(json!({ "ledger": "tasks" }))
        .await
        .unwrap();
    let result = tool(&tools, RECORD_ENTRY_TOOL)
        .execute(json!({ "ledger": "tasks", "id": "t1", "fields": { "title": "x" } }))
        .await
        .unwrap();
    let text = format!("{result:?}");
    assert!(text.contains("spawn_task"), "{text}");
    assert!(!text.contains("Recorded"), "{text}");
}

/// A short list that reads as complete is worse than a long one: the reader
/// concludes there is nothing more and re-proposes what was cut.
#[tokio::test]
async fn a_bounded_read_says_it_is_bounded() {
    let home = tempfile::tempdir().unwrap();
    let ctx = ctx(&home);
    let tools = tools(&ctx);
    tool(&tools, DEFINE_LEDGER_TOOL)
        .execute(risks())
        .await
        .unwrap();
    for n in 0..40 {
        tool(&tools, RECORD_ENTRY_TOOL)
            .execute(json!({
                "ledger": "risks",
                "id": format!("r{n}"),
                "fields": { "risk": "a", "status": "open" }
            }))
            .await
            .unwrap();
    }
    let read = tool(&tools, READ_LEDGER_TOOL)
        .execute(json!({ "ledger": "risks", "limit": 5 }))
        .await
        .unwrap();
    let text = format!("{read:?}");
    assert!(text.contains("5 of 40 shown"), "{text}");
    assert!(text.contains("not gone"), "{text}");
}

/// Every write is a consequence the policy gate can park; the two reads are
/// free. A read behind an approval would make an agent guess rather than look.
#[test]
fn reads_are_free_and_writes_are_consequences() {
    let home = tempfile::tempdir().unwrap();
    let tools = tools(&ctx(&home));
    for (name, level) in [
        (LIST_LEDGERS_TOOL, PermissionLevel::ReadOnly),
        (READ_LEDGER_TOOL, PermissionLevel::ReadOnly),
        (RECORD_ENTRY_TOOL, PermissionLevel::Write),
        (CLOSE_ENTRY_TOOL, PermissionLevel::Write),
        (DEFINE_LEDGER_TOOL, PermissionLevel::Write),
    ] {
        assert_eq!(
            tool(&tools, name).permission_level(),
            level,
            "`{name}` is at the wrong permission level"
        );
    }
}

/// The catalogue, not a pointer to one: a tool granted, unmentioned and never
/// called is the observed failure mode.
#[test]
fn the_brief_names_every_ledger_and_says_deletion_is_not_available() {
    let brief = ledger_brief(&Registry::build([]));
    for slug in ["tasks", "goals", "decisions"] {
        assert!(brief.contains(slug), "`{slug}` missing: {brief}");
    }
    assert!(brief.contains("read_ledger"), "{brief}");
    assert!(brief.contains("record_entry"), "{brief}");
    assert!(brief.contains("cannot delete"), "{brief}");
    // The board's line has to say it is not written here.
    assert!(brief.contains("read-only here"), "{brief}");
}

/// Every tool's schema must parse as a schema and require what it needs, or a
/// model silently sends a call the host cannot answer.
#[test]
fn every_schema_requires_the_ledger_argument() {
    let home = tempfile::tempdir().unwrap();
    let tools = tools(&ctx(&home));
    for name in [READ_LEDGER_TOOL, RECORD_ENTRY_TOOL, CLOSE_ENTRY_TOOL] {
        let schema = tool(&tools, name).parameters_schema();
        let required: Vec<&str> = schema["required"]
            .as_array()
            .expect("required")
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect();
        assert!(required.contains(&"ledger"), "`{name}`: {schema}");
    }
}

// -- per-agent ledger grants (`[[agent]].ledgers`) ---------------------------

/// An omitted `ledgers` grant is unrestricted: `list_ledgers` names every
/// ledger, matching the tool surface before this field existed.
#[tokio::test]
async fn an_unscoped_agent_lists_every_ledger() {
    let home = tempfile::tempdir().unwrap();
    let ctx = ctx(&home);
    let tools = ledger_tools(ctx, "ceo".to_string(), None, true);
    let result = tool(&tools, LIST_LEDGERS_TOOL)
        .execute(json!({}))
        .await
        .unwrap();
    let text = format!("{result:?}");
    for slug in ["tasks", "goals", "decisions"] {
        assert!(text.contains(slug), "`{slug}` missing: {text}");
    }
}

/// A declared `ledgers` grant confines `list_ledgers` to exactly what it
/// names — a ledger left off the list does not appear.
#[tokio::test]
async fn a_scoped_agent_lists_only_its_declared_ledgers() {
    let home = tempfile::tempdir().unwrap();
    let ctx = ctx(&home);
    let grants = vec![crate::company::LedgerGrant {
        name: "tasks".to_string(),
        access: crate::company::LedgerAccess::Record,
    }];
    let tools = ledger_tools(ctx, "ceo".to_string(), Some(grants), true);
    let result = tool(&tools, LIST_LEDGERS_TOOL)
        .execute(json!({}))
        .await
        .unwrap();
    let text = format!("{result:?}");
    assert!(text.contains("tasks"), "{text}");
    assert!(!text.contains("goals"), "{text}");
    assert!(!text.contains("decisions"), "{text}");
}

/// `read_ledger` on a slug this agent's grant does not name is refused —
/// visibility, not merely write access, is what the grant governs.
#[tokio::test]
async fn reading_an_undeclared_ledger_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let ctx = ctx(&home);
    let grants = vec![crate::company::LedgerGrant {
        name: "tasks".to_string(),
        access: crate::company::LedgerAccess::Record,
    }];
    let tools = ledger_tools(ctx, "ceo".to_string(), Some(grants), true);
    let result = tool(&tools, READ_LEDGER_TOOL)
        .execute(json!({ "ledger": "goals" }))
        .await
        .unwrap();
    assert!(result.is_error);
    let text = format!("{result:?}");
    assert!(text.contains("goals"), "{text}");
}

/// A `read`-only grant answers `read_ledger` but refuses `record_entry`.
#[tokio::test]
async fn a_read_only_grant_refuses_record_entry() {
    let home = tempfile::tempdir().unwrap();
    let ctx = ctx(&home);
    let grants = vec![crate::company::LedgerGrant {
        name: "goals".to_string(),
        access: crate::company::LedgerAccess::Read,
    }];
    let tools = ledger_tools(ctx, "ceo".to_string(), Some(grants), true);

    let read = tool(&tools, READ_LEDGER_TOOL)
        .execute(json!({ "ledger": "goals" }))
        .await
        .unwrap();
    assert!(!read.is_error, "{read:?}");

    let record = tool(&tools, RECORD_ENTRY_TOOL)
        .execute(json!({
            "ledger": "goals",
            "id": "grow-mrr",
            "fields": { "goal": "grow MRR", "status": "open" }
        }))
        .await
        .unwrap();
    assert!(record.is_error);
    assert!(format!("{record:?}").contains("record"));
}

/// A store whose `list_specs` always fails, so `ledgers::registry` never
/// resolves.
struct FailingRegistryStore;

#[async_trait::async_trait]
impl crate::ports::ledgers::LedgerStore for FailingRegistryStore {
    async fn list_specs(&self, _company: &CompanyId) -> crate::Result<Vec<LedgerSpec>> {
        Err(crate::error::OpenCompanyError::Store("boom".into()))
    }
    async fn put_spec(&self, _company: &CompanyId, _spec: &LedgerSpec) -> crate::Result<()> {
        unreachable!("list_ledgers only lists")
    }
    async fn delete_spec(&self, _company: &CompanyId, _slug: &str) -> crate::Result<bool> {
        unreachable!("list_ledgers only lists")
    }
    async fn append(
        &self,
        _company: &CompanyId,
        _event: &crate::ledger::LedgerEvent,
    ) -> crate::Result<()> {
        unreachable!("list_ledgers only lists")
    }
    async fn events(
        &self,
        _company: &CompanyId,
        _ledger: &str,
    ) -> crate::Result<Vec<crate::ledger::LedgerEvent>> {
        unreachable!("list_ledgers only lists")
    }
    async fn purge_entry(
        &self,
        _company: &CompanyId,
        _ledger: &str,
        _entry: &str,
    ) -> crate::Result<bool> {
        unreachable!("list_ledgers only lists")
    }
    async fn purge_ledger(&self, _company: &CompanyId, _ledger: &str) -> crate::Result<bool> {
        unreachable!("list_ledgers only lists")
    }
}

/// `list_ledgers` reads the registry before anything else. When the store
/// cannot answer `list_specs`, the tool must say so rather than report an
/// empty or partial board.
#[tokio::test]
async fn a_registry_read_failure_is_reported_not_shown_as_an_empty_board() {
    let store: Arc<dyn crate::ports::ledgers::LedgerStore> = Arc::new(FailingRegistryStore);
    let ctx = Ledgers::new(CompanyId::new("acme"), store);
    let tools = tools(&ctx);
    let out = tool(&tools, LIST_LEDGERS_TOOL)
        .execute(json!({}))
        .await
        .unwrap();
    assert!(
        out.is_error,
        "a failed registry read must not render as a (misleadingly empty) board: {out:?}"
    );
    assert!(
        format!("{out:?}").contains("Could not read this company's ledgers"),
        "{out:?}"
    );
}

/// A store whose `list_specs` succeeds (so the built-ins still resolve) but
/// whose `events` always fails, isolating a mid-read backend failure from the
/// registry lookup that precedes it.
struct FailingEventsStore;

#[async_trait::async_trait]
impl crate::ports::ledgers::LedgerStore for FailingEventsStore {
    async fn list_specs(&self, _company: &CompanyId) -> crate::Result<Vec<LedgerSpec>> {
        Ok(Vec::new())
    }
    async fn put_spec(&self, _company: &CompanyId, _spec: &LedgerSpec) -> crate::Result<()> {
        unreachable!("read_ledger only reads")
    }
    async fn delete_spec(&self, _company: &CompanyId, _slug: &str) -> crate::Result<bool> {
        unreachable!("read_ledger only reads")
    }
    async fn append(
        &self,
        _company: &CompanyId,
        _event: &crate::ledger::LedgerEvent,
    ) -> crate::Result<()> {
        unreachable!("read_ledger only reads")
    }
    async fn events(
        &self,
        _company: &CompanyId,
        _ledger: &str,
    ) -> crate::Result<Vec<crate::ledger::LedgerEvent>> {
        Err(crate::error::OpenCompanyError::Store("boom".into()))
    }
    async fn purge_entry(
        &self,
        _company: &CompanyId,
        _ledger: &str,
        _entry: &str,
    ) -> crate::Result<bool> {
        unreachable!("read_ledger only reads")
    }
    async fn purge_ledger(&self, _company: &CompanyId, _ledger: &str) -> crate::Result<bool> {
        unreachable!("read_ledger only reads")
    }
}

/// `read_ledger` on a built-in, `LedgerSource::Events`-backed ledger (`goals`)
/// must surface a backend failure as a refusal, not as a bounded-but-empty
/// page indistinguishable from "this ledger truly has nothing yet".
#[tokio::test]
async fn a_backend_read_failure_is_reported_not_shown_as_a_bounded_empty_page() {
    let store: Arc<dyn crate::ports::ledgers::LedgerStore> = Arc::new(FailingEventsStore);
    let ctx = Ledgers::new(CompanyId::new("acme"), store);
    let tools = tools(&ctx);
    let out = tool(&tools, READ_LEDGER_TOOL)
        .execute(json!({ "ledger": "goals" }))
        .await
        .unwrap();
    assert!(
        out.is_error,
        "a store failure mid-read must not render as an empty, fully-read ledger: {out:?}"
    );
    assert!(format!("{out:?}").contains("boom"), "{out:?}");
}

/// `can_declare_ledgers = false` refuses `define_ledger` outright, whatever
/// the manifest's other ledger grants say.
#[tokio::test]
async fn can_declare_ledgers_false_refuses_define_ledger() {
    let home = tempfile::tempdir().unwrap();
    let ctx = ctx(&home);
    let tools = ledger_tools(ctx, "ceo".to_string(), None, false);
    let result = tool(&tools, DEFINE_LEDGER_TOOL)
        .execute(risks())
        .await
        .unwrap();
    assert!(result.is_error);
    assert!(format!("{result:?}").contains("can_declare_ledgers"));
}

/// A store that answers every read from a real one and refuses every write.
///
/// The read failures above prove a read that cannot answer is not rendered as
/// an empty ledger. This is the other direction: the tools' three write paths
/// all reach the store *after* their own checks have passed, so a store that
/// refuses at that point is the only remaining way a write fails — and the
/// receipt the model reads is written by this layer, not by the store.
struct RefusingWriteStore {
    inner: Arc<dyn crate::ports::ledgers::LedgerStore>,
}

#[async_trait]
impl crate::ports::ledgers::LedgerStore for RefusingWriteStore {
    async fn list_specs(&self, company: &CompanyId) -> crate::Result<Vec<LedgerSpec>> {
        self.inner.list_specs(company).await
    }

    async fn put_spec(&self, _company: &CompanyId, _spec: &LedgerSpec) -> crate::Result<()> {
        Err(crate::error::OpenCompanyError::Store("disk is full".into()))
    }

    async fn delete_spec(&self, company: &CompanyId, slug: &str) -> crate::Result<bool> {
        self.inner.delete_spec(company, slug).await
    }

    async fn append(
        &self,
        _company: &CompanyId,
        _event: &crate::ledger::LedgerEvent,
    ) -> crate::Result<()> {
        Err(crate::error::OpenCompanyError::Store("disk is full".into()))
    }

    async fn events(
        &self,
        company: &CompanyId,
        ledger: &str,
    ) -> crate::Result<Vec<crate::ledger::LedgerEvent>> {
        self.inner.events(company, ledger).await
    }

    async fn purge_entry(
        &self,
        company: &CompanyId,
        ledger: &str,
        entry: &str,
    ) -> crate::Result<bool> {
        self.inner.purge_entry(company, ledger, entry).await
    }

    async fn purge_ledger(&self, company: &CompanyId, ledger: &str) -> crate::Result<bool> {
        self.inner.purge_ledger(company, ledger).await
    }
}

/// A write the store refuses must reach the model as a refusal that names the
/// reason — never as the success sentence the happy path returns. A turn told
/// "Recorded" stops carrying the fact it was recording, and the row is not
/// there.
#[tokio::test]
async fn a_write_the_store_refuses_is_reported_rather_than_receipted_as_recorded() {
    let home = tempfile::tempdir().unwrap();
    let real = ctx(&home);
    tool(&tools(&real), DEFINE_LEDGER_TOOL)
        .execute(risks())
        .await
        .unwrap();

    let store: Arc<dyn crate::ports::ledgers::LedgerStore> = Arc::new(RefusingWriteStore {
        inner: Arc::new(FsOps::new(home.path().to_path_buf())),
    });
    let ctx = Ledgers::new(CompanyId::new("acme"), store);
    let tools = tools(&ctx);

    let recorded = tool(&tools, RECORD_ENTRY_TOOL)
        .execute(json!({
            "ledger": "risks",
            "id": "vendor-slip",
            "fields": { "risk": "the vendor misses the date", "status": "open" }
        }))
        .await
        .unwrap();
    assert!(
        recorded.is_error,
        "an append the store refused must not read as a recorded row: {recorded:?}"
    );
    assert!(
        format!("{recorded:?}").contains("disk is full"),
        "{recorded:?}"
    );

    let closed = tool(&tools, CLOSE_ENTRY_TOOL)
        .execute(json!({
            "ledger": "risks",
            "id": "vendor-slip",
            "status": "closed",
            "reason": "they delivered on the 4th"
        }))
        .await
        .unwrap();
    assert!(
        closed.is_error,
        "a close whose append never landed must not read as closed: {closed:?}"
    );

    let mut second = risks();
    second["slug"] = json!("hazards");
    second["title"] = json!("Hazards");
    let declared = tool(&tools, DEFINE_LEDGER_TOOL)
        .execute(second)
        .await
        .unwrap();
    assert!(
        declared.is_error,
        "a spec the store refused to persist must not read as a declared ledger: {declared:?}"
    );

    let listed = tool(&tools, LIST_LEDGERS_TOOL)
        .execute(json!({}))
        .await
        .unwrap();
    assert!(
        !format!("{listed:?}").contains("hazards"),
        "nothing the store refused may show up on the board: {listed:?}"
    );
}

/// The catalogue's board line is the exact string an episode seat looks for,
/// and what replaces it names no verb that seat lacks.
///
/// `seat_persona` swaps this by `find()` on the rendered substring. That is
/// only safe while `ledger_brief` renders it through the same function, so
/// the first assertion is the contract: reformat the line there and the swap
/// stops matching, silently, and the seat is handed `spawn_task` again.
#[test]
fn the_boards_catalogue_line_is_replaceable_and_its_episode_form_promises_nothing() {
    let registry = Registry::build([]);
    let tasks = registry
        .specs()
        .iter()
        .find(|spec| spec.slug == "tasks")
        .expect("every company keeps a board");

    let standing = written_by_note(tasks);
    assert!(
        ledger_brief(&registry).contains(&standing),
        "the brief must render the board's line through `written_by_note`, or the episode \
         seat's swap silently stops matching: {standing}"
    );
    assert!(
        standing.contains("spawn_task"),
        "the standing line names the verbs that really do write the board: {standing}"
    );

    let episode = episode_written_by_note(crate::hive::host::TOOL_PREFIX);
    for withheld in crate::harness::built_in::EPISODE_WITHHELD_TOOLS {
        assert!(
            !episode.contains(withheld),
            "`{withheld}` is off an episode seat's belt, so its catalogue must not name it: \
             {episode}"
        );
    }
    assert!(
        episode.contains("desk_ask"),
        "and it must name the verb that does work here, prefixed as the belt carries it: \
         {episode}"
    );

    // **And it must not deny a verb the seat still has.**
    //
    // `assign_task` is the orchestrator's and is not withheld, while this
    // swap runs for every episode seat including that one. A sentence saying
    // the card verbs are gone would tell the orchestrator it cannot hand a
    // card over when it can -- the same defect, pointed the other way, and
    // the withheld-direction assertions above cannot see it.
    assert!(
        !episode.contains("hand"),
        "the note claims only that opening a card is gone, never handing one over: {episode}"
    );
    assert!(
        !episode.contains("assign_task") && !episode.contains("desk_assign_task"),
        "`assign_task` survives an episode on the orchestrator's belt: {episode}"
    );
}
