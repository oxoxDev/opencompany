use super::*;

use serde_json::json;
use tempfile::TempDir;

use crate::harness::build::file_tools;
use crate::harness::turn_outputs::TurnOutputClaim;
use crate::ports::types::ChatOutputKind;
use crate::ports::workspace::{BlobStream, FolderClaim};
use crate::store::FsOps;

/// One seat: its own sandbox directory, its own belt, one shared store.
struct Seat {
    _sandbox: TempDir,
    tools: Vec<Box<dyn Tool>>,
}

impl Seat {
    fn new(store: Arc<dyn WorkspaceStore>, company: &CompanyId, agent_id: &str) -> Self {
        let sandbox = tempfile::tempdir().expect("sandbox");
        let promotion = Arc::new(WritePromotion::new(
            store,
            company.clone(),
            agent_id.to_string(),
            TurnOutputCollector::default(),
            sandbox.path().to_path_buf(),
        ));
        Self {
            tools: file_tools(sandbox.path(), Some(promotion)),
            _sandbox: sandbox,
        }
    }

    fn with_collector(
        store: Arc<dyn WorkspaceStore>,
        company: &CompanyId,
        agent_id: &str,
        outputs: TurnOutputCollector,
    ) -> Self {
        let sandbox = tempfile::tempdir().expect("sandbox");
        let promotion = Arc::new(WritePromotion::new(
            store,
            company.clone(),
            agent_id.to_string(),
            outputs,
            sandbox.path().to_path_buf(),
        ));
        Self {
            tools: file_tools(sandbox.path(), Some(promotion)),
            _sandbox: sandbox,
        }
    }

    async fn call(&self, name: &str, args: Value) -> ToolResult {
        self.tools
            .iter()
            .find(|tool| tool.name() == name)
            .unwrap_or_else(|| panic!("`{name}` is on the belt"))
            .execute(args)
            .await
            .expect("the tool call itself must not fail")
    }
}

fn store() -> (TempDir, Arc<dyn WorkspaceStore>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store: Arc<dyn WorkspaceStore> = Arc::new(FsOps::new(dir.path()));
    (dir, store)
}

async fn path_of(store: &Arc<dyn WorkspaceStore>, company: &CompanyId, id: &str) -> String {
    let nodes = store.tree(company).await.expect("tree");
    let by_id: HashMap<&str, &WorkspaceNode> =
        nodes.iter().map(|node| (node.id.as_str(), node)).collect();
    let node = by_id.get(id).expect("the promoted node is in the tree");
    render_path(node, &by_id).expect("the promoted node is addressable by path")
}

async fn body_of(store: &Arc<dyn WorkspaceStore>, company: &CompanyId, id: &str) -> String {
    store
        .read(company, id)
        .await
        .expect("read")
        .expect("the promoted node exists")
        .1
}

/// The whole feature: a native write an agent makes in its private sandbox
/// becomes a node the company can open, announced on the turn that wrote it.
#[tokio::test]
async fn a_native_write_lands_under_the_agents_own_folder_and_is_announced() {
    let (_dir, store) = store();
    let company = CompanyId::new("acme");
    let outputs = TurnOutputCollector::default();
    let seat = Seat::with_collector(store.clone(), &company, "writer", outputs.clone());
    let claim = outputs.claim();

    let result = claim
        .scoped(seat.call(
            "file_write",
            json!({ "path": "report.md", "content": "# Report\nShipped." }),
        ))
        .await;
    assert!(!result.is_error, "{}", result.output());

    let announced = claim.drain();
    assert_eq!(announced.len(), 1, "{announced:?}");
    assert_eq!(announced[0].kind, ChatOutputKind::WorkspaceNode);
    assert_eq!(announced[0].title, "agents/writer/report.md");
    assert_eq!(
        path_of(&store, &company, &announced[0].target_id).await,
        "agents/writer/report.md"
    );
    assert_eq!(
        body_of(&store, &company, &announced[0].target_id).await,
        "# Report\nShipped."
    );
}

/// The agent asked for a file and got one. Promotion is an addition on top of
/// that, so a store that refuses must not turn a completed write into a
/// failure the model has to reason about.
#[tokio::test]
async fn a_refusing_store_leaves_the_inner_write_successful() {
    let (_dir, real) = store();
    let store: Arc<dyn WorkspaceStore> = Arc::new(RefusesFolders {
        inner: real.clone(),
    });
    let company = CompanyId::new("acme");
    let outputs = TurnOutputCollector::default();
    let seat = Seat::with_collector(store, &company, "writer", outputs.clone());
    let claim = outputs.claim();

    let result = claim
        .scoped(seat.call(
            "file_write",
            json!({ "path": "report.md", "content": "still written" }),
        ))
        .await;

    assert!(!result.is_error, "{}", result.output());
    assert!(result.output().contains("report.md"), "{}", result.output());
    assert!(claim.drain().is_empty(), "nothing was promoted");
    assert!(
        real.tree(&company).await.expect("tree").is_empty(),
        "a refused promotion must leave no half-built tree"
    );
}

/// The observed incident: four seats wrote the same filename. Promoting into
/// one flat namespace would have made this fix its own silent-overwrite path,
/// so each seat's copy must land on its own node under its own folder.
#[tokio::test]
async fn two_seats_writing_the_same_filename_get_two_distinct_nodes() {
    let (_dir, store) = store();
    let company = CompanyId::new("acme");

    let first = TurnOutputCollector::default();
    let second = TurnOutputCollector::default();
    let one = Seat::with_collector(store.clone(), &company, "seat-one", first.clone());
    let two = Seat::with_collector(store.clone(), &company, "seat-two", second.clone());

    let one_claim = first.claim();
    let two_claim = second.claim();
    one_claim
        .scoped(one.call(
            "file_write",
            json!({ "path": "plan.md", "content": "one's plan" }),
        ))
        .await;
    two_claim
        .scoped(two.call(
            "file_write",
            json!({ "path": "plan.md", "content": "two's plan" }),
        ))
        .await;

    let one_node = only(one_claim).target_id;
    let two_node = only(two_claim).target_id;
    assert_ne!(one_node, two_node, "one seat overwrote the other");
    assert_eq!(
        path_of(&store, &company, &one_node).await,
        "agents/seat-one/plan.md"
    );
    assert_eq!(
        path_of(&store, &company, &two_node).await,
        "agents/seat-two/plan.md"
    );
    assert_eq!(body_of(&store, &company, &one_node).await, "one's plan");
    assert_eq!(body_of(&store, &company, &two_node).await, "two's plan");
}

/// A write with no turn to attribute it to — a background job, a console-driven
/// call — must mint nothing. A node created there would either be announced on
/// whichever reply came next or stand in the tree with nothing pointing at it.
#[tokio::test]
async fn a_write_outside_a_claimed_turn_promotes_nothing() {
    let (_dir, store) = store();
    let company = CompanyId::new("acme");
    let seat = Seat::new(store.clone(), &company, "writer");

    let result = seat
        .call(
            "file_write",
            json!({ "path": "report.md", "content": "unscoped" }),
        )
        .await;

    assert!(!result.is_error, "{}", result.output());
    assert!(
        store.tree(&company).await.expect("tree").is_empty(),
        "an unscoped write must not reach the company tree"
    );
}

fn only(claim: TurnOutputClaim) -> crate::ports::types::ChatOutput {
    let mut outputs = claim.drain();
    assert_eq!(outputs.len(), 1, "{outputs:?}");
    outputs.remove(0)
}

/// A store that will not mint a folder, so promotion fails at its first step.
struct RefusesFolders {
    inner: Arc<dyn WorkspaceStore>,
}

#[async_trait]
impl WorkspaceStore for RefusesFolders {
    async fn adopt_or_create_folder(
        &self,
        _company: &CompanyId,
        _parent: Option<&str>,
        _name: &str,
        _origin: WorkspaceOrigin,
    ) -> crate::Result<FolderClaim> {
        Err(crate::error::OpenCompanyError::Store(
            "the workspace is unavailable".to_string(),
        ))
    }
    async fn tree(&self, company: &CompanyId) -> crate::Result<Vec<WorkspaceNode>> {
        self.inner.tree(company).await
    }
    async fn read(
        &self,
        company: &CompanyId,
        id: &str,
    ) -> crate::Result<Option<(WorkspaceNode, String)>> {
        self.inner.read(company, id).await
    }
    async fn read_capped(
        &self,
        company: &CompanyId,
        id: &str,
        max_bytes: u64,
    ) -> crate::Result<Option<(WorkspaceNode, String, u64)>> {
        self.inner.read_capped(company, id, max_bytes).await
    }
    async fn write_with_revision(
        &self,
        company: &CompanyId,
        id: &str,
        content: &str,
        author: WorkspaceOrigin,
        expected_updated_at: Option<u64>,
    ) -> crate::Result<WorkspaceNode> {
        self.inner
            .write_with_revision(company, id, content, author, expected_updated_at)
            .await
    }
    async fn create(
        &self,
        company: &CompanyId,
        node: &WorkspaceNode,
        content: Option<&str>,
    ) -> crate::Result<()> {
        self.inner.create(company, node, content).await
    }
    async fn create_binary(
        &self,
        company: &CompanyId,
        node: &WorkspaceNode,
        bytes: &[u8],
    ) -> crate::Result<WorkspaceNode> {
        self.inner.create_binary(company, node, bytes).await
    }
    async fn write_binary(
        &self,
        company: &CompanyId,
        id: &str,
        bytes: &[u8],
        mime: Option<&str>,
        author: WorkspaceOrigin,
    ) -> crate::Result<WorkspaceNode> {
        self.inner
            .write_binary(company, id, bytes, mime, author)
            .await
    }
    async fn read_bytes(
        &self,
        company: &CompanyId,
        id: &str,
    ) -> crate::Result<Option<(WorkspaceNode, BlobStream)>> {
        self.inner.read_bytes(company, id).await
    }
    async fn rename_move(
        &self,
        company: &CompanyId,
        id: &str,
        name: Option<&str>,
        parent: Option<Option<&str>>,
    ) -> crate::Result<WorkspaceNode> {
        self.inner.rename_move(company, id, name, parent).await
    }
    async fn swap_files(
        &self,
        company: &CompanyId,
        expected_id: Option<&str>,
        replacement_id: &str,
        name: &str,
    ) -> crate::Result<Option<WorkspaceNode>> {
        self.inner
            .swap_files(company, expected_id, replacement_id, name)
            .await
    }
    async fn delete(&self, company: &CompanyId, id: &str) -> crate::Result<bool> {
        self.inner.delete(company, id).await
    }
    async fn is_empty(&self, company: &CompanyId) -> crate::Result<bool> {
        self.inner.is_empty(company).await
    }
}
