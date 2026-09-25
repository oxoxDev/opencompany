//! `GET …/skills` reporting who each skill is scoped to, over HTTP.
//!
//! The inversion's own table is pinned in
//! `company/skill_scope_tests.rs`. What is driven here is the wiring: that the
//! route loads the roster at all, that the three states survive the record and
//! serde, and that a write answer the console folds into its list still carries
//! the scope rather than blanking it.

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use crate::company::CompanyManifest;
use crate::ports::CompanyStore;
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::runtime::RuntimeBuilder;
use crate::server::router;
use crate::server::test_support::{fixed_cookie, seed_fixed_admin};
use crate::{AppConfig, AppState};

/// A roster in all three scope states at once, which is the only shape that can
/// fail the collapse: `ceo` declares no `skills` line, `writer` names the slug
/// under test, and `hermit` carries a deliberate empty scope.
const ROSTER: &str = r#"
[company]
name = "Acme"
[policy]
mode = "full"

[[agent]]
id = "ceo"
role = "Chief Executive"

[[agent]]
id = "writer"
role = "Writer"
skills = ["brand-voice"]

[[agent]]
id = "hermit"
role = "Hermit"
skills = []
"#;

async fn state_with_roster(home: &std::path::Path) -> AppState {
    let id = CompanyId::new("acme");
    let manifest: CompanyManifest = toml::from_str(ROSTER).unwrap();
    crate::store::FsCompanyStore::new(home.to_path_buf())
        .save(&CompanyRecord {
            id: id.clone(),
            manifest: manifest.clone(),
            ledger: Vec::new(),
            lifecycle: "running".to_string(),
            overlay_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            overlay_retired_agents: Vec::new(),
            overlay_desk_hive: Vec::new(),
            overlay_desk_members: Vec::new(),
            overlay_desk_order: Vec::new(),
            overlay_desks: Vec::new(),
            overlay_workflows: Vec::new(),
            overlay_budgets: Vec::new(),
            overlay_policy: None,
            overlay_tool_grants: None,
            overlay_desk_tools: Default::default(),
            disabled_workflows: Vec::new(),
            template_provenance: None,
            setup: None,
            name_confirmed: false,
            activation_completed_at: None,
            created_at_millis: None,
        })
        .await
        .unwrap();
    let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest)
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, std::sync::Arc::new(runtime));
    seed_fixed_admin(&state, "acme").await;
    state
}

async fn send(
    state: &AppState,
    method: &str,
    uri: &str,
    body: Option<&str>,
) -> (StatusCode, Value, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", fixed_cookie("acme"));
    let request = match body {
        Some(body) => request
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => request.body(Body::empty()).unwrap(),
    };
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let raw = String::from_utf8_lossy(&bytes).to_string();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value, raw)
}

/// Authors `brand-voice`, so the slug the roster's scopes name actually exists
/// in the company's effective set.
async fn author_brand_voice(state: &AppState) -> Value {
    let body = serde_json::json!({
        "name": "Brand Voice",
        "description": "How we sound.",
    })
    .to_string();
    let (status, skill, raw) = send(state, "POST", "/api/v1/company/skills", Some(&body)).await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(skill["id"], "brand-voice", "{raw}");
    skill
}

/// One row out of `GET …/skills`.
async fn row(state: &AppState, slug: &str) -> Value {
    let (status, list, raw) = send(state, "GET", "/api/v1/company/skills", None).await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    list.as_array()
        .expect("the list is an array")
        .iter()
        .find(|row| row["id"] == slug)
        .cloned()
        .unwrap_or_else(|| panic!("no `{slug}` row in {raw}"))
}

/// One agent's entry out of a row's `agents`.
fn agent(row: &Value, id: &str) -> Value {
    row["agents"]
        .as_array()
        .unwrap_or_else(|| panic!("no `agents` on {row}"))
        .iter()
        .find(|agent| agent["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("no `{id}` in {}", row["agents"]))
}

/// The three states reach the wire, from the record, through the route.
///
/// Every field is asserted per agent rather than as a set: `state` and `holds`
/// answer different questions, and a projection that reported the effective set
/// for both would pass a `holds`-only assertion while reporting `hermit` and
/// `ceo` as the same thing.
#[tokio::test]
async fn the_skills_read_reports_every_roster_agents_standing() {
    let home = tempfile::tempdir().unwrap();
    let state = state_with_roster(home.path()).await;
    author_brand_voice(&state).await;

    let row = row(&state, "brand-voice").await;

    let ceo = agent(&row, "ceo");
    assert_eq!(ceo["state"], "inherited", "{row}");
    assert_eq!(ceo["holds"], true, "{row}");

    let writer = agent(&row, "writer");
    assert_eq!(writer["state"], "included", "{row}");
    assert_eq!(writer["holds"], true, "{row}");

    let hermit = agent(&row, "hermit");
    assert_eq!(
        hermit["state"], "excluded",
        "a deliberate empty scope excludes rather than inheriting: {row}"
    );
    assert_eq!(hermit["holds"], false, "{row}");
}

/// A skill nobody's list names: every agent that inherits still holds it, and
/// only the two that carry a list do not.
///
/// The row under test here is a baseline skill, so this also pins that the
/// projection covers every row of the list rather than only the one the fixture
/// authored.
#[tokio::test]
async fn a_skill_no_list_names_still_reaches_the_agents_that_inherit() {
    let home = tempfile::tempdir().unwrap();
    let state = state_with_roster(home.path()).await;

    let (status, list, raw) = send(&state, "GET", "/api/v1/company/skills", None).await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    let rows = list.as_array().expect("an array");
    let other = rows
        .iter()
        .find(|row| row["id"] != "brand-voice" && row["enabled"] == true)
        .unwrap_or_else(|| panic!("the global baseline installs skills in every company: {raw}"))
        .clone();

    assert_eq!(agent(&other, "ceo")["state"], "inherited", "{other}");
    assert_eq!(agent(&other, "ceo")["holds"], true, "{other}");
    assert_eq!(agent(&other, "writer")["state"], "excluded", "{other}");
    assert_eq!(agent(&other, "writer")["holds"], false, "{other}");
    assert_eq!(agent(&other, "hermit")["state"], "excluded", "{other}");
}

/// Disabling a skill takes it away from everybody while leaving every stored
/// state intact — and the toggle's own answer says so, because the console folds
/// that answer straight into the list it is showing.
///
/// This is the one write that changes reach without touching a scope. An answer
/// that omitted `agents` would leave the panel behind it reading a scope that
/// predates the switch.
#[tokio::test]
async fn disabling_a_skill_keeps_every_stored_state_and_takes_it_from_everybody() {
    let home = tempfile::tempdir().unwrap();
    let state = state_with_roster(home.path()).await;
    author_brand_voice(&state).await;

    let (status, toggled, raw) = send(
        &state,
        "PUT",
        "/api/v1/company/skills/brand-voice",
        Some(r#"{"enabled":false}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(toggled["enabled"], false, "{raw}");

    for (id, expected) in [
        ("ceo", "inherited"),
        ("writer", "included"),
        ("hermit", "excluded"),
    ] {
        let agent = agent(&toggled, id);
        assert_eq!(
            agent["state"], expected,
            "the switch decides reach, not scope: {toggled}"
        );
        assert_eq!(
            agent["holds"], false,
            "nothing reaches anybody while the switch is off: {toggled}"
        );
    }

    // And the same on a fresh read, so this is the stored record rather than the
    // handler's own answer.
    let reread = row(&state, "brand-voice").await;
    assert_eq!(agent(&reread, "ceo")["state"], "inherited", "{reread}");
    assert_eq!(agent(&reread, "ceo")["holds"], false, "{reread}");
    assert_eq!(agent(&reread, "hermit")["state"], "excluded", "{reread}");
}

/// A newly authored skill answers with the roster too, so the panel opened on
/// the row the console just folded in has a picker rather than a "cannot say".
#[tokio::test]
async fn authoring_a_skill_answers_with_the_roster() {
    let home = tempfile::tempdir().unwrap();
    let state = state_with_roster(home.path()).await;
    let authored = author_brand_voice(&state).await;

    assert_eq!(
        agent(&authored, "writer")["state"],
        "included",
        "{authored}"
    );
    assert_eq!(agent(&authored, "ceo")["state"], "inherited", "{authored}");
    assert_eq!(agent(&authored, "hermit")["holds"], false, "{authored}");
}

/// A scope narrowed to a slug the company does not have enabled is reported as
/// asked-for and not granted, over the wire.
///
/// `writer` names `brand-voice`, and before it is authored the company does not
/// have it — so `state` says `included` while `holds` says false. Collapsing
/// either into the other is what makes a narrowed teammate look like it holds
/// something it does not.
#[tokio::test]
async fn a_scope_naming_a_skill_the_company_lacks_confers_nothing() {
    let home = tempfile::tempdir().unwrap();
    let state = state_with_roster(home.path()).await;

    let (status, list, raw) = send(&state, "GET", "/api/v1/company/skills", None).await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert!(
        !list
            .as_array()
            .expect("an array")
            .iter()
            .any(|row| row["id"] == "brand-voice"),
        "the fixture's premise: nothing has authored `brand-voice` yet: {raw}"
    );

    author_brand_voice(&state).await;
    send(
        &state,
        "PUT",
        "/api/v1/company/skills/brand-voice",
        Some(r#"{"enabled":false}"#),
    )
    .await;

    let row = row(&state, "brand-voice").await;
    let writer = agent(&row, "writer");
    assert_eq!(writer["state"], "included", "{row}");
    assert_eq!(writer["holds"], false, "{row}");
}

/// The whole roster read, keyed by agent id.
async fn team(state: &AppState) -> std::collections::BTreeMap<String, Value> {
    let (status, list, raw) = send(state, "GET", "/api/v1/company/team", None).await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    list.as_array()
        .expect("an array")
        .iter()
        .map(|row| (row["id"].as_str().expect("an id").to_string(), row.clone()))
        .collect()
}

/// **The pinning test.** The per-skill projection is the inversion of the
/// teammate read over the same record and the same ceiling — asserted route
/// against route, for every skill and every agent.
///
/// Without this the change installs the duplication it exists to remove. Two
/// surfaces deriving a scope independently is how a console comes to advertise
/// a skill the harness never materializes, and the two derivations here live in
/// different modules behind different routes.
///
/// `holds` is checked against the teammate's own `effective`, and `state`
/// against the teammate's own `requested` — the two questions separately, so a
/// projection that answered one for both cannot pass.
#[tokio::test]
async fn the_per_skill_projection_inverts_the_teammate_read_exactly() {
    let home = tempfile::tempdir().unwrap();
    let state = state_with_roster(home.path()).await;
    author_brand_voice(&state).await;
    // One skill off, so the ceiling and the stored scopes disagree somewhere and
    // the comparison has a case that a scope-only or effective-only projection
    // would get wrong.
    let (status, _, raw) = send(
        &state,
        "PUT",
        "/api/v1/company/skills/brand-voice",
        Some(r#"{"enabled":false}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");

    let team = team(&state).await;
    let (status, skills, raw) = send(&state, "GET", "/api/v1/company/skills", None).await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    let skills = skills.as_array().expect("an array");
    assert!(!skills.is_empty(), "{raw}");

    let mut compared = 0usize;
    for row in skills {
        let slug = row["id"].as_str().expect("a slug");
        for agent in row["agents"].as_array().expect("agents") {
            let id = agent["id"].as_str().expect("an id");
            let member = &team[id];
            let scope = &member["skills"];

            let effective: Vec<&str> = scope["effective"]
                .as_array()
                .expect("effective")
                .iter()
                .map(|slug| slug.as_str().expect("a slug"))
                .collect();
            assert_eq!(
                agent["holds"].as_bool().expect("holds"),
                effective.contains(&slug),
                "`{slug}` / `{id}`: the panel and the teammate page disagree about \
                 what is held: {agent} vs {scope}"
            );

            let expected = match scope["requested"].as_array() {
                None => "inherited",
                Some(slugs) if slugs.iter().any(|want| want == slug) => "included",
                Some(_) => "excluded",
            };
            assert_eq!(
                agent["state"], expected,
                "`{slug}` / `{id}`: the projection does not invert the stored scope: \
                 {agent} vs {scope}"
            );
            compared += 1;
        }
    }
    assert!(
        compared >= 3,
        "the fixture has three agents and at least one skill, so this compared \
         nothing: {compared}"
    );
}

/// The roster read keeps `[]` apart from absent, which is where the distinction
/// dies if it dies anywhere.
///
/// `Option<Vec<String>>` with a `skip_serializing_if` on it would send nothing
/// for both, and the panel would then compute `hermit`'s next list from a scope
/// it read as "inherits every skill" — handing it the company's whole ceiling on
/// a save that was about one slug.
#[tokio::test]
async fn the_roster_read_keeps_an_empty_scope_apart_from_an_absent_one() {
    let home = tempfile::tempdir().unwrap();
    let state = state_with_roster(home.path()).await;
    let team = team(&state).await;

    assert!(
        team["ceo"]["skills"]["requested"].is_null(),
        "a teammate that declares no `skills` line inherits: {}",
        team["ceo"]["skills"]
    );
    assert_eq!(
        team["hermit"]["skills"]["requested"],
        serde_json::json!([]),
        "a deliberate empty scope survives as `[]`, not as absent: {}",
        team["hermit"]["skills"]
    );
    assert_eq!(
        team["writer"]["skills"]["requested"],
        serde_json::json!(["brand-voice"]),
        "{}",
        team["writer"]["skills"]
    );

    // And the ceiling is on every row, because the panel's next-list arithmetic
    // needs it to materialize an inherited scope.
    for id in ["ceo", "writer", "hermit"] {
        assert!(
            team[id]["skills"]["companyAvailable"].is_array(),
            "`{id}` carries the ceiling: {}",
            team[id]["skills"]
        );
    }
}
