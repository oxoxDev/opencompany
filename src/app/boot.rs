//! Starting an instance from inside another program.
//!
//! `serve` in `src/bin/opencompany.rs` is the command-line entry point, and it
//! does several things before it ever builds a router: it resolves the data
//! root, locks it, migrates a legacy bundle nest, materializes the canonical
//! workspace layout, and prepares the agent journal. An embedder — the desktop
//! shell, which links this crate rather than spawning the binary — has to do
//! exactly the same things, and a second copy of that sequence would drift from
//! the first the moment either changed.
//!
//! So the sequence lives here, in [`prepare_instance`]. `serve` still runs its
//! own copy: it resolves `--home` and the data root separately (they can
//! diverge, which is what `store::home_divergence_warning` is for) and it takes
//! the environment-mutating half of the journal preparation, which an embedder
//! must not. Anything added to one belongs in the other until that split is
//! closed — the [`DataLayout`](crate::store::DataLayout) step below is here
//! because it was in `serve` only, so a desktop instance never created its
//! `memory/`, `store/`, `files/`, `logs/` or `tmp/` directories and never
//! cleared stale scratch on restart.
//!
//! ## The `set_var` hazard, which is why this is not just a function call
//!
//! [`journal::prepare`](crate::app::journal::prepare) exports
//! `OPENHUMAN_WORKSPACE` with `std::env::set_var`, and its `SAFETY` comment is
//! conditioned on being called "once during startup, before any company
//! runtime, scheduler, mailbox poller or HTTP listener exists, so no other
//! thread is reading the environment concurrently".
//!
//! **In a desktop process that condition is false.** Tauri has already started
//! its async runtime, its webview process and its plugin threads before any of
//! this runs, and `setenv` racing a concurrent `getenv` is undefined behaviour
//! on glibc rather than merely a stale read.
//!
//! [`prepare_journal`] therefore uses the *pure* half,
//! [`prepare_with`](crate::app::journal::prepare_with), which resolves and
//! probes the root and mutates nothing. An embedder that needs the variable
//! exported must do it in `main`, before it starts anything else —
//! [`EmbeddedInstance::journal_env`] returns exactly what to set.

use std::path::{Path, PathBuf};

use crate::Result;
use crate::app::config::{ConfigFile, WorkspaceConfig};
use crate::app::journal::JournalRoot;
use crate::store::DataLayout;
use crate::store::lock::HomeLock;

/// A prepared instance root: locked, migrated, and with its journal resolved.
///
/// Holds the [`HomeLock`] for as long as it lives, so the caller keeps this
/// alive for the lifetime of the instance. Dropping it hands the data root to
/// whatever asks next while this process is still writing.
#[derive(Debug)]
#[must_use = "dropping this releases the instance data root"]
pub struct EmbeddedInstance {
    home: PathBuf,
    journal: JournalRoot,
    workspace: WorkspaceConfig,
    _lock: HomeLock,
}

impl EmbeddedInstance {
    /// The resolved instance data root.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The resolved agent journal root.
    pub fn journal(&self) -> &JournalRoot {
        &self.journal
    }

    /// The `[workspace]` section of the root's `config.toml`, resolved against
    /// its defaults.
    ///
    /// Returned rather than only applied, because two of its fields are not
    /// layout at all: `quota` and `git_enabled` belong on the embedder's
    /// [`AppConfig`](crate::AppConfig) exactly as `serve` puts them there. An
    /// embedder that ignores them runs with the compiled-in defaults and
    /// silently disregards an operator's `config.toml`.
    pub fn workspace(&self) -> &WorkspaceConfig {
        &self.workspace
    }

    /// The `(name, value)` an embedder must export **before** starting any
    /// other thread, in place of the `set_var` [`prepare`] would have done.
    ///
    /// See the module docs: doing this from `main` is safe, and doing it from
    /// here would not be.
    ///
    /// [`prepare`]: crate::app::journal::prepare
    pub fn journal_env(&self) -> (&'static str, &Path) {
        (
            crate::app::journal::OPENHUMAN_WORKSPACE_ENV,
            self.journal.root(),
        )
    }
}

/// Resolves, locks and prepares an instance data root.
///
/// The order is load-bearing:
///
/// 1. **Resolve** the root, so everything below agrees on where it is.
/// 2. **Lock** it, before reading or writing anything. Migration rewrites the
///    bundle layout, and two processes migrating one root concurrently is the
///    worst moment to discover they were sharing it.
/// 3. **Migrate** the legacy nest, exactly as `serve` does.
/// 4. **Materialize** the canonical workspace layout — `memory/`, `store/`,
///    `files/`, `logs/`, `tmp/` — clearing the ephemeral `tmp/` scratch first
///    when `[workspace].clear_tmp_on_startup` allows it (the default). Before
///    the journal, because both write under the root and a layout failure is
///    the cheaper of the two to hit.
/// 5. **Prepare** the journal, proving its root is writable — an unwritable one
///    aborts here rather than at the first agent turn.
///
/// One root, not two. An embedder passes the single directory it owns, so the
/// company bundles, the `config.toml` and the workspace layout all resolve
/// under it — the aligned shape
/// [`home_divergence_warning`](crate::store::home_divergence_warning) is silent
/// about. `serve` is the caller that can split them, with `--home` pointing the
/// bundles somewhere other than `OPENCOMPANY_DATA_DIR`, and it keeps its own
/// copy of this sequence for that reason.
pub async fn prepare_instance(home: Option<PathBuf>) -> Result<EmbeddedInstance> {
    let home = crate::store::resolve_home(home)?;
    let lock = crate::store::lock::acquire(&home)?;
    // The announced variant, same as `serve`: a migration that moved bundles is
    // something an operator should see in the log rather than infer later.
    crate::store::migrate::migrate_legacy_nest_announced(&home)?;
    // Read after the migration: an un-migrated install's `config.toml` may
    // still be one level down, and moving it up is what the step above does.
    let workspace = ConfigFile::load(&home)?
        .map(|file| file.workspace)
        .unwrap_or_default()
        .resolve();
    DataLayout::new(&home)
        .ensure(workspace.clear_tmp_on_startup)
        .await?;
    let journal = prepare_journal(&home).await?;

    Ok(EmbeddedInstance {
        home,
        journal,
        workspace,
        _lock: lock,
    })
}

/// Resolves and validates the journal root **without touching the environment**.
///
/// The non-mutating half of `journal::prepare`. See the module docs for why an
/// embedder must not take the mutating one.
async fn prepare_journal(home: &Path) -> Result<JournalRoot> {
    let existing = std::env::var(crate::app::journal::OPENHUMAN_WORKSPACE_ENV).ok();
    crate::app::journal::prepare_with(existing.as_deref(), home).await
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn a_prepared_root_is_locked_against_a_second_instance() {
        let dir = tempfile::tempdir().unwrap();
        let first = prepare_instance(Some(dir.path().to_path_buf()))
            .await
            .expect("the first instance prepares");
        assert_eq!(first.home(), dir.path());

        let refused = prepare_instance(Some(dir.path().to_path_buf()))
            .await
            .expect_err("a second instance on one root must be refused");
        assert!(
            refused
                .to_string()
                .contains("already using the data directory"),
            "unexpected refusal: {refused}"
        );
    }

    /// How long [`releasing_the_instance_frees_the_root`] waits for a released
    /// root to become takeable, and how long it sleeps between attempts.
    ///
    /// The window this rides out is measured in microseconds (see the test's
    /// own comment), so two seconds is several orders of magnitude of headroom
    /// — generous enough that only a genuinely stuck lock exhausts it, short
    /// enough that a real regression fails the suite promptly rather than
    /// looking like a hang.
    const RELEASE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
    const RELEASE_POLL: std::time::Duration = std::time::Duration::from_millis(10);

    #[tokio::test]
    async fn releasing_the_instance_frees_the_root() {
        // A desktop app restarted after a clean quit must be able to start.
        let dir = tempfile::tempdir().unwrap();
        drop(
            prepare_instance(Some(dir.path().to_path_buf()))
                .await
                .unwrap(),
        );

        // Retried rather than asserted in the same instant, because an instant
        // re-acquire is stricter than the lock promises. `store::lock`'s "The
        // fork window" section states it outright: the lock belongs to the open
        // file description, so between `fork()` and `exec()` a child transiently
        // shares every descriptor its parent held. This binary spawns
        // subprocesses in sibling tests (`app::journal`'s vendored-seam and
        // keyring-pin children), and one landing in this test's release window
        // keeps the just-released root locked for the microseconds until the
        // child `exec`s and its `O_CLOEXEC` copy closes.
        //
        // So this asserts what the lock actually guarantees — that the root
        // *becomes* takeable — and not that it is takeable within one
        // instruction. A lock that never releases still fails: the loop reports
        // the last refusal after the timeout rather than passing quietly, which
        // is what stops the retry being a blindfold over a real regression.
        let deadline = std::time::Instant::now() + RELEASE_TIMEOUT;
        let mut attempts = 0;
        let last = loop {
            attempts += 1;
            match prepare_instance(Some(dir.path().to_path_buf())).await {
                Ok(_) => return,
                Err(e) if std::time::Instant::now() >= deadline => break e,
                Err(_) => tokio::time::sleep(RELEASE_POLL).await,
            }
        };
        panic!(
            "a released root must become takeable, but {attempts} attempts over \
             {RELEASE_TIMEOUT:?} were all refused; last error: {last}"
        );
    }

    /// The gap this closed: `serve` materialized the workspace layout and the
    /// desktop did not, so an embedded instance never had `memory/`, `store/`,
    /// `files/`, `logs/` or `tmp/` and never cleared stale scratch on restart.
    #[tokio::test]
    async fn preparing_materializes_the_workspace_layout() {
        let dir = tempfile::tempdir().unwrap();
        let instance = prepare_instance(Some(dir.path().to_path_buf()))
            .await
            .unwrap();

        let layout = DataLayout::new(instance.home());
        for expected in [
            layout.memory_dir(),
            layout.store_dir(),
            layout.files_dir(),
            layout.logs_dir(),
            layout.tmp_dir(),
        ] {
            assert!(
                expected.is_dir(),
                "{} must exist after prepare_instance",
                expected.display()
            );
        }
    }

    #[tokio::test]
    async fn preparing_clears_tmp_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let stale = DataLayout::new(dir.path()).tmp_dir().join("stale.txt");
        std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
        std::fs::write(&stale, "from a previous run").unwrap();

        drop(
            prepare_instance(Some(dir.path().to_path_buf()))
                .await
                .unwrap(),
        );

        assert!(
            !stale.exists(),
            "the ephemeral tmp/ scratch must not survive a restart"
        );
    }

    /// `[workspace]` in the root's `config.toml` is honoured by the embedded
    /// path, not only by `serve` — both the layout knob and the two fields the
    /// embedder has to put on its own `AppConfig`.
    #[tokio::test]
    async fn preparing_reads_the_workspace_section_from_config_toml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[workspace]\nclear_tmp_on_startup = false\ngit_enabled = true\nmax_blob_mb = 8.0\n",
        )
        .unwrap();
        let keep = DataLayout::new(dir.path()).tmp_dir().join("keep.txt");
        std::fs::create_dir_all(keep.parent().unwrap()).unwrap();
        std::fs::write(&keep, "kept").unwrap();

        let instance = prepare_instance(Some(dir.path().to_path_buf()))
            .await
            .unwrap();

        assert!(
            keep.exists(),
            "clear_tmp_on_startup = false must be honoured"
        );
        assert!(instance.workspace().git_enabled);
        assert_eq!(
            instance.workspace().quota.max_blob_bytes,
            8 * 1024 * 1024,
            "the embedder must see the configured blob cap, not the default"
        );
    }

    #[tokio::test]
    async fn preparing_reports_the_journal_without_exporting_it() {
        // The whole point of the embedded path: no `set_var`, because in a
        // desktop process other threads are already running and racing a
        // concurrent `getenv` is undefined behaviour rather than a stale read.
        let dir = tempfile::tempdir().unwrap();
        let before = std::env::var("OPENHUMAN_WORKSPACE").ok();

        let instance = prepare_instance(Some(dir.path().to_path_buf()))
            .await
            .unwrap();

        assert_eq!(std::env::var("OPENHUMAN_WORKSPACE").ok(), before);
        let (name, value) = instance.journal_env();
        assert_eq!(name, "OPENHUMAN_WORKSPACE");
        assert!(
            value.starts_with(dir.path()),
            "the journal must sit under the instance root: {}",
            value.display()
        );
    }
}
