use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::company::CompanyManifest;
use crate::ports::CompanyStore;
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::runtime::RuntimeBuilder;
use crate::server::router;
use crate::store::FsCompanyStore;
use crate::{AppConfig, AppState};

fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("oc-team-")
        .tempdir()
        .expect("tempdir")
}

async fn state_with_manifest(home: &std::path::Path, manifest_toml: &str) -> AppState {
    state_with(home, toml::from_str(manifest_toml).unwrap()).await
}

/// As above, but with the **global baseline merged in** — the roster every
/// company actually boots with (`docs/spec/runtime/globals.md`).
///
/// Kept apart from `state_with_manifest` on purpose: most tests here are
/// about one hand-written teammate and are clearer without four extra rows,
/// while the provenance tests are meaningless without them.
async fn state_with_globals(home: &std::path::Path, manifest_toml: &str) -> AppState {
    let mut manifest: CompanyManifest = toml::from_str(manifest_toml).unwrap();
    manifest.apply_globals();
    state_with(home, manifest).await
}

async fn state_with(home: &std::path::Path, manifest: CompanyManifest) -> AppState {
    let store = FsCompanyStore::new(home.to_path_buf());
    let id = CompanyId::new("acme");
    store
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_desk_hive: Vec::new(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            id: id.clone(),
            manifest: manifest.clone(),
            ledger: Vec::new(),
            lifecycle: "running".to_string(),
            overlay_agents: Vec::new(),
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
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;
    state
}

async fn get_team(state: &AppState) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/company/team")
        .header("cookie", crate::server::test_support::fixed_cookie("acme"))
        .body(Body::empty())
        .unwrap();
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

/// Two teammates on one manifest: `analyst` is capped, `writer` is not.
const ROSTER: &str = "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
     [[agent]]\nid = \"analyst\"\nrole = \"Analyst\"\nbudget_usd_daily = 5.0\n\
     [[agent]]\nid = \"writer\"\nrole = \"Writer\"\n";

/// Drives any team route with an explicit cookie, so the auth boundary can
/// be exercised with an admin session, a member session, or none at all.
async fn send(
    state: &AppState,
    method: &str,
    uri: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    let request = match &body {
        Some(value) => builder
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(value).unwrap()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

fn admin_cookie() -> String {
    crate::server::test_support::fixed_cookie("acme")
}

/// One roster row from `GET …/team`.
async fn team_row(state: &AppState, agent: &str) -> Value {
    let (status, body) = get_team(state).await;
    assert_eq!(status, StatusCode::OK);
    body.as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == agent)
        .unwrap_or_else(|| panic!("no {agent} row in {body}"))
        .clone()
}

// --- Console budget writes (issue #343) ---------------------------------

/// A company that caps nobody renders exactly as it did before #304 — and
/// the meter is never consulted for it.
#[tokio::test]
async fn an_uncapped_company_is_unchanged() {
    let home_dir = home();
    let state = state_with_manifest(
        home_dir.path(),
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
         [[agent]]\nid = \"writer\"\nrole = \"Writer\"\n",
    )
    .await;

    let (status, body) = get_team(&state).await;
    assert_eq!(status, StatusCode::OK);
    let writer = &body.as_array().unwrap()[0];
    assert_eq!(writer["id"], "writer");
    assert!(
        writer.get("budgetUsdDaily").is_none() && writer.get("spentTodayUsd").is_none(),
        "{writer}"
    );
}

// --- Declared tier on the roster list (issue #643) -----------------------

/// A roster whose three teammates each answer the tier question
/// differently: `ceo` is tagged as the orchestrator, `writer` declares a
/// *non*-orchestrator tier, and `intern` declares nothing at all.
const TIERED_ROSTER: &str = "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
     [[agent]]\nid = \"ceo\"\nrole = \"Chief Executive\"\ntier = \"orchestrator\"\n\
     [[agent]]\nid = \"writer\"\nrole = \"Writer\"\ntier = \"reasoning\"\n\
     [[agent]]\nid = \"intern\"\nrole = \"Intern\"\n";

/// One agent from `GET …/team/{id}` — the detail read, for cross-checking
/// that the list has not grown a second opinion.
async fn agent_detail_row(state: &AppState, agent: &str) -> Value {
    let (status, body) = send(
        state,
        "GET",
        &format!("/api/v1/company/team/{agent}"),
        None,
        Some(&admin_cookie()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// Issue #643 — the declared tier reaches the roster list verbatim.
///
/// The list carried no tier at all, so the overview graph (built from this
/// read) stamped a literal `worker` on every node: a company declaring
/// `tier = "orchestrator"` read back as a worker on its own graph.
///
/// The **undeclared** teammate is the half that keeps the fix honest. Its
/// row must omit the key entirely — not `"worker"`, not `null` — because
/// absence is the only wire shape that says "this company declares no tier
/// here" rather than asserting one on its behalf.
#[tokio::test]
async fn the_roster_list_carries_each_declared_tier_verbatim() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), TIERED_ROSTER).await;

    let ceo = team_row(&state, "ceo").await;
    assert_eq!(ceo["tier"], "orchestrator", "{ceo}");
    assert_eq!(ceo["isOrchestrator"], true, "{ceo}");

    // A declared tier that is not the orchestrator tier: carried verbatim,
    // and it does not make the teammate the orchestrator.
    let writer = team_row(&state, "writer").await;
    assert_eq!(writer["tier"], "reasoning", "{writer}");
    assert_eq!(
        writer["isOrchestrator"], false,
        "a declared tier is a hint, not the roster rule: {writer}"
    );

    // The negative control: undeclared means no key.
    let intern = team_row(&state, "intern").await;
    assert!(
        intern.get("tier").is_none(),
        "an undeclared tier omits the key — a defaulted \"worker\" here is \
         indistinguishable from a declaration and is the whole of #643: {intern}"
    );
    assert_eq!(intern["isOrchestrator"], false, "{intern}");

    // No row anywhere invents the literal the graph used to print.
    let (_, all) = get_team(&state).await;
    for row in all.as_array().unwrap() {
        assert_ne!(row["tier"], "worker", "nobody declared \"worker\": {row}");
    }

    // And the list agrees with the detail read, which is the property the
    // shared helpers exist to make unrepresentable rather than merely true.
    for id in ["ceo", "writer", "intern"] {
        let (list, detail) = (
            team_row(&state, id).await,
            agent_detail_row(&state, id).await,
        );
        assert_eq!(
            list.get("tier"),
            detail.get("tier"),
            "{id}: {list} {detail}"
        );
        assert_eq!(
            list["isOrchestrator"], detail["isOrchestrator"],
            "{id}: {list} {detail}"
        );
    }
}

/// A company that tags nobody still has an orchestrator: the first declared
/// agent, by the same roster rule the harness resolves with.
///
/// This is the case a console that re-derived the marker from the tier
/// string would get wrong — and get wrong invisibly, since an untagged CEO
/// draws as an ordinary worker rather than as an error.
#[tokio::test]
async fn an_untagged_roster_still_names_an_orchestrator_on_the_list() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), ROSTER).await;

    let analyst = team_row(&state, "analyst").await;
    assert_eq!(
        analyst["isOrchestrator"], true,
        "the first declared agent is the orchestrator when nobody is tagged: {analyst}"
    );
    assert!(
        analyst.get("tier").is_none(),
        "…and it says so without inventing a tier for them: {analyst}"
    );

    // The negative control: exactly one, and it is the first.
    let writer = team_row(&state, "writer").await;
    assert_eq!(writer["isOrchestrator"], false, "{writer}");
}

/// An overlay teammate has no manifest row, so it declares no tier and the
/// roster rule never picks it — even on a company whose manifest roster is
/// empty, where "the first declared agent" names nobody at all.
#[tokio::test]
async fn an_overlay_teammate_declares_no_tier_and_is_not_the_orchestrator() {
    let home_dir = home();
    let state = state_with_manifest(
        home_dir.path(),
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n",
    )
    .await;

    let (status, created) = send(
        &state,
        "POST",
        "/api/v1/company/team",
        Some(json!({"name": "Nova", "role": "Researcher"})),
        Some(&admin_cookie()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert!(
        created.get("tier").is_none() && created["isOrchestrator"] == false,
        "the create response answers both the same way the reads do: {created}"
    );

    let id = created["id"].as_str().unwrap().to_string();
    let row = team_row(&state, &id).await;
    assert!(
        row.get("tier").is_none(),
        "an overlay teammate has no `[[agent]]` row to declare a tier: {row}"
    );
    assert_eq!(
        row["isOrchestrator"], false,
        "an empty manifest roster names nobody, so it does not fall through \
         to the overlay half: {row}"
    );
}

// --- Baseline provenance, and the first-run gate (issue #1404) ----------

/// The roster says which of its rows came from the global baseline.
///
/// This is the field the console's first-run gate turns on. `apply_globals`
/// appends `companies/_globals/agents/*.toml` to **every** company whatever its
/// manifest says, so a company nobody has ever staffed still answers this
/// route with a non-empty list — and "is the roster empty?" therefore
/// answered `no` everywhere, which is what made first-run setup unreachable
/// in the shipped product.
///
/// Asserted as "at least one row, all of them global" rather than against a
/// count or the four current ids: the baseline is meant to grow, and a test
/// that pins its contents here would fail for the wrong reason.
#[tokio::test]
async fn a_company_with_no_declared_roster_answers_with_the_baseline_only() {
    let home_dir = home();
    let state = state_with_globals(
        home_dir.path(),
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n",
    )
    .await;

    let (status, body) = get_team(&state).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows = body.as_array().unwrap();
    assert!(
        !rows.is_empty(),
        "the baseline is merged into every company, so this is never empty: {body}"
    );
    assert!(
        rows.iter().all(|row| row["global"] == true),
        "a company declaring no `[[agent]]` has nothing but baseline \
         teammates, and every one of them must say so: {body}"
    );
}

/// A teammate the company wrote, and one the operator adds, are both
/// `global: false` — beside a baseline that is `true` on the same read.
///
/// Both halves are the point. The gate must stay shut for a company that
/// shipped with a roster (`docs/spec/runtime/company-setup.md`), and it must
/// close the moment setup creates the first teammate.
#[tokio::test]
async fn a_declared_or_operator_added_teammate_is_never_marked_global() {
    let home_dir = home();
    let state = state_with_globals(home_dir.path(), ROSTER).await;

    let declared = team_row(&state, "analyst").await;
    assert_eq!(
        declared["global"], false,
        "a `[[agent]]` the company wrote is the company's own: {declared}"
    );

    let (status, created) = send(
        &state,
        "POST",
        "/api/v1/company/team",
        Some(json!({"name": "Nova", "role": "Researcher"})),
        Some(&admin_cookie()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(
        created["global"], false,
        "the create response answers the same way the read does: {created}"
    );
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(team_row(&state, &id).await["global"], false);

    // …and the baseline on the same roster still says otherwise, so the two
    // are distinguishable rather than uniformly false.
    let (_, body) = get_team(&state).await;
    assert!(
        body.as_array()
            .unwrap()
            .iter()
            .any(|row| row["global"] == true),
        "the baseline rows are on this roster too: {body}"
    );
}

/// A baseline teammate — a blueprint row like any other, merged into every
/// company — can be deleted, and stays deleted across a reload. It is a
/// tombstone rather than a manifest rewrite, so this is the assertion that
/// says the blueprint being re-read on every load does not resurrect it.
#[tokio::test]
async fn a_baseline_teammate_can_be_deleted_and_stays_deleted() {
    let home_dir = home();
    let state = state_with_globals(
        home_dir.path(),
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n",
    )
    .await;

    let (_, before) = get_team(&state).await;
    let before = before.as_array().unwrap().clone();
    assert!(
        before.len() > 1,
        "the baseline seeds more than one teammate: {before:?}"
    );
    let id = before[0]["id"].as_str().unwrap().to_string();

    let (status, _) = send(
        &state,
        "DELETE",
        &format!("/api/v1/company/team/{id}"),
        None,
        Some(&admin_cookie()),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, after) = get_team(&state).await;
    let after = after.as_array().unwrap();
    assert_eq!(after.len(), before.len() - 1, "{after:?}");
    assert!(
        !after.iter().any(|row| row["id"] == id.as_str()),
        "the blueprint still declares it, so a re-read must not bring it \
         back: {after:?}"
    );
}

/// The one refusal the roster keeps: a company must not be left with nobody
/// on it. Without this the console could empty the roster entirely, which
/// has no orchestrator, nobody to answer a message, and no way back.
#[tokio::test]
async fn the_last_teammate_cannot_be_deleted() {
    let home_dir = home();
    let state = state_with_globals(
        home_dir.path(),
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n",
    )
    .await;

    // Delete every teammate but one, which must succeed all the way down.
    let (_, body) = get_team(&state).await;
    let ids: Vec<String> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_string())
        .collect();
    for id in &ids[..ids.len() - 1] {
        let (status, _) = send(
            &state,
            "DELETE",
            &format!("/api/v1/company/team/{id}"),
            None,
            Some(&admin_cookie()),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "removing {id}");
    }

    let last = ids.last().unwrap();
    let (status, refusal) = send(
        &state,
        "DELETE",
        &format!("/api/v1/company/team/{last}"),
        None,
        Some(&admin_cookie()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refusal:?}");

    let (_, after) = get_team(&state).await;
    assert_eq!(after.as_array().unwrap().len(), 1, "{after}");
}
