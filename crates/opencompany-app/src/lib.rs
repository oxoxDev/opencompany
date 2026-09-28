//! The OpenCompany desktop shell.
//!
//! Three responsibilities, and nothing else:
//!
//! - **[`proxy`]** — every host's HTTP and event traffic, in Rust so that CORS
//!   does not apply and the credential never enters the webview.
//! - **[`embedded`]** — a host running in this process, for someone with no
//!   server to point at.
//! - **[`local`]** — the roster of those hosts, so one machine can run several
//!   companies side by side rather than exactly one.
//! - **[`ssh`]** — tunnels to hosts on *other* machines that are bound to
//!   loopback there, which is the one connector a browser cannot have.
//! - **[`keychain`]** — where a paired device's token lives, so the webview
//!   holds a handle and never the secret.
//! - **[`update`]** — replacing this application with a newer one, which is the
//!   one thing a desktop build cannot get from the host it is talking to.
//! - **[`commands`]** — the thin Tauri surface over all three.
//! - **[`bundle_migration`]** — carrying state over from the pre-rename
//!   bundle identifier, once.
//! - **[`crash`]** — where the shell's crash reports go, including the
//!   desktop project's compiled-in DSN and the hidden `sentry-test` check.
//!
//! The console itself is unchanged: it is the same `frontend/` bundle the web
//! deployment serves, and it reaches all of the above through the `Transport`
//! seam it already had.

pub mod acp;
/// State the OS filed under the pre-rename bundle identifier, carried over once
/// before the webview starts. See the module docs.
pub mod bundle_migration;
pub mod commands;
/// Where the shell's crash reports go: the operator's DSN, else the desktop
/// project's compiled-in one. See the module docs.
pub mod crash;
pub mod embedded;
/// Who is sitting at this machine, as the OS already knows — read once, to
/// prefill a profile nobody has filled in yet. See the module docs for why it is
/// a suggestion and never an import.
pub mod identity;
pub mod keychain;
pub mod local;
pub mod proxy;
pub mod ssh;
pub mod update;

use std::path::PathBuf;

use crate::local::LocalHosts;
use crate::ssh::SshTunnels;

/// Process-wide state the commands read.
pub struct AppHandleState {
    pub data_dir: PathBuf,
    /// Every host this machine runs, and which of them are listening.
    ///
    /// A roster rather than an `Option<EmbeddedHost>`: an operator running two
    /// companies on one laptop is the ordinary case this shell is for, and a
    /// single-valued field is exactly what makes the second one impossible.
    /// Behind a mutex because starting and stopping are operator actions
    /// arriving on command threads, not just a boot-time read.
    ///
    /// An instance that could not start is a row carrying its reason — most
    /// often another process holding its data root — not a reason to refuse to
    /// launch. The desktop also talks to *remote* hosts, and a busy root must
    /// not stop it doing that.
    pub local: tokio::sync::Mutex<LocalHosts>,
    /// Every SSH tunnel this application is holding open.
    ///
    /// Beside the local roster rather than inside it: both are processes this
    /// shell starts and must be able to stop, and neither is a host it can
    /// merely address. What they are not is the same thing — a tunnel's host
    /// belongs to somebody else's machine — so pruning one against the other
    /// would delete it.
    pub ssh: tokio::sync::Mutex<SshTunnels>,
}

/// The canonical directory this instance keeps its data in.
///
/// This must remain identical to the host binary's resolution: an explicit
/// `OPENCOMPANY_DATA_DIR`, otherwise `$HOME/.opencompany` (or `%USERPROFILE%`
/// on Windows), with a relative `.opencompany` only when no home is available.
pub fn default_data_dir() -> PathBuf {
    opencompany::app::config::data_dir_from_env()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    // The shell and every embedded host share the core's single process-wide
    // client, scrubber, panic hook, release format, and tracing bridge. The
    // DSN is the operator's `OPENCOMPANY_SENTRY_DSN` when set, else the
    // desktop project's compiled-in one (`crash::DesktopEnv`);
    // `OPENCOMPANY_SENTRY=off` silences both.
    let (crash_reporting, crash_guard) = opencompany::observability::init(
        opencompany::app::deployment::Deployment::Desktop,
        &crash::DesktopEnv::new(opencompany::app::config::ProcessEnv),
    );
    // The `tinyagents::observability` directive is the vendored durable-append
    // writer's reporting target, and it has to be named explicitly here for a
    // reason the host binary's filter does not share: this fallback carries no
    // global directive at all, only per-target ones, so an unnamed target is
    // dropped at *every* level — `error` included. Without this the writer's
    // "still failing" reminders, its "recovered, N observation(s) lost" summary
    // and its "never recovered before shutdown" summary are silent, and so is
    // the first-failure `error` line. See `DEFAULT_LOG_FILTER` in
    // `src/bin/opencompany.rs` for the full argument (issue #450). Latent while
    // this crate does not enable the `openhuman` feature; a landmine for
    // whoever does.
    let log_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        "opencompany_desktop_lib=info,opencompany=info,tinyagents::observability=warn".into()
    });
    tracing_subscriber::registry()
        .with(log_filter)
        .with(tracing_subscriber::fmt::layer())
        .with(opencompany::observability::tracing_layer())
        .init();
    tracing::info!("{}", crash_reporting.describe());

    // Before anything creates the webview: its data store (the console's saved
    // connections live in its `localStorage`) is filed under the bundle
    // identifier, and a fresh one would be created empty under the new id.
    bundle_migration::run();

    let data_dir = default_data_dir();

    // Tauri's own runtime, entered before the webview, because the local hosts
    // have to be listening before the console asks for their addresses.
    //
    // Deliberately *not* a `tokio::runtime::Runtime` built here. The hosts
    // started at boot must live on the same runtime as the ones an operator
    // starts later from a command — and commands run on Tauri's. Two runtimes
    // would mean a `start` awaited from a command while the boot-time hosts'
    // server tasks belong to a runtime nothing else holds a handle to.
    let local = tauri::async_runtime::block_on(LocalHosts::load(data_dir.clone()));

    let result = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        // Replacing this application with a newer one. The endpoint and the
        // minisign public key the downloaded bundle is verified against live in
        // `tauri.conf.json` under `plugins.updater`; the private half is a
        // release secret and is not in this repository. A build compiled
        // against the placeholder key is inert rather than broken — see
        // `update::is_configured` and `docs/spec/runtime/desktop-updates.md`.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(proxy::SharedProxy::default())
        .manage(update::PendingUpdate::default())
        .manage(AppHandleState {
            data_dir,
            local: tokio::sync::Mutex::new(local),
            ssh: tokio::sync::Mutex::new(SshTunnels::default()),
        })
        .invoke_handler(tauri::generate_handler![
            commands::oc_connect,
            commands::oc_pair_device,
            commands::oc_adopt_session,
            commands::oc_forget_device,
            commands::oc_disconnect,
            commands::oc_connections,
            commands::oc_request,
            commands::oc_subscribe,
            commands::oc_embedded,
            commands::oc_device_identity,
            commands::oc_local_instances,
            commands::oc_create_local_instance,
            commands::oc_start_local_instance,
            commands::oc_stop_local_instance,
            commands::oc_rename_local_instance,
            commands::oc_forget_local_instance,
            commands::oc_delete_local_instance,
            commands::oc_acp_harnesses,
            commands::oc_acp_confirm_harness,
            commands::oc_acp_install_harness,
            commands::oc_open_ssh_tunnel,
            commands::oc_close_ssh_tunnel,
            commands::oc_ssh_tunnels,
            commands::oc_app_update_check,
            commands::oc_app_update_download,
            commands::oc_app_update_install,
        ])
        .run(tauri::generate_context!());

    if !crash_guard.flush(opencompany::observability::FLUSH_TIMEOUT) {
        tracing::debug!("crash reporting: flush did not finish inside the shutdown budget");
    }
    result.expect("run the desktop shell");
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
