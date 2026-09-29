use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

static DB_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Whether a missing server must FAIL rather than skip. Issue #555.
///
/// The `OPENCOMPANY_TEST_MONGODB_URI` skip above is right for a laptop with
/// no MongoDB — it keeps a default `cargo test` offline — and wrong for the
/// CI lane whose entire purpose is running this suite. There, an unset URI
/// is a misconfigured job, and the skip would report it as a pass: the
/// whole suite silently absent behind a green tick, which is the exact
/// defect this lane was added to fix, reintroduced one layer down.
///
/// So CI sets this second variable and nothing else does. Set = the caller
/// has promised a reachable server, so not finding one is an error.
///
/// `0` and the empty string read as unset, so the variable can be threaded
/// through a workflow matrix or a shell wrapper that always defines it.
fn required() -> bool {
    std::env::var("OPENCOMPANY_TEST_MONGODB_REQUIRED")
        .is_ok_and(|value| !value.is_empty() && value != "0")
}

/// Issue #697. The partial filter must be keyed on the **field name passed
/// in**, never on the literal `"present"` — the parameter's own name.
///
/// Raised in review of #733 as a HIGH finding: that `doc!` stringifies
/// identifier keys, so `doc! {present: ...}` would build
/// `{"present": {"$exists": true}}`, a filter no document matches. The
/// index would then be built over an empty set and reject no insert,
/// silently removing the one-file-per-path guarantee on this backend.
///
/// It does not: the `bson!` key arms end at
/// `insert::<_, Bson>(($($key)+), $value)`, passing the key tokens as an
/// expression rather than through `stringify!`. But that is an argument
/// about a macro's expansion, and the cost of being wrong is a guard that
/// looks present and enforces nothing — so this asserts the built artifact
/// instead of the reasoning.
///
/// Needs no server: it inspects the `IndexModel` this code constructs, so
/// it runs on the `mongodb` feature alone and cannot pass vacuously the way
/// the URI-gated tests can.
#[test]
fn the_partial_filter_is_keyed_on_the_field_not_the_parameter_name() {
    let model = unique_partial(doc! {"company_id": 1, "file_path_key": 1}, "file_path_key");
    let filter = model
        .options
        .as_ref()
        .and_then(|options| options.partial_filter_expression.as_ref())
        .expect("the index is partial");

    assert!(
        filter.contains_key("file_path_key"),
        "the filter must name the field it guards: {filter:?}"
    );
    assert!(
        !filter.contains_key("present"),
        "a filter keyed on the parameter's own name would match no document, so the \
             index would be built over an empty set and reject nothing: {filter:?}"
    );
    assert_eq!(
        filter.get_document("file_path_key").expect("the condition"),
        &doc! {"$exists": true},
        "and the condition is existence of that field: {filter:?}"
    );
    assert!(
        model
            .options
            .as_ref()
            .and_then(|options| options.unique)
            .unwrap_or(false),
        "a partial filter without uniqueness would guard nothing at all"
    );
}

/// Issue #759's index, asserted the same way and for the same reason.
///
/// The folder guard is a second `unique_partial`, and a partial filter that
/// named the wrong field would be the identical silent failure: an index
/// built over an empty set, rejecting nothing, while every sequential test
/// still passed. Asserting the constructed `IndexModel` catches that with no
/// server, so it cannot pass vacuously.
///
/// It also pins the field **name**: the folder key must be its own field,
/// not `file_path_key`. Sharing one field would make a folder and a file
/// contend for a single name — a new tree rule this change explicitly does
/// not introduce.
#[test]
fn the_folder_claim_index_is_partial_unique_on_its_own_field() {
    let model = unique_partial(
        doc! {"company_id": 1, "folder_path_key": 1},
        "folder_path_key",
    );
    let filter = model
        .options
        .as_ref()
        .and_then(|options| options.partial_filter_expression.as_ref())
        .expect("the index is partial");

    assert!(
        filter.contains_key("folder_path_key"),
        "the filter must name the field it guards: {filter:?}"
    );
    assert!(
        !filter.contains_key("file_path_key"),
        "the folder guard must not key on the file field, or a folder and a note would \
             contend for one name: {filter:?}"
    );
    assert_eq!(
        filter
            .get_document("folder_path_key")
            .expect("the condition"),
        &doc! {"$exists": true},
    );
    assert!(
        model
            .options
            .as_ref()
            .and_then(|options| options.unique)
            .unwrap_or(false),
        "a partial filter without uniqueness would guard nothing at all"
    );
    // The two keys share an encoding, which is what lets one `path_key`
    // serve both — pinned so a future edit cannot make them silently differ.
    assert_eq!(
        folder_path_key(Some("p"), "task-42"),
        file_path_key(Some("p"), "task-42")
    );
}

/// Issue #759, the subtle half: a folder that is **moved** drops its claim.
///
/// `rename_move` has to `$unset` `folder_path_key`, and a missing unset is
/// invisible until somebody needs the vacated path again. The moved
/// document would keep guarding the path it left, so the next publish that
/// wanted `agents/cmo/task-42/` would be refused by an index entry
/// describing a folder that is no longer there — the permanent outage this
/// primitive exists to prevent, reintroduced by the fix itself.
///
/// Asserted by reclaiming the old path and checking a *new* folder was
/// minted there, rather than by reading the document: the claim is only
/// worth what the next claimer observes.
#[tokio::test]
async fn a_moved_folder_releases_its_claim_on_the_path_it_left() {
    use crate::ports::workspace::WorkspaceStore;
    let Some(s) = store().await else { return };
    let company = CompanyId::new("mover");
    let origin = crate::ports::workspace::WorkspaceOrigin::Seed;

    let parent = s
        .adopt_or_create_folder(&company, None, "Agents", origin.clone())
        .await
        .expect("the root")
        .into_node()
        .id;
    let moved = s
        .adopt_or_create_folder(&company, Some(&parent), "task-42", origin.clone())
        .await
        .expect("the folder")
        .into_node()
        .id;

    // The operator renames it out of the way.
    s.rename_move(&company, &moved, Some("task-42-archived"), None)
        .await
        .expect("rename to the workspace root");

    // The vacated path must be claimable again, by a genuinely new folder.
    let reclaimed = s
        .adopt_or_create_folder(&company, Some(&parent), "task-42", origin)
        .await
        .expect("the path the moved folder left must be free");
    assert!(
        reclaimed.was_created(),
        "a stale claim would have made this adopt a folder that is not there"
    );
    assert_ne!(reclaimed.node().id, moved);

    drop_db(&s).await;
}

/// **Issue #392 through the port**: the host-durable append asks the server
/// for `j:true`, and the process-durable one does not.
///
/// `assert_journal_store` cannot catch this — a backend that ignored the
/// `Durability` argument stores and orders every record identically and
/// passes the whole suite, silently dropping the guarantee that keeps an
/// already-fired effect from firing again after a primary crash. So the
/// constructed handles are asserted directly, the same way
/// `the_partial_filter_is_keyed_on_the_field_not_the_parameter_name` asserts
/// a built `IndexModel` rather than the reasoning behind it.
///
/// Needs no server: `Client::with_options` resolves lazily and
/// `collection_with_options` builds a handle locally, so this runs on the
/// `mongodb` feature alone and cannot pass vacuously the way the URI-gated
/// tests can. (It is a `tokio::test` only because the driver's constructor
/// spawns a cleanup task, not because anything here awaits the network.)
#[tokio::test]
async fn only_the_host_durable_journal_write_asks_for_j_true() {
    // A client handle, not a connection: `with_options` resolves lazily and
    // never touches the network, so this stays a pure shape assertion.
    let client = Client::with_options(
        mongodb::options::ClientOptions::builder()
            .hosts(vec![mongodb::options::ServerAddress::Tcp {
                host: "localhost".into(),
                port: Some(27017),
            }])
            .build(),
    )
    .expect("build a client handle");
    let store = MongoStore {
        db: client.database("oc_test_shape"),
        senders: Arc::new(StdMutex::new(HashMap::new())),
    };

    let host = store.journaled(JOURNAL);
    assert_eq!(
        host.write_concern().and_then(|concern| concern.journal),
        Some(true),
        "a host-durable record must be committed to the server's journal \
             before the insert is acknowledged"
    );

    let process = store.collection(JOURNAL);
    assert!(
        process
            .write_concern()
            .and_then(|concern| concern.journal)
            .is_none(),
        "the process-durable level must NOT pay a disk flush: these are the \
             journal's highest-volume records, and losing one makes the runtime \
             re-ask rather than re-fire"
    );
}

/// The URI with any `user:password@` replaced by `***@`, for the panic
/// message below.
///
/// The unreachable-server panic names the URI so the failure says *which*
/// server it could not reach — a bare "connection refused" in a CI log is
/// most of a debugging session. But a connection string carries its
/// credentials inline, and a panic lands in the CI log, the terminal
/// scrollback and any artifact that captures either. CI points at an
/// unauthenticated localhost, so nothing leaks there; a developer pointing
/// this suite at a real cluster is the case that would, and that is exactly
/// when the message is most useful. Redacting keeps the host and port,
/// which is the part worth printing.
fn redact_credentials(uri: &str) -> String {
    let Some((scheme, rest)) = uri.split_once("://") else {
        return uri.to_string();
    };
    // Userinfo, when present, precedes the first `/` of the path — so only
    // an `@` before that boundary delimits it. A password may itself
    // contain `@`, so split at the LAST one within the authority.
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    match authority.rfind('@') {
        Some(at) => format!("{scheme}://***{}{tail}", &authority[at..]),
        None => uri.to_string(),
    }
}

#[test]
fn redaction_keeps_the_host_and_drops_the_credentials() {
    // The CI shape: nothing to redact, nothing changed.
    assert_eq!(
        redact_credentials("mongodb://localhost:27017"),
        "mongodb://localhost:27017"
    );
    // The shape that would leak.
    assert_eq!(
        redact_credentials("mongodb://user:hunter2@cluster.example:27017"),
        "mongodb://***@cluster.example:27017"
    );
    // A password containing `@` — splitting at the FIRST one would leave
    // the tail of the password in the message.
    assert_eq!(
        redact_credentials("mongodb://user:p@ss@cluster.example:27017"),
        "mongodb://***@cluster.example:27017"
    );
    // An `@` in the path or query must not be mistaken for userinfo.
    assert_eq!(
        redact_credentials("mongodb://localhost:27017/db?replicaSet=a@b"),
        "mongodb://localhost:27017/db?replicaSet=a@b"
    );
    // A credentialed URI that also carries a path keeps the path.
    assert_eq!(
        redact_credentials("mongodb+srv://u:p@host/admin?retryWrites=true"),
        "mongodb+srv://***@host/admin?retryWrites=true"
    );
    // Not a URI at all: returned untouched rather than mangled.
    assert_eq!(redact_credentials("localhost:27017"), "localhost:27017");
}

pub(super) async fn store() -> Option<Arc<MongoStore>> {
    let uri = match std::env::var("OPENCOMPANY_TEST_MONGODB_URI") {
        Ok(uri) => uri,
        Err(_) => {
            assert!(
                !required(),
                "OPENCOMPANY_TEST_MONGODB_REQUIRED is set but \
                     OPENCOMPANY_TEST_MONGODB_URI is not. This lane exists to run the \
                     MongoDB conformance suite against a real server, so a skip here is \
                     a misconfigured job rather than a pass — point the URI at the \
                     service container."
            );
            eprintln!("skipping: OPENCOMPANY_TEST_MONGODB_URI is not set");
            return None;
        }
    };
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis();
    let db = format!(
        "oc_test_{}_{}_{}",
        std::process::id(),
        nonce,
        DB_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    // `connect` creates indexes, so it round-trips to the server rather
    // than resolving lazily: an unreachable host fails HERE, after the
    // driver's server-selection timeout, instead of much later inside
    // whichever assertion happened to touch the database first.
    let store = MongoStore::connect(&uri, &db).await.unwrap_or_else(|err| {
        panic!(
            "could not reach the MongoDB server at {}: {err}",
            redact_credentials(&uri)
        )
    });
    Some(Arc::new(store))
}

/// One failing index must not stop the other forty-three from being
/// created, **nor stop the store from opening at all**.
///
/// Two properties in one test, because they have the same setup.
///
/// `ensure_indexes` runs concurrently, so a short-circuit would drop
/// in-flight driver operations mid-await — and an operation cancelled after
/// its request is sent but before its reply is read leaves a connection the
/// pool cannot safely reuse. That hazard does not exist in a sequential
/// loop; it arrived with the concurrency, so it is pinned here.
///
/// And the failure is now reported by logging rather than by refusing to
/// construct the store (#1716). Returning `Err` here took the whole process
/// down; under the microVM runtime the workload is PID 1, so that panicked
/// the guest kernel and the tenant was gone. Indexes are a performance
/// property, not a precondition for the process existing.
#[tokio::test]
async fn a_failing_index_stops_neither_the_other_indexes_nor_the_boot() {
    let Some(store) = store().await else { return };

    // `store()` has already run `ensure_indexes` once, so the assertion has
    // to be about something this run RE-creates. Drop a known index first;
    // if the pipeline short-circuits before reaching it, it stays missing.
    store
        .collection("notifications")
        .drop_index("company_id_1_id_1")
        .await
        .expect("drop the index whose return proves the run continued");

    // Induce the failure the way MongoDB actually produces one: an existing
    // index on the same keys with conflicting options. Replacing the unique
    // `owners` index with a non-unique one makes `ensure_indexes` collide.
    store
        .collection("owners")
        .drop_index("company_id_1")
        .await
        .expect("drop owners index");
    store
        .collection("owners")
        .create_index(IndexModel::builder().keys(doc! {"company_id": 1}).build())
        .await
        .expect("seed the conflicting index");

    // The store must still open. A tenant that cannot boot because one
    // index conflicts is strictly worse than one serving without it.
    store
        .ensure_indexes()
        .await
        .expect("a conflicting index must not stop the store from opening");

    // The point: the run continued past the failure and recreated the index
    // dropped above. A short-circuit would leave it absent.
    let mut names = Vec::new();
    let mut cursor = store
        .collection("notifications")
        .list_indexes()
        .await
        .expect("list notifications indexes");
    while cursor.advance().await.expect("advance") {
        if let Some(name) = cursor
            .deserialize_current()
            .expect("model")
            .options
            .and_then(|o| o.name)
        {
            names.push(name);
        }
    }
    assert!(
        names.iter().any(|n| n == "company_id_1_id_1"),
        "a failure on `owners` must not stop other indexes being created; got {names:?}"
    );

    drop_db(&store).await;
}

/// Issue #1573: the backfill copies `agentId` out of `run_json` for rows
/// written before the mirror column existed, and does so in bounded batches
/// that re-probe between passes rather than holding the whole collection.
///
/// Seeded directly into the `runs` collection with no `agent_id` field —
/// the exact shape a row predating the upgrade has — because it is not
/// reachable through the port: `create_run`/`put_run` always write the
/// mirror. The store is built as a bare struct, not through `connect`, so
/// no background backfill task shares the database with this one's
/// assertions.
#[tokio::test]
async fn backfill_fills_legacy_run_rows_in_bounded_batches() {
    let uri = match std::env::var("OPENCOMPANY_TEST_MONGODB_URI") {
        Ok(uri) => uri,
        Err(_) => {
            assert!(
                !required(),
                "OPENCOMPANY_TEST_MONGODB_REQUIRED is set but \
                     OPENCOMPANY_TEST_MONGODB_URI is not"
            );
            eprintln!("skipping: OPENCOMPANY_TEST_MONGODB_URI is not set");
            return;
        }
    };
    let client = Client::with_uri_str(&uri).await.unwrap();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis();
    let db_name = format!(
        "oc_test_{}_{}_{}",
        std::process::id(),
        nonce,
        DB_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let store = MongoStore {
        db: client.database(&db_name),
        senders: Arc::new(StdMutex::new(HashMap::new())),
    };

    let company = CompanyId::new("legacy-co");
    let runs = store.collection("runs");
    // More than one batch, so the re-probe loop is exercised — the rows past
    // the first `BACKFILL_BATCH_SIZE` must be picked up by a later pass.
    for i in 0..BACKFILL_BATCH_SIZE + 3 {
        let agent = if i % 2 == 0 { "engineer" } else { "designer" };
        let record = crate::ports::runs::RunRecord {
            id: format!("legacy-{i}"),
            company: company.clone(),
            task_id: None,
            agent_id: agent.to_string(),
            chat_id: None,
            thread_root: None,
            workflow_run_id: None,
            node_id: None,
            episode_id: None,
            round_revision: None,
            attempt: 1,
            status: crate::ports::runs::RunStatus::Succeeded,
            trigger_event_seq: None,
            created_at_millis: 1_700_000_000_000,
            started_at_millis: None,
            finished_at_millis: None,
            error: None,
            usage: Default::default(),
            step_count: 1,
        };
        runs.insert_one(doc! {
            "company_id": company.as_ref(),
            "run_id": &record.id,
            "run_json": serde_json::to_string(&record).unwrap(),
        })
        .await
        .unwrap();
    }
    // A row that already carries the mirror (written through the port after
    // the upgrade) must be neither touched nor counted.
    let fresh = crate::ports::runs::RunRecord {
        id: "fresh".to_string(),
        company: company.clone(),
        task_id: None,
        agent_id: "engineer".to_string(),
        chat_id: None,
        thread_root: None,
        workflow_run_id: None,
        node_id: None,
        episode_id: None,
        round_revision: None,
        attempt: 1,
        status: crate::ports::runs::RunStatus::Pending,
        trigger_event_seq: None,
        created_at_millis: 1_700_000_000_000,
        started_at_millis: None,
        finished_at_millis: None,
        error: None,
        usage: Default::default(),
        step_count: 0,
    };
    runs.insert_one(doc! {
        "company_id": company.as_ref(),
        "run_id": &fresh.id,
        "agent_id": "engineer",
        "status": "pending",
        "attempt": 1i64,
        "created_ms": 1_700_000_000_000i64,
        "run_json": serde_json::to_string(&fresh).unwrap(),
    })
    .await
    .unwrap();

    let filled = store.backfill_run_agent_ids().await.unwrap();
    assert_eq!(
        filled,
        BACKFILL_BATCH_SIZE + 3,
        "every legacy row is filled; the fresh row is not counted"
    );

    // The mirror landed on disk, not just in the return value — one row from
    // each batch's worth of desks.
    let migrated = runs
        .find_one(doc! {"run_id": "legacy-0"})
        .await
        .unwrap()
        .unwrap();
    assert_eq!(get_str(&migrated, "agent_id").unwrap(), "engineer");
    let later = runs
        .find_one(doc! {"run_id": "legacy-1"})
        .await
        .unwrap()
        .unwrap();
    assert_eq!(get_str(&later, "agent_id").unwrap(), "designer");

    // A second pass has nothing left to do — the `$exists: false` probe is
    // exhausted.
    assert_eq!(store.backfill_run_agent_ids().await.unwrap(), 0);

    drop_db(&store).await;
}

pub(super) async fn drop_db(store: &MongoStore) {
    let _ = store.db.drop().await;
}

/// Ages every staged blob past [`ORPHAN_BLOB_MIN_AGE_MS`] (issue #664).
///
/// The sweep only reclaims blobs old enough to be abandoned, so a test that
/// uploads bytes and reboots in the same millisecond is staging an
/// *in-flight* upload, not an orphaned one. Rewriting `uploadDate` is how a
/// test says "and then an hour passed" without sleeping for one.
pub(super) async fn age_blobs_past_the_sweep_threshold(store: &MongoStore) {
    let old =
        mongodb::bson::DateTime::from_millis(now_millis() as i64 - ORPHAN_BLOB_MIN_AGE_MS - 60_000);
    store
        .db
        .collection::<Document>(&format!("{BLOB_BUCKET}.files"))
        .update_many(doc! {}, doc! { "$set": { "uploadDate": old } })
        .await
        .expect("backdate staged blobs");
}
