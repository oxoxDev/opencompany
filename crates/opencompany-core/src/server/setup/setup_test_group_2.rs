use crate::server::router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use super::setup_test_support_1::*;

/// A failed persist must leave the process exactly as it was: no live
/// auth-mode override, no seeded company, and `setup_complete` still false.
/// Before #908's fix, `apply_inner` set the live override and seeded the
/// company *before* calling `write_config_toml`, so a write failure returned
/// an error while the live host had already moved — breaking both the module
/// doc's "one transaction" claim and `AppliedDto::complete`'s "a partial
/// apply is an error, not a result".
///
/// The write is forced to fail by making `config.toml` a directory: the read
/// that opens `write_config_toml` fails before anything is touched, the same
/// shape a permission or disk-full failure would take.
#[tokio::test]
async fn a_failed_write_leaves_no_live_state_behind() {
    let home_dir = home();
    std::fs::create_dir(home_dir.path().join("config.toml")).unwrap();

    let state = fresh_state(home_dir.path());
    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({
            "fields": { "auth_mode": "wallet" },
            "template": "marketing_agency",
        }),
    )
    .await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert!(
        state.auth_mode_override().is_none(),
        "the live auth-mode override must not survive a failed write"
    );
    assert!(
        state.registry().is_empty(),
        "no company may be seeded when the write that should record it failed"
    );
    assert!(
        !state.setup_complete(),
        "setup must not read as complete when its write failed"
    );
}

/// Clearing a field removes the key so the layer below applies, rather than
/// writing a blank that shadows it.
#[tokio::test]
async fn clearing_a_field_removes_the_key() {
    let home_dir = home();
    std::fs::write(
        home_dir.path().join("config.toml"),
        "public_url = \"https://old.example\"\n",
    )
    .unwrap();

    let (status, _) = post_setup(
        fresh_state(home_dir.path()),
        serde_json::json!({ "fields": { "public_url": null } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let file = crate::app::config::ConfigFile::load(home_dir.path())
        .unwrap()
        .unwrap();
    assert!(file.public_url.is_none(), "the key must be gone");
}

// ---------------------------------------------------------------------------
// Access control
// ---------------------------------------------------------------------------

/// Both conditions, not either: an unconfigured host that is *routable* is not
/// open. Otherwise a freshly deployed instance would be configurable by whoever
/// reached it first.
#[tokio::test]
async fn an_unconfigured_but_routable_host_is_not_open() {
    let home_dir = home();
    let (status, _) = get_setup(routable_state(home_dir.path())).await;

    assert_ne!(
        status,
        StatusCode::OK,
        "a routable unconfigured host must not serve its configuration anonymously"
    );
}

/// A routable host must not accept an anonymous write either.
#[tokio::test]
async fn an_unconfigured_but_routable_host_refuses_an_anonymous_write() {
    let home_dir = home();
    let (status, _) = post_setup(
        routable_state(home_dir.path()),
        serde_json::json!({ "fields": { "bind": "0.0.0.0:1234" } }),
    )
    .await;

    assert_ne!(status, StatusCode::OK);
    assert!(
        !home_dir.path().join("config.toml").exists(),
        "nothing may be written by an unauthorized caller"
    );
}

/// A loopback-*configured* bind is not the same claim as a loopback
/// *request*: an undeclared reverse proxy in front of a loopback-bound
/// listener still presents a loopback peer to `TcpListener`, but the console
/// review on #908 flagged that `is_local_only()` alone cannot see that — it
/// only inspects the configured bind and `public_url`, never the request
/// itself. `request_looks_local` is the second gate that closes that gap: a
/// non-loopback peer on an otherwise loopback-configured, unconfigured host
/// must still be refused.
#[tokio::test]
async fn a_loopback_configured_host_still_refuses_a_non_loopback_peer() {
    use axum::extract::ConnectInfo;

    let home_dir = home();
    let app = router(fresh_state(home_dir.path()));

    let mut req = Request::builder()
        .uri("/api/v1/setup")
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(ConnectInfo(
        "203.0.113.7:54321".parse::<std::net::SocketAddr>().unwrap(),
    ));
    let response = app.oneshot(req).await.unwrap();
    assert_ne!(
        response.status(),
        StatusCode::OK,
        "a non-loopback peer must not pass the anonymous setup gate even on a \
         loopback-configured bind"
    );
}

/// The other half of the same gap: a same-host reverse proxy connects to a
/// loopback-bound listener over loopback too, so the peer alone cannot catch
/// an *undeclared* one — only a proxy-forwarding header can. Any request
/// carrying one must be refused just as a non-loopback peer is.
#[tokio::test]
async fn a_loopback_configured_host_refuses_a_forwarded_request() {
    let home_dir = home();
    let app = router(fresh_state(home_dir.path()));

    let req = Request::builder()
        .uri("/api/v1/setup")
        .header("x-forwarded-for", "203.0.113.7")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_ne!(
        response.status(),
        StatusCode::OK,
        "a request carrying a proxy-forwarding header must not pass the \
         anonymous setup gate even on a loopback-configured bind"
    );
}

/// The other half: a configured host is closed even on loopback, so a page in
/// the browser cannot rewrite a laptop's settings after setup has run.
#[tokio::test]
async fn a_configured_loopback_host_is_closed() {
    let home_dir = home();
    let state = fresh_state(home_dir.path()).with_setup_complete(true);
    with_company(&state, home_dir.path()).await;

    let (status, _) = get_setup(state.clone()).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "once setup is done, re-running it takes an admin"
    );

    let (status, _) = post_setup(state, serde_json::json!({ "fields": {} })).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// An admin may re-run setup on a configured host — the "Run setup again" path.
#[tokio::test]
async fn an_admin_may_re_run_setup_on_a_configured_host() {
    let home_dir = home();
    let state = fresh_state(home_dir.path()).with_setup_complete(true);
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

    assert_eq!(response.status(), StatusCode::OK);
    let dto = body_json(response).await;
    assert_eq!(dto["complete"], true);
}

/// A member is not an admin, and host-level configuration is an admin action.
#[tokio::test]
async fn a_member_may_not_re_run_setup() {
    let home_dir = home();
    let state = fresh_state(home_dir.path()).with_setup_complete(true);
    with_company(&state, home_dir.path()).await;
    let cookie =
        crate::server::test_support::seed_session(&state, "acme", crate::ports::UserRole::Member)
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

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

/// Regression: `serve --company <dir>` predates this flow entirely, so every
/// existing deployment has companies and no `setup_completed_at`. `/spec`
/// reporting the raw stamp sent all of them into the wizard on their next
/// console load — and because the wizard replaces the console outright, the
/// end-to-end suite sat waiting on selectors that would never appear, until the
/// 30-minute job timeout killed it.
///
/// The two questions come apart deliberately: `/spec` answers "must the console
/// offer setup", while `AppState::setup_complete` stays the literal stamp,
/// because `authorize` needs "has an admin to check against", not "has been
/// configured".
#[tokio::test]
async fn spec_reports_setup_complete_once_a_company_is_registered() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());

    assert!(
        !state.spec().setup_complete,
        "precondition: no stamp and no companies is the genuine first run"
    );

    with_company(&state, home_dir.path()).await;

    assert!(
        state.spec().setup_complete,
        "a host already serving a company has something to open, so the \
         console must not replace it with the first-run wizard"
    );
    assert!(
        !state.setup_complete(),
        "the raw stamp stays false — `authorize` reads it, and a host with \
         companies authorizes through its admin rather than anonymously"
    );
}

/// A template the operator picked is the roster they get back, not the curated
/// team matched from their words.
///
/// The two are different rosters and only one of them was chosen by anybody.
/// Picking "Agentic Marketing Agency" — a card that says eight teammates — and
/// skipping the model step returned the five-person curated marketing team,
/// under a heading naming the template. Asserted against the template's own
/// count rather than a literal, so a template that gains a teammate does not
/// fail this.
#[tokio::test]
async fn a_picked_template_proposes_its_own_roster() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());
    let expected = crate::desktop::preset("marketing_agency")
        .expect("a bundled template")
        .manifest_parsed()
        .expect("it parses")
        .agents;

    let (status, body) = post_roster(
        state,
        serde_json::json!({
            "template": "marketing_agency",
            "industry": "",
            "teamHint": "",
            "automate": "campaign briefs and weekly reporting",
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["source"], "preset",
        "the console needs to know this roster can be seeded as the template itself: {body}"
    );
    assert_eq!(
        body["agents"].as_array().map(Vec::len),
        Some(expected.len()),
        "the roster on the review screen must be the roster the card advertised: {body}"
    );
    let roles: Vec<&str> = body["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|agent| agent["role"].as_str().unwrap())
        .collect();
    assert!(
        expected
            .iter()
            .all(|agent| roles.contains(&agent.role.as_str())),
        "every teammate the template declares must be on it: {roles:?}"
    );
}

/// The curated path is untouched where no template was picked.
///
/// The pair matters: the fix above must not become "always ship a preset", or
/// an operator who typed their business in their own words and never opened the
/// template list would get a roster matched by slug instead of by what they
/// wrote.
#[tokio::test]
async fn answers_without_a_template_still_propose_the_curated_team() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());

    let (status, body) = post_roster(
        state,
        serde_json::json!({
            "industry": "E-commerce",
            "teamHint": "",
            "automate": "order dispatch and returns",
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["source"], "fallback", "{body}");
}

/// Setup seeds the template itself when the console sends a slug, under the
/// name the operator typed.
///
/// Both halves are the point. The template arm was unreachable from the console
/// — the wizard only ever sent a designed company — so a picked template was
/// rebuilt from the review screen and lost the belt and prompts it ships. And
/// the name was derived from the *industry* answer with no way to say
/// otherwise, on a field that mints the company id.
#[tokio::test]
async fn applying_a_template_seeds_it_under_the_name_the_operator_chose() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({
            "fields": {},
            "template": "marketing_agency",
            "name": "Northwind Studio",
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["seeded_company"], "northwind-studio",
        "the id is minted from the name the operator gave: {body}"
    );

    let id = crate::ports::types::CompanyId::new("northwind-studio");
    let runtime = state
        .registry()
        .get(&id)
        .expect("the seeded company is registered");
    // Read back off the store rather than off the runtime: what matters is the
    // bundle the next launch adopts, which is what `adopt_companies` reads.
    let record = runtime
        .store()
        .load(&id)
        .await
        .expect("the bundle is readable")
        .expect("the bundle exists");
    let manifest = record.manifest;
    assert_eq!(manifest.company.name, "Northwind Studio");
    // Every teammate the template declares, by role. Not a count: a registered
    // company's stored manifest also carries the roster `companies/_globals/` contributes,
    // so an equality here would be asserting the size of something this change
    // has nothing to do with.
    let template_roles: Vec<String> = crate::desktop::preset("marketing_agency")
        .unwrap()
        .manifest_parsed()
        .unwrap()
        .agents
        .iter()
        .map(|agent| agent.role.clone())
        .collect();
    let seeded_roles: Vec<String> = manifest.agents.iter().map(|a| a.role.clone()).collect();
    assert!(
        template_roles
            .iter()
            .all(|role| seeded_roles.contains(role)),
        "a renamed template is still that template's roster: {seeded_roles:?}"
    );
}

/// A template seed carries the address that will administer it.
///
/// No shipped product template names an admin, so on a host that asks people to
/// sign in, seeding one without this produces a company nobody can administer:
/// setup completes, email sign-in is on, and the address the operator typed two
/// screens earlier is ineligible. Only reachable since a picked template began
/// being seeded as itself — before that every company came through the designed
/// path, which has always written it.
#[tokio::test]
async fn a_template_seed_names_the_operator_as_its_admin() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({
            "fields": {},
            "template": "law_firm",
            "admin_email": "ada@example.com",
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let id = crate::ports::types::CompanyId::new("agentic-law-firm");
    let record = state
        .registry()
        .get(&id)
        .expect("the seeded company is registered")
        .store()
        .load(&id)
        .await
        .expect("the bundle is readable")
        .expect("the bundle exists");
    assert_eq!(
        record.manifest.users.admins,
        vec!["ada@example.com".to_string()],
        "a company that lists nobody cannot be signed into"
    );
}

/// The password typed on the "You" step is the account, not just a wish.
///
/// Seeding writes the address into `[users].admins`, which makes it
/// *eligible*; on a laptop with no mail that is a standing invite nobody can
/// redeem. With a password the apply mints the account there and then, so the
/// wizard can sign the operator straight in and the same password works on
/// every later visit.
#[tokio::test]
async fn a_seed_with_a_password_creates_a_usable_admin() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({
            "fields": { "auth_mode": "email" },
            "template": "law_firm",
            "admin_email": "Ada@Example.com",
            "admin_password": "correct horse battery staple",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let response = router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/companies/agentic-law-firm/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "email": "ada@example.com",
                        "password": "correct horse battery staple",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the password signs them in"
    );
    let me = body_json(response).await;
    assert_eq!(me["role"], "admin");
    assert_eq!(me["mustChangePassword"], false);
}

/// A password the policy refuses is refused before anything is written: no
/// company is seeded and setup is not marked complete, the same all-or-nothing
/// rule every other validation here follows.
#[tokio::test]
async fn a_weak_admin_password_refuses_the_whole_apply() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({
            "fields": { "auth_mode": "email" },
            "template": "law_firm",
            "admin_email": "ada@example.com",
            "admin_password": "short",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(state.registry().is_empty(), "nothing was seeded");
    assert!(!state.setup_complete(), "setup did not complete");
}

/// A pasted paragraph is truncated, not turned into a directory nobody can
/// write.
///
/// `company_id_from_name` keeps every alphanumeric character it is handed, and
/// that id becomes one component under the store — so an unbounded name fails
/// the apply while writing the bundle, on most filesystems at 255 bytes. The
/// derivation has always clamped at `MAX_COMPANY_NAME`; a name the operator
/// supplies now meets the same bound.
#[tokio::test]
async fn a_very_long_name_is_bounded_before_it_becomes_an_id() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());
    let long = "Northwind ".repeat(40);

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({
            "fields": {},
            "template": "law_firm",
            "name": long,
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let id = body["seeded_company"]
        .as_str()
        .expect("a company was seeded");
    assert!(
        id.len() <= crate::company::setup::MAX_COMPANY_NAME,
        "the id is a directory component and must stay one: {id}"
    );
    let registered = state
        .registry()
        .get(&crate::ports::types::CompanyId::new(id))
        .expect("the seeded company is registered");
    let record = registered
        .store()
        .load(&crate::ports::types::CompanyId::new(id))
        .await
        .expect("the bundle is readable")
        .expect("the bundle exists");
    assert!(
        record.manifest.company.name.chars().count() <= crate::company::setup::MAX_COMPANY_NAME,
        "the name is bounded too, not just the id: {}",
        record.manifest.company.name
    );
}

/// A blank name is not a name.
///
/// `company_id_from_name` slugs an empty string to the literal id `company`, so
/// obeying a cleared field would produce a company called nothing at an id
/// naming nothing. The template's own name is the better answer to "I typed no
/// name" than that is.
#[tokio::test]
async fn a_blank_name_falls_back_to_the_templates_own() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({
            "fields": {},
            "template": "law_firm",
            "name": "   ",
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["seeded_company"], "agentic-law-firm", "{body}");
}

/// The wizard's whole reason for a second route: it needs a roster *before*
/// there is a company to scope one to. The company-scoped twin resolves a
/// `CompanyRuntime` and would 404 here.
#[tokio::test]
async fn a_roster_is_proposed_before_any_company_exists() {
    let home = home();
    let state = fresh_state(home.path());
    assert!(state.registry().is_empty(), "the premise: no company yet");

    let (status, body) = post_roster(
        state,
        serde_json::json!({
            "industry": "E-commerce — I sell homeware online",
            "automate": "Meta ads, order dispatch, daily reports",
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["template"], "ecommerce", "{body}");
    let agents = body["agents"].as_array().expect("agents");
    assert!(
        (4..=6).contains(&agents.len()),
        "a proposal must be a workable team, got {}: {body}",
        agents.len()
    );
    // Every row has to be directly usable as an apply's roster, so a missing
    // field would surface as a half-built company rather than as a 400 here.
    for agent in agents {
        for key in ["name", "role", "description"] {
            assert!(
                agent[key].as_str().is_some_and(|v| !v.trim().is_empty()),
                "agent is missing `{key}`: {agent}"
            );
        }
    }
}

/// The default build links no harness, so the curated team is the whole answer.
/// It must still be a real team and must say where it came from — an operator
/// shown a canned roster with no indication judges the product on a team it
/// never designed.
#[tokio::test]
async fn with_no_model_the_curated_team_ships_and_says_so() {
    let home = home();
    let (status, body) = post_roster(
        fresh_state(home.path()),
        serde_json::json!({ "industry": "zzzz qqqq" }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["template"], "generic", "{body}");
    assert_eq!(
        body["source"], "fallback",
        "the default build has no harness, so nothing designed this: {body}"
    );
}

/// An operator who types nothing still gets a team. The last two questions are
/// skippable by design, and stranding someone on the wizard is worse than a
/// generic roster.
#[tokio::test]
async fn an_empty_body_still_yields_a_team() {
    let home = home();
    let (status, body) = post_roster(fresh_state(home.path()), serde_json::json!({})).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["agents"].as_array().expect("agents").len() >= 4,
        "{body}"
    );
}

/// The proposal creates nothing. The wizard shows it for review first, and the
/// company is built by the apply — so a wizard abandoned at the review step
/// leaves the host exactly as it was.
#[tokio::test]
async fn proposing_creates_no_company() {
    let home = home();
    let state = fresh_state(home.path());
    let (status, _) =
        post_roster(state.clone(), serde_json::json!({ "industry": "software" })).await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        state.registry().is_empty(),
        "the proposal route must not register a company"
    );
}

/// The same gate the rest of this flow uses: open while unconfigured on
/// loopback, closed on a routable host where it would let whoever reached a
/// fresh deployment first drive it.
#[tokio::test]
async fn a_routable_host_refuses_an_anonymous_proposal() {
    let home = home();
    let (status, _) = post_roster(
        routable_state(home.path()),
        serde_json::json!({ "industry": "software" }),
    )
    .await;

    assert_ne!(
        status,
        StatusCode::OK,
        "an unauthenticated caller must not reach this on a routable host"
    );
}

/// CONSOLE-ADMIN-058: `propose_roster` is a pure read gated by the same
/// [`authorize`](super::authorize) [`apply`](super::apply) is — but unlike
/// `apply`, it persists nothing, so several proposals in flight at once must
/// not block on each other (there is nothing to serialize) and must not leave
/// any of them half-registering a company.
#[tokio::test]
async fn concurrent_roster_proposals_do_not_persist_anything() {
    let home = home();
    let state = fresh_state(home.path());
    let request = || serde_json::json!({ "industry": "software", "teamHint": "", "automate": "" });

    let (a, b, c) = tokio::join!(
        post_roster(state.clone(), request()),
        post_roster(state.clone(), request()),
        post_roster(state.clone(), request()),
    );

    for (status, body) in [&a, &b, &c] {
        assert_eq!(*status, StatusCode::OK, "{body}");
    }
    assert!(
        state.registry().is_empty(),
        "concurrent roster proposals must never register a company"
    );
    assert!(
        !state.setup_complete(),
        "a roster proposal must never mark setup complete"
    );
}
