//! Tests for the own-folder lifecycle tools (issue #671).

use std::sync::Arc;

use super::*;
use crate::company::workspace_scaffold::{ensure_agent_folder, ensure_workspace_scaffold};
use crate::harness::workspace_tools::tests::{TEST_AGENT, agent_origin, file, folder, text, ws};
use crate::ports::artifacts::{ArtifactKind, ArtifactRecord, ArtifactStore};
use crate::ports::types::CompanyId;
use crate::ports::workspace::{
    BlobStream, FolderClaim, WorkspaceNode, WorkspaceOrigin, WorkspaceStore,
};
use crate::store::FsOps;

/// The revision [`file`] stamps, and therefore the CAS token every note in
/// these tests answers to.
const NOTE_REV: u64 = 2_000;
/// The revision [`folder`] stamps.
const FOLDER_REV: u64 = 1_000;

/// A live workspace shaped like the one the lifecycle tools were written for:
/// the boot scaffold, this agent's own home holding a note and an empty folder,
/// a teammate's home holding a note, plus `standards/` and a root `README.md`
/// to have something outside the agent's reach.
///
/// Carries an **artifact store** alongside the workspace store, because
/// `build_agent` always wires one and `workspace_delete` reads it to decide
/// whether a removal is recoverable. A fixture without one would exercise only
/// the [`History::Unknown`] branch and quietly stop checking the sentence the
/// agent actually reads.
struct Home {
    _dir: tempfile::TempDir,
    store: Arc<dyn WorkspaceStore>,
    artifacts: Arc<dyn ArtifactStore>,
    company: CompanyId,
}

async fn own_home(company: &str) -> Home {
    let dir = tempfile::tempdir().expect("tempdir");
    let ops = Arc::new(FsOps::new(dir.path()));
    let store: Arc<dyn WorkspaceStore> = ops.clone();
    let artifacts: Arc<dyn ArtifactStore> = ops;
    let id = CompanyId::new(company);

    store
        .create(&id, &folder("f-standards", "standards", None), None)
        .await
        .expect("folder");
    store
        .create(
            &id,
            &file("n-eng", "Engineering standards.md", Some("f-standards")),
            Some("# Engineering\nReview every PR."),
        )
        .await
        .expect("note");
    store
        .create(&id, &file("n-readme", "readme.md", None), Some("# Root"))
        .await
        .expect("readme");
    ensure_workspace_scaffold(store.as_ref(), &id)
        .await
        .unwrap();

    let mine = ensure_agent_folder(store.as_ref(), &id, TEST_AGENT)
        .await
        .unwrap();
    // Agent-authored on purpose: a rename must be provably origin-preserving,
    // and an operator-stamped fixture could not tell a preserved stamp from a
    // restamped one.
    store
        .create(
            &id,
            &WorkspaceNode {
                created_by: agent_origin(),
                updated_by: agent_origin(),
                ..file("n-draft", "Draft.md", Some(&mine))
            },
            Some("# Draft"),
        )
        .await
        .unwrap();
    store
        .create(&id, &folder("f-archive", "archive", Some(&mine)), None)
        .await
        .unwrap();

    let theirs = ensure_agent_folder(store.as_ref(), &id, "cmo")
        .await
        .unwrap();
    store
        .create(
            &id,
            &file("n-mate", "Plan.md", Some(&theirs)),
            Some("# Plan"),
        )
        .await
        .unwrap();

    Home {
        _dir: dir,
        store,
        artifacts,
        company: id,
    }
}

impl Home {
    /// The delete tool as the builder wires it: this agent, this company, the
    /// artifact store attached.
    fn deleter(&self) -> WorkspaceDeleteTool {
        WorkspaceDeleteTool::new(
            ws(self.store.clone(), self.company.clone())
                .with_artifacts(Some(self.artifacts.clone())),
        )
    }

    fn renamer(&self) -> WorkspaceRenameTool {
        WorkspaceRenameTool::new(ws(self.store.clone(), self.company.clone()))
    }

    async fn tree(&self) -> Vec<WorkspaceNode> {
        self.store.tree(&self.company).await.unwrap()
    }

    /// Whether a node id is still in the tree.
    async fn has(&self, id: &str) -> bool {
        self.tree().await.iter().any(|node| node.id == id)
    }

    async fn read(&self, id: &str) -> (WorkspaceNode, String) {
        self.store
            .read(&self.company, id)
            .await
            .unwrap()
            .expect("the node is still there")
    }

    async fn node(&self, id: &str) -> WorkspaceNode {
        self.read(id).await.0
    }

    /// This agent's home folder id, read live.
    async fn home_id(&self) -> String {
        self.tree()
            .await
            .iter()
            .find(|n| n.name == TEST_AGENT)
            .expect("the home folder")
            .id
            .clone()
    }

    /// Add a note directly inside this agent's own folder.
    async fn add_own(&self, node: WorkspaceNode, content: &str) {
        let parent = self.home_id().await;
        self.store
            .create(
                &self.company,
                &WorkspaceNode {
                    parent_id: Some(parent),
                    ..node
                },
                Some(content),
            )
            .await
            .unwrap();
    }

    /// Add a binary node directly inside this agent's own folder.
    async fn add_own_binary(&self, id: &str, name: &str, bytes: &[u8]) {
        let parent = self.home_id().await;
        let node = WorkspaceNode {
            mime: Some("image/png".to_string()),
            ..file(id, name, Some(&parent))
        };
        self.store
            .create_binary(&self.company, &node, bytes)
            .await
            .unwrap();
    }

    /// Record `node_id` as a published deliverable, so deleting it is the
    /// recoverable case.
    async fn publish(&self, artifact_id: &str, node_id: &str, body: &str) {
        let mut record = ArtifactRecord::new(
            artifact_id,
            "t-1",
            "Launch spec",
            ArtifactKind::Markdown,
            body,
            TEST_AGENT,
            1,
        );
        record.stamp_workspace_node(node_id);
        self.artifacts.upsert(&self.company, &record).await.unwrap();
    }
}

/// An artifact store whose `list` always errors, so `history_of`'s
/// `published_record_for_node` call fails on every delete.
struct FailingArtifacts;

#[async_trait::async_trait]
impl ArtifactStore for FailingArtifacts {
    async fn list(
        &self,
        _company: &CompanyId,
        _task_id: Option<&str>,
    ) -> crate::Result<Vec<crate::ports::artifacts::ArtifactRecord>> {
        Err(crate::error::OpenCompanyError::Store("boom".into()))
    }
    async fn get(
        &self,
        _company: &CompanyId,
        _id: &str,
    ) -> crate::Result<Option<crate::ports::artifacts::ArtifactRecord>> {
        unreachable!("history_of only lists")
    }
    async fn upsert(
        &self,
        _company: &CompanyId,
        _artifact: &crate::ports::artifacts::ArtifactRecord,
    ) -> crate::Result<()> {
        unreachable!("history_of only lists")
    }
    async fn delete(&self, _company: &CompanyId, _id: &str) -> crate::Result<bool> {
        unreachable!("history_of only lists")
    }
}

// ---------------------------------------------------------------------------
// workspace_delete — the happy paths
// ---------------------------------------------------------------------------

/// The whole point of the issue: an agent can clear a superseded draft out of
/// its own folder, and is told plainly that nothing will bring it back.
#[tokio::test]
async fn deleting_your_own_note_removes_it_and_names_the_loss_as_permanent() {
    let home = own_home("acme").await;

    let out = home
        .deleter()
        .execute(json!({ "path": "agents/ceo/Draft.md", "expected_updated_at": NOTE_REV }))
        .await
        .unwrap();
    assert!(!out.is_error, "{}", text(&out));
    let message = text(&out);
    assert!(message.contains("agents/ceo/Draft.md"), "{message}");
    assert!(
        message.contains("permanent"),
        "a directly-created note's deletion is final and must say so: {message}"
    );
    assert!(!home.has("n-draft").await, "the note survived the delete");
}

/// An empty folder of your own is deletable — which is what makes the
/// non-empty refusal below an instruction rather than a dead end.
#[tokio::test]
async fn an_empty_folder_of_your_own_can_be_deleted() {
    let home = own_home("acme").await;

    let out = home
        .deleter()
        .execute(json!({ "path": "agents/ceo/archive", "expected_updated_at": FOLDER_REV }))
        .await
        .unwrap();
    assert!(!out.is_error, "{}", text(&out));
    assert!(text(&out).contains("folder"), "{}", text(&out));
    assert!(!home.has("f-archive").await);
}

/// A published note's history lives on its artifact chain, so its deletion is
/// the **recoverable** case — and the message has to say the opposite of what
/// it says for an ordinary note, or the agent will treat the two identically.
#[tokio::test]
async fn deleting_a_published_note_says_its_history_survives_in_artifacts() {
    let home = own_home("acme").await;
    home.publish("art-1", "n-draft", "# Draft").await;

    let out = home
        .deleter()
        .execute(json!({ "id": "n-draft", "expected_updated_at": NOTE_REV }))
        .await
        .unwrap();
    assert!(!out.is_error, "{}", text(&out));
    let message = text(&out);
    assert!(message.contains("Artifacts"), "{message}");
    assert!(
        !message.contains("permanent"),
        "a published note is the recoverable case: {message}"
    );
    assert!(!home.has("n-draft").await);

    // The chain is untouched, dangling node id and all — the same state the
    // operator's own DELETE route leaves behind today, and deliberately not
    // "repaired" here.
    let stored = home
        .artifacts
        .get(&home.company, "art-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.workspace_node_id(), Some("n-draft"));
    assert_eq!(stored.versions.len(), 1);
}

/// When the artifact store cannot answer whether a node was published, the
/// delete still proceeds (documented at the `history_of` call site: "the
/// delete proceeds and the agent is told nothing either way") — but it must
/// fall back to the honest [`History::Unknown`] note, never fabricate
/// "permanent" or "survives in Artifacts" from a call that never completed.
#[tokio::test]
async fn a_publish_lookup_failure_still_deletes_and_claims_no_history_either_way() {
    let home = own_home("acme").await;
    let deleter = WorkspaceDeleteTool::new(
        ws(home.store.clone(), home.company.clone())
            .with_artifacts(Some(Arc::new(FailingArtifacts) as Arc<dyn ArtifactStore>)),
    );

    let out = deleter
        .execute(json!({ "path": "agents/ceo/Draft.md", "expected_updated_at": NOTE_REV }))
        .await
        .unwrap();
    assert!(
        !out.is_error,
        "an unreadable publish history must not block tidying your own folder: {}",
        text(&out)
    );
    let message = text(&out);
    assert!(!home.has("n-draft").await, "the note survived the delete");
    assert!(
        !message.contains("permanent") && !message.contains("Artifacts"),
        "an unresolved lookup must not fabricate either history claim: {message}"
    );
}

/// The port promises a binary node's payload goes with it (issue #553). This
/// pins that the agent-facing path reaches that promise rather than refusing
/// bytes the way `workspace_write` does.
#[tokio::test]
async fn a_binary_node_in_your_own_folder_deletes_with_its_payload() {
    let home = own_home("acme").await;
    home.add_own_binary("n-own-img", "chart.png", &[0x89, b'P', b'N', b'G'])
        .await;

    let out = home
        .deleter()
        .execute(json!({ "path": "agents/ceo/chart.png", "expected_updated_at": NOTE_REV }))
        .await
        .unwrap();
    assert!(!out.is_error, "{}", text(&out));
    assert!(
        home.store
            .read_bytes(&home.company, "n-own-img")
            .await
            .unwrap()
            .is_none(),
        "the payload outlived its node"
    );
}

// ---------------------------------------------------------------------------
// workspace_delete — the scope gate
// ---------------------------------------------------------------------------

/// Shared guidance is not the agent's to remove, and a refusal must leave it
/// byte-identical rather than merely un-deleted.
#[tokio::test]
async fn a_note_outside_your_own_folder_is_refused_and_left_alone() {
    let home = own_home("acme").await;

    let out = home
        .deleter()
        .execute(json!({
            "path": "standards/engineering-standards.md",
            "expected_updated_at": NOTE_REV,
        }))
        .await
        .unwrap();
    assert!(out.is_error, "{}", text(&out));
    assert!(
        text(&out).contains("outside your own folder"),
        "{}",
        text(&out)
    );
    assert_eq!(
        home.read("n-eng").await.1,
        "# Engineering\nReview every PR."
    );
}

/// The gate is checked on the **resolved** node, not on the argument — so the
/// `id` form cannot walk around a `path` form's refusal. This is the bypass the
/// whole scope argument rests on.
#[tokio::test]
async fn an_id_that_names_a_note_outside_your_folder_refuses_like_its_path_would() {
    let home = own_home("acme").await;
    let tool = home.deleter();

    for node in ["n-eng", "n-readme", "n-mate"] {
        let out = tool
            .execute(json!({ "id": node, "expected_updated_at": NOTE_REV }))
            .await
            .unwrap();
        assert!(out.is_error, "`{node}` was allowed: {}", text(&out));
        assert!(
            text(&out).contains("outside your own folder"),
            "`{node}`: {}",
            text(&out)
        );
        assert!(home.has(node).await, "`{node}` was deleted");
    }
}

/// A teammate's folder is a teammate's, whichever end of it is named — and the
/// `Agents` root itself belongs to nobody.
#[tokio::test]
async fn a_teammates_home_and_its_contents_are_refused() {
    let home = own_home("acme").await;
    let tool = home.deleter();

    for path in ["agents/cmo", "agents/cmo/Plan.md", "Agents"] {
        let out = tool
            .execute(json!({ "path": path, "expected_updated_at": NOTE_REV }))
            .await
            .unwrap();
        assert!(out.is_error, "`{path}` was allowed: {}", text(&out));
        assert!(
            text(&out).contains("outside your own folder"),
            "`{path}`: {}",
            text(&out)
        );
    }
    assert!(home.has("n-mate").await);
}

/// The home folder gets a refusal of its own, and it has to explain the actual
/// consequence: `ensure_agent_folder` resolves the home **by name**, so
/// deleting it does not merely remove a folder — it makes the next publish mint
/// a second, empty one and forks the company's view of where this agent's work
/// lives.
#[tokio::test]
async fn your_own_home_folder_gets_its_own_refusal() {
    let home = own_home("acme").await;

    let out = home
        .deleter()
        .execute(json!({ "path": "agents/ceo", "expected_updated_at": FOLDER_REV }))
        .await
        .unwrap();
    assert!(out.is_error, "{}", text(&out));
    let message = text(&out);
    assert!(message.contains("your own folder itself"), "{message}");
    assert!(
        !message.contains("outside your own folder"),
        "the home must not get the generic outside-scope message: {message}"
    );
    assert!(
        message.contains("Act on what is inside it"),
        "the refusal must name the next useful action: {message}"
    );
    assert!(home.tree().await.iter().any(|n| n.name == TEST_AGENT));
}

/// One node per call, in the destructive direction. A recursive delete would be
/// one approval card naming one path that takes an unbounded amount of work
/// with it — so a non-empty folder is refused, having changed nothing at all.
#[tokio::test]
async fn a_folder_that_still_holds_anything_is_refused_and_deletes_nothing() {
    let home = own_home("acme").await;
    home.store
        .create(
            &home.company,
            &file("n-in-archive", "old.md", Some("f-archive")),
            Some("old"),
        )
        .await
        .unwrap();
    let before = home.tree().await.len();

    let out = home
        .deleter()
        .execute(json!({ "path": "agents/ceo/archive", "expected_updated_at": FOLDER_REV }))
        .await
        .unwrap();
    assert!(out.is_error, "{}", text(&out));
    let message = text(&out);
    assert!(message.contains("still holds 1 node(s)"), "{message}");
    assert!(
        message.contains("one call each"),
        "the refusal must say how to proceed: {message}"
    );
    assert_eq!(
        home.tree().await.len(),
        before,
        "a refused folder delete removed something"
    );
}

// ---------------------------------------------------------------------------
// workspace_delete — the revision guard
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_delete_without_a_revision_is_refused() {
    let home = own_home("acme").await;

    let out = home
        .deleter()
        .execute(json!({ "path": "agents/ceo/Draft.md" }))
        .await
        .unwrap();
    assert!(out.is_error, "{}", text(&out));
    assert!(text(&out).contains("expected_updated_at"), "{}", text(&out));
    assert!(home.has("n-draft").await);
}

/// A note edited in the console since the agent last read it is not deleted on
/// the strength of a view that predates the change.
#[tokio::test]
async fn a_stale_revision_is_refused_and_names_the_current_one() {
    let home = own_home("acme").await;

    let out = home
        .deleter()
        .execute(json!({ "path": "agents/ceo/Draft.md", "expected_updated_at": 1 }))
        .await
        .unwrap();
    assert!(out.is_error, "{}", text(&out));
    let message = text(&out);
    assert!(message.contains(&NOTE_REV.to_string()), "{message}");
    assert!(
        message.contains("do NOT retry with the same expected_updated_at"),
        "{message}"
    );
    assert!(home.has("n-draft").await);
}

/// Models stringify numbers constantly. `workspace_write` learned this the
/// expensive way; the same leniency is inherited rather than re-litigated.
#[tokio::test]
async fn a_revision_is_accepted_as_a_number_or_a_string() {
    for revision in [json!(NOTE_REV), json!(NOTE_REV.to_string())] {
        let home = own_home("acme").await;
        let out = home
            .deleter()
            .execute(json!({
                "path": "agents/ceo/Draft.md",
                "expected_updated_at": revision,
            }))
            .await
            .unwrap();
        assert!(!out.is_error, "{revision}: {}", text(&out));
        assert!(!home.has("n-draft").await, "{revision}");
    }

    // A string that is not a number is still not a revision.
    let home = own_home("acme").await;
    let out = home
        .deleter()
        .execute(json!({ "path": "agents/ceo/Draft.md", "expected_updated_at": "latest" }))
        .await
        .unwrap();
    assert!(out.is_error, "{}", text(&out));
    assert!(home.has("n-draft").await);
}

/// The tenancy argument the module rests on, applied to the destructive tool: a
/// node id borrowed from another company is simply absent from this company's
/// index, so the store is never asked about it.
#[tokio::test]
async fn tenancy_a_borrowed_node_id_cannot_be_deleted_by_another_company() {
    let home = own_home("acme").await;
    let intruder = WorkspaceDeleteTool::new(
        ws(home.store.clone(), CompanyId::new("other"))
            .with_artifacts(Some(home.artifacts.clone())),
    );

    let out = intruder
        .execute(json!({ "id": "n-draft", "expected_updated_at": NOTE_REV }))
        .await
        .unwrap();
    assert!(out.is_error, "{}", text(&out));
    assert!(
        text(&out).contains("No workspace note matches"),
        "{}",
        text(&out)
    );
    assert!(home.has("n-draft").await);
}

// ---------------------------------------------------------------------------
// workspace_rename
// ---------------------------------------------------------------------------

/// A rename is content-, id- and authorship-preserving. All three matter: the
/// id is what every artifact record points at, and the origins are what keep an
/// agent's work attributed after somebody tidies it.
#[tokio::test]
async fn renaming_your_own_note_keeps_its_body_id_and_authorship() {
    let home = own_home("acme").await;

    let out = home
        .renamer()
        .execute(json!({ "path": "agents/ceo/Draft.md", "new_name": "Q3 launch brief.md" }))
        .await
        .unwrap();
    assert!(!out.is_error, "{}", text(&out));
    let message = text(&out);
    assert!(
        message.contains("agents/ceo/q3-launch-brief.md"),
        "{message}"
    );
    assert!(
        message.contains("rev="),
        "the move restamps the revision, so it has to hand the new one back: {message}"
    );

    let (node, body) = home.read("n-draft").await;
    assert_eq!(node.name, "q3-launch-brief.md");
    assert_eq!(body, "# Draft");
    assert_eq!(node.created_by, agent_origin());
    assert_eq!(node.updated_by, agent_origin());
}

/// `workspace_rename` takes no `expected_updated_at` at all — unlike
/// `workspace_delete` and `workspace_write`, which both refuse a call whose
/// revision has gone stale. Two renames back to back on the same node prove
/// the absence: the second runs against a node the first has already
/// restamped, and nothing about that intervening change is surfaced to it —
/// it neither refuses nor is told the node moved since whatever the caller
/// last read.
#[tokio::test]
async fn a_second_rename_proceeds_with_no_staleness_signal_from_the_first() {
    let home = own_home("acme").await;

    let first = home
        .renamer()
        .execute(json!({ "path": "agents/ceo/Draft.md", "new_name": "First rename.md" }))
        .await
        .unwrap();
    assert!(!first.is_error, "{}", text(&first));
    let stamped_after_first = home.node("n-draft").await.updated_at_millis;

    // Nothing here carries the revision the first rename just stamped — the
    // schema has no field for it — so this call cannot even express "as of
    // what I last saw".
    let second = home
        .renamer()
        .execute(json!({ "id": "n-draft", "new_name": "Second rename.md" }))
        .await
        .unwrap();
    assert!(
        !second.is_error,
        "a rename immediately following another must not be silently blocked either: {}",
        text(&second)
    );

    let (node, _body) = home.read("n-draft").await;
    assert_eq!(node.name, "second-rename.md");
    assert!(
        node.updated_at_millis >= stamped_after_first,
        "the second rename ran unconditionally against whatever the node had become, with no \
         check against what the caller believed it still was"
    );
}

/// Filing a note under a subfolder you made is the other half of tidying, and
/// the destination may be either the home itself or anything inside it.
#[tokio::test]
async fn a_note_can_be_moved_into_and_back_out_of_a_subfolder_of_your_own() {
    let home = own_home("acme").await;
    let tool = home.renamer();

    let out = tool
        .execute(json!({ "path": "agents/ceo/Draft.md", "new_parent": "agents/ceo/archive" }))
        .await
        .unwrap();
    assert!(!out.is_error, "{}", text(&out));
    assert_eq!(
        home.node("n-draft").await.parent_id.as_deref(),
        Some("f-archive")
    );

    // …and back up into the home itself, which is a legal *destination* even
    // though it is never a legal target for a delete or a rename.
    let out = tool
        .execute(json!({ "id": "n-draft", "new_parent": "agents/ceo" }))
        .await
        .unwrap();
    assert!(!out.is_error, "{}", text(&out));
    assert_eq!(
        home.node("n-draft").await.parent_id,
        Some(home.home_id().await)
    );
}

/// A rename and a move in one call, since the tool advertises both.
#[tokio::test]
async fn a_name_and_a_parent_can_change_in_one_call() {
    let home = own_home("acme").await;

    let out = home
        .renamer()
        .execute(json!({
            "path": "agents/ceo/Draft.md",
            "new_name": "superseded.md",
            "new_parent": "agents/ceo/archive",
        }))
        .await
        .unwrap();
    assert!(!out.is_error, "{}", text(&out));
    assert!(
        text(&out).contains("agents/ceo/archive/superseded.md"),
        "{}",
        text(&out)
    );
    let node = home.node("n-draft").await;
    assert_eq!(node.name, "superseded.md");
    assert_eq!(node.parent_id.as_deref(), Some("f-archive"));
}

/// A folder must never land inside its own subtree: `archive` → `archive/deep`
/// would make the tree unreadable for every agent from then on. Refused before
/// the store is asked to create the cycle.
#[tokio::test]
async fn a_folder_cannot_be_moved_into_its_own_subfolder() {
    let home = own_home("acme").await;
    let mine = home.home_id().await;
    home.store
        .create(
            &home.company,
            &folder("f-deep", "deep", Some("f-archive")),
            None,
        )
        .await
        .unwrap();

    // By path, into the descendant.
    let out = home
        .renamer()
        .execute(json!({
            "path": "agents/ceo/archive",
            "new_parent": "agents/ceo/archive/deep",
        }))
        .await
        .unwrap();
    assert!(out.is_error, "{}", text(&out));
    assert!(
        text(&out).contains("unreadable"),
        "the refusal says the tree would be unreadable: {}",
        text(&out)
    );

    // By id, into itself — the same guard at the end of the ancestry walk.
    let out = home
        .renamer()
        .execute(json!({ "id": "f-archive", "new_parent": "agents/ceo/archive" }))
        .await
        .unwrap();
    assert!(out.is_error, "{}", text(&out));

    // The tree is untouched.
    assert_eq!(
        home.node("f-archive").await.parent_id.as_deref(),
        Some(mine.as_str())
    );
}

/// A binary node renames like any other — the port moves the payload with it.
#[tokio::test]
async fn a_binary_node_can_be_renamed_and_keeps_its_payload() {
    let home = own_home("acme").await;
    home.add_own_binary("n-own-img", "chart.png", &[0x89, b'P', b'N', b'G'])
        .await;

    let out = home
        .renamer()
        .execute(json!({ "path": "agents/ceo/chart.png", "new_name": "q3-chart.png" }))
        .await
        .unwrap();
    assert!(!out.is_error, "{}", text(&out));
    let (node, _) = home
        .store
        .read_bytes(&home.company, "n-own-img")
        .await
        .unwrap()
        .expect("the payload survived the rename");
    assert_eq!(node.name, "q3-chart.png");
    assert_eq!(node.size, Some(4));
}

/// The scope gate is the one delete uses, checked on the resolved node — so
/// both argument forms refuse identically outside the agent's folder.
#[tokio::test]
async fn renaming_anything_outside_your_own_folder_is_refused() {
    let home = own_home("acme").await;
    let tool = home.renamer();

    for args in [
        json!({ "path": "standards/engineering-standards.md", "new_name": "mine.md" }),
        json!({ "id": "n-eng", "new_name": "mine.md" }),
        json!({ "path": "agents/cmo/Plan.md", "new_name": "mine.md" }),
        json!({ "id": "n-mate", "new_parent": "agents/ceo" }),
        json!({ "path": "readme.md", "new_name": "mine.md" }),
    ] {
        let out = tool.execute(args.clone()).await.unwrap();
        assert!(out.is_error, "{args} was allowed: {}", text(&out));
        assert!(
            text(&out).contains("outside your own folder"),
            "{args}: {}",
            text(&out)
        );
    }
    assert_eq!(home.node("n-eng").await.name, "Engineering standards.md");
    assert_eq!(home.node("n-mate").await.name, "Plan.md");
}

/// The home folder is the agent's identity anchor for renames too, and for the
/// sharper reason: its name **is** the lookup key.
#[tokio::test]
async fn your_own_home_folder_cannot_be_renamed() {
    let home = own_home("acme").await;

    let out = home
        .renamer()
        .execute(json!({ "path": "agents/ceo", "new_name": "chief-exec" }))
        .await
        .unwrap();
    assert!(out.is_error, "{}", text(&out));
    assert!(
        text(&out).contains("your own folder itself"),
        "{}",
        text(&out)
    );
    assert!(home.tree().await.iter().any(|n| n.name == TEST_AGENT));
}

/// Moving to the workspace root is always refused, and gets the scope answer
/// rather than the parse error an empty path would otherwise produce — the
/// reason is that the root is not the agent's, not that `""` is malformed.
#[tokio::test]
async fn moving_to_the_workspace_root_is_refused_with_the_scope_reason() {
    let home = own_home("acme").await;
    let tool = home.renamer();

    for root in ["", "/", "  ", "//"] {
        let out = tool
            .execute(json!({ "path": "agents/ceo/Draft.md", "new_parent": root }))
            .await
            .unwrap();
        assert!(out.is_error, "`{root}` was allowed: {}", text(&out));
        assert!(
            text(&out).contains("workspace root is outside your own folder"),
            "`{root}`: {}",
            text(&out)
        );
    }
    assert_eq!(
        home.node("n-draft").await.parent_id,
        Some(home.home_id().await),
        "a refused move relocated the note anyway"
    );
}

/// A destination outside the agent's folder is refused even when the node
/// itself is inside it — otherwise "confined to your own folder" would only be
/// half true, and the escape would be a one-call move.
#[tokio::test]
async fn moving_your_own_note_out_of_your_folder_is_refused() {
    let home = own_home("acme").await;
    let tool = home.renamer();

    for parent in ["standards", "agents", "agents/cmo"] {
        let out = tool
            .execute(json!({ "path": "agents/ceo/Draft.md", "new_parent": parent }))
            .await
            .unwrap();
        assert!(out.is_error, "`{parent}` was allowed: {}", text(&out));
        assert!(
            text(&out).contains("outside your own folder"),
            "`{parent}`: {}",
            text(&out)
        );
    }
    assert_eq!(
        home.node("n-draft").await.parent_id,
        Some(home.home_id().await),
        "a refused move relocated the note anyway"
    );
}

/// The `fs` backend rejects an unsafe name, but sqlite and mongodb do not — so
/// the tool layer validates rather than relying on whichever backend is wired.
#[tokio::test]
async fn a_new_name_that_is_not_a_single_segment_is_refused() {
    let home = own_home("acme").await;
    let tool = home.renamer();

    for name in ["..", ".", "a/b", "a\\b", "sub/dir/note.md"] {
        let out = tool
            .execute(json!({ "path": "agents/ceo/Draft.md", "new_name": name }))
            .await
            .unwrap();
        assert!(out.is_error, "`{name}` was allowed: {}", text(&out));
        assert!(
            text(&out).contains("single path segment"),
            "`{name}`: {}",
            text(&out)
        );
    }
    assert_eq!(home.node("n-draft").await.name, "Draft.md");
}

/// Two nodes at one path make it ambiguous for every agent from then on — the
/// argument `workspace_create` refuses on, applied to the other way of reaching
/// the same state.
#[tokio::test]
async fn a_rename_onto_an_occupied_path_is_refused_and_changes_nothing() {
    let home = own_home("acme").await;
    home.add_own(file("n-other", "Notes.md", None), "notes")
        .await;

    let out = home
        .renamer()
        .execute(json!({ "path": "agents/ceo/Draft.md", "new_name": "Notes.md" }))
        .await
        .unwrap();
    assert!(out.is_error, "{}", text(&out));
    assert!(text(&out).contains("already exists"), "{}", text(&out));
    assert_eq!(home.node("n-draft").await.name, "Draft.md");
    assert_eq!(
        home.read("n-other").await.1,
        "notes",
        "the note that was already there must be untouched"
    );
}

/// A rename that names no change is a refusal rather than a silent success —
/// the store would answer `Ok` and announce nothing, which reads to the agent
/// as "the move happened" when nothing moved.
#[tokio::test]
async fn a_rename_that_changes_nothing_says_so() {
    let home = own_home("acme").await;
    let tool = home.renamer();

    for args in [
        json!({ "path": "agents/ceo/Draft.md", "new_name": "Draft.md" }),
        json!({ "path": "agents/ceo/Draft.md", "new_parent": "agents/ceo" }),
    ] {
        let out = tool.execute(args.clone()).await.unwrap();
        assert!(out.is_error, "{args}: {}", text(&out));
        assert!(
            text(&out).contains("already exactly where you asked to put it"),
            "{args}: {}",
            text(&out)
        );
    }
}

#[tokio::test]
async fn a_destination_that_is_missing_or_is_a_note_is_refused_with_what_to_do() {
    let home = own_home("acme").await;
    let tool = home.renamer();

    let out = tool
        .execute(json!({ "path": "agents/ceo/Draft.md", "new_parent": "agents/ceo/nope" }))
        .await
        .unwrap();
    assert!(out.is_error, "{}", text(&out));
    assert!(text(&out).contains("does not exist"), "{}", text(&out));
    assert!(
        text(&out).contains(WORKSPACE_CREATE_TOOL),
        "the refusal must name the tool that fixes it: {}",
        text(&out)
    );

    home.add_own(file("n-other", "Notes.md", None), "notes")
        .await;
    let out = tool
        .execute(json!({ "path": "agents/ceo/Draft.md", "new_parent": "agents/ceo/Notes.md" }))
        .await
        .unwrap();
    assert!(out.is_error, "{}", text(&out));
    assert!(
        text(&out).contains("is a note, not a folder"),
        "{}",
        text(&out)
    );
}

#[tokio::test]
async fn a_rename_needs_at_least_one_of_new_name_and_new_parent() {
    let home = own_home("acme").await;

    let out = home
        .renamer()
        .execute(json!({ "path": "agents/ceo/Draft.md" }))
        .await
        .unwrap();
    assert!(out.is_error, "{}", text(&out));
    assert!(text(&out).contains("new_name"), "{}", text(&out));
    assert!(text(&out).contains("new_parent"), "{}", text(&out));
}

#[tokio::test]
async fn tenancy_a_borrowed_node_id_cannot_be_renamed_by_another_company() {
    let home = own_home("acme").await;
    let intruder = WorkspaceRenameTool::new(ws(home.store.clone(), CompanyId::new("other")));

    let out = intruder
        .execute(json!({ "id": "n-draft", "new_name": "mine.md" }))
        .await
        .unwrap();
    assert!(out.is_error, "{}", text(&out));
    assert!(
        text(&out).contains("No workspace note matches"),
        "{}",
        text(&out)
    );
    assert_eq!(home.node("n-draft").await.name, "Draft.md");
}

// ---------------------------------------------------------------------------
// The declared surface
// ---------------------------------------------------------------------------

/// Both tools are honest about what they do, and both answer a refusal as a
/// readable `ToolResult` rather than as an `Err` the harness renders as a
/// crash.
#[tokio::test]
async fn both_tools_declare_write_and_answer_refusals_as_results() {
    let home = own_home("acme").await;
    let delete = home.deleter();
    let rename = home.renamer();

    assert_eq!(delete.name(), WORKSPACE_DELETE_TOOL);
    assert_eq!(rename.name(), WORKSPACE_RENAME_TOOL);
    assert_eq!(delete.permission_level(), PermissionLevel::Write);
    assert_eq!(rename.permission_level(), PermissionLevel::Write);

    // Neither `path` nor `id`: the resolver's own message, delivered as a
    // result.
    for out in [
        delete
            .execute(json!({ "expected_updated_at": NOTE_REV }))
            .await
            .unwrap(),
        rename.execute(json!({ "new_name": "x.md" })).await.unwrap(),
    ] {
        assert!(out.is_error, "{}", text(&out));
        assert!(text(&out).contains("Invalid arguments"), "{}", text(&out));
    }

    // The schemas name only the arguments each tool actually reads.
    let delete_schema = delete.parameters_schema();
    assert_eq!(delete_schema["required"], json!(["expected_updated_at"]));
    assert_eq!(delete_schema["additionalProperties"], json!(false));
    let rename_schema = rename.parameters_schema();
    assert!(rename_schema["properties"]["new_parent"].is_object());
    assert_eq!(rename_schema["additionalProperties"], json!(false));
}

/// A workspace store that answers reads from a real one and makes the single
/// mutating call under test fail — either by reporting the node was already
/// gone (`Ok(false)`), or by erroring outright.
struct BrittleStore {
    inner: Arc<dyn WorkspaceStore>,
    vanished: bool,
}

#[async_trait::async_trait]
impl WorkspaceStore for BrittleStore {
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

    async fn is_empty(&self, company: &CompanyId) -> crate::Result<bool> {
        self.inner.is_empty(company).await
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

    async fn adopt_or_create_folder(
        &self,
        company: &CompanyId,
        parent: Option<&str>,
        name: &str,
        origin: WorkspaceOrigin,
    ) -> crate::Result<FolderClaim> {
        self.inner
            .adopt_or_create_folder(company, parent, name, origin)
            .await
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

    async fn delete(&self, _company: &CompanyId, _id: &str) -> crate::Result<bool> {
        if self.vanished {
            Ok(false)
        } else {
            Err(crate::error::OpenCompanyError::Store(
                "the workspace backend is unreachable".into(),
            ))
        }
    }

    async fn rename_move_with_revision(
        &self,
        _company: &CompanyId,
        _id: &str,
        _new_name: Option<&str>,
        _new_parent: Option<Option<&str>>,
        _expected_updated_at: Option<u64>,
    ) -> crate::Result<WorkspaceNode> {
        Err(crate::error::OpenCompanyError::Store(
            "the workspace backend is unreachable".into(),
        ))
    }
}

/// The node survived the CAS check and then the store refused. The success
/// sentence claims a removal AND names whether the loss is permanent, so the
/// refusal has to reach the agent instead — as the store's stable error code
/// (per `store_reason`, the raw host-state string never does), and without
/// the node being reported gone.
#[tokio::test]
async fn a_delete_the_store_cannot_perform_is_refused_and_names_why() {
    let home = own_home("acme").await;
    let brittle: Arc<dyn WorkspaceStore> = Arc::new(BrittleStore {
        inner: home.store.clone(),
        vanished: false,
    });
    let deleter = WorkspaceDeleteTool::new(
        ws(brittle, home.company.clone()).with_artifacts(Some(home.artifacts.clone())),
    );

    let out = deleter
        .execute(json!({ "path": "agents/ceo/Draft.md", "expected_updated_at": NOTE_REV }))
        .await
        .unwrap();
    let message = text(&out);
    assert!(out.is_error, "{message}");
    assert!(
        message.contains("the workspace store failed"),
        "a store failure must be refused, not silently swallowed: {message}"
    );
    assert!(
        !message.contains("unreachable"),
        "the raw store reason is host state and must not reach the agent: {message}"
    );
    assert!(
        !message.contains("Deleted"),
        "a failed delete must not borrow the success sentence: {message}"
    );
    assert!(
        home.has("n-draft").await,
        "the note must still be there after a delete that did not happen"
    );
}

/// The CAS token narrows the window it does not close: between the tree
/// snapshot and the store call the node can go. `Ok(false)` is the store
/// saying exactly that, and it must not be read as a delete that worked.
#[tokio::test]
async fn a_node_that_vanished_between_the_check_and_the_delete_is_not_reported_deleted() {
    let home = own_home("acme").await;
    let brittle: Arc<dyn WorkspaceStore> = Arc::new(BrittleStore {
        inner: home.store.clone(),
        vanished: true,
    });
    let deleter = WorkspaceDeleteTool::new(
        ws(brittle, home.company.clone()).with_artifacts(Some(home.artifacts.clone())),
    );

    let out = deleter
        .execute(json!({ "path": "agents/ceo/Draft.md", "expected_updated_at": NOTE_REV }))
        .await
        .unwrap();
    let message = text(&out);
    assert!(out.is_error, "{message}");
    assert!(
        message.contains("already gone") && message.contains("Nothing was changed"),
        "the agent must be told the removal was somebody else's, not its own: {message}"
    );
}

/// The rename receipt hands back a new `rev` for the agent to reuse this turn.
/// A store that never performed the move has no rev to give, so the failure
/// must surface rather than the tool inventing a path it never landed at.
#[tokio::test]
async fn a_rename_the_store_cannot_perform_is_refused_and_leaves_the_path_alone() {
    let home = own_home("acme").await;
    let brittle: Arc<dyn WorkspaceStore> = Arc::new(BrittleStore {
        inner: home.store.clone(),
        vanished: false,
    });
    let renamer = WorkspaceRenameTool::new(ws(brittle, home.company.clone()));

    let out = renamer
        .execute(json!({ "path": "agents/ceo/Draft.md", "new_name": "final.md" }))
        .await
        .unwrap();
    let message = text(&out);
    assert!(out.is_error, "{message}");
    assert!(message.contains("the workspace store failed"), "{message}");
    assert!(
        !message.contains("unreachable"),
        "the raw store reason is host state and must not reach the agent: {message}"
    );
    assert!(
        !message.contains("Moved"),
        "a move that did not happen must not read as one: {message}"
    );
    let node = home.node("n-draft").await;
    assert_eq!(
        node.name, "Draft.md",
        "the node kept its name, and so must the report"
    );
}

/// `workspace_rename` carries no compare-and-swap token — the module header
/// argues one is unnecessary because a rename destroys nothing. That holds for
/// the *body*, and not for the ordering: two renames of one node are both
/// admitted against whatever the tree said when each of them looked, so the
/// second silently replaces the first's result and the first caller is told
/// its name landed. `workspace_delete` on the same node, in the same folder,
/// refuses exactly this with `expected_updated_at`.
///
/// The second rename here stands in for the concurrent one: it resolves the
/// node by id, sees a tree the first rename has already changed, and is
/// accepted anyway with nothing it could have passed to say which revision it
/// meant.
#[tokio::test]
#[ignore = "workspace_rename accepts no expected_updated_at, so a second rename of the same node cannot be ordered against the first"]
async fn a_second_rename_of_one_node_cannot_be_ordered_against_the_first() {
    let home = own_home("acme").await;
    let renamer = home.renamer();

    let first = renamer
        .execute(json!({ "id": "n-draft", "new_name": "launch-plan.md" }))
        .await
        .unwrap();
    assert!(!first.is_error, "{}", text(&first));

    let schema = renamer.parameters_schema();
    assert!(
        schema
            .get("properties")
            .and_then(|p| p.get("expected_updated_at"))
            .is_some(),
        "a rename must be able to say which revision of the node it meant, the way \
         `workspace_delete` does: {schema}"
    );

    let second = renamer
        .execute(json!({ "id": "n-draft", "new_name": "q3-plan.md" }))
        .await
        .unwrap();
    assert!(
        second.is_error,
        "a rename made against a view the first rename already invalidated must be refused, not \
         silently applied over it: {}",
        text(&second)
    );
}
