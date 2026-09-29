use super::*;

/// Issue #632, end to end: a packaged install must be enterable with no
/// terminal, no mail server and no platform credential.
///
/// It used to be enterable by *signing in*: the shell asked for a magic
/// link at a synthetic loopback mailbox, read the code back out of the
/// response, and redeemed it for a cookie. That is gone. The desktop runs
/// [`AuthMode::None`], where the person at the machine is the principal by
/// configuration — so the console's very first request is already
/// authenticated and there is no screen in front of it.
///
/// The change is worth stating as more than a simplification: the shell had
/// no working session carrier for that cookie in the first place. The proxy
/// client holds no cookie store, `x-opencompany-session` is stripped as a
/// reserved header, and `needsCarriedSession()` is false on desktop — so the
/// session the magic link minted was discarded the moment it arrived, and
/// what actually let the console through was that every request came from
/// loopback anyway.
///
/// Over HTTP rather than against the registry, because the point is the
/// request the console makes: the company has to exist *and* be reachable
/// through the sole-company alias the console addresses before it knows any
/// id, and the principal has to resolve with nothing presented. Asserting a
/// company was registered would prove neither.
///
/// Seeded explicitly through [`start`], because a launched install no longer
/// seeds — it opens the wizard instead (`local::start_at`). What is under
/// test here is the host once it *has* a company, which is where a launched
/// install arrives the moment setup completes.
#[tokio::test]
async fn a_host_with_a_company_opens_with_no_sign_in() {
    let dir = tempfile::tempdir().unwrap();
    let host = start(dir.path().to_path_buf()).await.expect("host starts");
    let base = host.base_url();
    let http = reqwest::Client::new();

    assert_eq!(
        host.companies().len(),
        1,
        "the seeding entry point registers exactly one starter company"
    );

    // Where the console starts, and where it used to stop with a 401.
    let listed = http
        .get(format!("{base}/api/v1/companies"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        listed.status(),
        200,
        "an unauthenticated loopback request is the owner's, by configuration"
    );
    let companies: serde_json::Value = listed.json().await.unwrap();
    assert_eq!(
        companies.as_array().map(Vec::len),
        Some(1),
        "and it sees the company: {companies}"
    );

    // Attributed to a real stored record rather than a principal invented
    // per request — chat, task assignment and the audit trail all key off
    // `UserRecord::id`. Asked through the sole-company alias, which is what
    // the console addresses before it has discovered any id.
    let me: serde_json::Value = http
        .get(format!("{base}/api/v1/company/auth/me"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["email"], "local:owner", "{me}");
    assert_eq!(
        me["role"], "admin",
        "the write plane has to work, and there is nobody to outrank: {me}"
    );

    // And no second way in survives beside it. A host that still answered
    // `auth/request` would be one where the mode had not actually reached
    // the company — the seeded manifest names no mode of its own.
    let requested = http
        .post(format!("{base}/api/v1/company/auth/request"))
        .json(&serde_json::json!({ "email": "someone@example.com" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        requested.status(),
        409,
        "a magic link is refused by mode, not answered with a silent 202"
    );
}

/// A `none`-mode desktop is the **default**, not a ceiling.
///
/// The setup wizard offers all three modes on a loopback host, and an
/// operator who wants to share their instance with a colleague can pick
/// `email` — it writes `auth_mode` to the root's `config.toml` and applies
/// it live. A mode forced in the literal above would be a mode the file can
/// never win against: the choice would hold until quit and silently revert
/// on the next launch, which is precisely the "configuration ignored"
/// failure the setup surface exists to prevent.
///
/// So the file is read and `none` is what it falls back to.
#[tokio::test]
async fn a_configured_sign_in_survives_a_relaunch() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), "auth_mode = \"email\"\n").unwrap();

    let host = start(dir.path().to_path_buf()).await.expect("host starts");
    let requested = reqwest::Client::new()
        .post(format!("{}/api/v1/company/auth/request", host.base_url()))
        .json(&serde_json::json!({ "email": "ada@example.com" }))
        .send()
        .await
        .unwrap();

    assert_ne!(
        requested.status(),
        409,
        "409 is `auth_mode` refusing the route by mode — the file said email"
    );
}

/// The config layer reaches this host at all.
///
/// It did not. The `AppConfig` was built from a literal over
/// `AppConfig::default()`, so `api_url` was the compiled-in production
/// constant no matter what the root's `config.toml` said — a desktop
/// pointed at staging reported, and talked to, production. Asked through
/// `/spec` rather than the struct because `/spec` is what an operator
/// checks, and what reported the wrong answer.
#[tokio::test]
async fn the_root_config_names_the_hub_this_host_talks_to() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        "api_url = \"https://staging-api.tinyhumans.ai\"\n",
    )
    .unwrap();

    let host = start(dir.path().to_path_buf()).await.expect("host starts");
    let spec: serde_json::Value = reqwest::get(format!("{}/spec", host.base_url()))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(
        spec["api_url"], "https://staging-api.tinyhumans.ai",
        "the root's config.toml must name the hub: {spec}"
    );
}

/// The account surfaces stop disowning the host.
///
/// Unwired, `hub_identity()` is `None` and every surface that asks the hub
/// about this host's key answers that the host belongs to no TinyHumans
/// ecosystem — the Account page reports the balance unknown, and
/// `credential/link/start` refuses before it builds a URL. The exchange is
/// wired at boot now, and `/spec` advertises it.
#[tokio::test]
async fn the_host_knows_it_belongs_to_an_ecosystem() {
    let dir = tempfile::tempdir().unwrap();
    let host = start(dir.path().to_path_buf()).await.expect("host starts");

    let spec: serde_json::Value = reqwest::get(format!("{}/spec", host.base_url()))
        .await
        .expect("the route answers")
        .json()
        .await
        .expect("the spec is JSON");

    assert!(
        spec["capabilities"]
            .as_array()
            .is_some_and(|caps| caps.iter().any(|c| c == "hub-identity")),
        "the host must not disown its own account: {spec}"
    );
}

/// A key grant is sent back to the port this host actually bound.
#[tokio::test]
async fn a_key_grant_returns_to_the_port_this_host_bound() {
    let dir = tempfile::tempdir().unwrap();
    let host = start(dir.path().to_path_buf()).await.expect("host starts");
    let company = host.companies().first().expect("a seeded company").clone();

    let started: serde_json::Value = reqwest::Client::new()
        .post(format!(
            "{}/api/v1/companies/{company}/credential/link/start",
            host.base_url()
        ))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("the route answers")
        .json()
        .await
        .expect("a JSON body");
    let url = started["authorizeUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("an authorize URL: {started}"));

    assert!(
        !url.contains("127.0.0.1%3A0") && !url.contains("127.0.0.1:0"),
        "the return leg must not name port 0: {url}"
    );
    let expected = format!(
        "127.0.0.1%3A{}%2Fauth%2Fkey%2Fcallback",
        host.address().port()
    );
    assert!(
        url.contains(&expected),
        "the return leg must be this host's own callback on its bound port: {url}"
    );
}

/// The starter company is seeded once, not per launch.
#[tokio::test]
async fn a_relaunch_reuses_the_company_the_root_already_holds() {
    let dir = tempfile::tempdir().unwrap();
    let first = start(dir.path().to_path_buf()).await.unwrap();
    let seeded = first.companies().to_vec();
    drop(first);

    // `take_root` retries because the data root is released asynchronously;
    // see the note in `stopping_a_host_frees_its_root_and_its_port`.
    let relaunched = take_root(dir.path().to_path_buf()).await;
    assert_eq!(relaunched.companies(), seeded.as_slice());
}

#[tokio::test]
async fn an_embedded_host_answers_on_loopback() {
    let dir = tempfile::tempdir().unwrap();
    let host = start(dir.path().to_path_buf()).await.expect("host starts");

    assert!(host.address().ip().is_loopback(), "must never be routable");
    assert_ne!(
        host.address().port(),
        0,
        "the OS-chosen port must be reported"
    );

    let health = reqwest::get(format!("{}/healthz", host.base_url()))
        .await
        .expect("the reported address is reachable");
    assert!(health.status().is_success());
}

#[tokio::test]
async fn the_embedded_host_holds_its_data_root() {
    // The desktop being launched twice is ordinary rather than exceptional,
    // and two hosts over one root overwrite each other's companies.
    let dir = tempfile::tempdir().unwrap();
    let _first = start(dir.path().to_path_buf())
        .await
        .expect("the first starts");

    let second = start(dir.path().to_path_buf()).await;
    assert!(
        second.is_err(),
        "a second host over one root must be refused"
    );
}

#[tokio::test]
async fn two_embedded_hosts_over_different_roots_coexist() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let first = start(a.path().to_path_buf()).await.unwrap();
    let second = start(b.path().to_path_buf()).await.unwrap();
    assert_ne!(first.address().port(), second.address().port());
}

#[tokio::test]
async fn stopping_a_host_frees_its_root_and_its_port() {
    let dir = tempfile::tempdir().unwrap();
    let host = start(dir.path().to_path_buf()).await.unwrap();
    drop(host);

    // Both resources come back: a desktop restarted after a clean quit must
    // start, and it must not leak a listener per restart.
    //
    // Retried briefly, and the reason is worth recording rather than
    // hiding behind a sleep. `flock` belongs to the *open file
    // description*, and between `fork()` and `exec()` a concurrently
    // spawned child shares every descriptor its parent had. So if anything
    // else in this process spawns a subprocess in the same instant this
    // host releases its root — and the suite does, constantly: `git` in the
    // worktree tests, `python3` in the ACP ones — the lock survives until
    // that child reaches `exec` and `O_CLOEXEC` closes it. Microseconds,
    // and reproducible here about one run in five.
    //
    // Not worth engineering away: the production shape is a person quitting
    // and relaunching seconds later, and a harness the desktop spawned
    // always execs. Asserting instantaneous release would be asserting
    // something stricter than the product needs, so this asserts what it
    // does need — that the root comes back promptly.
    let mut last = None;
    for _ in 0..50 {
        match start(dir.path().to_path_buf()).await {
            Ok(host) => {
                assert_ne!(host.address().port(), 0);
                return;
            }
            Err(error) => last = Some(error),
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("a released root must become takeable: {last:?}");
}

/// The property the console keys its connection list on (#615).
///
/// The port deliberately changes on every launch, so a client that
/// recognises this host by address recognises a *new* host every run and
/// the dead ones pile up in its sidebar. The identity is what survives
/// exactly that restart, and this is the assertion the fix rests on.
///
/// The complementary half — that the port does *not* survive — is left
/// unasserted on purpose: the OS is free to hand the same ephemeral port
/// back, so `assert_ne!` on it would be asserting a coincidence. Two
/// concurrent hosts differing is covered by
/// `two_embedded_hosts_over_different_roots_coexist`.
#[tokio::test]
async fn a_restarted_host_keeps_its_identity() {
    let dir = tempfile::tempdir().unwrap();
    let first = start(dir.path().to_path_buf()).await.unwrap();
    let id = first.instance_id().to_string();
    assert!(!id.is_empty(), "a host must report an identity");
    drop(first);

    let second = take_root(dir.path().to_path_buf()).await;
    assert_eq!(second.instance_id(), id, "the same root is the same host");
}

/// Starts over `root`, retrying while a just-released `flock` clears.
///
/// See `stopping_a_host_frees_its_root_and_its_port` for why the release is
/// not instantaneous.
async fn take_root(root: PathBuf) -> EmbeddedHost {
    let mut last = None;
    for _ in 0..50 {
        match start(root.clone()).await {
            Ok(host) => return host,
            Err(error) => last = Some(error),
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("a released root must become takeable: {last:?}");
}
