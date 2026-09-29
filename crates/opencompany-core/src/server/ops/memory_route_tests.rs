use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use super::MAX_CONTEXT_ENTRIES;
use crate::company::CompanyManifest;
use crate::ports::context::ContextStore;
use crate::ports::types::{
    ChunkAddr, ChunkHit, ChunkMeta, CompanyId, CompanyRecord, CompressedTrace, ContextChunk,
};
use crate::runtime::RuntimeBuilder;
use crate::server::router;
use crate::store::FsCompanyStore;
use crate::{AppConfig, AppState};

/// A scripted [`ContextStore`]: `list` answers a fixed meta set (oldest
/// first, as every real backend lists), and reads are counted so a test
/// can pin HOW the route fetched bodies, not only what it rendered.
struct ScriptedContext {
    metas: Vec<ChunkMeta>,
    bodies: HashMap<String, String>,
    single_peeks: AtomicUsize,
    bulk_peeks: AtomicUsize,
}

impl ScriptedContext {
    /// `total` chunks stamped `1..=total`, listed oldest-first, with the
    /// two context origins interleaved: even stamps are task outcomes,
    /// odd ones agent memories. The mix is load-bearing — with a
    /// single-origin fixture the route's own bucketing hid a cross-origin
    /// ordering bug from every newest-first assertion below (the newest
    /// chunk here, `total`, is deliberately a task outcome).
    fn with_chunks(total: usize) -> Arc<Self> {
        let mut metas = Vec::new();
        let mut bodies = HashMap::new();
        for i in 1..=total {
            let addr = format!("addr-{i:04}");
            let label = if i % 2 == 0 {
                "task-outcome/agent-1".to_string()
            } else {
                format!("agent-1/note-{i}")
            };
            metas.push(ChunkMeta {
                addr: ChunkAddr::new(addr.clone()),
                label,
                len: 0,
                stored_at_millis: i as u64,
            });
            bodies.insert(addr, format!("note {i}"));
        }
        Arc::new(Self {
            metas,
            bodies,
            single_peeks: AtomicUsize::new(0),
            bulk_peeks: AtomicUsize::new(0),
        })
    }

    fn with_labels(labels: &[&str]) -> Arc<Self> {
        let mut metas = Vec::new();
        let mut bodies = HashMap::new();
        for (index, label) in labels.iter().enumerate() {
            let addr = format!("addr-{index:04}");
            metas.push(ChunkMeta {
                addr: ChunkAddr::new(addr.clone()),
                label: (*label).to_string(),
                len: 0,
                stored_at_millis: (index + 1) as u64,
            });
            bodies.insert(addr, format!("note {index}"));
        }
        Arc::new(Self {
            metas,
            bodies,
            single_peeks: AtomicUsize::new(0),
            bulk_peeks: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl ContextStore for ScriptedContext {
    async fn put(&self, _id: &CompanyId, chunk: ContextChunk) -> crate::Result<ChunkAddr> {
        // Nothing in these read-only tests writes context; answer with a
        // derived addr rather than failing an incidental write.
        Ok(ChunkAddr::new(format!("scripted/{}", chunk.label)))
    }

    async fn list(&self, _id: &CompanyId, prefix: &str) -> crate::Result<Vec<ChunkMeta>> {
        Ok(self
            .metas
            .iter()
            .filter(|m| m.label.starts_with(prefix))
            .cloned()
            .collect())
    }

    async fn peek(
        &self,
        _id: &CompanyId,
        addr: &ChunkAddr,
        _range: Option<Range<usize>>,
    ) -> crate::Result<String> {
        self.single_peeks.fetch_add(1, Ordering::SeqCst);
        self.bodies.get(addr.as_ref()).cloned().ok_or_else(|| {
            crate::error::OpenCompanyError::Store(format!(
                "context chunk not found: {}",
                addr.as_ref()
            ))
        })
    }

    async fn peek_many(
        &self,
        _id: &CompanyId,
        addrs: &[ChunkAddr],
    ) -> crate::Result<Vec<Option<String>>> {
        self.bulk_peeks.fetch_add(1, Ordering::SeqCst);
        Ok(addrs
            .iter()
            .map(|addr| self.bodies.get(addr.as_ref()).cloned())
            .collect())
    }

    async fn search(
        &self,
        _id: &CompanyId,
        _query: &str,
        _limit: usize,
    ) -> crate::Result<Vec<ChunkHit>> {
        Ok(Vec::new())
    }

    async fn delete(&self, _id: &CompanyId, _addr: &ChunkAddr) -> crate::Result<bool> {
        Ok(false)
    }

    async fn delete_label(
        &self,
        _id: &CompanyId,
        _addr: &ChunkAddr,
        _label: &str,
    ) -> crate::Result<bool> {
        Ok(false)
    }
}

/// A scripted [`crate::store::MemoryScopes`] whose archive tier answers a
/// fixed trace list — the provider-only surface the fs default refuses.
struct ScriptedScopes {
    archived: Vec<CompressedTrace>,
}

#[async_trait]
impl crate::store::MemoryScopes for ScriptedScopes {
    fn agent_context(&self, _agent_id: &str) -> Arc<dyn ContextStore> {
        panic!("the archives route never touches agent context")
    }

    fn desk_context(&self, _desk_id: &str) -> Arc<dyn ContextStore> {
        panic!("the archives route never touches desk context")
    }

    async fn archived_traces(&self, _company: &CompanyId) -> crate::Result<Vec<CompressedTrace>> {
        Ok(self.archived.clone())
    }
}

/// An [`AppState`] whose company runtime reads context from `context`,
/// with everything else on fresh fs stores under `home`.
async fn state_over(home: &std::path::Path, context: Arc<ScriptedContext>) -> AppState {
    state_over_with_scopes(home, context, None).await
}

/// [`state_over`] with an injected [`crate::store::MemoryScopes`], so a
/// test can exercise the provider-only archive surface.
async fn state_over_with_scopes(
    home: &std::path::Path,
    context: Arc<ScriptedContext>,
    scopes: Option<Arc<dyn crate::store::MemoryScopes>>,
) -> AppState {
    use crate::ports::CompanyStore;
    let manifest: CompanyManifest =
        toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n").unwrap();
    let store = FsCompanyStore::new(home.to_path_buf());
    let id = CompanyId::new("acme");
    store
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_desk_hive: Vec::new(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            id: id.clone(),
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
        })
        .await
        .unwrap();
    let mut builder = RuntimeBuilder::new(home.to_path_buf(), manifest)
        .with_id(id.clone())
        .with_context(context);
    if let Some(scopes) = scopes {
        builder = builder.with_memory_scopes(scopes);
    }
    let runtime = builder.build().await.unwrap();
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;
    state
}

async fn get_memory(state: &AppState) -> (StatusCode, Value) {
    get_json(state, "/api/v1/company/memory").await
}

async fn get_stats(state: &AppState) -> (StatusCode, Value) {
    get_json(state, "/api/v1/company/memory/stats").await
}

async fn get_json(state: &AppState, uri: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("cookie", crate::server::test_support::fixed_cookie("acme"))
        .body(Body::empty())
        .unwrap();
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn post_json(state: &AppState, uri: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("cookie", crate::server::test_support::fixed_cookie("acme"))
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

#[tokio::test]
async fn brain_stats_exclude_operator_fact_mirrors_from_the_display_partition() {
    let home = tempfile::tempdir().unwrap();
    let context = ScriptedContext::with_labels(&[
        "agent-memory/ceo/note",
        "task-outcome/ceo",
        "operator-fact/fact-123",
        "document/contract/0",
    ]);
    let state = state_over(home.path(), context).await;

    let (created, _) = post_json(
        &state,
        "/api/v1/company/memory",
        serde_json::json!({
            "kind": "fact",
            "title": "Operator note",
            "body": "A durable fact",
        }),
    )
    .await;
    assert_eq!(created, StatusCode::OK);

    let (status, stats) = get_stats(&state).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(stats["facts"], 1);
    assert_eq!(stats["teammateMemory"], 1);
    assert_eq!(stats["taskOutcomes"], 1);
    assert_eq!(stats["documentMemory"], 1);
    assert_eq!(stats["totalItems"], 4);
    assert!(
        stats.get("agentChunks").is_none(),
        "the ambiguous all-chunks count must not reach the display"
    );
}

/// The document-label contract (`ingest::chunk`): a dropped document or
/// link is operator-supplied material, so its chunks must never inflate
/// teammate memory. One multi-chunk upload is several `document/…` rows;
/// they get their own bucket while `totalItems` still counts them.
#[tokio::test]
async fn brain_stats_keep_document_chunks_out_of_teammate_memory() {
    let home = tempfile::tempdir().unwrap();
    let context = ScriptedContext::with_labels(&[
        "document/contract/0",
        "document/contract/1",
        "document/contract/2",
        "agent-memory/ceo/note",
    ]);
    let state = state_over(home.path(), context).await;

    let (status, stats) = get_stats(&state).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(stats["facts"], 0);
    assert_eq!(stats["teammateMemory"], 1);
    assert_eq!(stats["documentMemory"], 3);
    assert_eq!(stats["taskOutcomes"], 0);
    assert_eq!(
        stats["totalItems"], 4,
        "document chunks stay in the display partition, just not under teammate memory"
    );
}

/// The truncation notice must be decided from ONE server snapshot: the
/// list response carries `totalContext`/`contextTruncated` for the same
/// read that produced the rows, so the console never compares the capped
/// rows against an independently-timed count.
#[tokio::test]
async fn the_brain_list_reports_its_own_truncation() {
    let home = tempfile::tempdir().unwrap();
    let total = MAX_CONTEXT_ENTRIES + 2;
    let state = state_over(home.path(), ScriptedContext::with_chunks(total)).await;

    let (status, list) = get_memory(&state).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        list["contextTruncated"], true,
        "past the 500-row list cap, the list read must say it truncated"
    );
    assert_eq!(
        list["totalContext"], total as u64,
        "the uncapped context count is the 'M' in the notice, from the same read"
    );
    assert_eq!(
        list["items"].as_array().unwrap().len(),
        MAX_CONTEXT_ENTRIES,
        "the rows are capped to the newest 500"
    );

    // The stats counts are never capped — the display partition still
    // reports the full store when the list truncates.
    let (status, stats) = get_stats(&state).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stats["totalItems"], total as u64);
}

/// A `?query=` request returns search matches, not "the newest N", so the
/// truncation metadata is not applicable — it must not claim a search
/// result was omitted by the cap, however far past it the store is.
#[tokio::test]
async fn the_brain_list_omits_truncation_metadata_for_queried_requests() {
    let home = tempfile::tempdir().unwrap();
    let total = MAX_CONTEXT_ENTRIES + 2;
    let state = state_over(home.path(), ScriptedContext::with_chunks(total)).await;

    let (status, list) = get_json(&state, "/api/v1/company/memory?query=note").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        list["contextTruncated"], false,
        "a query result is not 'the newest N', so nothing was 'truncated'"
    );
    assert_eq!(
        list["totalContext"], 0,
        "truncation metadata does not describe queried rows"
    );
    assert!(
        list["items"].as_array().unwrap().len() <= MAX_CONTEXT_ENTRIES,
        "the query still returns its (capped) matches"
    );
}

/// #1488: with more chunks than the cap, the list must keep the NEWEST
/// `MAX_CONTEXT_ENTRIES`, newest first. Before the fix the route took the
/// head of the backend's oldest-first list, so a company past the cap
/// never saw a new memory again.
#[tokio::test]
async fn the_brain_list_keeps_the_newest_chunks_once_over_the_cap() {
    let home = tempfile::tempdir().unwrap();
    let total = MAX_CONTEXT_ENTRIES + 2;
    let state = state_over(home.path(), ScriptedContext::with_chunks(total)).await;

    let (status, list) = get_memory(&state).await;
    assert_eq!(status, StatusCode::OK);
    let rows = list["items"].as_array().expect("a JSON items array");
    assert_eq!(rows.len(), MAX_CONTEXT_ENTRIES);
    let stamps: Vec<u64> = rows
        .iter()
        .map(|row| row["updatedAt"].as_u64().unwrap())
        .collect();
    assert_eq!(stamps[0], total as u64, "the newest chunk heads the list");
    assert_eq!(
        rows[0]["origin"], "task-outcome",
        "the newest chunk heads the list whatever its origin — grouping \
         outcomes behind memories put it last"
    );
    assert!(
        stamps.windows(2).all(|pair| pair[0] >= pair[1]),
        "context rows render newest-first"
    );
    assert!(
        stamps.iter().all(|stamp| *stamp > 2),
        "the oldest chunks fell off the cap, not the newest"
    );
}

/// The other half of #1488: bodies arrive through ONE bulk `peek_many`,
/// not a peek-per-chunk loop.
#[tokio::test]
async fn the_brain_list_reads_bodies_in_one_bulk_peek() {
    let home = tempfile::tempdir().unwrap();
    let context = ScriptedContext::with_chunks(3);
    let state = state_over(home.path(), context.clone()).await;

    let (status, list) = get_memory(&state).await;
    assert_eq!(status, StatusCode::OK);
    let rows = list["items"].as_array().expect("a JSON items array");
    assert_eq!(rows.len(), 3);
    // The bodies really flowed through the bulk read (newest first).
    assert_eq!(rows[0]["title"], "note 3");
    assert_eq!(
        context.bulk_peeks.load(Ordering::SeqCst),
        1,
        "one bulk read"
    );
    assert_eq!(
        context.single_peeks.load(Ordering::SeqCst),
        0,
        "the per-chunk peek loop is gone"
    );
}

/// The route is registered for every company, but only a provider-backed
/// engine has an archive tier. The store/embedded default must answer a
/// 404 that names the condition — never a 500 that reads as a server
/// fault — and an empty list would falsely imply there are no archived
/// traces.
#[tokio::test]
async fn the_archives_route_refuses_a_store_backend_with_404() {
    let home = tempfile::tempdir().unwrap();
    let state = state_over(home.path(), ScriptedContext::with_labels(&[])).await;

    let (status, body) = get_json(&state, "/api/v1/company/memory/archives").await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "not_found");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("does not provide archived traces"),
        "the refusal must name the condition: {body}"
    );
}

/// The archive surface speaks the same camelCase [`TraceEntry`] contract
/// as `/memory/traces` — `cycleId`/`atMillis`, never the storage type's
/// snake_case — and orders newest-last, the same total order as the
/// retained window.
#[tokio::test]
async fn the_archives_route_serializes_camelcase_newest_last() {
    let home = tempfile::tempdir().unwrap();
    let scopes: Arc<dyn crate::store::MemoryScopes> = Arc::new(ScriptedScopes {
        archived: vec![
            CompressedTrace {
                cycle_id: "c-old".into(),
                summary: "older".into(),
                at_millis: 100,
            },
            CompressedTrace {
                cycle_id: "c-new".into(),
                summary: "newer".into(),
                at_millis: 300,
            },
        ],
    });
    let state =
        state_over_with_scopes(home.path(), ScriptedContext::with_labels(&[]), Some(scopes)).await;

    let (status, body) = get_json(&state, "/api/v1/company/memory/archives").await;

    assert_eq!(status, StatusCode::OK);
    let rows = body.as_array().expect("a JSON array of traces");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["cycleId"], "c-old");
    assert_eq!(
        rows[1]["cycleId"], "c-new",
        "newest last, like /memory/traces"
    );
    assert_eq!(rows[1]["atMillis"], 300);
    assert_eq!(rows[1]["summary"], "newer");
    assert!(
        rows[0].get("cycle_id").is_none() && rows[0].get("at_millis").is_none(),
        "the storage type's snake_case must not leak onto the wire"
    );
}
