//! Where the desktop shell's crash reports go.
//!
//! A runtime `OPENCOMPANY_SENTRY_DSN` still wins: an operator who sets it gets
//! exactly that destination. What a double-clicked `.app` does not have is any
//! environment at all, so the desktop carries one more source, below the
//! process env and never above it: [`DESKTOP_DSN`], a compiled-in constant.
//!
//! That value names the **desktop's own** Sentry project (`opencompany-tauri`),
//! not the server's. A key inside a downloadable bundle is readable by anyone
//! who unzips it, so the only thing it can be trusted to write to is a project
//! that exists for exactly this binary.
//!
//! The constant only stands in for an absent `OPENCOMPANY_SENTRY_DSN`; `OPENCOMPANY_SENTRY=off` is resolved before any DSN
//! is looked at (`observability::config::resolve`) and silences this one too.
//! See `docs/spec/runtime/crash-reporting.md`.

use std::ffi::OsString;

use opencompany::app::config::EnvSource;
use opencompany::observability::config::DSN_ENV;

/// The TinyHumans desktop (`opencompany-tauri`) project's DSN.
pub const DESKTOP_DSN: &str = "https://b76bb868394f25c4e930a2c20bfcbc72@sentry.tinyhumans.ai/14";

/// The desktop project's DSN: always [`DESKTOP_DSN`].
pub fn baked_dsn() -> Option<&'static str> {
    Some(DESKTOP_DSN)
}

/// An [`EnvSource`] that answers `OPENCOMPANY_SENTRY_DSN` from the baked DSN
/// when the wrapped environment has no usable value, and passes every other key
/// (including `OPENCOMPANY_SENTRY`) straight through.
pub struct DesktopEnv<E> {
    inner: E,
    baked: Option<&'static str>,
}

impl<E: EnvSource> DesktopEnv<E> {
    /// Wraps `inner` with this build's [`baked_dsn`].
    pub fn new(inner: E) -> Self {
        Self::with_baked(inner, baked_dsn())
    }

    /// Wraps `inner` with an explicit fallback, so both branches are testable
    /// from a build that has none baked in.
    pub fn with_baked(inner: E, baked: Option<&'static str>) -> Self {
        Self { inner, baked }
    }
}

impl<E: EnvSource> EnvSource for DesktopEnv<E> {
    fn get_os(&self, key: &str) -> Option<OsString> {
        let value = self.inner.get_os(key);
        if key != DSN_ENV {
            return value;
        }
        // An unset or blank runtime DSN falls back; anything else — including
        // bytes that are not a DSN — is the operator's and is left for
        // `resolve` to judge, so a typo is reported rather than papered over.
        let blank = value
            .as_ref()
            .is_none_or(|raw| raw.to_str().is_some_and(|s| s.trim().is_empty()));
        if blank && let Some(baked) = self.baked {
            return Some(OsString::from(baked));
        }
        value
    }
}

/// `opencompany-desktop sentry-test [--message <text>]`: send one deliberate
/// event through the same client, decision and scrubber a launched app uses,
/// print its id on stdout, and exit non-zero when nothing could be sent or the
/// queue did not drain. The desktop's counterpart to `opencompany sentry-test`,
/// kept out of the UI on purpose — it is a release check, not a feature.
pub fn run_sentry_test(message: Option<String>) -> std::process::ExitCode {
    let env = DesktopEnv::new(opencompany::app::config::ProcessEnv);
    let (decision, guard) =
        opencompany::observability::init(opencompany::app::deployment::Deployment::Desktop, &env);
    eprintln!("{}", decision.describe());
    let message = message.unwrap_or_else(|| "opencompany-desktop sentry-test ping".to_string());
    let Some(event_id) = opencompany::observability::capture_test_event(&message) else {
        eprintln!("crash reporting is not active in this process, so there is nothing to test");
        return std::process::ExitCode::FAILURE;
    };
    let drained = guard.flush(std::time::Duration::from_secs(15));
    println!("{event_id}");
    if !drained {
        eprintln!("the crash-reporting queue did not drain within 15s; delivery is unconfirmed");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

/// Recognises the hidden `sentry-test` argument, returning its `--message`.
///
/// Exact matches only: macOS hands a Finder-launched app arguments of its own
/// (`-psn_…`), and none of them may be mistaken for this.
pub fn sentry_test_args<I: IntoIterator<Item = String>>(args: I) -> Option<Option<String>> {
    let mut args = args.into_iter().skip(1);
    if args.next().as_deref() != Some("sentry-test") {
        return None;
    }
    let mut message = None;
    while let Some(arg) = args.next() {
        if arg == "--message" {
            message = args.next();
        } else if let Some(value) = arg.strip_prefix("--message=") {
            message = Some(value.to_string());
        }
    }
    Some(message)
}

#[cfg(test)]
#[path = "crash_tests.rs"]
mod tests;
