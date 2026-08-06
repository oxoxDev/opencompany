//! Where the embedded OpenHuman runtime keeps its durable agent journal.
//!
//! The vendored runtime writes every agent observation under
//! `{workspace}/tinyagents_store/`, and it resolves `{workspace}` from *its
//! own* config — which, absent an override, is a subdirectory of the user's
//! home directory. In a hosted tenant that home directory sits on the
//! **read-only root filesystem**: the store's first `create_dir_all` fails with
//! `EROFS`, and the vendored append worker reports the failure once per queued
//! event, forever, on stderr. Nothing else in the container's log survives that
//! (issue #446).
//!
//! OpenCompany's writable per-instance root is the data directory
//! ([`data_dir_from_env`](super::config::data_dir_from_env) — `/data` in a
//! tenant, where the manager mounts the persistent volume). This module points
//! the vendored resolver there through the one seam it honours — the
//! `OPENHUMAN_WORKSPACE` environment variable, which its config loader consults
//! *before* any home-directory default — and proves the resulting root is
//! writable while still in `serve`'s startup path.
//!
//! Two properties follow from doing the check at startup:
//!
//! * **It is loud once, not quiet forever.** A root that cannot be created is a
//!   misconfiguration the operator must see immediately, so [`prepare`] returns
//!   an error and `serve` aborts rather than running an agent whose work is
//!   never recorded.
//! * **The per-append flood cannot begin.** The vendored append worker has no
//!   dedup or backoff and reports straight to stderr, so no log filter on this
//!   side can bound it. Refusing to boot means the loop is never entered — the
//!   condition it would retry against cannot resolve itself while the process
//!   lives, because the mount is read-only for the lifetime of the pod.
//!
//! Only the resolved *path* is reported at startup. No environment value other
//! than `OPENHUMAN_WORKSPACE` is read here and none is echoed, so the startup
//! line cannot carry credential material.

use std::path::{Path, PathBuf};

use crate::Result;
use crate::error::OpenCompanyError;

/// The vendored runtime's workspace override. Its config loader reads this
/// before consulting the home-directory default, which makes it the only seam
/// a host process has for redirecting the journal.
pub const OPENHUMAN_WORKSPACE_ENV: &str = "OPENHUMAN_WORKSPACE";

/// The data-dir subdirectory handed to the vendored runtime as its root.
const OPENHUMAN_SUBDIR: &str = "openhuman";

/// The workspace subdirectory the vendored resolver appends to the root.
const WORKSPACE_SUBDIR: &str = "workspace";

/// The store subtree the journal and kv stores live in, under the workspace.
const STORE_SUBDIR: &str = "tinyagents_store";

/// Where a resolved journal root came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RootSource {
    /// An operator set `OPENHUMAN_WORKSPACE`; its value is used verbatim.
    Env,
    /// Derived from the instance data directory — the normal hosted path.
    DataDir,
}

impl RootSource {
    /// The knob an operator would change to move the root, for the startup line.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Env => OPENHUMAN_WORKSPACE_ENV,
            Self::DataDir => "OPENCOMPANY_DATA_DIR",
        }
    }
}

/// A resolved (but not yet proven-writable) journal location.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalRoot {
    root: PathBuf,
    source: RootSource,
}

impl JournalRoot {
    /// The directory handed to the vendored runtime as `OPENHUMAN_WORKSPACE`.
    /// Everything the journal writes lands beneath it, so this is the path
    /// whose writability decides whether observations persist.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Which knob put the root where it is.
    pub fn source(&self) -> RootSource {
        self.source
    }

    /// The store subtree the journal is expected to open,
    /// `<root>/workspace/tinyagents_store`.
    ///
    /// This mirrors the vendored resolver's ordinary result. That resolver has
    /// one legacy branch which, when a sibling `<root>/../.openhuman/config.toml`
    /// exists, treats `<root>` *itself* as the workspace and would place the
    /// store at `<root>/tinyagents_store` instead. It requires an
    /// operator-created directory that a provisioned tenant never has, and both
    /// outcomes lie inside [`root`](Self::root) — which is why the startup probe
    /// checks the root rather than this leaf.
    pub fn store_root(&self) -> PathBuf {
        self.root.join(WORKSPACE_SUBDIR).join(STORE_SUBDIR)
    }

    /// The one-line startup report: where observations are being written, and
    /// which knob decided that.
    pub fn summary(&self) -> String {
        format!(
            "agent journal: {} (root {}, from {})",
            self.store_root().display(),
            self.root.display(),
            self.source.as_str()
        )
    }
}

/// Resolves the journal root from an `OPENHUMAN_WORKSPACE` value and the
/// instance data directory, without touching the filesystem or the environment.
///
/// An operator-set root wins so a self-hoster keeps an existing OpenHuman
/// workspace. A missing — or blank — value falls back to the data directory,
/// the one location a tenant container is guaranteed to be able to write.
pub fn resolve(env_value: Option<&str>, data_dir: &Path) -> JournalRoot {
    match env_value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => JournalRoot {
            root: PathBuf::from(value),
            source: RootSource::Env,
        },
        None => JournalRoot {
            root: data_dir.join(OPENHUMAN_SUBDIR),
            source: RootSource::DataDir,
        },
    }
}

/// Resolves the journal root and proves it is writable, without reading or
/// writing the process environment.
///
/// The pure-input half of [`prepare`], so the resolution and the probe can be
/// exercised without the `set_var` races that make environment mutation
/// unsuitable for parallel tests.
pub async fn prepare_with(env_value: Option<&str>, data_dir: &Path) -> Result<JournalRoot> {
    let resolved = resolve(env_value, data_dir);
    ensure_writable(resolved.root(), resolved.source()).await?;
    Ok(resolved)
}

/// Resolves the journal root, proves it is writable, and — when the root was
/// derived rather than supplied — exports it as `OPENHUMAN_WORKSPACE` so the
/// vendored config loader finds it instead of a home-directory default.
///
/// Returns the resolved root for the caller to report. An unwritable root is an
/// error: see the module docs for why that aborts startup instead of degrading.
///
/// Call this once, early in `serve`, before any company runtime is built — the
/// vendored loader reads the variable when the first agent harness is
/// constructed, and the export must already have happened.
pub async fn prepare(data_dir: &Path) -> Result<JournalRoot> {
    let raw = std::env::var(OPENHUMAN_WORKSPACE_ENV).ok();
    let resolved = prepare_with(raw.as_deref(), data_dir).await?;

    if resolved.source() == RootSource::DataDir {
        // SAFETY: `serve` calls this once during startup, before any company
        // runtime, scheduler, mailbox poller or HTTP listener exists, so no
        // other thread is reading the environment concurrently. The tokio
        // worker and blocking threads alive at this point are idle and do not
        // call `getenv`. Setting this later — from a request handler, say —
        // would race those readers and must not be done.
        unsafe { std::env::set_var(OPENHUMAN_WORKSPACE_ENV, resolved.root()) };
    }

    Ok(resolved)
}

/// Creates `root` and confirms a file can actually be written inside it.
///
/// `create_dir_all` alone is not proof: it succeeds silently when the directory
/// already exists but is not writable by this user, which is exactly the shape
/// a stale volume or a wrong `fsGroup` produces. The probe file is named per
/// process so two instances sharing a root cannot collide, and is removed
/// again — a failure to remove it is not fatal, since the write already proved
/// the point.
async fn ensure_writable(root: &Path, source: RootSource) -> Result<()> {
    let fail = |stage: &str, error: std::io::Error| {
        OpenCompanyError::Config(format!(
            "agent journal root {} is not writable ({stage}: {error}); \
             refusing to start, because agent observations would be silently \
             discarded on every turn. Point {} at a writable volume — in a \
             tenant container that is the mounted data volume, not the \
             read-only root filesystem.",
            root.display(),
            source.as_str()
        ))
    };

    tokio::fs::create_dir_all(root)
        .await
        .map_err(|error| fail("create", error))?;

    let probe = root.join(format!(".opencompany-journal-probe-{}", std::process::id()));
    tokio::fs::write(&probe, b"")
        .await
        .map_err(|error| fail("write", error))?;
    tokio::fs::remove_file(&probe).await.ok();

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    fn scratch_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("oc-journal-{}-{tag}", std::process::id()))
    }

    #[test]
    fn absent_env_roots_the_journal_in_the_data_dir() {
        let resolved = resolve(None, Path::new("/data"));

        assert_eq!(resolved.root(), Path::new("/data/openhuman"));
        assert_eq!(resolved.source(), RootSource::DataDir);
        assert_eq!(
            resolved.store_root(),
            Path::new("/data/openhuman/workspace/tinyagents_store"),
            "the store must land inside the mounted data volume, not $HOME"
        );
    }

    #[test]
    fn an_operator_supplied_root_wins() {
        let resolved = resolve(Some("/srv/openhuman"), Path::new("/data"));

        assert_eq!(resolved.root(), Path::new("/srv/openhuman"));
        assert_eq!(resolved.source(), RootSource::Env);
        assert_eq!(
            resolved.store_root(),
            Path::new("/srv/openhuman/workspace/tinyagents_store")
        );
    }

    #[test]
    fn blank_env_values_fall_back_to_the_data_dir() {
        // The vendored resolver ignores an empty value but would honour a
        // whitespace-only one as a relative path; treating both as unset keeps
        // a shell that exports a declared-but-unset variable out of $HOME.
        for blank in ["", "   ", "\t"] {
            let resolved = resolve(Some(blank), Path::new("/data"));
            assert_eq!(
                resolved.root(),
                Path::new("/data/openhuman"),
                "{blank:?} should not be taken as a root"
            );
            assert_eq!(resolved.source(), RootSource::DataDir);
        }
    }

    #[test]
    fn the_summary_names_the_store_and_the_knob() {
        let summary = resolve(None, Path::new("/data")).summary();

        assert!(summary.contains("/data/openhuman/workspace/tinyagents_store"));
        assert!(summary.contains("OPENCOMPANY_DATA_DIR"));

        let env_summary = resolve(Some("/srv/oh"), Path::new("/data")).summary();
        assert!(env_summary.contains(OPENHUMAN_WORKSPACE_ENV));
    }

    #[tokio::test]
    async fn a_writable_data_dir_prepares_and_leaves_no_probe_behind() {
        let root = scratch_root("writable");
        tokio::fs::create_dir_all(&root).await.unwrap();

        let resolved = prepare_with(None, &root).await.unwrap();
        assert_eq!(resolved.root(), root.join("openhuman"));
        assert!(resolved.root().is_dir(), "the root is created, not assumed");

        let mut entries = tokio::fs::read_dir(resolved.root()).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            let name = entry.file_name();
            assert!(
                !name.to_string_lossy().contains("probe"),
                "the write probe must be cleaned up, found {name:?}"
            );
        }

        tokio::fs::remove_dir_all(&root).await.ok();
    }

    #[tokio::test]
    async fn preparing_twice_is_idempotent() {
        let root = scratch_root("idempotent");
        tokio::fs::create_dir_all(&root).await.unwrap();

        let first = prepare_with(None, &root).await.unwrap();
        let second = prepare_with(None, &root).await.unwrap();
        assert_eq!(first, second, "a restart must reuse the same root");

        tokio::fs::remove_dir_all(&root).await.ok();
    }

    /// The read-only-root condition from issue #446, reproduced with an
    /// unwritable parent: startup must fail with a message that names the path
    /// and the knob, rather than letting the runtime discover it per append.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unwritable_root_fails_loudly_at_startup() {
        use std::os::unix::fs::PermissionsExt;

        let root = scratch_root("readonly");
        tokio::fs::create_dir_all(&root).await.unwrap();
        let mut perms = tokio::fs::metadata(&root).await.unwrap().permissions();
        perms.set_mode(0o500);
        tokio::fs::set_permissions(&root, perms).await.unwrap();

        let outcome = prepare_with(None, &root).await;

        // A root user ignores the mode bits, so the probe would succeed and
        // there is nothing to assert about. Restore and skip rather than fail.
        let Err(error) = outcome else {
            restore_and_remove(&root).await;
            return;
        };
        let message = error.to_string();

        assert!(
            message.contains("openhuman"),
            "the failure must name the path: {message}"
        );
        assert!(
            message.contains("OPENCOMPANY_DATA_DIR"),
            "the failure must name the knob to change: {message}"
        );
        assert!(
            message.contains("refusing to start"),
            "the failure must say the instance will not run degraded: {message}"
        );

        restore_and_remove(&root).await;
    }

    /// A root that exists but cannot be written to is the shape a stale volume
    /// or a wrong `fsGroup` produces; `create_dir_all` alone reports success.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_existing_but_unwritable_root_is_caught_by_the_write_probe() {
        use std::os::unix::fs::PermissionsExt;

        let data_dir = scratch_root("existing-ro");
        let root = data_dir.join("openhuman");
        tokio::fs::create_dir_all(&root).await.unwrap();
        let mut perms = tokio::fs::metadata(&root).await.unwrap().permissions();
        perms.set_mode(0o500);
        tokio::fs::set_permissions(&root, perms).await.unwrap();

        let outcome = prepare_with(None, &data_dir).await;

        // As above: a root user is not bound by the mode bits.
        if let Err(error) = outcome {
            assert!(
                error.to_string().contains("write"),
                "create_dir_all succeeds on an existing dir; the probe must be \
                 what fails: {error}"
            );
        }

        restore_and_remove(&root).await;
        tokio::fs::remove_dir_all(&data_dir).await.ok();
    }

    /// The seam itself, against the real vendored resolver.
    ///
    /// Everything else here tests our side of the contract. This asserts the
    /// other side: that [`OPENHUMAN_WORKSPACE`](OPENHUMAN_WORKSPACE_ENV) is
    /// still the name the vendored config loader reads, and that a root of the
    /// shape [`resolve`] produces still lands its workspace where
    /// [`JournalRoot::store_root`] says it will. If upstream renames the
    /// variable or changes its resolution, this fails instead of the journal
    /// silently reverting to `$HOME` in production.
    ///
    /// Serialized and env-mutating, so it is `openhuman`-gated and deliberately
    /// the only test here that touches the process environment.
    #[cfg(feature = "openhuman")]
    #[tokio::test]
    async fn the_vendored_loader_still_honours_the_workspace_variable() {
        use openhuman_core::openhuman as oh;

        let data_dir = scratch_root("vendored-seam");
        tokio::fs::create_dir_all(&data_dir).await.unwrap();
        let resolved = resolve(None, &data_dir);

        let previous = std::env::var_os(OPENHUMAN_WORKSPACE_ENV);
        // SAFETY: the variable is read by the vendored loader below and by no
        // other test in this binary; the original value is restored before the
        // test returns.
        unsafe { std::env::set_var(OPENHUMAN_WORKSPACE_ENV, resolved.root()) };

        let config = oh::config::Config::load_or_init().await;

        // SAFETY: as above — restoring the environment we captured.
        unsafe {
            match previous {
                Some(value) => std::env::set_var(OPENHUMAN_WORKSPACE_ENV, value),
                None => std::env::remove_var(OPENHUMAN_WORKSPACE_ENV),
            }
        }

        let config = config.expect("the vendored config must load from the exported root");
        assert_eq!(
            config.workspace_dir,
            resolved.root().join(WORKSPACE_SUBDIR),
            "the vendored runtime must place its workspace under the root we \
             export, not under $HOME"
        );
        assert!(
            config.workspace_dir.starts_with(&data_dir),
            "the journal must resolve inside the data dir, got {}",
            config.workspace_dir.display()
        );

        tokio::fs::remove_dir_all(&data_dir).await.ok();
    }

    /// Puts an owner-writable mode back on `dir` so the test's own cleanup can
    /// remove it.
    #[cfg(unix)]
    async fn restore_and_remove(dir: &Path) {
        use std::os::unix::fs::PermissionsExt;

        if let Ok(metadata) = tokio::fs::metadata(dir).await {
            let mut perms = metadata.permissions();
            perms.set_mode(0o700);
            tokio::fs::set_permissions(dir, perms).await.ok();
        }
        tokio::fs::remove_dir_all(dir).await.ok();
    }
}
