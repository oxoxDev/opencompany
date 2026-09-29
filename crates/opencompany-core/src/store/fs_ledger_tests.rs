use super::tests::tmp_root;
use super::tests_company_store::sample_manifest;
use super::*;
use futures::StreamExt;

#[tokio::test]
async fn append_ledger_grows_without_rewrite() {
    let root_dir = tmp_root();
    let root = root_dir.path().to_path_buf();
    let store = FsCompanyStore::new(&root);
    let id = CompanyId::new("acme");
    store
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            overlay_desk_hive: Vec::new(),
            id: id.clone(),
            manifest: sample_manifest(),
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
        })
        .await
        .unwrap();

    for i in 0..3 {
        store
            .append_ledger(
                &id,
                LedgerEntry {
                    at_millis: now_millis(),
                    kind: "inference.spend".to_string(),
                    amount_usd: i as f64,
                    memo: format!("entry {i}"),
                },
            )
            .await
            .unwrap();
    }
    let loaded = store.load(&id).await.unwrap().unwrap();
    assert_eq!(loaded.ledger.len(), 3);
    assert_eq!(loaded.ledger[2].memo, "entry 2");
}

/// A company with a damaged ledger still boots, and the damage is
/// quarantined rather than deleted (issue #387).
///
/// Before this, `read_jsonl` parsed inside its loop, so the first bad line
/// returned `Err` from `FsCompanyStore::load`, the builder propagated it,
/// and one torn accounting line made the company unbootable — with the
/// console that would repair it sitting behind the boot it killed.
///
/// Four lines, two of them damaged in the two ways a torn write actually
/// produces: JSON that stops mid-value, and a byte sequence that is not
/// UTF-8 at all. The second is the reason this reads bytes rather than a
/// `String`: a whole-file decode would fail on that one byte and lose all
/// four lines to damage confined to one.
#[tokio::test]
async fn a_damaged_ledger_line_is_skipped_and_left_on_disk() {
    let root_dir = tmp_root();
    let root = root_dir.path().to_path_buf();
    let store = FsCompanyStore::new(&root);
    let id = CompanyId::new("acme");
    store
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            overlay_desk_hive: Vec::new(),
            id: id.clone(),
            manifest: sample_manifest(),
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
        })
        .await
        .unwrap();

    // The memo text the report must never echo. Distinctive enough that a
    // substring search cannot pass by accident.
    const MEMO_TWO: &str = "acquire-northwind-holdings";
    const MEMO_THREE: &str = "settle-quarterly-invoice";

    let mut bytes: Vec<u8> = Vec::new();
    // 1. Intact.
    bytes.extend_from_slice(
        br#"{"at_millis":1,"kind":"inference.spend","amount_usd":1.0,"memo":"first entry"}"#,
    );
    bytes.push(b'\n');
    // 2. A torn write: the JSON stops in the middle of the memo string.
    bytes.extend_from_slice(
        format!(r#"{{"at_millis":2,"kind":"inference.spend","amount_usd":2.0,"memo":"{MEMO_TWO}"#)
            .as_bytes(),
    );
    bytes.push(b'\n');
    // 3. Invalid UTF-8 in a structural position, so the lossy U+FFFD lands
    //    where a key is expected and the line cannot parse. (Damage *inside*
    //    a string would decode to a valid — if mangled — record, which is
    //    the recoverable case and deliberately not skipped.)
    bytes.extend_from_slice(br#"{"at_millis":3,"#);
    bytes.push(0xFF);
    bytes.extend_from_slice(
        format!(r#""kind":"inference.spend","amount_usd":3.0,"memo":"{MEMO_THREE}"}}"#).as_bytes(),
    );
    bytes.push(b'\n');
    // 4. Intact.
    bytes.extend_from_slice(
        br#"{"at_millis":4,"kind":"inference.spend","amount_usd":4.0,"memo":"fourth entry"}"#,
    );
    bytes.push(b'\n');

    let path = Bundle::new(root.clone(), &id).ledger_jsonl();
    tokio::fs::write(&path, &bytes).await.unwrap();

    // The company boots, carrying the entries that survived.
    let loaded = store
        .load(&id)
        .await
        .expect("a damaged ledger line must not fail the load")
        .expect("the company exists");
    assert_eq!(
        loaded.ledger.len(),
        2,
        "the two intact entries load; the two damaged ones are skipped"
    );
    assert_eq!(loaded.ledger[0].memo, "first entry");
    assert_eq!(loaded.ledger[1].memo, "fourth entry");

    // The report locates the damage without quoting it.
    let (entries, skipped) = read_jsonl_lenient::<LedgerEntry>(&path).await.unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(
        skipped.iter().map(|s| s.line).collect::<Vec<_>>(),
        vec![2, 3],
        "the report names the 1-based line numbers of the damaged lines"
    );
    for entry in &skipped {
        assert!(
            entry.bytes > 0,
            "the report carries the on-disk line length"
        );
        assert!(
            !entry.message.is_empty(),
            "the report says what was rejected"
        );
        for memo in [MEMO_TWO, MEMO_THREE] {
            assert!(
                !entry.message.contains(memo),
                "a memo is free text and must never reach the report: {:?}",
                entry.message
            );
        }
    }

    // Quarantine, not repair: the file is untouched, so an operator can
    // still recover the damaged lines by hand.
    let after = tokio::fs::read(&path).await.unwrap();
    assert_eq!(
        after, bytes,
        "loading must not rewrite the ledger — skipping a line the reader \
             could not parse must never become deleting it"
    );
}

#[tokio::test]
async fn event_log_assigns_monotonic_seqs_and_resumes() {
    let root_dir = tmp_root();
    let root = root_dir.path().to_path_buf();
    let log = FsEventLog::new(&root);
    let id = CompanyId::new("acme");

    let s0 = log
        .append(
            &id,
            CompanyEvent::OperatorMessage {
                mentions: Vec::new(),
                parent: None,
                text: "a".into(),
                by: None,
                chat: None,
                deliverable: None,
                attachments: Vec::new(),
            },
        )
        .await
        .unwrap();
    let s1 = log
        .append(
            &id,
            CompanyEvent::OperatorMessage {
                mentions: Vec::new(),
                parent: None,
                text: "b".into(),
                by: None,
                chat: None,
                deliverable: None,
                attachments: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(s0, EventSeq::new(0));
    assert_eq!(s1, EventSeq::new(1));

    let from_start = log.read_from(&id, EventSeq::new(0), 10).await.unwrap();
    assert_eq!(from_start.len(), 2);
    let from_one = log.read_from(&id, EventSeq::new(1), 10).await.unwrap();
    assert_eq!(from_one.len(), 1);
    assert_eq!(from_one[0].seq, EventSeq::new(1));
}

#[tokio::test]
async fn event_log_subscribe_delivers_new_event() {
    let root_dir = tmp_root();
    let root = root_dir.path().to_path_buf();
    let log = FsEventLog::new(&root);
    let id = CompanyId::new("acme");
    let mut stream = log.subscribe(&id);

    log.append(
        &id,
        CompanyEvent::OperatorMessage {
            mentions: Vec::new(),
            parent: None,
            text: "hi".into(),
            by: None,
            chat: None,
            deliverable: None,
            attachments: Vec::new(),
        },
    )
    .await
    .unwrap();
    let received = stream.next().await.expect("event delivered");
    let EventStreamItem::Event(received) = received else {
        panic!("subscription unexpectedly reported a gap");
    };
    assert_eq!(
        received.event,
        CompanyEvent::OperatorMessage {
            mentions: Vec::new(),
            parent: None,
            text: "hi".into(),
            by: None,
            chat: None,
            deliverable: None,
            attachments: Vec::new(),
        }
    );
}

#[tokio::test]
async fn memory_store_traces_tail_and_evict() {
    let root_dir = tmp_root();
    let root = root_dir.path().to_path_buf();
    let mem = FsMemoryStore::new(&root);
    let id = CompanyId::new("acme");
    for i in 0..5 {
        mem.save_trace(&id, CompressedTrace::now(format!("c{i}"), format!("s{i}")))
            .await
            .unwrap();
    }
    let recent = mem.recent_traces(&id, 2).await.unwrap();
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[1].cycle_id, "c4");

    let removed = mem
        .evict(&id, EvictionPolicy::KeepRecent { n: 1 })
        .await
        .unwrap();
    assert_eq!(removed, 4);
    assert_eq!(mem.recent_traces(&id, 10).await.unwrap().len(), 1);
}

#[tokio::test]
async fn context_store_put_peek_search() {
    let root_dir = tmp_root();
    let root = root_dir.path().to_path_buf();
    let ctx = FsContextStore::new(&root);
    let id = CompanyId::new("acme");
    let addr = ctx
        .put(
            &id,
            ContextChunk {
                label: "notes/intro".into(),
                body: "the quick brown fox jumps".into(),
            },
        )
        .await
        .unwrap();

    let full = ctx.peek(&id, &addr, None).await.unwrap();
    assert_eq!(full, "the quick brown fox jumps");
    let ranged = ctx.peek(&id, &addr, Some(4..9)).await.unwrap();
    assert_eq!(ranged, "quick");

    let listed = ctx.list(&id, "notes/").await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].label, "notes/intro");

    let hits = ctx.search(&id, "brown", 5).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].snippet.contains("brown"));
}

#[tokio::test]
async fn secret_store_isolates_companies() {
    let root_dir = tmp_root();
    let root = root_dir.path().to_path_buf();
    let secrets = FsSecretStore::new(&root);
    let a = CompanyId::new("company-a");
    let b = CompanyId::new("company-b");

    secrets
        .set(&a, "github_token", SecretValue("ghp_secret".into()))
        .await
        .unwrap();
    assert_eq!(
        secrets.get(&a, "github_token").await.unwrap(),
        Some(SecretValue("ghp_secret".into()))
    );
    // Company B cannot see company A's secret.
    assert_eq!(secrets.get(&b, "github_token").await.unwrap(), None);
}

#[tokio::test]
async fn secret_store_reads_legacy_file_and_keeps_it_after_rotation() {
    let root_dir = tmp_root();
    let root = root_dir.path().to_path_buf();
    let secrets = FsSecretStore::new(&root);
    let company = CompanyId::new("company-a");
    let key = "mcp/acme prod/auth";
    let bundle = Bundle::new(root, &company);
    bundle.ensure_dirs().await.unwrap();
    let legacy_path = bundle.legacy_secret(key);
    tokio::fs::write(&legacy_path, "old-not-a-real-token")
        .await
        .unwrap();

    assert_eq!(
        secrets.get(&company, key).await.unwrap(),
        Some(SecretValue("old-not-a-real-token".into()))
    );

    secrets
        .set(
            &company,
            key,
            SecretValue("rotated-not-a-real-token".into()),
        )
        .await
        .unwrap();

    // The legacy file is kept for a non-empty rotation: a slug may be shared
    // by several keys, so it may still hold a colliding alias's value. The
    // canonical file shadows it for this key, so `get` returns the rotated
    // value.
    assert!(tokio::fs::metadata(&legacy_path).await.is_ok());
    assert_eq!(
        secrets.get(&company, key).await.unwrap(),
        Some(SecretValue("rotated-not-a-real-token".into()))
    );
}

#[tokio::test]
async fn rotating_one_colliding_key_keeps_the_other_alias_readable() {
    // Issue #1510 migration hazard: two distinct keys can share one legacy
    // slug (`mcp/acme prod/auth` and `mcp/acme_prod/auth` both slug to
    // `mcp_acme_prod_auth`). Rotating one of them used to delete the shared
    // legacy file, so the other alias's next `get` fell through to `None`
    // even though it had been reading its own value before the upgrade.
    let root_dir = tmp_root();
    let root = root_dir.path().to_path_buf();
    let secrets = FsSecretStore::new(&root);
    let company = CompanyId::new("company-a");
    let bundle = Bundle::new(root, &company);
    bundle.ensure_dirs().await.unwrap();

    // The shared file, exactly as a pre-injective install would have left
    // it: one value for both keys, last write wins.
    let key_a = "mcp/acme prod/auth";
    let key_b = "mcp/acme_prod/auth";
    let shared = bundle.legacy_secret(key_a);
    assert_eq!(shared, bundle.legacy_secret(key_b));
    tokio::fs::write(&shared, "token-for-underscore-name")
        .await
        .unwrap();

    // Rotate only A. B's value must survive in the kept legacy file.
    secrets
        .set(&company, key_a, SecretValue("rotated-token-a".into()))
        .await
        .unwrap();
    assert_eq!(
        secrets.get(&company, key_a).await.unwrap(),
        Some(SecretValue("rotated-token-a".into()))
    );
    assert_eq!(
        secrets.get(&company, key_b).await.unwrap(),
        Some(SecretValue("token-for-underscore-name".into()))
    );
}

#[tokio::test]
async fn clearing_one_colliding_key_revokes_the_ambiguous_legacy_value() {
    let root_dir = tmp_root();
    let root = root_dir.path().to_path_buf();
    let secrets = FsSecretStore::new(&root);
    let company = CompanyId::new("company-a");
    let bundle = Bundle::new(root, &company);
    bundle.ensure_dirs().await.unwrap();

    let key_a = "mcp/acme prod/auth";
    let key_b = "mcp/acme_prod/auth";
    let shared = bundle.legacy_secret(key_a);
    assert_eq!(shared, bundle.legacy_secret(key_b));
    tokio::fs::write(&shared, "legacy-token-must-not-return")
        .await
        .unwrap();

    // Clearing A must not leave the old shared credential available to B.
    secrets
        .set(&company, key_a, SecretValue(String::new()))
        .await
        .unwrap();
    assert!(!tokio::fs::try_exists(&shared).await.unwrap());
    assert_eq!(secrets.get(&company, key_b).await.unwrap(), None);
}
#[tokio::test]
async fn canonical_namespace_does_not_bleed_into_legacy_fallback() {
    // Issue #1510's follow-up: `key-` was itself a valid legacy slug, so
    // the old canonical file for `foo` (`key-foo`) was returned when
    // reading `key-foo` through the legacy fallback, and writing `key-foo`
    // deleted `foo`. The `%` canonical prefix makes the two namespaces
    // disjoint.
    let root_dir = tmp_root();
    let root = root_dir.path().to_path_buf();
    let secrets = FsSecretStore::new(&root);
    let company = CompanyId::new("company-a");

    secrets
        .set(&company, "foo", SecretValue("value-for-foo".into()))
        .await
        .unwrap();
    // `key-foo` was never set, and the legacy fallback must not reach the
    // canonical file of `foo`.
    assert_eq!(
        secrets.get(&company, "key-foo").await.unwrap(),
        None,
        "legacy fallback reached a canonical file of a different key"
    );

    // Writing `key-foo` must not disturb `foo`'s value.
    secrets
        .set(&company, "key-foo", SecretValue("value-for-key-foo".into()))
        .await
        .unwrap();
    assert_eq!(
        secrets.get(&company, "foo").await.unwrap(),
        Some(SecretValue("value-for-foo".into())),
        "writing `key-foo` deleted `foo`"
    );
    assert_eq!(
        secrets.get(&company, "key-foo").await.unwrap(),
        Some(SecretValue("value-for-key-foo".into()))
    );
}

/// A key whose legacy slug is far past `NAME_MAX`. 300 characters is the
/// length that reproduced the incident end to end; the canonical filename
/// is digest-truncated and unaffected, so this exercises only the legacy
/// fallback.
fn over_long_key() -> String {
    format!("provider/{}/key", "a".repeat(300))
}

#[tokio::test]
async fn a_key_too_long_for_a_legacy_path_reads_as_absent() {
    // `get` used to fall through to `legacy_secret`, take `ENAMETOOLONG`
    // from the kernel, and return `Err` — 500ing every route that merely
    // reads a credential, including the add flow's own existence check.
    let root_dir = tmp_root();
    let root = root_dir.path().to_path_buf();
    let secrets = FsSecretStore::new(&root);
    let company = CompanyId::new("company-a");
    let key = over_long_key();

    assert_eq!(
        secrets.get(&company, &key).await.unwrap(),
        None,
        "an unset over-long key must read as absent, not as a store error"
    );

    secrets
        .set(&company, &key, SecretValue("sk-not-a-real-key".into()))
        .await
        .unwrap();
    assert_eq!(
        secrets.get(&company, &key).await.unwrap(),
        Some(SecretValue("sk-not-a-real-key".into()))
    );
}

#[tokio::test]
async fn clearing_a_key_too_long_for_a_legacy_path_succeeds() {
    // The P0: `set` writes the canonical file first, then removes the
    // legacy one. With an over-long key that removal answered
    // `ENAMETOOLONG`, so `set` returned `Err` *after* truncating the stored
    // credential — a 500 on `DELETE` with the key already at zero bytes and
    // the row still listed.
    let root_dir = tmp_root();
    let root = root_dir.path().to_path_buf();
    let secrets = FsSecretStore::new(&root);
    let company = CompanyId::new("company-a");
    let key = over_long_key();

    secrets
        .set(&company, &key, SecretValue("sk-not-a-real-key".into()))
        .await
        .unwrap();
    secrets
        .set(&company, &key, SecretValue(String::new()))
        .await
        .expect("clearing an over-long key must not fail after the write lands");
    // The port has no delete: a clear is a write of the empty string, which
    // every caller reads as unset. What matters here is that the write and
    // its result agree — the incident was a 500 over a key that was already
    // empty on disk.
    assert_eq!(
        secrets.get(&company, &key).await.unwrap(),
        Some(SecretValue(String::new()))
    );
}

#[test]
fn only_absence_like_errors_are_read_as_a_missing_legacy_file() {
    use std::io::{Error, ErrorKind};
    assert!(legacy_secret_absent(&Error::from(ErrorKind::NotFound)));
    assert!(legacy_secret_absent(&Error::from(
        ErrorKind::InvalidFilename
    )));
    // Everything else stays loud: a secrets directory that cannot be read
    // must not be mistaken for one holding nothing.
    assert!(!legacy_secret_absent(&Error::from(
        ErrorKind::PermissionDenied
    )));
    assert!(!legacy_secret_absent(&Error::from(ErrorKind::Other)));
}

#[tokio::test]
async fn secret_set_succeeds_for_encoding_heavy_keys() {
    // A 20-emoji MCP server name used to exceed the filesystem component
    // limit once percent-encoded; the filename must stay bounded so `set`
    // does not fail with ENAMETOOLONG.
    let root_dir = tmp_root();
    let root = root_dir.path().to_path_buf();
    let secrets = FsSecretStore::new(&root);
    let company = CompanyId::new("company-a");
    let key = format!("mcp/{}/auth", "🎯".repeat(20));
    let value = SecretValue("not-a-real-token".into());

    secrets.set(&company, &key, value.clone()).await.unwrap();
    assert_eq!(secrets.get(&company, &key).await.unwrap(), Some(value));
}
/// The put/delete race the index lock exists for: a same-address write
/// and delete interleaving as write-blob / delete-both / append-index
/// would leave an index row whose blob is gone — list answers, peek
/// fails. With the blob write under the lock, every surviving index row
/// must have a readable blob, whichever order the race resolved.
#[tokio::test]
async fn concurrent_same_address_put_and_delete_stay_coherent() {
    use crate::ports::ContextStore;
    let dir = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(FsContextStore::new(dir.path().to_path_buf()));
    let id = CompanyId::new("race-co");
    let chunk = || ContextChunk {
        label: "race/probe".into(),
        body: "identical body".into(),
    };
    let addr = store.put(&id, chunk()).await.unwrap();

    for _ in 0..20 {
        let s1 = store.clone();
        let s2 = store.clone();
        let id1 = id.clone();
        let id2 = id.clone();
        let a = addr.clone();
        let put = tokio::spawn(async move { s1.put(&id1, chunk()).await });
        let del = tokio::spawn(async move { s2.delete(&id2, &a).await });
        put.await.unwrap().unwrap();
        del.await.unwrap().unwrap();

        // Whatever interleaving happened: every listed row peeks.
        for meta in store.list(&id, "").await.unwrap() {
            store
                .peek(&id, &meta.addr, None)
                .await
                .unwrap_or_else(|e| panic!("index row {} has no readable blob: {e}", meta.label));
        }
        // Reset to a known present state for the next round.
        store.put(&id, chunk()).await.unwrap();
    }
}
