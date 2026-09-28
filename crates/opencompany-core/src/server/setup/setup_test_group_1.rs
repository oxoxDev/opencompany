use crate::app::config::MapEnv;
use crate::ports::CompanyStore;
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::runtime::RuntimeBuilder;
use crate::server::router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use tower::ServiceExt;

use super::setup_test_support_1::*;

// ---------------------------------------------------------------------------
// The payload
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_fresh_host_reports_itself_unconfigured() {
    let home_dir = home();
    let (status, dto) = get_setup(fresh_state(home_dir.path())).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(dto["complete"], false);
    assert!(
        dto["companies"].as_array().unwrap().is_empty(),
        "a first run has no company yet"
    );
    assert!(
        dto["config_path"]
            .as_str()
            .unwrap()
            .ends_with("config.toml"),
        "the flow must name the file it writes"
    );
}

/// The template catalog is the shipped preset list, and each entry carries
/// enough to draw a card without the console parsing manifests itself.
#[tokio::test]
async fn the_payload_lists_the_shipped_templates() {
    let home_dir = home();
    let (_, dto) = get_setup(fresh_state(home_dir.path())).await;

    let templates = dto["templates"].as_array().unwrap();
    assert_eq!(
        templates.len(),
        crate::desktop::PRESETS.len(),
        "every shipped preset must be offered"
    );
    let default = templates
        .iter()
        .find(|t| t["id"] == crate::desktop::DEFAULT_PRESET_ID)
        .expect("the default preset is in the catalog");
    assert!(
        default["agent_count"].as_u64().unwrap() > 0,
        "a template's roster size must be readable: {default}"
    );
}

/// ACP is a cargo feature whose transport is mounted under that feature, so
/// the flow reports build state rather than offering a switch. A flag that
/// claimed otherwise would send a client to an endpoint that 404s.
#[tokio::test]
async fn the_payload_reports_acp_as_build_state_not_a_setting() {
    let home_dir = home();
    let (_, dto) = get_setup(fresh_state(home_dir.path())).await;

    assert_eq!(dto["build"]["acp_in_build"], cfg!(feature = "acp"));
    assert_eq!(
        dto["build"]["acp_transport_mounted"],
        cfg!(feature = "acp"),
        "the flag must match whether the /acp handler is actually mounted"
    );
    assert!(
        !dto["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["key"].as_str().unwrap().contains("acp")),
        "ACP must not appear as a writable field"
    );
}

/// `none` has no sign-in. Offering it on a routable bind would produce a choice
/// the next boot refuses, so it is withheld there.
#[tokio::test]
async fn none_is_offered_only_on_a_loopback_host() {
    let home_dir = home();
    let (_, local) = get_setup(fresh_state(home_dir.path())).await;
    assert!(
        local["auth_modes"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("none")),
        "loopback may choose `none`"
    );

    // Read the routable host's modes through the admin path, since the
    // anonymous gate is (correctly) shut there.
    let state = routable_state(home_dir.path());
    with_company(&state, home_dir.path()).await;
    let cookie =
        crate::server::test_support::seed_session(&state, "acme", crate::ports::UserRole::Admin)
            .await;
    let response = router(state)
        .oneshot(
            Request::builder()
                .uri("/api/v1/setup")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let dto = body_json(response).await;
    assert!(
        !dto["auth_modes"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("none")),
        "a routable host must not offer an unauthenticated console: {dto}"
    );
}

/// The packaged desktop boots with `none` already in force, and the wizard
/// must preselect it rather than ask an operator to re-derive a fact about
/// their own computer. Reported by the host so a browser tab against the
/// desktop's host gets the same answer as the webview. A plain `serve` on
/// loopback has no override and gets no default — `email` stays what it was.
#[tokio::test]
async fn the_desktop_host_reports_none_as_the_default_sign_in() {
    let home_dir = home();
    let (_, plain) = get_setup(fresh_state(home_dir.path())).await;
    assert!(
        plain.get("default_auth_mode").is_none(),
        "a plain loopback serve names no default: {plain}"
    );

    let desktop = crate::AppState::new(crate::AppConfig {
        bind: "127.0.0.1:8080".to_string(),
        auth_mode_override: Some(crate::app::config::AuthMode::None),
        ..crate::AppConfig::default()
    })
    .with_home(home_dir.path().to_path_buf());
    let (_, dto) = get_setup(desktop).await;
    assert_eq!(dto["default_auth_mode"], "none", "{dto}");
}

/// A laptop with no SMTP is not a broken host — it is the one shape where the
/// honest hand-off is a link the operator opens themselves. The wizard has to
/// be able to tell that apart from a host where a magic link simply goes
/// nowhere, and only the payload can say which it is on.
#[tokio::test]
async fn mail_on_a_loopback_host_with_no_transport_reports_the_code_echo() {
    let home_dir = home();
    let (_, dto) = get_setup(fresh_state(home_dir.path())).await;

    assert_eq!(dto["mail"]["wired"], false, "nothing is configured: {dto}");
    assert_eq!(
        dto["mail"]["echoes_code"], true,
        "a loopback host hands the code back in the response: {dto}"
    );
}

/// The dead end. A routable host with no transport can neither mail a link nor
/// echo one, so a wizard that offered the link form here would be offering a
/// sign-in that arrives nowhere.
#[tokio::test]
async fn mail_on_a_routable_host_with_no_transport_reports_neither() {
    let home_dir = home();
    let state = routable_state(home_dir.path());
    with_company(&state, home_dir.path()).await;
    let dto = get_setup_as_admin(state).await;

    assert_eq!(dto["mail"]["wired"], false, "{dto}");
    assert_eq!(
        dto["mail"]["echoes_code"], false,
        "a routable host must not be described as echoing codes: {dto}"
    );
}

/// With a transport wired the link is a real send, and the echo stops — the
/// same either/or the login route itself branches on.
#[tokio::test]
async fn mail_with_a_transport_wired_reports_a_real_send() {
    let home_dir = home();
    let (_, dto) = get_setup(state_with_mail(home_dir.path())).await;

    assert_eq!(dto["mail"]["wired"], true, "{dto}");
    assert_eq!(
        dto["mail"]["echoes_code"], false,
        "a wired transport is delivered to, never echoed: {dto}"
    );
}

/// `auth_modes` says which modes are *legal*, not which are convenient today.
/// A host with no SMTP still runs `email` mode perfectly well over passwords,
/// so withholding the mode here would take away a working
/// sign-in on the strength of a transport it does not need. `mail` is the field
/// that says what the mailbox path can do; this one must stay a policy answer.
#[tokio::test]
async fn email_is_still_offered_on_a_host_that_cannot_mail() {
    let home_dir = home();
    let state = routable_state(home_dir.path());
    with_company(&state, home_dir.path()).await;
    let dto = get_setup_as_admin(state).await;

    assert_eq!(
        dto["mail"]["wired"], false,
        "this host is the one that cannot mail: {dto}"
    );
    assert!(
        dto["auth_modes"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("email")),
        "email mode does not depend on a transport: {dto}"
    );
}

/// A credential's status is reportable; its bytes are not.
#[tokio::test]
async fn a_secret_field_never_echoes_its_value() {
    let home_dir = home();
    std::fs::write(
        home_dir.path().join("config.toml"),
        "tinyhumans_api_key = \"sk-do-not-echo\"\n",
    )
    .unwrap();

    let (_, dto) = get_setup(fresh_state(home_dir.path())).await;

    let key = field(&dto, "tinyhumans_api_key");
    assert_eq!(key["secret"], true);
    assert!(key["value"].is_null(), "the value must not be echoed");
    assert!(
        !dto.to_string().contains("sk-do-not-echo"),
        "the secret must appear nowhere in the payload"
    );
}

// ---------------------------------------------------------------------------
// Precedence honesty
// ---------------------------------------------------------------------------

/// A value in the file is reported as owned by `config.toml` and stays editable.
#[tokio::test]
async fn a_file_owned_field_is_editable() {
    let home_dir = home();
    std::fs::write(
        home_dir.path().join("config.toml"),
        "bind = \"127.0.0.1:9999\"\n",
    )
    .unwrap();

    let (_, dto) = get_setup(fresh_state(home_dir.path())).await;

    let bind = field(&dto, "bind");
    assert_eq!(bind["layer"], "config.toml");
    assert_eq!(bind["value"], "127.0.0.1:9999");
    assert_eq!(bind["editable"], true);
    assert_eq!(
        bind["requires_restart"], true,
        "a bind change only takes effect at the next boot"
    );
}

/// A field nothing sets falls to its built-in default and is still editable.
#[tokio::test]
async fn an_unset_field_reports_its_default_layer() {
    let home_dir = home();
    let (_, dto) = get_setup(fresh_state(home_dir.path())).await;

    let quota = field(&dto, "workspace.tree_quota_gb");
    assert_eq!(quota["layer"], "default");
    assert!(quota["value"].is_null());
    assert_eq!(quota["editable"], true);
}

/// Writing a field the environment owns would produce a file nothing reads, so
/// the flow refuses rather than reporting a success that changes nothing at the
/// next boot. This is the failure mode the whole surface exists to prevent.
/// Driven through the injected [`EnvSource`] rather than `std::env::set_var`.
/// Tests share one process, so mutating the real environment would leak into
/// whichever unrelated test happened to resolve config at the same moment —
/// which is exactly why `resolve` takes this seam in the first place.
#[tokio::test]
async fn a_write_to_an_env_owned_field_is_refused() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());
    let env = MapEnv::new([("OPENCOMPANY_AUTH_MODE", "wallet")]);

    let dto = serde_json::to_value(super::snapshot(&state, &env).unwrap()).unwrap();
    let mode = field(&dto, "auth_mode");
    assert_eq!(mode["layer"], "env");
    assert_eq!(
        mode["editable"], false,
        "an env-owned field must render read-only"
    );

    let err = super::apply_inner(
        &state,
        super::SetupRequest {
            fields: [("auth_mode".to_string(), Some("email".to_string()))]
                .into_iter()
                .collect(),
            template: None,
            company: None,
            name: None,
            admin_email: None,
            admin_password: None,
            tinyhumans_key: None,
            tinyhumans_model: None,
            provider_draft: None,
            composio_draft: None,
        },
        &env,
    )
    .await
    .expect_err("writing an env-owned field must be refused");

    assert_eq!(err.code(), "conflict");
    assert!(
        err.to_string().contains("environment"),
        "the refusal must explain why: {err}"
    );
    assert!(
        !home_dir.path().join("config.toml").exists(),
        "a refused apply must write nothing"
    );
    assert!(
        !state.setup_complete(),
        "a refused apply must not mark the instance configured"
    );
}

// ---------------------------------------------------------------------------
// Applying
// ---------------------------------------------------------------------------

#[tokio::test]
async fn applying_writes_the_file_and_marks_setup_complete() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({
            "fields": { "bind": "127.0.0.1:9100", "workspace.max_blob_mb": "64" }
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["complete"], true);
    assert!(
        body["restart_required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("bind")),
        "the flow must say what is still pending: {body}"
    );

    let file = crate::app::config::ConfigFile::load(home_dir.path())
        .unwrap()
        .unwrap();
    assert_eq!(file.bind.as_deref(), Some("127.0.0.1:9100"));
    assert_eq!(file.workspace.max_blob_mb, Some(64.0));
    assert!(
        file.setup_completed_at.is_some(),
        "completion must be recorded in the file, not just in memory"
    );
    assert!(state.setup_complete(), "the live flag must flip too");
}

/// The template choice seeds the operator's pick, not the hardcoded default.
#[tokio::test]
async fn applying_seeds_the_chosen_template() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({ "fields": {}, "template": "law_firm" }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["seeded_company"].is_string(),
        "a company must have been seeded: {body}"
    );
    assert_eq!(state.registry().len(), 1);

    let id = state.registry().list().into_iter().next().unwrap();
    let record = crate::store::FsCompanyStore::new(home_dir.path().to_path_buf())
        .load(&id)
        .await
        .unwrap()
        .expect("the seeded company is persisted");
    assert_eq!(
        record
            .template_provenance
            .as_ref()
            .map(|p| p.source_id.as_str()),
        Some("law_firm"),
        "provenance must record which template this install started from"
    );
}

/// Choosing "no sign-in" must actually mean no sign-in, immediately.
///
/// The mode is resolved once, at build, and cached on the runtime, so writing
/// `auth_mode` to `config.toml` alone only takes effect at the next boot. That
/// left an operator who picked "no sign-in" looking at a login form on a host
/// they had just told not to have one — the setting appeared to save and did
/// nothing. Setup now makes it live before it builds anything with it.
#[tokio::test]
async fn choosing_no_sign_in_applies_to_the_company_it_seeds() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({
            "fields": { "auth_mode": "none" },
            "template": "law_firm",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let id = state.registry().list().into_iter().next().expect("seeded");
    assert_eq!(
        state.registry().get(&id).unwrap().auth_mode(),
        crate::app::config::AuthMode::None,
        "the seeded company must be built with the mode the operator just chose, \
         not the one the process booted with"
    );
    assert!(
        !body["restart_required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("auth_mode")),
        "it applied live, so telling the operator to restart for it would be a lie: {body}"
    );
}

/// The host-wide mode is what a later build reads, not the frozen boot value.
#[tokio::test]
async fn the_chosen_mode_becomes_the_hosts_live_mode() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());
    assert_eq!(state.auth_mode_override(), None, "nothing set at boot");

    let (status, _) = post_setup(
        state.clone(),
        serde_json::json!({ "fields": { "auth_mode": "wallet" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        state.auth_mode_override(),
        Some(crate::app::config::AuthMode::Wallet),
    );

    // Clearing it hands the answer back to each manifest's `[users].mode`.
    let (status, _) = post_setup(
        state.clone(),
        serde_json::json!({ "fields": { "auth_mode": null } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state.auth_mode_override(), None);
}

/// A host with no rebuilder cannot re-apply the mode to a company it already
/// built, so it must say a restart is needed rather than claim success.
#[tokio::test]
async fn an_existing_company_that_cannot_rebuild_reports_a_restart() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());
    with_company(&state, home_dir.path()).await;

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({ "fields": { "auth_mode": "none" } }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["restart_required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("auth_mode")),
        "no rebuilder is wired in this fixture, so the honest answer is `restart`: {body}"
    );
}

/// PLAT-052: the rebuild loop is per-company best-effort. A company that
/// cannot rebuild — in the middle of the list, not just the only one — must
/// not stop the companies after it, and the honest `restart_required` must
/// still be reported rather than silently dropped once anything succeeded.
#[tokio::test]
async fn a_failed_rebuild_mid_list_does_not_stop_the_rest() {
    let home_dir = home();
    let store = crate::store::FsCompanyStore::new(home_dir.path().to_path_buf());
    let state = fresh_state(home_dir.path());
    for name in ["acme", "globex", "initech"] {
        let id = CompanyId::new(name);
        store
            .save(&CompanyRecord {
                general_channel: Default::default(),
                overlay_desk_hive: Vec::new(),
                overlay_retired_agents: Vec::new(),
                overlay_agent_edits: Vec::new(),
                id: id.clone(),
                manifest: manifest(),
                ledger: Vec::new(),
                lifecycle: "running".to_string(),
                overlay_agents: Vec::new(),
                overlay_desk_members: Vec::new(),
                overlay_tool_grants: None,
                overlay_desk_tools: std::collections::BTreeMap::new(),
                overlay_desk_order: Vec::new(),
                overlay_desks: Vec::new(),
                overlay_workflows: Vec::new(),
                overlay_budgets: Vec::new(),
                overlay_policy: None,
                disabled_workflows: Vec::new(),
                template_provenance: None,
                setup: None,
                name_confirmed: false,
                activation_completed_at: None,
                created_at_millis: None,
            })
            .await
            .unwrap();
        let runtime = RuntimeBuilder::new(home_dir.path().to_path_buf(), manifest())
            .with_id(id.clone())
            .build()
            .await
            .unwrap();
        state.registry().insert(id, Arc::new(runtime));
    }
    let state = state.with_rebuilder(Arc::new(SelectiveRebuilder {
        home: home_dir.path().to_path_buf(),
        fails: vec!["globex".to_string()],
    }));

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({ "fields": { "auth_mode": "none" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    assert_eq!(
        state
            .registry()
            .get(&CompanyId::new("acme"))
            .unwrap()
            .auth_mode(),
        crate::app::config::AuthMode::None,
        "a company before the failing one in the list must still be rebuilt"
    );
    assert_eq!(
        state
            .registry()
            .get(&CompanyId::new("initech"))
            .unwrap()
            .auth_mode(),
        crate::app::config::AuthMode::None,
        "a company after the failing one must still be rebuilt: the loop must not abort mid-list"
    );
    assert!(
        body["restart_required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("auth_mode")),
        "the failing company means a restart is still genuinely owed, even though two of \
         three succeeded: {body}"
    );
}

/// A re-run must never hand the operator a second starter company.
#[tokio::test]
async fn applying_does_not_seed_when_a_company_already_exists() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());
    with_company(&state, home_dir.path()).await;

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({ "fields": {}, "template": "law_firm" }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["seeded_company"].is_null(), "{body}");
    assert_eq!(state.registry().len(), 1, "still exactly one company");
}

#[tokio::test]
async fn an_unknown_field_is_refused() {
    let home_dir = home();
    let (status, body) = post_setup(
        fresh_state(home_dir.path()),
        serde_json::json!({ "fields": { "not_a_setting": "x" } }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("not_a_setting"));
    assert!(!home_dir.path().join("config.toml").exists());
}

#[tokio::test]
async fn a_malformed_value_is_refused_before_anything_is_written() {
    let home_dir = home();
    let (status, body) = post_setup(
        fresh_state(home_dir.path()),
        serde_json::json!({ "fields": { "workspace.max_blob_mb": "lots" } }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"].as_str().unwrap().contains("a number"),
        "{body}"
    );
    assert!(
        !home_dir.path().join("config.toml").exists(),
        "validation happens before the write"
    );
}

/// An unparseable `auth_mode` aborts boot, so a typo here would leave a host
/// that will not come back up. It is caught at the write instead.
#[tokio::test]
async fn an_invalid_auth_mode_is_refused() {
    let home_dir = home();
    let (status, body) = post_setup(
        fresh_state(home_dir.path()),
        serde_json::json!({ "fields": { "auth_mode": "sso" } }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(!home_dir.path().join("config.toml").exists());
}

/// An unresolvable `bind` (a malformed port, here) would abort `TcpListener`
/// at the next boot, so it is refused at the write instead — the same
/// treatment `auth_mode` gets just above.
#[tokio::test]
async fn an_unresolvable_bind_is_refused() {
    let home_dir = home();
    let (status, body) = post_setup(
        fresh_state(home_dir.path()),
        serde_json::json!({ "fields": { "bind": "127.0.0.1:notaport" } }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        !home_dir.path().join("config.toml").exists(),
        "validation happens before the write"
    );
}

/// The boot path resolves `bind` through `ToSocketAddrs`, which accepts a
/// hostname alongside a literal IP — so `localhost:PORT` must be accepted
/// here too, not just an IP-shaped address.
#[tokio::test]
async fn a_hostname_bind_is_accepted() {
    let home_dir = home();
    let (status, body) = post_setup(
        fresh_state(home_dir.path()),
        serde_json::json!({ "fields": { "bind": "localhost:8080" } }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(home_dir.path().join("config.toml").exists());
}
