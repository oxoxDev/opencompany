//! The workspace's system roots — `Agents/`, `Desks/`, and `secrets/` — and the
//! content the runtime owns beneath them.
//!
//! Before this module an agent had nowhere in the shared tree that was
//! recognisably *its own*: everything it produced landed in its private
//! per-agent sandbox or on a task artifact, neither of which the operator or
//! another agent browses. `Agents/` gives each roster member a named place in
//! the one tree both sides read, so "where did the CMO put the launch brief"
//! has an answer a human can navigate to. `Desks/` is the same idea one level
//! up, for work a desk produces rather than one teammate (issue #552 wires the
//! producer).
//!
//! # One eager root, everything else lazy
//!
//! Provisioning runs on deliberately different schedules, and the line between
//! them is whether anything actually writes there yet:
//!
//! * `Agents/` is scaffolding. [`ensure_workspace_scaffold`] lays it down on
//!   every boot, empty, whether or not the company has a roster — it is part of
//!   what a workspace *is*, the same way the template-seeded `Playbooks/` and
//!   `Standards/` are, and it has a real producer behind it: the persona brief
//!   steers every agent to write beneath it, so an operator opening the
//!   Workspace tab on a brand-new company is being shown where things are about
//!   to appear rather than a void.
//! * `Desks/` is **not** scaffolded (issue #645). It was, until it turned out
//!   nothing writes into it: issue #552's publish path is still unwired, so
//!   [`ensure_desk_folder`] has no callers and every company carried a
//!   permanently empty root advertising a feature it does not yet have. An
//!   eager root nobody fills is the same promise-not-record mistake as an eager
//!   folder per roster member, one level up. It is therefore minted *whole* —
//!   root and member folder in one call — by [`ensure_desk_folder`], so it
//!   appears exactly when a desk first has something to put in it.
//! * `secrets/` is operator-only scaffolding. It is laid down eagerly with a
//!   `README.md` explaining that agent workspace tools omit the entire subtree.
//! * A **member folder** was never scaffolding either; it is a container for
//!   something. `Agents/<agent-id>/` and `Desks/<desk-id>/` are minted on demand
//!   — by [`ensure_agent_folder`] / [`ensure_desk_folder`], at the moment that
//!   agent or desk first produces a task, artifact or note. An eager folder per
//!   roster member fills the tree with empty directories for teammates who have
//!   never done anything, which is noise that grows with the roster and tells
//!   the operator nothing.
//!
//! Dropping the root changes nothing about how a desk folder is reached: the
//! minter has always created an absent root on its way down, so it is the same
//! one call it always was.
//!
//! # What this is, and what it very deliberately is not
//!
//! It is an **organizational and attribution unit**, identified by path. It is
//! **not** a permission boundary. Agents write anywhere in the tree — that is
//! the settled design (a `workspace_write` has always been able to overwrite
//! any note, and gating *create* while *overwrite* stays free would protect
//! nothing, since overwriting is the strictly more destructive of the two).
//! What keeps the tree tidy is steering — the persona brief names
//! `Agents/<your id>/` as the default home for anything an agent produces —
//! plus the authorship stamps from issue #326, which make it visible after the
//! fact who put what where. Containment lives one level up, in company tenancy,
//! the explicit `workspace` write grant, the CAS token, and policy parking.
//!
//! # Fail-closed adoption
//!
//! Identity is by path, and nothing in the [`WorkspaceStore`] port enforces
//! unique sibling names, so every lookup here is check-then-act. Ambiguity
//! always resolves the same way: **never guess and never overwrite**.
//!
//! * Exactly one folder carrying the name → adopt it as-is, authorship and all.
//! * A *file* carrying the name, or several nodes carrying it → refuse to touch
//!   it. Creating a rival would make the path permanently ambiguous, which the
//!   tool layer's resolver then refuses for every agent (see
//!   `harness::workspace_tools`).
//!
//! How a refusal is *reported* differs by caller, because the callers differ:
//!
//! * [`ensure_workspace_scaffold`] runs at boot with nobody waiting on a
//!   result, so it warns and skips — a convenience folder must not take down a
//!   boot, and the next boot retries. A tree read that fails still propagates:
//!   that is the store being broken, not the tree being odd.
//! * [`ensure_agent_folder`] / [`ensure_desk_folder`] are called *by a producer
//!   that needs the id back*, so there is nothing honest to fail soft to. They
//!   return the collision as an error and let the caller decide.
//!
//! Every function here is idempotent, which is what lets the scaffold run on
//! every boot and a minter run on every publish without accumulating anything.

use crate::Result;
use crate::error::OpenCompanyError;
use crate::ports::types::CompanyId;
use crate::ports::workspace::{NodeKind, WorkspaceNode, WorkspaceOrigin, WorkspaceStore};
use crate::ports::{generate_id, now_millis};

/// The reserved root folder holding one subfolder per agent that has produced
/// something.
///
/// A literal, because identity here is by path: this is the name the persona
/// brief tells agents to look for and the name issue #552's published
/// deliverables land under.
pub const AGENTS_ROOT: &str = "Agents";

/// The reserved root folder holding one subfolder per desk that has produced
/// something.
///
/// Not scaffolded at boot — see [`SYSTEM_ROOTS`] and [`ensure_desk_folder`].
pub const DESKS_ROOT: &str = "Desks";

/// The reserved root folder holding one subfolder per agent-authored
/// dashboard page: `Pages/<slug>/`.
///
/// Not scaffolded at boot, for the same reason [`DESKS_ROOT`] is not: nothing
/// writes here until an agent creates its first page. Named here — rather
/// than only inside `harness::pages_tools`, which compiles only under the
/// `openhuman` feature — because [`crate::server::ops::pages`] (always
/// compiled) needs the identical root name to serve what
/// `harness::pages_tools::pages_tools` wrote. One literal, two callers.
pub const PAGES_ROOT: &str = "Pages";

/// The page manifest node's name inside `Pages/<slug>/`.
pub const PAGE_MANIFEST_NAME: &str = "page.toml";
/// The page source node's name inside `Pages/<slug>/`.
pub const PAGE_SOURCE_NAME: &str = "Page.tsx";
/// The compiled page node's name inside `Pages/<slug>/`.
pub const PAGE_COMPILED_NAME: &str = "Page.compiled.mjs";
/// The mime [`crate::server::ops::pages`] serves [`PAGE_COMPILED_NAME`] as.
pub const PAGE_COMPILED_MIME: &str = "application/javascript";

/// The operator-only workspace subtree.
///
/// Agents never receive this root or anything beneath it through their
/// workspace list, read, search, or write tools. The operator surfaces still
/// use the full workspace store, so notes here remain ordinarily browsable and
/// editable in the console.
pub const SECRETS_ROOT: &str = "secrets";

/// The note provisioned inside [`SECRETS_ROOT`] on first boot.
pub const SECRETS_README: &str = "# Workspace secrets\n\nStore private operator notes and secret values in this folder. Everything under `secrets/` is hidden from agent workspace tools, including listing, reading, searching, and writing. Operators can still browse and edit these notes in the Workspace view.\n\nDo not treat this folder as an application credential store: use the Connections and inference settings for credentials that OpenCompany must inject into tools or providers.\n";

/// The system roots the runtime lays down eagerly, on every boot.
///
/// Deliberately *not* derived from the manifest: `Agents/` exists because a
/// workspace has it, not because a particular company has agents.
///
/// [`DESKS_ROOT`] is deliberately absent (issue #645). Nothing writes into it
/// yet, so scaffolding it gave every company a permanently empty root; it is
/// minted on first use instead. It is a root either way — this list is about
/// *when* a root appears, not which names are reserved.
///
/// Kept an array, and kept public, so a caller that has to tell scaffolding
/// apart from content — the re-seed tests, a future console filter — can ask
/// rather than hard-code the names, and so promoting a root back to eager stays
/// a one-line change.
pub const SYSTEM_ROOTS: [&str; 2] = [AGENTS_ROOT, SECRETS_ROOT];

/// Whether a logical workspace path belongs to the operator-only subtree.
///
/// This is case-insensitive on the root segment so a colliding `Secrets` node
/// cannot become an accidental agent-visible twin. Descendants are tested by
/// segments rather than string prefix, so `secrets-old/` remains ordinary
/// shared workspace content.
pub fn is_agent_hidden_path(path: &str) -> bool {
    path.trim()
        .trim_start_matches('/')
        .split('/')
        .next()
        .is_some_and(|root| root.eq_ignore_ascii_case(SECRETS_ROOT))
}

/// Adopt-or-create the eagerly-scaffolded roots ([`SYSTEM_ROOTS`]) for
/// `company`.
///
/// Two `tree()` reads (the second resolves the README parent), then only the
/// creates that are actually missing. Safe to
/// call on every boot: it depends on nothing but the company id, so an existing
/// company picks the roots up the next time it starts.
///
/// What it creates is stamped [`WorkspaceOrigin::Seed`] — scaffolding the
/// runtime lays down, authored by no operator and no agent. `secrets/` receives
/// one explanatory `README.md`; member folders beneath `Agents/` remain lazy
/// (see [`ensure_agent_folder`]). `Desks/` is not created here at all, and an
/// existing one is never even looked at: this walks
/// [`SYSTEM_ROOTS`] by name, so a legacy company's `Desks/` (from before issue
/// #645, or hand-made by an operator) is left exactly as it stands, contents
/// and authorship included.
///
/// Errors from the tree read propagate; a failed or ambiguous *create* warns
/// and moves on, and the next boot retries it.
pub async fn ensure_workspace_scaffold(
    store: &dyn WorkspaceStore,
    company: &CompanyId,
) -> Result<()> {
    let nodes = store.tree(company).await?;

    for root in SYSTEM_ROOTS {
        // Each root is resolved independently, so one colliding name never
        // withholds another. The loop is the contract rather than an accident
        // of arity — it read the same when there were two.
        match find(&nodes, None, root) {
            Found::Folder(_) => {}
            Found::Collision(why) => tracing::warn!(
                company = %company,
                "[workspace] {why}; not provisioning the `{root}` root"
            ),
            Found::Free => {
                // Through the store primitive, so a boot racing a publish (or
                // two tenant replicas booting together) adopts rather than
                // duplicating the root — see `ensure_member_folder`. The
                // warn-and-continue reporting is unchanged: a convenience folder
                // must not take down a boot.
                if let Err(e) = store
                    .adopt_or_create_folder(company, None, root, WorkspaceOrigin::Seed)
                    .await
                {
                    tracing::warn!(
                        company = %company,
                        error = %e,
                        "[workspace] could not create the `{root}` root; will retry on the next boot"
                    );
                }
            }
        }
    }

    // `secrets/` is useful before an operator has put anything in it, and the
    // note explains the boundary at the place they encounter it. Refresh the
    // tree after claiming roots so a newly-created root has an id to parent the
    // note beneath. As with the roots, collisions fail closed and retry later.
    let nodes = store.tree(company).await?;
    let secret_root = match find(&nodes, None, SECRETS_ROOT) {
        Found::Folder(id) => Some(id),
        Found::Collision(why) => {
            tracing::warn!(
                company = %company,
                "[workspace] {why}; not provisioning `{SECRETS_ROOT}/README.md`"
            );
            None
        }
        Found::Free => None,
    };
    if let Some(root_id) = secret_root {
        match find(&nodes, Some(root_id.as_str()), "README.md") {
            Found::Folder(_) | Found::Collision(_) => tracing::warn!(
                company = %company,
                "[workspace] `{SECRETS_ROOT}/README.md` is not one unambiguous note; leaving it untouched"
            ),
            Found::Free => {
                let readme = WorkspaceNode {
                    id: generate_id(),
                    name: "README.md".to_string(),
                    kind: NodeKind::File,
                    parent_id: Some(root_id),
                    updated_at_millis: now_millis(),
                    created_by: WorkspaceOrigin::Seed,
                    updated_by: WorkspaceOrigin::Seed,
                    mime: None,
                    size: None,
                    sha256: None,
                };
                if let Err(error) = store.create(company, &readme, Some(SECRETS_README)).await {
                    tracing::warn!(
                        company = %company,
                        %error,
                        "[workspace] could not create `{SECRETS_ROOT}/README.md`; will retry on the next boot"
                    );
                }
            }
        }
    }

    Ok(())
}

/// Adopt-or-create `Agents/<agent_id>/`, returning its node id.
///
/// The lazy half of the feature: call this at the moment `agent_id` first
/// produces something that needs a home, not when it joins the roster. Creates
/// the `Agents` root too if the scaffold has not run (or could not create it),
/// so one call is enough to get a usable parent id.
///
/// The folder is stamped [`WorkspaceOrigin::Agent`] for the agent it belongs
/// to, so the console can say whose folder it is without parsing the path.
///
/// Idempotent: a second call on the same agent returns the same id and creates
/// nothing.
pub async fn ensure_agent_folder(
    store: &dyn WorkspaceStore,
    company: &CompanyId,
    agent_id: &str,
) -> Result<String> {
    let agent_id = agent_id.trim();
    ensure_member_folder(
        store,
        company,
        AGENTS_ROOT,
        agent_id,
        WorkspaceOrigin::Agent {
            id: agent_id.to_string(),
        },
    )
    .await
}

/// Adopt-or-create `Desks/<desk_id>/`, returning its node id.
///
/// [`ensure_agent_folder`]'s counterpart for a desk — call it when a desk first
/// produces an artifact. Nothing calls it yet; issue #552's publish path is the
/// first producer.
///
/// Unlike `Agents/`, the `Desks/` root is not scaffolded at boot (issue #645),
/// so this mints the root as well when it is missing. That is the point rather
/// than a fallback: `Desks/` appears the first time a desk has something to put
/// in it, instead of standing empty in every company that never uses one.
///
/// Both the root and the member folder are stamped [`WorkspaceOrigin::Seed`]
/// rather than an author, because a desk is not one: [`WorkspaceOrigin`] names
/// the seed, the operator, or a single agent, and claiming
/// `Agent { id: <desk-id> }` would attribute the folder to a teammate that does
/// not exist. A lazily-minted root carries the same stamp the boot scaffold
/// used to give it, so nothing downstream can tell the two apart. The desk's
/// *contents* still carry the real agent that wrote each of them.
pub async fn ensure_desk_folder(
    store: &dyn WorkspaceStore,
    company: &CompanyId,
    desk_id: &str,
) -> Result<String> {
    ensure_member_folder(
        store,
        company,
        DESKS_ROOT,
        desk_id.trim(),
        WorkspaceOrigin::Seed,
    )
    .await
}

/// The shared body of [`ensure_agent_folder`] and [`ensure_desk_folder`]:
/// resolve `root`, then resolve `id` beneath it, creating what is missing.
///
/// # The tree read is a fast path; the store decides (issue #759)
///
/// [`find`] answering `Free` describes the instant the tree was read, and the
/// create used to act on it afterwards. Two agents first producing something at
/// once therefore both saw `Agents/` free — or both saw `Agents/<id>/` free —
/// and both created, leaving two folders under one name. Nothing repairs that:
/// [`find`] answers a duplicated name with `Collision` from then on, so a race
/// lasting microseconds refuses that agent's folder forever.
///
/// Both creates now go through [`WorkspaceStore::adopt_or_create_folder`], which
/// resolves the contention inside the store. That also makes the stale snapshot
/// harmless: a caller whose read predates another's create adopts the folder
/// that exists rather than minting a rival, and — because the root claim returns
/// the *winner's* id — the member folder beneath it is claimed under the same
/// parent either way.
async fn ensure_member_folder(
    store: &dyn WorkspaceStore,
    company: &CompanyId,
    root: &str,
    id: &str,
    origin: WorkspaceOrigin,
) -> Result<String> {
    // The id becomes a node name, and a name carrying a separator renders an
    // ambiguous or traversal-shaped path. The `fs` backend refuses such names
    // outright and the sqlite/mongodb backends do not, so the guard lives here
    // rather than being assumed of the store.
    if !is_legal_segment(id) {
        return Err(OpenCompanyError::InvalidRequest(format!(
            "`{id}` is not a legal workspace path segment, so it cannot name a folder under \
             `{root}/`"
        )));
    }

    let nodes = store.tree(company).await?;

    let root_id = match find(&nodes, None, root) {
        Found::Folder(id) => id,
        Found::Free => {
            store
                .adopt_or_create_folder(company, None, root, WorkspaceOrigin::Seed)
                .await?
                .into_node()
                .id
        }
        Found::Collision(why) => return Err(OpenCompanyError::Conflict(why)),
    };

    match find(&nodes, Some(&root_id), id) {
        Found::Folder(existing) => Ok(existing),
        Found::Free => Ok(store
            .adopt_or_create_folder(company, Some(&root_id), id, origin)
            .await?
            .into_node()
            .id),
        Found::Collision(why) => Err(OpenCompanyError::Conflict(why)),
    }
}

/// What a lookup for one named node under one parent found.
///
/// `pub(crate)` alongside [`find`], for the one other module that has to resolve
/// a system root: [`workspace_sweep`](crate::company::workspace_sweep). A sweep
/// that removes folders *under* `Agents/` has to agree with the scaffold about
/// which node that root is, and about when there isn't one — a second lookup
/// with its own idea of "the `Agents` folder" could adopt a node this module
/// refuses to touch, and then delete beneath it.
pub(crate) enum Found {
    /// Exactly one folder carries the name — adopt it, by id.
    Folder(String),
    /// Nothing carries the name; it is free to create.
    Free,
    /// A *file* carries the name, or several nodes do. Never resolvable, with
    /// the reason phrased for a log line or an error body.
    Collision(String),
}

/// Look for a node named `name` whose parent is `parent` (`None` = the
/// workspace root).
///
/// `pub(crate)` so the fail-closed adoption rule above has exactly one
/// implementation. See [`Found`].
pub(crate) fn find(nodes: &[WorkspaceNode], parent: Option<&str>, name: &str) -> Found {
    let matches: Vec<&WorkspaceNode> = nodes
        .iter()
        .filter(|node| node.parent_id.as_deref() == parent && node.name == name)
        .collect();

    match matches.as_slice() {
        [one] if one.kind == NodeKind::Folder => Found::Folder(one.id.clone()),
        [_] => Found::Collision(format!(
            "`{name}` already exists as a file, not a folder, so it is left alone"
        )),
        [] => Found::Free,
        many => Found::Collision(format!(
            "{count} nodes are named `{name}`, so the path is ambiguous",
            count = many.len()
        )),
    }
}

/// Whether `name` is usable as a single workspace path segment.
///
/// Mirrors the `fs` backend's `reject_unsafe_name` and the agent tool layer's
/// `is_legal_segment`. Duplicated rather than shared because this module is in
/// the default build and the tool layer links only under the `openhuman`
/// feature; the rule is three lines and a shared home for it would drag the
/// whole harness into every build.
fn is_legal_segment(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::store::FsOps;

    fn agent(id: &str) -> WorkspaceOrigin {
        WorkspaceOrigin::Agent { id: id.to_string() }
    }

    async fn store() -> (tempfile::TempDir, Arc<dyn WorkspaceStore>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let ops: Arc<dyn WorkspaceStore> = Arc::new(FsOps::new(dir.path()));
        (dir, ops)
    }

    /// Seeds root folders that share `name` by writing the workspace index
    /// directly.
    ///
    /// The filesystem store refuses to *create* two siblings under one name,
    /// because on that backend they would resolve to one path (issue #666).
    /// The trees below are the ones that check what the scaffold does when it
    /// nevertheless *finds* an ambiguous root — an index written before that
    /// refusal existed, or one an id-keyed backend can still represent legally.
    /// So the state is written rather than requested: going through `create`
    /// would only re-assert the store's refusal and never reach the scaffold.
    async fn seed_duplicate_roots(
        dir: &std::path::Path,
        company: &CompanyId,
        name: &str,
        ids: &[&str],
    ) {
        let index: std::collections::HashMap<String, WorkspaceNode> = ids
            .iter()
            .map(|id| {
                (
                    (*id).to_string(),
                    WorkspaceNode {
                        id: (*id).to_string(),
                        name: name.to_string(),
                        kind: NodeKind::Folder,
                        parent_id: None,
                        updated_at_millis: 1,
                        created_by: WorkspaceOrigin::Operator,
                        updated_by: WorkspaceOrigin::Operator,
                        mime: None,
                        size: None,
                        sha256: None,
                    },
                )
            })
            .collect();
        let bundle = crate::store::Bundle::new(dir.to_path_buf(), company);
        tokio::fs::create_dir_all(bundle.workspace_dir())
            .await
            .expect("workspace dir");
        tokio::fs::write(
            bundle.workspace_index_json(),
            serde_json::to_vec(&index).expect("index json"),
        )
        .await
        .expect("seed index");
    }

    /// A node's rendered `parent/child` path, for readable assertions.
    fn path_of(nodes: &[WorkspaceNode], node: &WorkspaceNode) -> String {
        match &node.parent_id {
            None => node.name.clone(),
            Some(parent) => match nodes.iter().find(|n| &n.id == parent) {
                Some(p) => format!("{}/{}", path_of(nodes, p), node.name),
                None => node.name.clone(),
            },
        }
    }

    fn paths(nodes: &[WorkspaceNode]) -> Vec<String> {
        let mut out: Vec<String> = nodes.iter().map(|n| path_of(nodes, n)).collect();
        out.sort();
        out
    }

    async fn tree_paths(ws: &Arc<dyn WorkspaceStore>, company: &CompanyId) -> Vec<String> {
        paths(&ws.tree(company).await.unwrap())
    }

    fn scaffold_paths() -> Vec<&'static str> {
        vec!["Agents", "secrets", "secrets/README.md"]
    }

    /// The scaffold has an empty agent root plus the operator-only secrets
    /// folder and its explanatory note. It never creates roster member folders
    /// or the unused `Desks/` root.
    #[tokio::test]
    async fn it_provisions_one_empty_system_root() {
        let (_dir, ws) = store().await;
        let company = CompanyId::new("acme");

        ensure_workspace_scaffold(ws.as_ref(), &company)
            .await
            .unwrap();

        let nodes = ws.tree(&company).await.unwrap();
        assert_eq!(
            paths(&nodes),
            scaffold_paths(),
            "`Desks/` has no producer, so boot must not lay it down"
        );
        for node in nodes.iter().filter(|node| node.kind == NodeKind::Folder) {
            assert_eq!(
                node.created_by,
                WorkspaceOrigin::Seed,
                "{} is runtime scaffolding, not anybody's writing",
                node.name
            );
        }
        let readme = nodes.iter().find(|node| node.name == "README.md").unwrap();
        let (_, body) = ws.read(&company, &readme.id).await.unwrap().unwrap();
        assert_eq!(body, SECRETS_README);
    }

    /// The scaffold takes no roster and asks for none: a company with no agents
    /// at all still gets the shape of its workspace. (This reverses the earlier
    /// eager design, where an empty roster deliberately created nothing —
    /// there, a root with no children was a stray; here it is the point.)
    #[tokio::test]
    async fn a_company_with_no_roster_still_gets_the_agents_root() {
        let (_dir, ws) = store().await;
        let company = CompanyId::new("solo");

        ensure_workspace_scaffold(ws.as_ref(), &company)
            .await
            .unwrap();

        assert_eq!(tree_paths(&ws, &company).await, scaffold_paths());
    }

    /// The property that lets this run on every boot.
    #[tokio::test]
    async fn it_is_idempotent() {
        let (_dir, ws) = store().await;
        let company = CompanyId::new("acme");

        for _ in 0..3 {
            ensure_workspace_scaffold(ws.as_ref(), &company)
                .await
                .unwrap();
        }

        assert_eq!(tree_paths(&ws, &company).await, scaffold_paths());
    }

    /// An operator-made `Agents/` folder is adopted as-is rather than
    /// duplicated — identity is by path, so a second root would make every
    /// `Agents/...` path permanently ambiguous.
    #[tokio::test]
    async fn an_existing_root_folder_is_adopted() {
        let (_dir, ws) = store().await;
        let company = CompanyId::new("acme");
        ws.create(
            &company,
            &WorkspaceNode {
                id: "hand-made".to_string(),
                name: AGENTS_ROOT.to_string(),
                kind: NodeKind::Folder,
                parent_id: None,
                updated_at_millis: 1,
                created_by: WorkspaceOrigin::Operator,
                updated_by: WorkspaceOrigin::Operator,
                mime: None,
                size: None,
                sha256: None,
            },
            None,
        )
        .await
        .unwrap();

        ensure_workspace_scaffold(ws.as_ref(), &company)
            .await
            .unwrap();

        let nodes = ws.tree(&company).await.unwrap();
        assert_eq!(paths(&nodes), scaffold_paths());
        let root = nodes.iter().find(|n| n.name == AGENTS_ROOT).unwrap();
        assert_eq!(root.id, "hand-made", "the operator's folder must be reused");
        assert_eq!(
            root.created_by,
            WorkspaceOrigin::Operator,
            "adoption must not rewrite the operator's authorship"
        );
    }

    /// Fail-closed: a root *file* named `Agents` is a collision this module has
    /// no honest way to resolve, so it leaves it alone rather than shadowing
    /// the operator's note with a rival folder of the same name — and creates
    /// nothing else in its place.
    #[tokio::test]
    async fn a_root_file_is_left_alone_rather_than_shadowed() {
        let (_dir, ws) = store().await;
        let company = CompanyId::new("acme");
        ws.create(
            &company,
            &WorkspaceNode {
                id: "note".to_string(),
                name: AGENTS_ROOT.to_string(),
                kind: NodeKind::File,
                parent_id: None,
                updated_at_millis: 1,
                created_by: WorkspaceOrigin::Operator,
                updated_by: WorkspaceOrigin::Operator,
                mime: None,
                size: None,
                sha256: None,
            },
            Some("# not a folder"),
        )
        .await
        .unwrap();

        ensure_workspace_scaffold(ws.as_ref(), &company)
            .await
            .unwrap();

        let nodes = ws.tree(&company).await.unwrap();
        assert_eq!(
            paths(&nodes),
            scaffold_paths(),
            "the collision must not be shadowed; unrelated scaffold still provisions"
        );
        assert_eq!(
            nodes.iter().find(|n| n.name == AGENTS_ROOT).unwrap().kind,
            NodeKind::File,
            "the operator's note must not be shadowed by a folder of the same name"
        );
    }

    /// Several root nodes sharing a reserved name is the other unresolvable
    /// shape: adding a third would make it worse, so nothing is created.
    #[tokio::test]
    async fn several_nodes_sharing_a_root_name_are_left_alone() {
        let (dir, ws) = store().await;
        let company = CompanyId::new("acme");
        seed_duplicate_roots(dir.path(), &company, AGENTS_ROOT, &["dup-a", "dup-b"]).await;

        ensure_workspace_scaffold(ws.as_ref(), &company)
            .await
            .unwrap();

        let nodes = ws.tree(&company).await.unwrap();
        assert_eq!(
            nodes.iter().filter(|n| n.name == AGENTS_ROOT).count(),
            2,
            "an ambiguous root must not gain a third candidate"
        );
        assert_eq!(
            paths(&nodes),
            vec!["Agents", "Agents", "secrets", "secrets/README.md"],
            "only the unrelated secrets scaffold may be created beside the collision"
        );
    }

    /// The tree is company-scoped: scaffolding one company leaves another's
    /// workspace untouched.
    #[tokio::test]
    async fn scaffolding_is_per_company() {
        let (_dir, ws) = store().await;
        let acme = CompanyId::new("acme");
        let other = CompanyId::new("other");

        ensure_workspace_scaffold(ws.as_ref(), &acme).await.unwrap();

        assert!(ws.is_empty(&other).await.unwrap());
    }

    // -- the lazy minters ---------------------------------------------------

    /// The property #552's publish path depends on: minting on every publish
    /// must be free after the first one, and must hand back the *same* parent
    /// id so two deliverables land in one folder rather than two.
    #[tokio::test]
    async fn ensure_agent_folder_is_idempotent_and_stable() {
        let (_dir, ws) = store().await;
        let company = CompanyId::new("acme");
        ensure_workspace_scaffold(ws.as_ref(), &company)
            .await
            .unwrap();

        let first = ensure_agent_folder(ws.as_ref(), &company, "ceo")
            .await
            .unwrap();
        let second = ensure_agent_folder(ws.as_ref(), &company, "ceo")
            .await
            .unwrap();

        assert_eq!(first, second, "a second call minted a rival folder");
        assert_eq!(
            tree_paths(&ws, &company).await,
            vec!["Agents", "Agents/ceo", "secrets", "secrets/README.md"]
        );
        let nodes = ws.tree(&company).await.unwrap();
        let ceo = nodes.iter().find(|n| n.name == "ceo").unwrap();
        assert_eq!(ceo.kind, NodeKind::Folder);
        assert_eq!(ceo.created_by, agent("ceo"));
    }

    /// One agent producing something must not conjure folders for the rest of
    /// the roster — that is the whole difference from the eager design.
    #[tokio::test]
    async fn minting_one_agent_folder_leaves_the_roster_alone() {
        let (_dir, ws) = store().await;
        let company = CompanyId::new("acme");
        ensure_workspace_scaffold(ws.as_ref(), &company)
            .await
            .unwrap();

        ensure_agent_folder(ws.as_ref(), &company, "cmo")
            .await
            .unwrap();

        assert_eq!(
            tree_paths(&ws, &company).await,
            vec!["Agents", "Agents/cmo", "secrets", "secrets/README.md"]
        );
    }

    /// A minter is also its own repair path: it creates the root when the
    /// scaffold never ran, so a boot whose create fail-softed still ends up
    /// with a usable `Agents/` the first time an agent produces anything.
    #[tokio::test]
    async fn ensure_agent_folder_creates_the_root_it_needs() {
        let (_dir, ws) = store().await;
        let company = CompanyId::new("acme");

        let id = ensure_agent_folder(ws.as_ref(), &company, "ceo")
            .await
            .unwrap();

        let nodes = ws.tree(&company).await.unwrap();
        assert_eq!(paths(&nodes), vec!["Agents", "Agents/ceo"]);
        let root = nodes.iter().find(|n| n.name == AGENTS_ROOT).unwrap();
        assert_eq!(root.created_by, WorkspaceOrigin::Seed);
        assert_eq!(nodes.iter().find(|n| n.id == id).unwrap().name, "ceo");
    }

    /// An operator's hand-made `Agents/ceo` is adopted, not duplicated.
    #[tokio::test]
    async fn ensure_agent_folder_adopts_an_existing_folder() {
        let (_dir, ws) = store().await;
        let company = CompanyId::new("acme");
        ensure_workspace_scaffold(ws.as_ref(), &company)
            .await
            .unwrap();
        let root_id = ws
            .tree(&company)
            .await
            .unwrap()
            .into_iter()
            .find(|n| n.name == AGENTS_ROOT)
            .unwrap()
            .id;
        ws.create(
            &company,
            &WorkspaceNode {
                id: "hand-made".to_string(),
                name: "ceo".to_string(),
                kind: NodeKind::Folder,
                parent_id: Some(root_id),
                updated_at_millis: 1,
                created_by: WorkspaceOrigin::Operator,
                updated_by: WorkspaceOrigin::Operator,
                mime: None,
                size: None,
                sha256: None,
            },
            None,
        )
        .await
        .unwrap();

        let id = ensure_agent_folder(ws.as_ref(), &company, "ceo")
            .await
            .unwrap();

        assert_eq!(id, "hand-made");
        assert_eq!(
            ws.tree(&company)
                .await
                .unwrap()
                .iter()
                .find(|n| n.id == "hand-made")
                .unwrap()
                .created_by,
            WorkspaceOrigin::Operator,
            "adoption must not rewrite the operator's authorship"
        );
    }

    /// The minter has a caller waiting on an id, so a collision it cannot
    /// resolve is an error rather than a warn-and-carry-on — there is no id to
    /// hand back and pretending otherwise would strand the caller's write.
    #[tokio::test]
    async fn a_colliding_member_file_is_an_error_not_a_silent_skip() {
        let (_dir, ws) = store().await;
        let company = CompanyId::new("acme");
        ensure_workspace_scaffold(ws.as_ref(), &company)
            .await
            .unwrap();
        let root_id = ws
            .tree(&company)
            .await
            .unwrap()
            .into_iter()
            .find(|n| n.name == AGENTS_ROOT)
            .unwrap()
            .id;
        ws.create(
            &company,
            &WorkspaceNode {
                id: "ceo-note".to_string(),
                name: "ceo".to_string(),
                kind: NodeKind::File,
                parent_id: Some(root_id),
                updated_at_millis: 1,
                created_by: WorkspaceOrigin::Operator,
                updated_by: WorkspaceOrigin::Operator,
                mime: None,
                size: None,
                sha256: None,
            },
            Some("# notes about the ceo"),
        )
        .await
        .unwrap();

        let err = ensure_agent_folder(ws.as_ref(), &company, "ceo")
            .await
            .expect_err("a colliding note must not resolve to a folder id");
        assert!(err.to_string().contains("ceo"), "{err}");
        assert_eq!(
            ws.tree(&company)
                .await
                .unwrap()
                .iter()
                .find(|n| n.name == "ceo")
                .unwrap()
                .kind,
            NodeKind::File,
            "the operator's note must not be shadowed by a folder of the same name"
        );
    }

    /// An id that is not a legal path segment would render an unaddressable or
    /// traversal-shaped path, so it is refused before anything is created.
    #[tokio::test]
    async fn an_illegal_id_is_refused_and_creates_nothing() {
        let (_dir, ws) = store().await;
        let company = CompanyId::new("acme");

        for id in ["../escape", "", ".", "a/b", "a\\b"] {
            ensure_agent_folder(ws.as_ref(), &company, id)
                .await
                .expect_err("`{id}` is not a legal path segment");
        }

        assert!(ws.is_empty(&company).await.unwrap());
    }

    /// The desk minter is the same shape one root over — and since issue #645
    /// it is the *only* thing that ever creates `Desks/`. Deliberately run with
    /// no scaffold at all: the first call must mint the root and the member
    /// folder together, which is what lets boot stop laying down an empty root
    /// nothing was filling.
    ///
    /// The root it mints stamps `Seed`, exactly as the boot scaffold used to,
    /// so no consumer can tell a lazily-minted root from the old eager one. The
    /// desk folder stamps `Seed` too, because a desk is not an agent and
    /// `WorkspaceOrigin` has no way to name one.
    #[tokio::test]
    async fn ensure_desk_folder_mints_the_desks_root_on_first_use() {
        let (_dir, ws) = store().await;
        let company = CompanyId::new("acme");

        let first = ensure_desk_folder(ws.as_ref(), &company, "creative_studio")
            .await
            .unwrap();
        let second = ensure_desk_folder(ws.as_ref(), &company, "creative_studio")
            .await
            .unwrap();

        assert_eq!(first, second, "a second call minted a rival folder");
        assert_eq!(
            tree_paths(&ws, &company).await,
            vec!["Desks", "Desks/creative_studio"],
            "the root appears with its first occupant, and brings nothing else"
        );
        let nodes = ws.tree(&company).await.unwrap();
        let desk = nodes.iter().find(|n| n.id == first).unwrap();
        assert_eq!(desk.kind, NodeKind::Folder);
        assert_eq!(desk.created_by, WorkspaceOrigin::Seed);
        let root = nodes.iter().find(|n| n.name == DESKS_ROOT).unwrap();
        assert_eq!(root.kind, NodeKind::Folder);
        assert_eq!(
            root.created_by,
            WorkspaceOrigin::Seed,
            "a lazily-minted root must carry the stamp boot used to give it"
        );
    }

    /// The migration story for every company that booted before issue #645: its
    /// `Desks/` root already exists, and the scaffold must leave it completely
    /// alone rather than notice it is no longer managed and tidy it away.
    ///
    /// The scaffold only ever looks up the names in `SYSTEM_ROOTS`, so a
    /// `Desks/` node is not even inspected — id, authorship and contents all
    /// survive untouched.
    #[tokio::test]
    async fn a_pre_existing_desks_root_survives_the_scaffold_untouched() {
        let (_dir, ws) = store().await;
        let company = CompanyId::new("legacy");
        ws.create(
            &company,
            &WorkspaceNode {
                id: "legacy-desks".to_string(),
                name: DESKS_ROOT.to_string(),
                kind: NodeKind::Folder,
                parent_id: None,
                updated_at_millis: 1,
                created_by: WorkspaceOrigin::Operator,
                updated_by: WorkspaceOrigin::Operator,
                mime: None,
                size: None,
                sha256: None,
            },
            None,
        )
        .await
        .unwrap();

        ensure_workspace_scaffold(ws.as_ref(), &company)
            .await
            .unwrap();

        let nodes = ws.tree(&company).await.unwrap();
        assert_eq!(
            paths(&nodes),
            vec!["Agents", "Desks", "secrets", "secrets/README.md"],
            "dropping `Desks/` from the scaffold must not delete an existing one"
        );
        let desks = nodes.iter().find(|n| n.name == DESKS_ROOT).unwrap();
        assert_eq!(desks.id, "legacy-desks", "the existing root must be kept");
        assert_eq!(
            desks.created_by,
            WorkspaceOrigin::Operator,
            "an unmanaged root's authorship must not be rewritten"
        );
    }

    /// The un-managed counterpart to `several_nodes_sharing_a_root_name_are_
    /// left_alone`: duplicate `Desks` nodes are not a collision the scaffold
    /// has to resolve any more, they are simply none of its business — and the
    /// root it *does* manage still provisions beside them.
    #[tokio::test]
    async fn duplicate_desks_nodes_do_not_disturb_the_scaffold() {
        let (dir, ws) = store().await;
        let company = CompanyId::new("acme");
        seed_duplicate_roots(dir.path(), &company, DESKS_ROOT, &["dup-a", "dup-b"]).await;

        ensure_workspace_scaffold(ws.as_ref(), &company)
            .await
            .unwrap();

        let nodes = ws.tree(&company).await.unwrap();
        assert_eq!(
            nodes.iter().filter(|n| n.name == DESKS_ROOT).count(),
            2,
            "an unmanaged name must be neither deduplicated nor added to"
        );
        assert_eq!(
            nodes.iter().filter(|n| n.name == AGENTS_ROOT).count(),
            1,
            "an odd name elsewhere is no reason to withhold a managed root"
        );
    }

    /// The two roots stay independent: minting a desk folder does not reach
    /// into `Agents/`, and vice versa.
    #[tokio::test]
    async fn the_two_roots_do_not_leak_into_each_other() {
        let (_dir, ws) = store().await;
        let company = CompanyId::new("acme");

        ensure_agent_folder(ws.as_ref(), &company, "shared")
            .await
            .unwrap();
        ensure_desk_folder(ws.as_ref(), &company, "shared")
            .await
            .unwrap();

        assert_eq!(
            tree_paths(&ws, &company).await,
            vec!["Agents", "Agents/shared", "Desks", "Desks/shared"]
        );
    }
}
