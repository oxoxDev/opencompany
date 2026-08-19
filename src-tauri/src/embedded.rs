//! The host, running inside the desktop process.
//!
//! ## Why it still binds a real socket
//!
//! The obvious optimisation is to skip the network: hold the axum `Router` and
//! drive it in-process through `tower::Service`. It is rejected on purpose.
//!
//! With a real listener, the embedded path exercises the identical
//! serialisation, auth extractors (`ScopedCompany`, the session carrier), CORS
//! branch, error envelopes and event framing as a remote host. Every Playwright
//! spec, every future ACP conformance test, and the proxy's own tests are then
//! valid evidence about embedded mode too. Skipping the socket saves perhaps
//! 50µs per request and buys a *second code path* that will diverge — and
//! divergence in an auth extractor is precisely the class of bug that cannot be
//! afforded.
//!
//! ## Loopback and an ephemeral port
//!
//! `127.0.0.1:0`, never `0.0.0.0`: an embedded instance is this machine's, and
//! binding a routable address would quietly publish someone's company to their
//! network. Port `0` because a fixed 8080 collides with a dev server or a second
//! app — a support case that reads "it works unless I have a terminal open".
//! The OS picks; [`EmbeddedHost::address`] reports what it picked.

use std::path::PathBuf;

use opencompany::app::EmbeddedInstance;
use opencompany::{AppConfig, AppState};

/// A running in-process host.
pub struct EmbeddedHost {
    address: std::net::SocketAddr,
    instance_id: String,
    companies: Vec<String>,
    /// Holds the data root's exclusive lock for as long as the host runs.
    /// Dropping it would release the root while this process kept writing.
    _instance: EmbeddedInstance,
    server: tokio::task::JoinHandle<()>,
}

impl EmbeddedHost {
    /// The loopback address the console should point at.
    pub fn address(&self) -> std::net::SocketAddr {
        self.address
    }

    /// The base URL for a connection record.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// This host's stable identity, from `instance-id` under the data root.
    ///
    /// The console needs this precisely *because* [`Self::base_url`] is not
    /// stable: the port is ephemeral by design (see above), so a client that
    /// recognises this host by its address recognises a new host on every
    /// launch and accumulates a dead connection per run. The address says where
    /// to knock; this says who answers.
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// The address this host will sign a person in as.
    ///
    /// The console has to be told, because nobody could guess it and the login
    /// form is otherwise a blank field on a host that admits exactly one
    /// address. Not a secret: it grants nothing without a code minted by this
    /// host and returned over loopback.
    pub fn operator_email(&self) -> &'static str {
        opencompany::desktop::DESKTOP_OPERATOR_EMAIL
    }

    /// The companies registered at boot, in listing order.
    pub fn companies(&self) -> &[String] {
        &self.companies
    }
}

impl Drop for EmbeddedHost {
    fn drop(&mut self) {
        // The task owns the listener; aborting it closes the port. Without this
        // a restarted embedded host would leak a listener per restart.
        self.server.abort();
    }
}

/// What a host does about a data root that holds no company yet.
///
/// The two answers to the same question, and only one may be given per host.
/// A host that seeds is a host that is *already set up* — `AppSpec` reports
/// `setup_complete` as `stamp || !registry.is_empty()` — so seeding does not
/// merely add a company, it suppresses the first-run wizard the console would
/// otherwise open (`views/setup/SetupWizard.tsx`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstRun {
    /// Register a starter company from the default preset when the root is
    /// empty, so the application is enterable with no terminal and no
    /// decisions ([issue #632](https://github.com/tinyhumansai/opencompany/issues/632)).
    ///
    /// What the instance rooted at the data dir does, which is every install
    /// that predates a roster.
    SeedStarterCompany,
    /// Register only what the root already holds, leaving an empty root empty.
    ///
    /// What an instance an operator *asked for* does. They are standing in
    /// front of the application having just named a company, so the decisions
    /// the wizard asks for are ones they are already making — and a seeded
    /// starter company would answer them silently and hide the wizard for good.
    RunSetupWizard,
}

/// Boots a host over `data_dir`, seeding a starter company on an empty root.
///
/// `data_dir` is passed explicitly rather than resolved from the environment.
/// A desktop app knows its platform data directory and should say so — and the
/// crate's own fallback resolves a *relative* path when neither `HOME` nor
/// `USERPROFILE` is set, which for a double-clicked application is wherever the
/// launcher happened to put it.
pub async fn start(data_dir: PathBuf) -> opencompany::Result<EmbeddedHost> {
    start_with(data_dir, FirstRun::SeedStarterCompany).await
}

/// Boots a host over `data_dir`, deciding what an empty root means.
///
/// See [`FirstRun`]. Everything else — the lock, the migration, the journal
/// check, the loopback bind — is identical, because the two kinds of host
/// differ in exactly one decision and must not be allowed to drift in any
/// other.
pub async fn start_with(
    data_dir: PathBuf,
    first_run: FirstRun,
) -> opencompany::Result<EmbeddedHost> {
    // Resolve, lock, migrate, and prove the journal root is writable — the same
    // sequence `serve` runs, shared rather than copied so the two cannot drift.
    // The lock is what refuses a second instance over one data root, including
    // the very ordinary case of a terminal already running `opencompany serve`
    // against the same default.
    let instance = opencompany::app::prepare_instance(Some(data_dir)).await?;

    let config = AppConfig {
        bind: "127.0.0.1:0".to_string(),
        // The `[workspace]` section of the root's `config.toml`, resolved by
        // `prepare_instance`. Not layout — these two are the knobs every
        // company builder reads — but they come from the same file, and `serve`
        // sets them from it. A desktop that skipped them ran with the
        // compiled-in defaults and silently ignored the operator's config.
        workspace_quota: instance.workspace().quota,
        workspace_git_enabled: instance.workspace().git_enabled,
        // The standing local admin, and the same seam the hosted control plane
        // fills with `OPENCOMPANY_ADMIN_EMAIL`. Without an eligible address no
        // company on this host can be signed into, whoever created it — the
        // seeded one names the operator in its own manifest, but a company
        // added later by any other means would name nobody (issue #632).
        admin_email: Some(opencompany::desktop::DESKTOP_OPERATOR_EMAIL.to_string()),
        ..AppConfig::default()
    };
    let state = AppState::new(config).with_home(instance.home().to_path_buf());
    // Read before `state` moves into `bind`. Minting here rather than on the
    // first `/spec` also means the console can be told who this host is without
    // waiting to contact it — which is the whole point, since the address it
    // would contact is what changed.
    let instance_id = state.instance_id().to_string();
    // Before the listener, not after: a console that reached a host with an
    // empty registry would render the "no companies" dead end this exists to
    // remove, and the race is winnable — the address is handed to the webview
    // the moment `start` returns.
    let companies = match first_run {
        FirstRun::SeedStarterCompany => opencompany::desktop::bootstrap_companies(
            &state,
            opencompany::desktop::DEFAULT_PRESET_ID,
        )
        .await?
        .into_iter()
        .map(|id| id.as_ref().to_string())
        .collect::<Vec<_>>(),
        // The adopt half of the same call, and *only* that half. Adoption is
        // not optional for either kind of host: a company the setup wizard
        // wrote into this root is a bundle on disk, and a host that skipped
        // adoption would come back from every restart serving nothing.
        FirstRun::RunSetupWizard => opencompany::desktop::adopt_companies(&state)
            .await?
            .into_iter()
            .map(|(id, _)| id.as_ref().to_string())
            .collect::<Vec<_>>(),
    };

    let (address, serving) = opencompany::server::bind("127.0.0.1:0", state).await?;
    let server = tokio::spawn(async move {
        if let Err(error) = serving.run().await {
            tracing::error!(%error, "the embedded host stopped");
        }
    });

    tracing::info!(
        %address,
        %instance_id,
        companies = companies.len(),
        home = %instance.home().display(),
        "embedded host listening"
    );
    Ok(EmbeddedHost {
        address,
        instance_id,
        companies,
        _instance: instance,
        server,
    })
}

#[cfg(test)]
mod test {
    use super::*;

    /// Issue #632, end to end: a packaged install must be enterable with no
    /// terminal, no mail server and no platform credential.
    ///
    /// Over HTTP rather than against the registry, because every step here is
    /// one the console actually takes and each has its own way to fail: the
    /// company has to exist *and* be reachable through the sole-company alias
    /// the console addresses before it knows any id, the operator has to be
    /// eligible or no code is minted, and the host has to be local-only or the
    /// code is withheld from the response. Asserting a company was registered
    /// would prove none of it.
    #[tokio::test]
    async fn a_fresh_install_can_be_signed_into() {
        let dir = tempfile::tempdir().unwrap();
        let host = start(dir.path().to_path_buf()).await.expect("host starts");
        let base = host.base_url();
        let http = reqwest::Client::new();

        assert_eq!(
            host.companies().len(),
            1,
            "a fresh root gets exactly one starter company"
        );

        // Where the console starts, and where it used to stop: no session yet.
        let listed = http
            .get(format!("{base}/api/v1/companies"))
            .send()
            .await
            .unwrap();
        assert_eq!(listed.status(), 401);

        // The sole-company alias — what the console addresses before it has
        // discovered any company id.
        let requested: serde_json::Value = http
            .post(format!("{base}/api/v1/company/auth/request"))
            .json(&serde_json::json!({ "email": host.operator_email() }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let code = requested["dev_code"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("a loopback host with no mail transport echoes the code: {requested}")
            })
            .to_string();

        let verified = http
            .post(format!("{base}/api/v1/company/auth/verify"))
            .json(&serde_json::json!({ "code": code }))
            .send()
            .await
            .unwrap();
        assert_eq!(verified.status(), 200, "the code redeems into a session");
        let session = verified
            .headers()
            .get(reqwest::header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .expect("a session cookie comes back")
            .to_string();

        // And the call that was refused above now answers.
        let listed = http
            .get(format!("{base}/api/v1/companies"))
            .header(reqwest::header::COOKIE, session)
            .send()
            .await
            .unwrap();
        assert_eq!(listed.status(), 200);
        let companies: serde_json::Value = listed.json().await.unwrap();
        assert_eq!(
            companies.as_array().map(Vec::len),
            Some(1),
            "the signed-in operator sees their company: {companies}"
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
}
