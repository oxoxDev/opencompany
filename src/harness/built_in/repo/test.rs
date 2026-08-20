//! The checkout tier against a real `git`, and no network anywhere.
//!
//! Every fixture is a bare repository in a temp directory reached over
//! `file://`, for the reason the operator tier states: mocking git would test a
//! mock, and the bug this module can actually have — a checkout that still
//! resolves through the host's mirror — is a bug in how git is *driven*, and
//! only a real git catches it.
//!
//! The headline is [`a_checkout_cannot_reach_the_mirror_it_came_from`]. It does
//! not assert that `origin` is absent, because absent `origin` is exactly what
//! the rejected `--shared` design also had. It commits a poison file, tries
//! both pushes an agent could actually make, and then compares the mirror's
//! refs **and object list** byte for byte.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use async_trait::async_trait;

use super::*;
use crate::ports::SecretStore;
use crate::ports::types::{CompanyId, SecretValue};
use crate::runtime::repo_manager::types::{
    PullRequestRef, PullRequestView, RepoCoordinates, RepoHost, RepoMeta,
};

/// A scratch directory removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "oc-checkout-{}-{}-{tag}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn join(&self, part: &str) -> PathBuf {
        self.0.join(part)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// An in-memory [`SecretStore`], so a manager can be built with no filesystem
/// store behind it.
#[derive(Default)]
struct MemSecrets {
    values: StdMutex<HashMap<(String, String), String>>,
}

#[async_trait]
impl SecretStore for MemSecrets {
    async fn get(&self, company: &CompanyId, key: &str) -> crate::Result<Option<SecretValue>> {
        Ok(self
            .values
            .lock()
            .unwrap()
            .get(&(company.to_string(), key.to_string()))
            .cloned()
            .map(SecretValue))
    }

    async fn set(&self, company: &CompanyId, key: &str, value: SecretValue) -> crate::Result<()> {
        self.values
            .lock()
            .unwrap()
            .insert((company.to_string(), key.to_string()), value.0);
        Ok(())
    }
}

/// A forge that answers one scripted pull request.
struct ScriptedHost {
    diff: String,
}

#[async_trait]
impl RepoHost for ScriptedHost {
    async fn repo_meta(&self, _coords: &RepoCoordinates, _token: &str) -> crate::Result<RepoMeta> {
        Ok(RepoMeta {
            default_branch: "main".into(),
            size_kb: 1,
            can_push: false,
        })
    }

    async fn pull_request(
        &self,
        _coords: &RepoCoordinates,
        number: u64,
        _token: &str,
    ) -> crate::Result<PullRequestView> {
        Ok(PullRequestView {
            number,
            title: "a change".into(),
            state: "open".into(),
            head_sha: "cafebabe".into(),
            base_ref: "main".into(),
            diff: self.diff.clone(),
        })
    }

    async fn create_pull_request(
        &self,
        _coords: &RepoCoordinates,
        _token: &str,
        _head: &str,
        _base: &str,
        _title: &str,
        _body: &str,
    ) -> crate::Result<PullRequestRef> {
        Ok(PullRequestRef {
            number: 7,
            html_url: "https://github.com/acme/fixture/pull/7".into(),
        })
    }
}

/// Runs git in `cwd`, panicking with its stderr on failure.
fn git_at(cwd: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Runs git in `cwd` **as an agent would** — no hardening, no assertion — and
/// returns whether it succeeded plus its stderr.
fn git_try(cwd: &Path, args: &[&str]) -> (bool, String) {
    let out = std::process::Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// Configures a working tree so commits succeed without a host git identity.
fn identify(work: &Path) {
    for (k, v) in [
        ("user.email", "agent@example.test"),
        ("user.name", "Agent"),
        ("commit.gpgsign", "false"),
    ] {
        git_at(work, &["config", k, v]);
    }
}

/// Builds a bare fixture repository with `main`, `topic` and `refs/pull/7/head`.
fn fixture_remote(scratch: &Scratch) -> String {
    let work = scratch.join("origin-work");
    let bare = scratch.join("origin.git");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&bare).unwrap();

    git_at(
        &bare,
        &["init", "--bare", "--quiet", "--initial-branch=main"],
    );
    git_at(&work, &["init", "--quiet", "--initial-branch=main"]);
    identify(&work);
    std::fs::write(work.join("README.md"), "# fixture\n").unwrap();
    git_at(&work, &["add", "README.md"]);
    git_at(&work, &["commit", "--quiet", "-m", "initial"]);

    git_at(&work, &["checkout", "--quiet", "-b", "topic"]);
    std::fs::write(work.join("topic.txt"), "topic\n").unwrap();
    git_at(&work, &["add", "topic.txt"]);
    git_at(&work, &["commit", "--quiet", "-m", "topic"]);
    git_at(&work, &["checkout", "--quiet", "main"]);

    let bare_str = bare.to_string_lossy().to_string();
    git_at(&work, &["remote", "add", "origin", &bare_str]);
    git_at(&work, &["push", "--quiet", "origin", "main", "topic"]);
    git_at(
        &work,
        &["push", "--quiet", "origin", "topic:refs/pull/7/head"],
    );
    git_at(&bare, &["symbolic-ref", "HEAD", "refs/heads/main"]);

    format!("file://{bare_str}")
}

/// A manager over a scratch cache, with one `file://` fixture bound as
/// `fixture`. Returns the manager and the binding.
async fn bound(scratch: &Scratch, branches: &[&str]) -> (Arc<RepoManager>, RepoBinding) {
    let url = fixture_remote(scratch);
    let manager = RepoManager::new(
        CompanyId::new("acme"),
        scratch.join("data/companies/acme/repos"),
        Arc::new(MemSecrets::default()),
    );
    let binding = manager
        .bind_local(
            &url,
            "fixture",
            branches.iter().map(|b| (*b).to_string()).collect(),
        )
        .await
        .expect("bind the fixture");
    (Arc::new(manager), binding)
}

/// The tool context over a fresh agent workspace.
fn context(
    scratch: &Scratch,
    repos: Arc<RepoManager>,
    bindings: Vec<RepoBinding>,
) -> RepoToolContext {
    let workspace = scratch.join("harness/acme/desk/workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    RepoToolContext {
        repos,
        bindings: bindings.into(),
        workspace,
        ledger: CheckoutLedger::default(),
        agent: "desk".to_string(),
        approvals: crate::harness::policy::ApprovalRequestQueue::default(),
    }
}

/// The tool by name, from a built pair.
fn tool_named(tools: &[Box<dyn Tool>], name: &str) -> usize {
    tools
        .iter()
        .position(|t| t.name() == name)
        .unwrap_or_else(|| panic!("no `{name}` tool"))
}

/// Every regular file under `dir`, as `(path, bytes)`.
fn all_files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                out.push((
                    entry.path(),
                    std::fs::read(entry.path()).unwrap_or_default(),
                ));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// A mirror's refs and its object list, as one comparable snapshot.
fn mirror_state(mirror: &Path) -> (String, Vec<PathBuf>) {
    let refs = git_at(mirror, &["show-ref"]);
    let mut objects: Vec<PathBuf> = all_files(&mirror.join("objects"))
        .into_iter()
        .map(|(p, _)| p)
        .collect();
    objects.sort();
    (refs, objects)
}

// ---------------------------------------------------------------------------
// The headline: confinement
// ---------------------------------------------------------------------------

/// **The attack this tier exists to prevent.**
///
/// The rejected `git clone --shared` design left the mirror's path in
/// `.git/objects/info/alternates` and `origin` pointing at it, so a commit plus
/// `git push origin HEAD:refs/heads/main` advanced the host's cache directly.
/// Asserting "`origin` is absent" would not catch that — removing `origin` was
/// never the missing piece, and the alternates entry survived it.
///
/// So this makes the two pushes an agent can actually make, and then compares
/// the mirror's refs **and** its object files before and after. Nothing in the
/// host's cache may have moved.
#[tokio::test]
async fn a_checkout_cannot_reach_the_mirror_it_came_from() {
    let scratch = Scratch::new("attack");
    let (manager, binding) = bound(&scratch, &["main"]).await;
    let mirror = manager.mirror_path(&binding.key);
    let checkout = scratch.join("checkout");

    materialize(&mirror, &checkout, Some("main"), None)
        .await
        .expect("materialize");

    let before = mirror_state(&mirror);

    // The agent commits something it would very much like the company to read.
    identify(&checkout);
    std::fs::write(checkout.join("POISON.md"), "owned\n").unwrap();
    git_at(&checkout, &["add", "POISON.md"]);
    git_at(&checkout, &["commit", "--quiet", "-m", "poison"]);

    // (i) The sanctioned attack: push to whatever `origin` resolves to. There
    //     is no such remote, so there is no address to aim at.
    let (ok, stderr) = git_try(&checkout, &["push", "origin", "HEAD:refs/heads/main"]);
    assert!(!ok, "a push to `origin` must fail: {stderr}");

    // (ii) The determined attack: name the mirror explicitly. The mirror's
    //      `pre-receive` hook refuses it at the receiving end.
    let url = format!("file://{}", mirror.display());
    let (ok, stderr) = git_try(&checkout, &["push", &url, "HEAD:refs/heads/main"]);
    assert!(
        !ok,
        "an explicit push to the mirror must be refused by its pre-receive hook: {stderr}"
    );
    assert!(
        stderr.contains("read-only"),
        "the refusal must come from the mirror's own hook: {stderr}"
    );

    // What actually matters: the host's cache is untouched, refs and objects.
    let after = mirror_state(&mirror);
    assert_eq!(before.0, after.0, "the mirror's refs moved");
    assert_eq!(before.1, after.1, "the mirror's object list changed");
}

/// Isolation proved by destruction: delete the mirror entirely and the checkout
/// is still a complete, self-contained repository.
///
/// This is the assertion a `--shared` clone cannot pass, and it is stronger than
/// reading the alternates file — a checkout that resolves nothing through the
/// cache keeps working when the cache is gone.
#[tokio::test]
async fn a_checkout_survives_the_mirror_being_deleted() {
    let scratch = Scratch::new("isolation");
    let (manager, binding) = bound(&scratch, &["main"]).await;
    let mirror = manager.mirror_path(&binding.key);
    let checkout = scratch.join("checkout");
    materialize(&mirror, &checkout, Some("main"), None)
        .await
        .expect("materialize");

    assert!(
        !checkout.join(".git/objects/info/alternates").exists(),
        "a checkout must never carry an alternates file"
    );

    // Every object is this checkout's own inode, not the mirror's. A hardlinked
    // clone — the other rejected design — would show a link count of 2 here.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mut checked = 0;
        for (path, _) in all_files(&checkout.join(".git/objects")) {
            let links = std::fs::metadata(&path).unwrap().nlink();
            assert_eq!(
                links,
                1,
                "{} is shared with another file — the objects were hardlinked",
                path.display()
            );
            checked += 1;
        }
        assert!(checked > 0, "no object files to check");
    }

    std::fs::remove_dir_all(&mirror).unwrap();

    git_at(&checkout, &["fsck", "--strict"]);
    let log = git_at(&checkout, &["log", "--oneline"]);
    assert!(log.contains("initial"), "history is unreachable: {log}");
    assert!(checkout.join("README.md").is_file());
}

/// No byte under the checkout's `.git/` names the mirror.
///
/// The reflog is why this is asserted over every file rather than over
/// `.git/config`: a clone writes `clone: from file:///…` into `.git/logs/HEAD`,
/// which no `remote remove` touches, so "severed" would have been a claim about
/// one file rather than a property of the directory.
#[tokio::test]
async fn nothing_in_the_checkout_names_the_host_mirror() {
    let scratch = Scratch::new("scrub");
    let (manager, binding) = bound(&scratch, &["main"]).await;
    let mirror = manager.mirror_path(&binding.key);
    let checkout = scratch.join("checkout");
    materialize(&mirror, &checkout, Some("main"), None)
        .await
        .expect("materialize");

    let needle = mirror.to_string_lossy().to_string();
    let needle = needle.as_bytes();
    for (path, bytes) in all_files(&checkout.join(".git")) {
        assert!(
            !bytes.windows(needle.len()).any(|window| window == needle),
            "{} still names the host's mirror",
            path.display()
        );
    }
    for name in ["FETCH_HEAD", "ORIG_HEAD"] {
        assert!(
            !checkout.join(".git").join(name).exists(),
            ".git/{name} records the source URL and must be removed"
        );
    }
}

/// A pull-request checkout takes the same path and lands detached on the PR's
/// head — and is severed just as completely.
#[tokio::test]
async fn a_pull_request_checkout_is_detached_and_severed() {
    let scratch = Scratch::new("pr");
    let (manager, binding) = bound(&scratch, &["main"]).await;
    manager
        .fetch(&binding.key, &[7])
        .await
        .expect("fetch the pull request head");
    let mirror = manager.mirror_path(&binding.key);
    let checkout = scratch.join("checkout");
    materialize(&mirror, &checkout, None, Some(7))
        .await
        .expect("materialize");

    // `refs/pull/7/head` is the `topic` branch's tip in the fixture.
    assert!(checkout.join("topic.txt").is_file(), "not the PR's tree");
    // `symbolic-ref` fails on a detached HEAD, which is exactly the state being
    // asserted — so this reads the exit status rather than the output.
    let (on_a_branch, _) = git_try(&checkout, &["symbolic-ref", "-q", "HEAD"]);
    assert!(
        !on_a_branch,
        "a PR checkout must be detached, not on a branch"
    );

    let needle = mirror.to_string_lossy().to_string();
    let needle = needle.as_bytes();
    for (path, bytes) in all_files(&checkout.join(".git")) {
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle),
            "{} still names the host's mirror after a PR checkout",
            path.display()
        );
    }
}

/// The credential the mirror was fetched with appears nowhere in a checkout.
///
/// The operator tier proves it for the mirror; this proves the checkout does not
/// reintroduce it — a clone copies configuration, and a token that reached
/// `.git/config` would be copied with it.
#[tokio::test]
async fn a_checkout_carries_no_credential() {
    const SENTINEL: &str = "github_pat_SENTINEL";
    let scratch = Scratch::new("sentinel");
    let url = fixture_remote(&scratch);
    let secrets = Arc::new(MemSecrets::default());
    let manager = RepoManager::new(
        CompanyId::new("acme"),
        scratch.join("data/companies/acme/repos"),
        secrets.clone(),
    );
    // A binding whose stored credential is the sentinel. `bind_local` binds with
    // no credential (the fixture needs none), so the token is written beside it
    // — which is exactly the state a real binding is in.
    let binding = manager
        .bind_local(&url, "fixture", vec!["main".into()])
        .await
        .unwrap();
    secrets
        .set(
            &CompanyId::new("acme"),
            &crate::runtime::repo_manager::repo_token_key(&binding.key),
            SecretValue(SENTINEL.to_string()),
        )
        .await
        .unwrap();

    let mirror = manager.mirror_path(&binding.key);
    let checkout = scratch.join("checkout");
    materialize(&mirror, &checkout, Some("main"), None)
        .await
        .expect("materialize");

    for (path, bytes) in all_files(&checkout) {
        assert!(
            !bytes
                .windows(SENTINEL.len())
                .any(|w| w == SENTINEL.as_bytes()),
            "{} carries the credential",
            path.display()
        );
    }
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// The ledger deletes what it recorded, and forgets it afterwards.
#[test]
fn purging_the_ledger_removes_every_recorded_path() {
    let scratch = Scratch::new("ledger");
    let tree = scratch.join("a-checkout");
    std::fs::create_dir_all(tree.join("nested")).unwrap();
    std::fs::write(tree.join("nested/file.txt"), "x").unwrap();
    let spill = scratch.join("a.diff");
    std::fs::write(&spill, "diff").unwrap();

    let ledger = CheckoutLedger::default();
    ledger.record(tree.clone());
    ledger.record(spill.clone());
    // Recording twice must not double-count — a turn may check the same
    // repository out twice.
    ledger.record(tree.clone());
    assert_eq!(ledger.paths().len(), 2);

    assert_eq!(ledger.purge(), 2);
    assert!(!tree.exists(), "the checkout survived the purge");
    assert!(!spill.exists(), "the spill survived the purge");
    assert!(ledger.paths().is_empty(), "the ledger did not empty");

    // A second purge is a no-op rather than an error, which is what makes the
    // janitor safe to claim on a path that already purged.
    assert_eq!(ledger.purge(), 0);
}

/// A path that has already vanished is not an error: the boot sweep, an
/// operator, or a redirect's mid-loop purge may have got there first.
#[test]
fn purging_a_path_that_is_already_gone_succeeds() {
    let scratch = Scratch::new("ledger-gone");
    let ledger = CheckoutLedger::default();
    ledger.record(scratch.join("never-existed"));
    assert_eq!(ledger.purge(), 1);
}

/// A checkout retained for a task survives the turn's purge, comes back on
/// reclaim, and is deleted by the turn's janitor only once the task finishes —
/// the lifecycle that lets a checkout outlive an approval park (issue #796).
#[test]
fn a_retained_checkout_survives_a_purge_and_returns_on_reclaim() {
    let scratch = Scratch::new("retain");
    let tree = scratch.join("held");
    std::fs::create_dir_all(&tree).unwrap();

    let ledger = CheckoutLedger::default();
    ledger.record(tree.clone());
    assert!(ledger.has_active(&tree));

    // Parked: move it off the turn-scoped list, under the task.
    ledger.retain_for_task("t-1");
    assert!(
        !ledger.has_active(&tree),
        "retain left it on the active list"
    );
    assert_eq!(ledger.retained_tasks(), vec!["t-1".to_string()]);

    // The turn's janitor now purges nothing — the tree is held.
    assert_eq!(ledger.purge(), 0);
    assert!(
        tree.is_dir(),
        "a retained checkout was purged with the turn"
    );

    // Resumed: reclaim brings it back under the turn's janitor.
    ledger.reclaim("t-1");
    assert!(ledger.has_active(&tree));
    assert!(ledger.retained_tasks().is_empty());

    // ...and now the janitor deletes it, the task having finished.
    assert_eq!(ledger.purge(), 1);
    assert!(!tree.exists());
}

/// `purge_task` deletes a task's held checkout directly — the task-end path,
/// where the work resumed and finished rather than being reclaimed by a turn.
/// A second call, or an unknown task, is a no-op.
#[test]
fn purge_task_deletes_a_held_checkout() {
    let scratch = Scratch::new("purge-task");
    let tree = scratch.join("held");
    std::fs::create_dir_all(&tree).unwrap();
    let ledger = CheckoutLedger::default();
    ledger.record(tree.clone());
    ledger.retain_for_task("t-1");

    assert_eq!(ledger.purge_task("t-1"), 1);
    assert!(!tree.exists());
    assert!(ledger.retained_tasks().is_empty());
    assert_eq!(ledger.purge_task("t-1"), 0);
    assert_eq!(ledger.purge_task("nope"), 0);
}

/// `sweep_orphans` deletes a held checkout the moment no live grant names its
/// task — the denied/expired cleanup — and leaves a task still awaiting its
/// resume alone (issue #796).
#[test]
fn sweep_orphans_purges_only_tasks_with_no_live_grant() {
    let scratch = Scratch::new("sweep-orphans");
    let live = scratch.join("live");
    let dead = scratch.join("dead");
    std::fs::create_dir_all(&live).unwrap();
    std::fs::create_dir_all(&dead).unwrap();

    let ledger = CheckoutLedger::default();
    ledger.record(live.clone());
    ledger.retain_for_task("t-live");
    ledger.record(dead.clone());
    ledger.retain_for_task("t-dead");

    // Only `t-live` still has a grant behind it.
    let removed = ledger.sweep_orphans(|task| task == "t-live");
    assert_eq!(removed, 1);
    assert!(live.is_dir(), "a task still awaiting its resume was swept");
    assert!(!dead.exists(), "an orphaned task's checkout survived");
    assert_eq!(ledger.retained_tasks(), vec!["t-live".to_string()]);
}

/// A parked-but-unresolved approval mints no grant, yet the checkout its step is
/// holding must survive an unrelated turn's sweep. `any_for_task` counts a
/// pending approval as live, so `sweep_orphans` spares the task until the
/// operator decides — closing the window between a park and its resolution that
/// would otherwise reopen the #796 deadlock one turn upstream. Once the approval
/// resolves (here, denied — clearing the mark), the next sweep reclaims it.
#[test]
fn sweep_orphans_spares_a_task_whose_approval_is_still_parked() {
    use crate::ports::types::ApprovalId;
    use crate::runtime::grants::GrantSet;

    let scratch = Scratch::new("sweep-pending");
    let held = scratch.join("held");
    std::fs::create_dir_all(&held).unwrap();

    let ledger = CheckoutLedger::default();
    ledger.record(held.clone());
    ledger.retain_for_task("t-parked");

    // The task parked a new step: an approval awaits the operator, so no grant
    // names the task yet. The shared grant set is the sweep's liveness oracle.
    let grants = GrantSet::default();
    let approval = ApprovalId::new("appr-parked");
    grants.mark_pending(&approval, "t-parked".to_string());

    // An unrelated turn claims the janitor and sweeps. The parked task has no
    // grant, but its pending approval keeps it live — the checkout survives.
    let removed = ledger.sweep_orphans(|task| grants.any_for_task(task));
    assert_eq!(removed, 0, "a task with a parked approval was swept");
    assert!(held.is_dir(), "the parked step's checkout was deleted");
    assert_eq!(ledger.retained_tasks(), vec!["t-parked".to_string()]);

    // The operator denies it: the mark clears, nothing names the task, and the
    // next sweep reclaims the disk.
    grants.clear_pending(&approval);
    let removed = ledger.sweep_orphans(|task| grants.any_for_task(task));
    assert_eq!(removed, 1);
    assert!(
        !held.exists(),
        "a denied task's checkout survived the sweep"
    );
    assert!(ledger.retained_tasks().is_empty());
}

/// A second park of the same task merges its paths in rather than replacing the
/// set, so a checkout retained by the first park survives the second (issue
/// #796).
#[test]
fn retaining_a_task_twice_keeps_both_checkouts() {
    let scratch = Scratch::new("retain-twice");
    let first = scratch.join("first");
    let second = scratch.join("second");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();

    let ledger = CheckoutLedger::default();
    ledger.record(first.clone());
    ledger.retain_for_task("t-1");
    ledger.record(second.clone());
    ledger.retain_for_task("t-1");

    assert_eq!(ledger.purge_task("t-1"), 2);
    assert!(!first.exists());
    assert!(!second.exists());
}

/// The boot sweep removes every agent's `workspace/repos` under **this**
/// company and leaves its siblings — the tenant-scoping that keeps one
/// company's boot from deleting another's bytes.
#[tokio::test]
async fn the_boot_sweep_clears_this_companys_checkouts_and_no_others() {
    let scratch = Scratch::new("sweep");
    let root = scratch.join("harness");
    let seed = |company: &str, agent: &str| {
        let repos = root
            .join(company)
            .join(agent)
            .join("workspace")
            .join(CHECKOUT_SUBDIR);
        std::fs::create_dir_all(repos.join("fixture")).unwrap();
        std::fs::write(repos.join("fixture/README.md"), "cloned\n").unwrap();
        repos
    };
    let ours = seed("acme", "desk");
    let also_ours = seed("acme", "ceo");
    let theirs = seed("other", "desk");

    // A sibling the sweep must not touch: the agent's own workspace files.
    let keep = root.join("acme/desk/workspace/notes.md");
    std::fs::write(&keep, "mine\n").unwrap();

    let reclaimed = sweep_orphaned_checkouts(&root, &CompanyId::new("acme")).await;

    assert!(!ours.exists(), "this company's checkout survived the sweep");
    assert!(!also_ours.exists(), "a second agent's checkout survived");
    assert!(theirs.exists(), "another company's checkout was deleted");
    assert!(keep.is_file(), "the sweep took a workspace file with it");
    assert!(reclaimed > 0, "nothing was reported reclaimed");
}

/// Sweeping a company that has never run is a silent no-op.
#[tokio::test]
async fn the_boot_sweep_is_quiet_when_there_is_nothing_to_sweep() {
    let scratch = Scratch::new("sweep-empty");
    assert_eq!(
        sweep_orphaned_checkouts(&scratch.join("harness"), &CompanyId::new("acme")).await,
        0
    );
}

// ---------------------------------------------------------------------------
// Resolution and refusals
// ---------------------------------------------------------------------------

/// A repository argument is a **lookup** against what is bound, in all three
/// accepted spellings — and an unknown one is refused with the list rather than
/// interpolated into a path.
#[tokio::test]
async fn a_repository_argument_only_ever_selects_something_already_bound() {
    let scratch = Scratch::new("resolve");
    let (manager, binding) = bound(&scratch, &["main"]).await;
    // The key is DERIVED, not written out: resolution by canonical URL parses
    // the URL and looks the derived key up, so a hand-written key would make
    // that arm pass or fail for reasons unrelated to the lookup.
    let coords = parse_repo_url("https://github.com/acme/widgets").unwrap();
    let mut named = binding.clone();
    named.key = coords.key();
    named.owner = "acme".into();
    named.repo = "widgets".into();
    named.url = coords.canonical_url();
    let ctx = context(&scratch, manager, vec![named.clone()]);

    for spelling in [
        named.key.as_str(),
        "acme/widgets",
        "AcMe/Widgets",
        "https://github.com/acme/widgets",
    ] {
        assert_eq!(
            ctx.resolve(spelling).expect(spelling).key,
            named.key,
            "{spelling} should resolve"
        );
    }

    for unknown in [
        "",
        "other/thing",
        "https://github.com/other/thing",
        "../../etc",
    ] {
        let err = ctx.resolve(unknown).expect_err(unknown);
        assert!(
            err.contains("acme/widgets"),
            "a refusal must name what IS bound: {err}"
        );
    }
}

/// A branch the binding does not mirror is refused by name, before git is asked
/// — the mirror fetches by refspec, so "did not resolve" from git would be a
/// worse way to learn it.
#[tokio::test]
async fn checking_out_an_unmirrored_branch_is_refused_with_the_bound_list() {
    let scratch = Scratch::new("branch");
    let (manager, binding) = bound(&scratch, &["main"]).await;
    let ctx = context(&scratch, manager, vec![binding]);
    let tools = repo_tools(ctx);
    let checkout = &tools[tool_named(&tools, REPO_CHECKOUT_TOOL)];

    let result = checkout
        .execute(json!({ "repo": "fixture", "ref": "topic" }))
        .await
        .unwrap();
    assert!(result.is_error, "{result:?}");
    assert!(result.text().contains("topic"), "{}", result.text());
    assert!(result.text().contains("main"), "{}", result.text());
}

/// A ref that would be read as a command-line option is refused by the same
/// validator the bind path uses. One shape rule, not two.
#[tokio::test]
async fn a_ref_that_looks_like_an_option_is_refused() {
    let scratch = Scratch::new("ref-option");
    let (manager, binding) = bound(&scratch, &["main"]).await;
    let ctx = context(&scratch, manager, vec![binding]);
    let tools = repo_tools(ctx);
    let checkout = &tools[tool_named(&tools, REPO_CHECKOUT_TOOL)];

    for bad in ["--upload-pack=touch /tmp/pwn", "a..b", "main.lock"] {
        let result = checkout
            .execute(json!({ "repo": "fixture", "ref": bad }))
            .await
            .unwrap();
        assert!(result.is_error, "`{bad}` should be refused: {result:?}");
    }
}

/// A checkout lands at a workspace-**relative** path, records itself on the
/// ledger, and never names the host's mirror in what the agent is told.
#[tokio::test]
async fn a_successful_checkout_reports_a_relative_path_and_records_it() {
    let scratch = Scratch::new("happy");
    let (manager, binding) = bound(&scratch, &["main"]).await;
    let mirror = manager.mirror_path(&binding.key);
    let ctx = context(&scratch, manager, vec![binding.clone()]);
    let workspace = ctx.workspace.clone();
    let ledger = ctx.ledger.clone();
    let tools = repo_tools(ctx);
    let checkout = &tools[tool_named(&tools, REPO_CHECKOUT_TOOL)];

    let result = checkout
        .execute(json!({ "repo": "fixture" }))
        .await
        .unwrap();
    assert!(!result.is_error, "{result:?}");
    assert!(
        result.text().contains("repos/fixture"),
        "the reply must name the relative path: {}",
        result.text()
    );
    assert!(
        !result
            .text()
            .contains(&mirror.to_string_lossy().to_string()),
        "the reply leaked the host's mirror path: {}",
        result.text()
    );
    let tree = workspace.join(CHECKOUT_SUBDIR).join(&binding.key);
    assert!(tree.join("README.md").is_file(), "no working tree");
    assert_eq!(ledger.paths(), vec![tree.clone()]);

    // And the janitor's contract, end to end.
    ledger.purge();
    assert!(!tree.exists());
}

/// A second `repo_checkout` of a repo the task already holds is reused only when
/// it asks for the **same** ref. A different branch — or a pull request — is
/// refused by name rather than silently returning the held tree (the wrong ref)
/// or cloning over the commits the parked step is holding (issue #796).
#[tokio::test]
async fn a_second_checkout_at_a_different_ref_is_refused_not_silently_reused() {
    let scratch = Scratch::new("reuse-ref");
    let (manager, binding) = bound(&scratch, &["main", "topic"]).await;
    let ctx = context(&scratch, manager, vec![binding]);
    let tools = repo_tools(ctx);
    let checkout = &tools[tool_named(&tools, REPO_CHECKOUT_TOOL)];

    // First checkout materializes `main` and records it on the shared ledger.
    let first = checkout
        .execute(json!({ "repo": "fixture", "ref": "main" }))
        .await
        .unwrap();
    assert!(!first.is_error, "{first:?}");

    // Same ref: the held tree is on `main`, so it is reused, not re-cloned.
    let same = checkout
        .execute(json!({ "repo": "fixture", "ref": "main" }))
        .await
        .unwrap();
    assert!(!same.is_error, "same ref must reuse: {same:?}");
    assert!(
        same.text().contains("already have") && same.text().contains("branch main"),
        "the reuse notice must name the ref: {}",
        same.text()
    );

    // A different branch: refused, naming both what it is on and what was asked.
    let other = checkout
        .execute(json!({ "repo": "fixture", "ref": "topic" }))
        .await
        .unwrap();
    assert!(other.is_error, "a different ref must be refused: {other:?}");
    assert!(
        other.text().contains("branch main") && other.text().contains("branch topic"),
        "the refusal must name the held ref and the requested one: {}",
        other.text()
    );

    // A pull request over the same held branch: refused the same way.
    let pr = checkout
        .execute(json!({ "repo": "fixture", "pr": 7 }))
        .await
        .unwrap();
    assert!(
        pr.is_error,
        "a pr over a held branch must be refused: {pr:?}"
    );
    assert!(
        pr.text().contains("branch main") && pr.text().contains("pull request #7"),
        "the refusal must name the held branch and the requested pr: {}",
        pr.text()
    );
}

/// A checkout that would push the company past its cap is refused **before**
/// anything is transferred — refusal, not eviction, matching the operator tier.
///
/// The binding is made through an uncapped manager and the cap applied to a
/// second one over the same cache and the same secret store, because the
/// operator tier's own post-fetch cap would otherwise refuse the *bind* and this
/// test would never reach the checkout path it is about.
#[tokio::test]
async fn an_over_quota_checkout_is_refused_and_nothing_is_written() {
    let scratch = Scratch::new("quota");
    let url = fixture_remote(&scratch);
    let secrets = Arc::new(MemSecrets::default());
    let root = scratch.join("data/companies/acme/repos");
    let uncapped = RepoManager::new(CompanyId::new("acme"), root.clone(), secrets.clone());
    let binding = uncapped
        .bind_local(&url, "fixture", vec!["main".into()])
        .await
        .expect("bind the fixture");

    // A cap the mirror REFRESH clears and the checkout does not. A cap of one
    // byte would be refused by the operator tier's own post-fetch check first,
    // and this test would pass while never reaching the code it is about.
    let capped = Arc::new(
        RepoManager::new(CompanyId::new("acme"), root, secrets)
            .with_quota(Some(binding.size_bytes + 4096)),
    );
    let ctx = context(&scratch, capped, vec![binding.clone()]);
    let workspace = ctx.workspace.clone();
    let ledger = ctx.ledger.clone();
    let tools = repo_tools(ctx);
    let checkout = &tools[tool_named(&tools, REPO_CHECKOUT_TOOL)];

    let result = checkout
        .execute(json!({ "repo": "fixture" }))
        .await
        .unwrap();
    assert!(result.is_error, "{result:?}");
    let text = result.text();
    assert!(text.contains("capped at"), "{text}");
    assert!(
        text.contains("tree_quota_gb"),
        "the refusal must name the knob to change: {text}"
    );
    assert!(
        !workspace.join(CHECKOUT_SUBDIR).join(&binding.key).exists(),
        "an over-quota refusal must transfer nothing"
    );
    assert!(ledger.paths().is_empty(), "nothing should be on the ledger");
}

/// A manager whose fixture is bound *and* credentialed, so the pull-request
/// path reaches the scripted forge instead of the operator tier's
/// "bound without a credential" refusal.
async fn bound_with_forge(scratch: &Scratch, diff: &str) -> (Arc<RepoManager>, RepoBinding) {
    let url = fixture_remote(scratch);
    let secrets = Arc::new(MemSecrets::default());
    let manager = RepoManager::new(
        CompanyId::new("acme"),
        scratch.join("data/companies/acme/repos"),
        secrets.clone(),
    )
    .with_host(Arc::new(ScriptedHost {
        diff: diff.to_string(),
    }));
    let binding = manager
        .bind_local(&url, "fixture", vec!["main".into()])
        .await
        .expect("bind the fixture");
    secrets
        .set(
            &CompanyId::new("acme"),
            &crate::runtime::repo_manager::repo_token_key(&binding.key),
            SecretValue("github_pat_fixture".to_string()),
        )
        .await
        .unwrap();
    (Arc::new(manager), binding)
}

/// A small diff is read inline and writes nothing.
#[tokio::test]
async fn a_small_pull_request_diff_is_read_inline_and_spills_nothing() {
    let scratch = Scratch::new("pr-small");
    let (manager, binding) = bound_with_forge(&scratch, "--- a\n+++ b\n+one line\n").await;
    let ctx = context(&scratch, manager, vec![binding]);
    let workspace = ctx.workspace.clone();
    let ledger = ctx.ledger.clone();
    let tools = repo_tools(ctx);
    let pr = &tools[tool_named(&tools, REPO_PR_TOOL)];

    let result = pr
        .execute(json!({ "repo": "fixture", "number": 7 }))
        .await
        .unwrap();
    assert!(!result.is_error, "{result:?}");
    let text = result.text();
    assert!(
        text.contains("+one line"),
        "the diff must be inline: {text}"
    );
    assert!(text.contains("#7"), "{text}");
    assert!(text.contains("a change"), "the title is missing: {text}");
    assert!(
        ledger.paths().is_empty(),
        "a small diff must not create a file"
    );
    assert!(!workspace.join(CHECKOUT_SUBDIR).exists());
}

/// A diff too large to read inline is written to the workspace, named in the
/// reply, and recorded on the ledger so the turn's janitor removes it.
///
/// Not a second truncation for its own sake: every tool result is cut on its
/// way into the model's context, so an in-band megabyte would be clipped with
/// no way to reach the rest. A file can be read and grepped.
#[tokio::test]
async fn a_large_pull_request_diff_is_spilled_to_a_file_the_agent_can_read() {
    let scratch = Scratch::new("pr-large");
    let big = format!("--- a\n+++ b\n{}", "+line\n".repeat(20_000));
    assert!(
        big.len() > MAX_INLINE_DIFF_BYTES,
        "the fixture must be over the cap"
    );
    let (manager, binding) = bound_with_forge(&scratch, &big).await;
    let ctx = context(&scratch, manager, vec![binding.clone()]);
    let workspace = ctx.workspace.clone();
    let ledger = ctx.ledger.clone();
    let tools = repo_tools(ctx);
    let pr = &tools[tool_named(&tools, REPO_PR_TOOL)];

    let result = pr
        .execute(json!({ "repo": "fixture", "number": 7 }))
        .await
        .unwrap();
    assert!(!result.is_error, "{result:?}");
    let text = result.text();
    let relative = format!("{CHECKOUT_SUBDIR}/{}.pr-7.diff", binding.key);
    assert!(
        text.contains(&relative),
        "the reply must name the file: {text}"
    );
    assert!(
        !text.contains("+line\n+line"),
        "an oversized diff must not also be inline: {} bytes",
        text.len()
    );

    let spilled = workspace.join(&relative);
    assert_eq!(
        std::fs::read_to_string(&spilled).unwrap(),
        big,
        "the file must hold the whole diff the host obtained"
    );
    assert_eq!(ledger.paths(), vec![spilled.clone()]);
    ledger.purge();
    assert!(!spilled.exists(), "the janitor must remove the spill");
}

/// The host's own 1 MiB cut is named in the reply, so an agent can tell which
/// of the two truncations it is looking at.
#[tokio::test]
async fn a_host_truncated_diff_says_the_rest_was_never_transferred() {
    let scratch = Scratch::new("pr-hostcap");
    let big = format!(
        "--- a\n+++ b\n{}\n\n[truncated: 5000 more bytes not shown]\n",
        "+line\n".repeat(20_000)
    );
    let (manager, binding) = bound_with_forge(&scratch, &big).await;
    let ctx = context(&scratch, manager, vec![binding]);
    let tools = repo_tools(ctx);
    let pr = &tools[tool_named(&tools, REPO_PR_TOOL)];

    let result = pr
        .execute(json!({ "repo": "fixture", "number": 7 }))
        .await
        .unwrap();
    assert!(!result.is_error, "{result:?}");
    assert!(result.text().contains("1 MiB"), "{}", result.text());
}

/// With no forge client wired, `repo_pr` answers the operator tier's honest
/// "not wired" rather than an empty diff a caller would read as "no changes".
#[tokio::test]
async fn repo_pr_says_the_forge_is_not_wired_rather_than_inventing_an_empty_diff() {
    let scratch = Scratch::new("pr-unwired");
    let (manager, binding) = bound(&scratch, &["main"]).await;
    let ctx = context(&scratch, manager, vec![binding]);
    let tools = repo_tools(ctx);
    let pr = &tools[tool_named(&tools, REPO_PR_TOOL)];

    let result = pr
        .execute(json!({ "repo": "fixture", "number": 7 }))
        .await
        .unwrap();
    assert!(result.is_error, "{result:?}");
    let text = result.text();
    // The operator tier's `Unimplemented`, passed through rather than dressed
    // up. An agent told the capability is missing can say so; one told "no
    // changes" reports a wrong conclusion confidently — which is the whole
    // reason the manager refuses to answer with an empty diff.
    assert!(text.contains("forge client"), "{text}");
    assert!(text.contains("github"), "{text}");
    assert!(
        text.contains("#7"),
        "the refusal must name what was asked: {text}"
    );
}

/// Both tools are built, under their pinned names.
#[tokio::test]
async fn the_pair_is_wired_under_the_declared_names() {
    let scratch = Scratch::new("names");
    let (manager, binding) = bound(&scratch, &["main"]).await;
    let tools = repo_tools(context(&scratch, manager, vec![binding]));
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert_eq!(names, vec![REPO_CHECKOUT_TOOL, REPO_PR_TOOL]);
    assert_eq!(REPO_CHECKOUT_TOOL, "repo_checkout");
    assert_eq!(REPO_PR_TOOL, "repo_pr");
}

// ---------------------------------------------------------------------------
// repo_publish (issue #735)
// ---------------------------------------------------------------------------

/// Materializes a checkout at the path `repo_publish` resolves and commits one
/// file there, standing in for an agent that checked out and committed.
async fn committed_checkout(ctx: &RepoToolContext, mirror: &Path, key: &str) {
    let dest = ctx.workspace.join(CHECKOUT_SUBDIR).join(key);
    materialize(mirror, &dest, Some("main"), None)
        .await
        .expect("materialize");
    identify(&dest);
    std::fs::write(dest.join("FIX.md"), "the fix\n").unwrap();
    git_at(&dest, &["add", "FIX.md"]);
    git_at(&dest, &["commit", "--quiet", "-m", "the fix"]);
}

/// Issue #796: a materialized checkout has commit/tag signing turned OFF, so an
/// agent's `git commit` never blocks on a GPG key the sandbox cannot reach —
/// even when the host operator's own git config turns signing on. Without this
/// the change stages but never commits and the whole write flow stalls.
#[tokio::test]
async fn a_checkout_disables_commit_signing() {
    let scratch = Scratch::new("no-gpgsign");
    let dest = scratch.join("checkout");
    std::fs::create_dir_all(&dest).unwrap();
    git_at(&dest, &["init", "--quiet"]);
    // A host that signs its own commits.
    git_at(&dest, &["config", "commit.gpgsign", "true"]);

    attribute_checkout(&dest, "coder").await;

    assert_eq!(
        git_at(&dest, &["config", "--get", "commit.gpgsign"]),
        "false"
    );
    assert_eq!(git_at(&dest, &["config", "--get", "tag.gpgsign"]), "false");
    // And the identity is still the agent's seat (issue #735).
    assert_eq!(git_at(&dest, &["config", "--get", "user.name"]), "coder");
}

/// Publishing outside a task refuses — this tier is task turns only, and there
/// is no card to name the branch. Nothing is staged and nothing is queued.
#[tokio::test]
async fn repo_publish_without_a_task_refuses() {
    let scratch = Scratch::new("publish-no-task");
    let (manager, mut binding) = bound(&scratch, &["main"]).await;
    binding.can_push = Some(true); // push-capable, so the task check is what refuses
    let ctx = context(&scratch, manager, vec![binding.clone()]);
    ctx.ledger.set_task(None); // a chat turn: no card
    let tool = repo_publish_tool(ctx.clone());

    let result = tool
        .execute(json!({ "repo": binding.key, "message": "the fix" }))
        .await
        .unwrap();
    assert!(result.is_error, "{result:?}");
    assert!(result.text().contains("task"), "{}", result.text());
    assert_eq!(
        ctx.approvals.queued(),
        0,
        "nothing may be queued when refused"
    );
}

/// On a task turn, publishing stages the agent's commit onto the mirror's
/// namespaced branch and queues a native (`agent: None`) `repo.publish` approval
/// for the operator. The push itself is NOT done in the tool.
#[tokio::test]
async fn repo_publish_stages_and_queues_a_native_approval() {
    let scratch = Scratch::new("publish-queue");
    let (manager, mut binding) = bound(&scratch, &["main"]).await;
    binding.can_push = Some(true);
    let mirror = manager.mirror_path(&binding.key);
    let ctx = context(&scratch, manager, vec![binding.clone()]);
    ctx.ledger.set_task(Some("card-1".to_string()));
    committed_checkout(&ctx, &mirror, &binding.key).await;
    let tool = repo_publish_tool(ctx.clone());

    let result = tool
        .execute(json!({ "repo": binding.key, "message": "the fix" }))
        .await
        .unwrap();
    assert!(!result.is_error, "{result:?}");
    assert!(
        result.text().contains("approve") || result.text().contains("pending"),
        "the agent must be told it is pending, not delivered: {}",
        result.text()
    );

    // The commit is staged onto the host-owned branch in the mirror...
    let staged = git_at(&mirror, &["rev-parse", "refs/heads/oc/acme/card-1"]);
    assert!(!staged.is_empty(), "the mirror carries the staged branch");

    // ...and a single native approval is queued for the push.
    let drained = ctx.approvals.drain(16);
    assert_eq!(
        drained.requests.len(),
        1,
        "one approval queued: {drained:?}"
    );
    let req = &drained.requests[0];
    assert_eq!(req.tool, REPO_PUBLISH_TOOL);
    assert_eq!(req.effect.kind, "repo.publish");
    assert_eq!(
        req.effect.agent, None,
        "a native effect: the runtime performs the push on approval, not a re-dispatched agent"
    );
    assert_eq!(
        req.effect.payload.get("branch").and_then(|v| v.as_str()),
        Some("oc/acme/card-1"),
        "the approval carries the host-generated branch"
    );
    // The approval is bound to the exact staged commit, so the push is not at the
    // mercy of a later re-stage of the same task.
    assert_eq!(
        req.effect.payload.get("head").and_then(|v| v.as_str()),
        Some(staged.as_str()),
        "the approval carries the staged commit id"
    );
}

/// The tool is wired when any bound credential can push, but a specific
/// repository whose own credential is read-only must be refused before staging —
/// not left to fail when the host tries the push (issue #735).
#[tokio::test]
async fn repo_publish_refuses_a_read_only_binding() {
    let scratch = Scratch::new("publish-readonly");
    let (manager, mut binding) = bound(&scratch, &["main"]).await;
    binding.can_push = Some(false); // this repository's credential cannot push
    let mirror = manager.mirror_path(&binding.key);
    let ctx = context(&scratch, manager, vec![binding.clone()]);
    ctx.ledger.set_task(Some("card-1".to_string()));
    committed_checkout(&ctx, &mirror, &binding.key).await;
    let tool = repo_publish_tool(ctx.clone());

    let result = tool
        .execute(json!({ "repo": binding.key, "message": "the fix" }))
        .await
        .unwrap();
    assert!(result.is_error, "{result:?}");
    assert!(
        result.text().contains("read-only"),
        "the refusal must name the read-only credential: {}",
        result.text()
    );
    assert_eq!(
        ctx.approvals.queued(),
        0,
        "a read-only binding must stage and queue nothing"
    );
    // And nothing was staged into the mirror.
    let (ok, _) = git_try(&mirror, &["rev-parse", "refs/heads/oc/acme/card-1"]);
    assert!(!ok, "no branch may be staged for a read-only binding");
}
