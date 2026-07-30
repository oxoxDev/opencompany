//! In-pod, persistent TinyCortex memory engine (degraded-embedding mode).
//!
//! [`EngineCortex`] is the real, engine-backed [`CortexClient`] that replaces the
//! former inert HTTP client. It runs the OpenHuman `tinycortex` engine **inside
//! the pod** with durable local storage: every company gets its own workspace
//! under `<data_root>/memory/<company>/`, and the engine's canonical per-workspace
//! SQLite database (opened + migrated through the crate's own
//! [`chunks::shared_connection`]) holds that company's traces, task results, and
//! context chunks. The engine "never makes a network call".
//!
//! ## Degraded (lexical/recency) recall — no embedding compute
//!
//! This is issue #188 slice **188c1**: it lights up the persistent engine but
//! ships in degraded mode. The engine's `MemoryConfig` is built directly with
//! `embedding.strict = false`, so an inert embedder is tolerated and recall
//! degrades to lexical/recency instead of semantic — with **zero** embedding
//! compute injected. Injecting a real embedding provider (and thereby the
//! summary-tree + hybrid-retrieval path) is slice 188c2.
//!
//! ## Why chunks persist through the KV tier, not `ingest`/retrieval primitives
//!
//! The plan first mapped `put_chunk` onto the crate's `ingest` pipeline and
//! `search_chunks` onto its hybrid-retrieval primitives. Two crate realities make
//! that the wrong fit for this slice, so chunks are persisted through the engine's
//! **KV tier** (on the engine's shared workspace connection) instead:
//!
//! 1. **Retrieval can't rank lexically in degraded mode.** The crate's own
//!    `retrieval` module documents that its primitives currently rank by *stored
//!    admission score, semantic-cosine rerank, or recency* — the keyword/graph
//!    scorers are "defined, not yet wired". With an inert embedder there is no
//!    semantic signal, so a `search_chunks` built on them would not produce the
//!    `[0, 1]` token-overlap relevance the [`ContextStore`](crate::ports::context::ContextStore)
//!    contract (and the degraded-mode search test) requires.
//! 2. **`ingest` fragments the 1:1 addr↔body + label contract.** OpenCompany's
//!    context store addresses each chunk by a content hash and lists by a
//!    label prefix; the crate's ingest path re-chunks/scores a document into its
//!    own sha-256 chunk ids, which cannot round-trip OpenCompany's `addr`, its
//!    `peek(addr)`, or its `list(prefix)` semantics.
//!
//! So chunk bodies + metadata live as content-addressed KV entries in the same
//! per-company engine database, and `search_chunks` ranks them with the same
//! lexical token-overlap the offline [`InMemoryCortex`](crate::store::tinycortex::InMemoryCortex)
//! backend already defines. Semantic recall over the summary tree arrives with
//! embeddings in 188c2.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use tinycortex::memory::chunks::shared_connection;
use tinycortex::memory::config::MemoryConfig;
use tinycortex::memory::store::KvStore;

use crate::Result;
use crate::error::OpenCompanyError;
use crate::ports::now_millis;
use crate::ports::types::{
    ChunkAddr, ChunkHit, ChunkMeta, CompressedTrace, ContextChunk, EvictionPolicy, TaskResult,
};
use crate::store::tinycortex::{CortexClient, CortexContextStore, CortexMemoryStore};

// ---------------------------------------------------------------------------
// KV layout
// ---------------------------------------------------------------------------

/// Key holding a company's live traces, as a JSON array (newest last).
const KEY_TRACES: &str = "oc:traces";
/// Key holding a company's archived traces, as a JSON array.
const KEY_ARCHIVE: &str = "oc:archive";
/// Key prefix for one task result per completed background task.
const KEY_TASK_PREFIX: &str = "oc:task:";
/// Key prefix for one context chunk per content address.
const KEY_CHUNK_PREFIX: &str = "oc:chunk:";

/// A persisted context chunk: its label, body, and first-stored wall-clock.
#[derive(Clone, Serialize, Deserialize)]
struct StoredChunk {
    label: String,
    body: String,
    stored_at_millis: u64,
}

// ---------------------------------------------------------------------------
// Per-company engine handle
// ---------------------------------------------------------------------------

/// One company's opened engine: the KV tier over its per-workspace SQLite DB.
struct CompanyEngine {
    kv: KvStore,
}

impl CompanyEngine {
    /// Opens (creating on first use) the engine for a company workspace.
    ///
    /// Builds the crate's `MemoryConfig` directly (`strict = false` for degraded
    /// recall), then rides a [`KvStore`] on the engine's shared per-workspace
    /// SQLite connection — the exact handle-sharing the crate documents for
    /// embedding hosts. The workspace directory is created if absent; nothing
    /// here touches the network.
    fn open(workspace: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&workspace).map_err(|e| {
            OpenCompanyError::Store(format!(
                "create engine workspace {}: {e}",
                workspace.display()
            ))
        })?;

        let mut config = MemoryConfig::new(workspace);
        // Degraded-embedding mode: tolerate an inert embedder, no compute.
        config.embedding.strict = false;
        config
            .validate()
            .map_err(|e| OpenCompanyError::Store(format!("invalid memory config: {e}")))?;

        let conn = shared_connection(&config)
            .map_err(|e| OpenCompanyError::Store(format!("open engine connection: {e}")))?;
        let kv = KvStore::from_shared_connection(conn)
            .map_err(|e| OpenCompanyError::Store(format!("open engine kv store: {e}")))?;

        Ok(Self { kv })
    }

    /// Reads and JSON-decodes the value at `key`, `None` when absent.
    fn get_json<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        match self
            .kv
            .get_global(key)
            .map_err(|e| OpenCompanyError::Store(format!("kv get {key}: {e}")))?
        {
            None => Ok(None),
            Some(value) => Ok(Some(serde_json::from_value(value)?)),
        }
    }

    /// JSON-encodes `value` and upserts it at `key`.
    fn put_json<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        // Store as a *structured* JSON value, never a stringified blob. The
        // engine's KV `set_global` runs a write-time safety guard over string
        // values (a checksum-gated national-ID PII scrubber + secret redaction);
        // a stringified payload would expose its integer fields — chiefly the
        // epoch-millis timestamps — to that scrubber, which can rewrite the digits
        // and corrupt the JSON. Stored as structured JSON, numbers pass through
        // the guard untouched and only string *content* is scrubbed, so
        // timestamps round-trip intact.
        let encoded = serde_json::to_value(value)?;
        self.kv
            .set_global(key, &encoded)
            .map_err(|e| OpenCompanyError::Store(format!("kv set {key}: {e}")))
    }

    /// Returns `(addr, chunk)` for every stored context chunk.
    fn all_chunks(&self) -> Result<Vec<(String, StoredChunk)>> {
        let records = self
            .kv
            .records_global()
            .map_err(|e| OpenCompanyError::Store(format!("kv list chunks: {e}")))?;
        let mut out = Vec::new();
        for record in records {
            let Some(addr) = record
                .key
                .strip_prefix(KEY_CHUNK_PREFIX)
                .map(str::to_string)
            else {
                continue;
            };
            let chunk: StoredChunk = serde_json::from_value(record.value)?;
            out.push((addr, chunk));
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// EngineCortex
// ---------------------------------------------------------------------------

/// A persistent, in-pod [`CortexClient`] over the OpenHuman `tinycortex` engine.
///
/// Per-company engine handles are opened lazily and cached, each rooted at
/// `<memory_root>/<company>/`. Isolation is physical: two companies never share
/// a workspace or a database file, so company A cannot observe company B's data.
pub struct EngineCortex {
    memory_root: PathBuf,
    companies: StdMutex<HashMap<String, Arc<CompanyEngine>>>,
}

impl EngineCortex {
    /// Builds an engine rooted at `memory_root` (typically `<data_dir>/memory`).
    ///
    /// Opening is lazy: the first call touching a company creates + migrates that
    /// company's workspace database.
    pub fn new(memory_root: impl Into<PathBuf>) -> Self {
        Self {
            memory_root: memory_root.into(),
            companies: StdMutex::new(HashMap::new()),
        }
    }

    /// Sanitizes a company id into a single path-safe workspace directory name.
    fn workspace_name(company: &str) -> String {
        let name: String = company
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if name.is_empty() {
            "_".to_string()
        } else {
            name
        }
    }

    /// Returns the (lazily opened, cached) engine handle for `company`.
    fn engine(&self, company: &str) -> Result<Arc<CompanyEngine>> {
        let mut map = self.companies.lock().expect("engine cortex mutex poisoned");
        if let Some(existing) = map.get(company) {
            return Ok(existing.clone());
        }
        let workspace = self.memory_root.join(Self::workspace_name(company));
        let engine = Arc::new(CompanyEngine::open(workspace)?);
        map.insert(company.to_string(), engine.clone());
        Ok(engine)
    }
}

#[async_trait]
impl CortexClient for EngineCortex {
    async fn append_trace(&self, company: &str, trace: CompressedTrace) -> Result<()> {
        let engine = self.engine(company)?;
        let mut traces: Vec<CompressedTrace> = engine.get_json(KEY_TRACES)?.unwrap_or_default();
        traces.push(trace);
        engine.put_json(KEY_TRACES, &traces)
    }

    async fn recent_traces(&self, company: &str, limit: usize) -> Result<Vec<CompressedTrace>> {
        let engine = self.engine(company)?;
        let traces: Vec<CompressedTrace> = engine.get_json(KEY_TRACES)?.unwrap_or_default();
        let start = traces.len().saturating_sub(limit);
        Ok(traces[start..].to_vec())
    }

    async fn put_task_result(&self, company: &str, result: TaskResult) -> Result<()> {
        let engine = self.engine(company)?;
        let key = format!("{KEY_TASK_PREFIX}{}", result.task_id);
        engine.put_json(&key, &result)
    }

    async fn archive_traces(&self, company: &str, policy: EvictionPolicy) -> Result<u64> {
        let engine = self.engine(company)?;
        let mut traces: Vec<CompressedTrace> = engine.get_json(KEY_TRACES)?.unwrap_or_default();
        let mut archive: Vec<CompressedTrace> = engine.get_json(KEY_ARCHIVE)?.unwrap_or_default();

        // Eviction archives rather than destroys — same policy as InMemoryCortex.
        let moved: Vec<CompressedTrace> = match policy {
            EvictionPolicy::KeepRecent { n } => {
                let cut = traces.len().saturating_sub(n);
                traces.drain(..cut).collect()
            }
            EvictionPolicy::OlderThan { before_millis } => {
                let mut kept = Vec::with_capacity(traces.len());
                let mut moved = Vec::new();
                for trace in traces.drain(..) {
                    if trace.at_millis < before_millis {
                        moved.push(trace);
                    } else {
                        kept.push(trace);
                    }
                }
                traces = kept;
                moved
            }
        };

        let count = moved.len() as u64;
        archive.extend(moved);
        engine.put_json(KEY_TRACES, &traces)?;
        engine.put_json(KEY_ARCHIVE, &archive)?;
        Ok(count)
    }

    async fn put_chunk(&self, company: &str, addr: &str, chunk: ContextChunk) -> Result<()> {
        let engine = self.engine(company)?;
        let key = format!("{KEY_CHUNK_PREFIX}{addr}");
        // Content-addressed: an identical body is stored once.
        if engine
            .kv
            .get_global(&key)
            .map_err(|e| OpenCompanyError::Store(format!("kv get {key}: {e}")))?
            .is_some()
        {
            return Ok(());
        }
        engine.put_json(
            &key,
            &StoredChunk {
                label: chunk.label,
                body: chunk.body,
                stored_at_millis: now_millis(),
            },
        )
    }

    async fn list_chunks(&self, company: &str, prefix: &str) -> Result<Vec<ChunkMeta>> {
        let engine = self.engine(company)?;
        Ok(engine
            .all_chunks()?
            .into_iter()
            .filter(|(_, chunk)| chunk.label.starts_with(prefix))
            .map(|(addr, chunk)| ChunkMeta {
                addr: ChunkAddr::new(addr),
                len: chunk.body.len(),
                label: chunk.label,
                stored_at_millis: chunk.stored_at_millis,
            })
            .collect())
    }

    async fn peek_chunk(&self, company: &str, addr: &str) -> Result<Option<String>> {
        let engine = self.engine(company)?;
        let key = format!("{KEY_CHUNK_PREFIX}{addr}");
        Ok(engine.get_json::<StoredChunk>(&key)?.map(|c| c.body))
    }

    async fn search_chunks(
        &self,
        company: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ChunkHit>> {
        let engine = self.engine(company)?;
        let chunks = engine.all_chunks()?;
        let mut hits = score_chunks(&chunks, query);
        hits.truncate(limit);
        Ok(hits)
    }

    async fn hard_delete_trace(&self, company: &str, cycle_id: &str) -> Result<bool> {
        let engine = self.engine(company)?;
        let mut traces: Vec<CompressedTrace> = engine.get_json(KEY_TRACES)?.unwrap_or_default();
        let mut archive: Vec<CompressedTrace> = engine.get_json(KEY_ARCHIVE)?.unwrap_or_default();
        let before = traces.len() + archive.len();
        traces.retain(|t| t.cycle_id != cycle_id);
        archive.retain(|t| t.cycle_id != cycle_id);
        let changed = before != traces.len() + archive.len();
        if changed {
            engine.put_json(KEY_TRACES, &traces)?;
            engine.put_json(KEY_ARCHIVE, &archive)?;
        }
        Ok(changed)
    }

    async fn hard_delete_chunk(&self, company: &str, addr: &str) -> Result<bool> {
        let engine = self.engine(company)?;
        let key = format!("{KEY_CHUNK_PREFIX}{addr}");
        engine
            .kv
            .delete_global(&key)
            .map_err(|e| OpenCompanyError::Store(format!("kv delete {key}: {e}")))
    }

    async fn redact(&self, company: &str, needle: &str, replacement: &str) -> Result<u64> {
        if needle.is_empty() {
            return Ok(0);
        }
        let engine = self.engine(company)?;
        let mut replaced = 0u64;

        let mut traces: Vec<CompressedTrace> = engine.get_json(KEY_TRACES)?.unwrap_or_default();
        let mut archive: Vec<CompressedTrace> = engine.get_json(KEY_ARCHIVE)?.unwrap_or_default();
        for trace in traces.iter_mut().chain(archive.iter_mut()) {
            replaced += replace_in_place(&mut trace.summary, needle, replacement);
        }
        engine.put_json(KEY_TRACES, &traces)?;
        engine.put_json(KEY_ARCHIVE, &archive)?;

        for (addr, mut chunk) in engine.all_chunks()? {
            let hits = replace_in_place(&mut chunk.body, needle, replacement);
            if hits > 0 {
                replaced += hits;
                engine.put_json(&format!("{KEY_CHUNK_PREFIX}{addr}"), &chunk)?;
            }
        }
        Ok(replaced)
    }
}

// ---------------------------------------------------------------------------
// Injection helper
// ---------------------------------------------------------------------------

/// Builds a [`MemoryStore`](crate::ports::memory::MemoryStore) +
/// [`ContextStore`](crate::ports::context::ContextStore) pair over one shared
/// [`EngineCortex`] rooted at `memory_root`, so both ports read and write the
/// same persistent per-company engine databases.
///
/// This is the injection shape [`open_tinycortex`](crate::store::select) uses
/// when a data directory is present: feed the returned stores into
/// `RuntimeBuilder::with_memory_overlay`.
pub fn engine(
    memory_root: impl Into<PathBuf>,
) -> (Arc<CortexMemoryStore>, Arc<CortexContextStore>) {
    let client: Arc<dyn CortexClient> = Arc::new(EngineCortex::new(memory_root));
    (
        Arc::new(CortexMemoryStore::new(client.clone())),
        Arc::new(CortexContextStore::new(client)),
    )
}

// ---------------------------------------------------------------------------
// Degraded lexical scoring (mirrors InMemoryCortex)
// ---------------------------------------------------------------------------

/// Replaces every occurrence of `needle` in `s`, returning the count replaced.
fn replace_in_place(s: &mut String, needle: &str, replacement: &str) -> u64 {
    let count = s.matches(needle).count() as u64;
    if count > 0 {
        *s = s.replace(needle, replacement);
    }
    count
}

/// Ranks chunks by distinct-query-token overlap against their bodies, best
/// first, dropping zero-overlap chunks. Lexical and offline — the degraded-mode
/// recall this slice ships; semantic recall lands with embeddings in 188c2.
fn score_chunks(chunks: &[(String, StoredChunk)], query: &str) -> Vec<ChunkHit> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|t| t.to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    if terms.is_empty() {
        return Vec::new();
    }
    let mut distinct: Vec<&String> = Vec::new();
    for t in &terms {
        if !distinct.contains(&t) {
            distinct.push(t);
        }
    }

    let mut scored: Vec<ChunkHit> = Vec::new();
    for (addr, chunk) in chunks {
        let body_lower = chunk.body.to_lowercase();
        let matched = distinct
            .iter()
            .filter(|t| body_lower.contains(t.as_str()))
            .count();
        if matched == 0 {
            continue;
        }
        let score = matched as f64 / distinct.len() as f64;
        // Anchor the snippet on the first matching term.
        let pos = distinct
            .iter()
            .filter_map(|t| body_lower.find(t.as_str()).map(|p| (p, t.len())))
            .min_by_key(|(p, _)| *p);
        let snippet = match pos {
            Some((p, len)) => snippet_around(&chunk.body, p, len),
            None => chunk.body.clone(),
        };
        scored.push(ChunkHit {
            addr: ChunkAddr::new(addr.clone()),
            snippet,
            score,
        });
    }
    scored.sort_by(|a, b| b.score.total_cmp(&a.score));
    scored
}

/// Extracts a char-boundary-safe window around `pos` of a matched term.
fn snippet_around(body: &str, pos: usize, term_len: usize) -> String {
    let raw_start = pos.saturating_sub(24);
    let raw_end = (pos + term_len + 24).min(body.len());
    let start = (raw_start..=pos)
        .find(|&i| body.is_char_boundary(i))
        .unwrap_or(pos);
    let end = (raw_end..=body.len())
        .find(|&i| body.is_char_boundary(i))
        .unwrap_or(body.len());
    body[start..end].to_string()
}

#[cfg(test)]
mod test {
    use super::*;
    use std::path::Path;

    use crate::ports::context::ContextStore;
    use crate::ports::events::EventLog;
    use crate::ports::memory::MemoryStore;
    use crate::ports::store::CompanyStore;
    use crate::ports::types::CompanyId;
    use crate::store::conformance;
    use crate::store::{FsCompanyStore, FsEventLog};

    /// The four port trait objects the conformance suite drives: fs company and
    /// event stores paired with the engine-backed cortex memory + context stores.
    type ConformanceStores = (
        Arc<dyn CompanyStore>,
        Arc<dyn EventLog>,
        Arc<CortexMemoryStore>,
        Arc<CortexContextStore>,
    );

    /// Builds engine-backed cortex memory+context stores rooted under a tempdir,
    /// plus fs company/event stores (over the same tempdir) for the two
    /// conformance slots the cortex backend does not implement.
    fn stores(dir: &Path) -> ConformanceStores {
        let (mem, ctx) = engine(dir.join("memory"));
        (
            Arc::new(FsCompanyStore::new(dir.to_path_buf())),
            Arc::new(FsEventLog::new(dir.to_path_buf())),
            mem,
            ctx,
        )
    }

    #[tokio::test]
    async fn conformance_isolation_by_company() {
        let dir = tempfile::tempdir().unwrap();
        let (store, events, mem, ctx) = stores(dir.path());
        conformance::assert_isolation_by_company(store, events, mem, ctx).await;
    }

    #[tokio::test]
    async fn conformance_export_totality() {
        let dir = tempfile::tempdir().unwrap();
        let (store, events, mem, ctx) = stores(dir.path());
        conformance::assert_export_totality(store, events, mem, ctx).await;
    }

    #[tokio::test]
    async fn conformance_context_chunk_stamps() {
        let dir = tempfile::tempdir().unwrap();
        let (_store, _events, _mem, ctx) = stores(dir.path());
        conformance::assert_context_chunk_stamps(ctx).await;
    }

    fn company() -> CompanyId {
        CompanyId::new("acme")
    }

    /// Data written through one engine instance survives dropping it and
    /// reopening a fresh engine at the same root — the durability contract, plus
    /// a real on-disk SQLite artifact under the company workspace.
    #[tokio::test]
    async fn persistence_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("memory");
        let id = company();

        let addr = {
            let (mem, ctx) = engine(root.clone());
            mem.save_trace(&id, CompressedTrace::now("c0", "the q3 report shipped"))
                .await
                .unwrap();
            ctx.put(
                &id,
                ContextChunk {
                    label: "notes/q3".into(),
                    body: "revenue grew in the q3 report".into(),
                },
            )
            .await
            .unwrap()
        };

        // The engine wrote a real on-disk database under the company workspace.
        let db = root.join("acme").join("memory_tree").join("chunks.db");
        assert!(
            db.exists(),
            "engine must persist a SQLite db on disk: {db:?}"
        );

        // A fresh engine at the same root recalls both the trace and the chunk.
        let (mem2, ctx2) = engine(root);
        let traces = mem2.recent_traces(&id, 10).await.unwrap();
        assert_eq!(traces.len(), 1, "trace survived reopen");
        assert_eq!(traces[0].cycle_id, "c0");
        let body = ctx2.peek(&id, &addr, None).await.unwrap();
        assert_eq!(
            body, "revenue grew in the q3 report",
            "chunk survived reopen"
        );
        let metas = ctx2.list(&id, "notes/").await.unwrap();
        assert_eq!(metas.len(), 1, "chunk metadata survived reopen");
    }

    /// Two companies never observe each other's data — physical isolation by
    /// separate per-company workspaces/databases.
    #[tokio::test]
    async fn two_company_isolation() {
        let dir = tempfile::tempdir().unwrap();
        let (mem, ctx) = engine(dir.path().join("memory"));
        let alpha = CompanyId::new("alpha");
        let beta = CompanyId::new("beta");

        mem.save_trace(&alpha, CompressedTrace::now("c0", "alpha only"))
            .await
            .unwrap();
        ctx.put(
            &alpha,
            ContextChunk {
                label: "doc/a".into(),
                body: "alpha secret body".into(),
            },
        )
        .await
        .unwrap();

        // beta was never written: every port reads empty for it.
        assert!(mem.recent_traces(&beta, 10).await.unwrap().is_empty());
        assert!(ctx.list(&beta, "").await.unwrap().is_empty());
        assert!(
            ctx.search(&beta, "alpha secret", 10)
                .await
                .unwrap()
                .is_empty(),
            "cross-company recall must not bleed"
        );

        // alpha still sees its own data.
        assert_eq!(mem.recent_traces(&alpha, 10).await.unwrap().len(), 1);
        assert_eq!(ctx.list(&alpha, "").await.unwrap().len(), 1);
    }

    /// The overlay selector picks the persistent engine when a data dir is
    /// present, and the offline in-memory backend otherwise. Proven by
    /// durability: only the engine path recalls data across a fresh overlay.
    #[tokio::test]
    async fn overlay_selection_engine_when_data_dir_set() {
        use crate::store::{MemoryBackend, StorageSettings, open_memory_overlay};

        let dir = tempfile::tempdir().unwrap();
        let id = company();
        let settings = StorageSettings {
            memory_backend: MemoryBackend::Tinycortex,
            data_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        };

        // First overlay writes a chunk, then is dropped.
        let addr = {
            let overlay = open_memory_overlay(&settings).unwrap().expect("overlay");
            overlay
                .context
                .put(
                    &id,
                    ContextChunk {
                        label: "notes/q3".into(),
                        body: "revenue grew in the q3 report".into(),
                    },
                )
                .await
                .unwrap()
        };

        // A fresh overlay from the same settings recalls it — proving the engine
        // (not the ephemeral in-memory backend) was selected.
        let overlay2 = open_memory_overlay(&settings).unwrap().expect("overlay");
        let body = overlay2.context.peek(&id, &addr, None).await.unwrap();
        assert_eq!(body, "revenue grew in the q3 report");

        // With no data dir, the in-memory backend is selected: a fresh overlay
        // starts empty (nothing persisted across instances).
        let ephemeral = StorageSettings {
            memory_backend: MemoryBackend::Tinycortex,
            data_dir: None,
            ..Default::default()
        };
        let a = open_memory_overlay(&ephemeral).unwrap().expect("overlay");
        a.context
            .put(
                &id,
                ContextChunk {
                    label: "notes/x".into(),
                    body: "ephemeral".into(),
                },
            )
            .await
            .unwrap();
        let b = open_memory_overlay(&ephemeral).unwrap().expect("overlay");
        assert!(
            b.context.list(&id, "").await.unwrap().is_empty(),
            "in-memory backend must not persist across instances"
        );
    }

    /// Degraded-mode search ranks chunks by lexical token-overlap, dropping
    /// zero-overlap chunks and ordering better matches first — no embeddings.
    #[tokio::test]
    async fn degraded_search_ranks_lexically() {
        let dir = tempfile::tempdir().unwrap();
        let (_mem, ctx) = engine(dir.path().join("memory"));
        let id = company();

        for (label, body) in [
            ("doc/a", "quarterly revenue growth strategy"),
            ("doc/b", "revenue only"),
            ("doc/c", "unrelated note"),
        ] {
            ctx.put(
                &id,
                ContextChunk {
                    label: label.into(),
                    body: body.into(),
                },
            )
            .await
            .unwrap();
        }

        let hits = ctx.search(&id, "revenue growth", 10).await.unwrap();
        assert_eq!(
            hits.len(),
            2,
            "the unrelated chunk scores zero and is dropped"
        );
        // The chunk matching both query terms outranks the one matching one.
        assert!(hits[0].score > hits[1].score);
        assert!(hits[0].snippet.contains("revenue"));

        // A limit truncates the ranked list.
        let one = ctx.search(&id, "revenue growth", 1).await.unwrap();
        assert_eq!(one.len(), 1);
    }

    #[tokio::test]
    async fn evict_archives_rather_than_destroys() {
        use crate::ports::types::EvictionPolicy;

        let dir = tempfile::tempdir().unwrap();
        let (mem, _ctx) = engine(dir.path().join("memory"));
        let id = company();
        for i in 0..5 {
            mem.save_trace(&id, CompressedTrace::now(format!("c{i}"), format!("s{i}")))
                .await
                .unwrap();
        }

        let archived = mem
            .evict(&id, EvictionPolicy::KeepRecent { n: 2 })
            .await
            .unwrap();
        assert_eq!(archived, 3, "three of five traces archived");
        let kept = mem.recent_traces(&id, 10).await.unwrap();
        assert_eq!(kept.len(), 2, "live set shrank to the recent two");
        assert_eq!(kept[1].cycle_id, "c4");

        // Hard delete reaches the archive, not just the live set.
        assert!(mem.hard_delete_trace(&id, "c0").await.unwrap());
        assert!(!mem.hard_delete_trace(&id, "missing").await.unwrap());
    }

    #[tokio::test]
    async fn redact_rewrites_traces_and_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let (mem, ctx) = engine(dir.path().join("memory"));
        let id = company();

        mem.save_trace(&id, CompressedTrace::now("c0", "contact bob-9 noted"))
            .await
            .unwrap();
        let addr = ctx
            .put(
                &id,
                ContextChunk {
                    label: "doc/a".into(),
                    body: "bob-9 twice: bob-9".into(),
                },
            )
            .await
            .unwrap();

        let replaced = mem.redact_all(&id, "bob-9", "[redacted]").await.unwrap();
        assert_eq!(replaced, 3, "one in the trace, two in the chunk");
        let body = ctx.peek(&id, &addr, None).await.unwrap();
        assert!(!body.contains("bob-9"));
        assert!(body.contains("[redacted]"));
        let trace = mem.recent_traces(&id, 1).await.unwrap();
        assert!(!trace[0].summary.contains("bob-9"));
    }
}
