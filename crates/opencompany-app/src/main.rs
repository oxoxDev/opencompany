// Prevents a console window opening alongside the app on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    // Hidden release check: `opencompany-desktop sentry-test [--message …]`
    // sends one event and exits, without opening a window. See `crash`.
    //
    // Read through `args_os` and convert lossily, not `args`: the latter
    // panics on the first non-Unicode argument, which would turn an unusual
    // launch (a wrapper script, an odd `-psn_…` variant) into a crash before
    // the app ever opens a window. A lossy argument can only fail to match
    // `sentry-test` exactly — the one behavior this parser depends on — so
    // normal launches are unaffected and abnormal ones fall through to the
    // real app instead of aborting.
    let args = std::env::args_os().map(|arg| arg.to_string_lossy().into_owned());
    if let Some(message) = opencompany_desktop_lib::crash::sentry_test_args(args) {
        return opencompany_desktop_lib::crash::run_sentry_test(message);
    }

    // `OPENHUMAN_WORKSPACE` must be exported HERE, before anything else starts.
    //
    // The library path deliberately does not do it: `journal::prepare`'s
    // `set_var` is only sound before any other thread exists, and by the time
    // Tauri's runtime, webview process and plugin threads are up, `setenv`
    // racing a concurrent `getenv` is undefined behaviour on glibc rather than
    // a stale read. `main` is the one moment this process is single-threaded.
    //
    // Resolved the same way the embedded host will resolve it, so the value
    // exported here and the root it later probes are the same directory.
    let data_dir = opencompany_desktop_lib::default_data_dir();
    if std::env::var_os("OPENHUMAN_WORKSPACE").is_none() {
        // SAFETY: first statement of `main`, before any thread is spawned and
        // before Tauri or tokio exist. Nothing else can be reading the
        // environment yet.
        unsafe { std::env::set_var("OPENHUMAN_WORKSPACE", data_dir.join("openhuman")) };
    }

    opencompany_desktop_lib::run();
    std::process::ExitCode::SUCCESS
}
