use super::*;

fn scratch_root(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("oc-journal-{}-{tag}", std::process::id()))
}

/// The child test the seam test re-invokes this binary to run.
#[cfg(feature = "openhuman")]
const SEAM_CHILD_TEST: &str = "app::journal::tests::vendored_seam_child";

/// Which half of the seam the child should assert.
#[cfg(feature = "openhuman")]
const SEAM_ROLE_ENV: &str = "OC_JOURNAL_SEAM_ROLE";

/// The data dir the child compares against.
#[cfg(feature = "openhuman")]
const SEAM_DATA_DIR_ENV: &str = "OC_JOURNAL_SEAM_DATA_DIR";

/// The child test the keyring-pin test re-invokes this binary to run.
#[cfg(feature = "openhuman")]
const KEYRING_CHILD_TEST: &str = "app::journal::tests::keyring_pin_child";

/// The data dir the keyring-pin child pins against.
#[cfg(feature = "openhuman")]
const KEYRING_DATA_DIR_ENV: &str = "OC_KEYRING_PIN_DATA_DIR";

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
fn a_padded_root_is_used_exactly_as_configured() {
    // Spaces are legal in a POSIX path, so a value that is not blank must
    // survive verbatim — trimming it would resolve somewhere the operator
    // never named.
    for padded in [" /srv/oh ", "/srv/oh ", " /srv/oh", "/srv/my oh"] {
        let resolved = resolve(Some(padded), Path::new("/data"));
        assert_eq!(
            resolved.root(),
            Path::new(padded),
            "{padded:?} must not be rewritten"
        );
        assert_eq!(resolved.source(), RootSource::Env);
    }
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

    // Built by joining rather than written out, because the summary renders the
    // platform's own separator: on Windows it says `\data\openhuman\...` and the
    // forward-slash literal this replaces never matched. What the summary owes
    // an operator is the right *path*, not one spelled the POSIX way.
    let store = Path::new("/data")
        .join("openhuman")
        .join("workspace")
        .join("tinyagents_store");
    assert!(summary.contains(&store.display().to_string()), "{summary}");
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
/// Run in a **child process** rather than by mutating this process's
/// environment.
///
/// `set_var` is process-global, and this library's test binary contains
/// unguarded environment readers and writers in several other modules
/// (`store::select`, `server::ops::connections`, `harness::composio`,
/// `server::ops::mailer`). A lock would only protect this test if every one
/// of those took the same lock, and nothing stops the next test from adding
/// an unguarded `set_var`. Handing the variable to a child at spawn time
/// removes the shared mutable state instead of scheduling around it, and
/// exercises the more faithful thing: a process that *starts* with the
/// environment a tenant container is given.
#[cfg(feature = "openhuman")]
#[test]
fn the_vendored_loader_still_honours_the_workspace_variable() {
    let data_dir = scratch_root("vendored-seam");
    // Deliberately outside the data dir, so "did the workspace land under
    // the data dir?" cannot be satisfied by the home directory instead.
    let home = scratch_root("vendored-seam-home");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    let expected = resolve(None, &data_dir);
    let run = |role: &str, export_seam: bool| {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([SEAM_CHILD_TEST, "--exact", "--ignored", "--nocapture"])
            .env(SEAM_ROLE_ENV, role)
            .env(SEAM_DATA_DIR_ENV, &data_dir)
            .env("HOME", &home);
        if export_seam {
            command.env(OPENHUMAN_WORKSPACE_ENV, expected.root());
        } else {
            command.env_remove(OPENHUMAN_WORKSPACE_ENV);
        }
        command.output().expect("spawn the seam child process")
    };

    // A filter that stops matching (a rename, a moved module) makes libtest
    // run nothing and still exit 0, so success alone would be a vacuous
    // pass. Require that exactly one test actually ran.
    let check = |what: &str, out: &std::process::Output| {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let detail = format!(
            "{what}\n--- child stdout ---\n{stdout}\n--- child stderr ---\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{detail}");
        assert!(
            stdout.contains("1 passed"),
            "the child must have actually run the assertion, not filtered \
             it away — is {SEAM_CHILD_TEST} still the right path?\n{detail}"
        );
    };

    // Exporting the variable puts the vendored workspace under the data dir.
    check("the exported root must be honoured", &run("inside", true));

    // Negative control: without it, the vendored runtime goes to $HOME. If
    // this ever passes, the check above proves nothing.
    check(
        "without the variable the runtime must fall back to $HOME",
        &run("outside", false),
    );

    std::fs::remove_dir_all(&data_dir).ok();
    std::fs::remove_dir_all(&home).ok();
}

/// The assertion half of the seam test, executed in the child process the
/// test above spawns. Ignored so a normal run never picks it up, and inert
/// unless the parent set the role, so a bare `cargo test -- --ignored`
/// cannot fail on a missing environment.
#[cfg(feature = "openhuman")]
#[tokio::test]
#[ignore = "spawned by the_vendored_loader_still_honours_the_workspace_variable"]
async fn vendored_seam_child() {
    let Ok(role) = std::env::var(SEAM_ROLE_ENV) else {
        return;
    };
    let data_dir = PathBuf::from(std::env::var(SEAM_DATA_DIR_ENV).expect("parent sets this"));
    let expected = resolve(None, &data_dir);

    let config = openhuman_core::config::Config::load_or_init()
        .await
        .expect("the vendored config must load");

    match role.as_str() {
        "inside" => {
            assert_eq!(
                config.workspace_dir,
                expected.root().join(WORKSPACE_SUBDIR),
                "the vendored runtime must place its workspace under the \
                 root we export"
            );
            assert!(
                config.workspace_dir.starts_with(&data_dir),
                "the journal must resolve inside the data dir, got {}",
                config.workspace_dir.display()
            );
        }
        "outside" => assert!(
            !config.workspace_dir.starts_with(&data_dir),
            "without {OPENHUMAN_WORKSPACE_ENV} the runtime must not reach \
             the data dir on its own — the export is what does the work, \
             got {}",
            config.workspace_dir.display()
        ),
        other => panic!("unknown seam role {other:?}"),
    }
}

/// The temp-dir guard compares whole components, so a sibling directory
/// whose name merely starts with the temp dir's is not "inside" it.
#[cfg(feature = "openhuman")]
#[test]
fn the_temp_dir_guard_compares_components_not_prefixes() {
    let temp = Path::new("/tmp");

    assert!(under_temp_dir(Path::new("/tmp/oc/openhuman"), temp));
    assert!(under_temp_dir(temp, temp), "the temp dir itself counts");
    assert!(
        !under_temp_dir(Path::new("/tmpfoo/openhuman"), temp),
        "a name that merely begins with the temp dir's is a different directory"
    );
    assert!(!under_temp_dir(Path::new("/data/openhuman"), temp));
    assert!(!under_temp_dir(Path::new("/var/lib/oc"), temp));
}

/// With `OPENHUMAN_WORKSPACE` unset, the vendored keyring must **still**
/// resolve to the data-dir root — i.e. `$HOME` and its `/tmp` fallback are
/// unreachable because the directory was registered, not inferred (#451).
///
/// The negative half is what makes this worth running: the child's `HOME`
/// points somewhere real and writable, so if the pin did nothing the
/// resolver would happily answer with `$HOME/.openhuman` and the assertion
/// would fail rather than pass vacuously.
///
/// Run in a **child process** for the same reason the vendored-seam test
/// above is: the registration is a process-global `OnceLock` set-once, so it
/// cannot be undone between tests, and `HOME` cannot be varied in-process
/// without racing every other environment reader in this binary.
#[cfg(feature = "openhuman")]
#[test]
fn pinning_keeps_the_keyring_out_of_home_without_the_env_var() {
    let data_dir = scratch_root("keyring-pin");
    // Deliberately a sibling of the data dir, not a child, so "did the
    // keyring land under the data dir?" cannot be satisfied by $HOME.
    let home = scratch_root("keyring-pin-home");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([KEYRING_CHILD_TEST, "--exact", "--ignored", "--nocapture"])
        .env(KEYRING_DATA_DIR_ENV, &data_dir)
        .env("HOME", &home)
        // The whole point: the pin must work with the seam variable absent.
        .env_remove(OPENHUMAN_WORKSPACE_ENV)
        .output()
        .expect("spawn the keyring-pin child process");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let detail = format!(
        "--- child stdout ---\n{stdout}\n--- child stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{detail}");
    // A filter that stops matching makes libtest run nothing and still exit
    // 0, so success alone would be a vacuous pass.
    assert!(
        stdout.contains("1 passed"),
        "the child must have actually run the assertion, not filtered it \
         away — is {KEYRING_CHILD_TEST} still the right path?\n{detail}"
    );

    std::fs::remove_dir_all(&data_dir).ok();
    std::fs::remove_dir_all(&home).ok();
}

/// The assertion half of the keyring-pin test, executed in the child process
/// the test above spawns. Ignored so a normal run never picks it up, and
/// inert unless the parent set the data dir, so a bare
/// `cargo test -- --ignored` cannot fail on a missing environment.
#[cfg(feature = "openhuman")]
#[test]
#[ignore = "spawned by pinning_keeps_the_keyring_out_of_home_without_the_env_var"]
fn keyring_pin_child() {
    let Ok(data_dir) = std::env::var(KEYRING_DATA_DIR_ENV) else {
        return;
    };
    let data_dir = PathBuf::from(data_dir);
    assert!(
        std::env::var(OPENHUMAN_WORKSPACE_ENV).is_err(),
        "the parent must unset the seam variable — the registration, not the \
         export, is what this test proves"
    );

    let expected = resolve(None, &data_dir);
    let pin = pin_keyring(&expected);
    assert_eq!(pin.dir(), expected.root());

    let resolved = openhuman_core::security::keyring::store::workspace_dir_for_file_backend();
    assert_eq!(
        resolved,
        expected.root(),
        "the vendored keyring must answer with the registered root"
    );
    assert!(
        resolved.starts_with(&data_dir),
        "secret material must resolve inside the data dir, got {}",
        resolved.display()
    );

    // The home fallback is real and writable here, so this is the branch the
    // pin actually displaced — and `/tmp` sits one step further down the
    // same chain.
    let home = PathBuf::from(std::env::var("HOME").expect("the parent sets HOME"));
    assert!(
        !resolved.starts_with(&home),
        "the keyring must not fall back to $HOME once a root is registered"
    );

    // The data dir is scratch space under the system temp dir, so the
    // loudness guard fires — and it warns rather than refusing, which is why
    // `pin_keyring` returned at all.
    assert!(
        pin.is_temporary(),
        "a root under the system temp dir must be flagged"
    );
    assert!(
        pin.summary().contains("WARNING"),
        "the startup line must carry the caveat: {}",
        pin.summary()
    );
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
