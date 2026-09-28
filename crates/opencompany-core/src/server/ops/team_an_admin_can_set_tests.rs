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

/// `PUT …/team/{id}/budget` as the seeded admin.
async fn put_budget(state: &AppState, agent: &str, body: Value) -> (StatusCode, Value) {
    send(
        state,
        "PUT",
        &format!("/api/v1/company/team/{agent}/budget"),
        Some(body),
        Some(&admin_cookie()),
    )
    .await
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

/// The acceptance criterion, on the wire: an admin sets, changes and clears
/// a cap, and every state is visible on the next read.
///
/// The `writer` starts **uncapped in the manifest**, which is the case the
/// pre-#343 code could not express at all — there was no field to write.
#[tokio::test]
async fn an_admin_can_set_change_and_clear_a_cap() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), ROSTER).await;

    // Uncapped to begin with — no cap key, no attribution.
    let before = team_row(&state, "writer").await;
    assert!(before.get("budgetUsdDaily").is_none(), "{before}");
    assert!(before.get("budgetSetBy").is_none(), "{before}");

    // Set.
    let (status, row) = put_budget(&state, "writer", json!({"budgetUsdDaily": 12.5})).await;
    assert_eq!(status, StatusCode::OK, "{row}");
    assert_eq!(row["budgetUsdDaily"], 12.5, "{row}");
    let after_set = team_row(&state, "writer").await;
    assert_eq!(after_set["budgetUsdDaily"], 12.5, "{after_set}");
    assert!(
        after_set["budgetSetBy"].is_string(),
        "a set cap is attributable to the admin who set it: {after_set}"
    );
    assert!(
        after_set["budgetSetAtMillis"].as_u64().unwrap() > 0,
        "{after_set}"
    );

    // Change.
    let (status, _) = put_budget(&state, "writer", json!({"budgetUsdDaily": 3.0})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(team_row(&state, "writer").await["budgetUsdDaily"], 3.0);

    // Remove the cap (explicit null).
    let (status, row) = put_budget(&state, "writer", json!({"budgetUsdDaily": null})).await;
    assert_eq!(status, StatusCode::OK, "{row}");
    let uncapped = team_row(&state, "writer").await;
    assert!(
        uncapped.get("budgetUsdDaily").is_none(),
        "an uncapped teammate omits the cap key entirely: {uncapped}"
    );
    assert!(
        uncapped["budgetSetBy"].is_string(),
        "…but the attribution stays, so an operator can see that a human \
         uncapped this teammate rather than that nobody ever capped it: {uncapped}"
    );
}

/// A cap set from the console **wins over the manifest**, and `DELETE`
/// puts the manifest back. This is the pair that makes the override a
/// remedy rather than a second opinion.
#[tokio::test]
async fn an_override_beats_the_manifest_and_delete_restores_it() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), ROSTER).await;

    assert_eq!(team_row(&state, "analyst").await["budgetUsdDaily"], 5.0);

    let (status, _) = put_budget(&state, "analyst", json!({"budgetUsdDaily": 50.0})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        team_row(&state, "analyst").await["budgetUsdDaily"],
        50.0,
        "the stored cap wins over the manifest's $5"
    );

    let (status, row) = send(
        &state,
        "DELETE",
        "/api/v1/company/team/analyst/budget",
        None,
        Some(&admin_cookie()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{row}");
    let reset = team_row(&state, "analyst").await;
    assert_eq!(
        reset["budgetUsdDaily"], 5.0,
        "DELETE drops the override, so the manifest default applies again: {reset}"
    );
    assert!(
        reset.get("budgetSetBy").is_none(),
        "with no override there is nothing to attribute: {reset}"
    );
}

/// The issue's third rule, pinned **on the wire** rather than in Rust: `0`
/// and `null` are different bodies with different stored outcomes.
///
/// `0` caps the teammate at nothing (the cap key comes back as `0.0`);
/// `null` removes the cap (the key is absent). If these ever collapsed, an
/// operator lifting a cap would instead have silenced the teammate.
#[tokio::test]
async fn zero_and_null_are_different_states() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), ROSTER).await;

    let (status, _) = put_budget(&state, "analyst", json!({"budgetUsdDaily": 0})).await;
    assert_eq!(status, StatusCode::OK);
    let zeroed = team_row(&state, "analyst").await;
    assert_eq!(
        zeroed["budgetUsdDaily"], 0.0,
        "a zero cap is sent as 0, not omitted: {zeroed}"
    );

    let (status, _) = put_budget(&state, "analyst", json!({"budgetUsdDaily": null})).await;
    assert_eq!(status, StatusCode::OK);
    let cleared = team_row(&state, "analyst").await;
    assert!(
        cleared.get("budgetUsdDaily").is_none(),
        "a cleared cap omits the key — and beats the manifest's $5: {cleared}"
    );
}

/// An omitted key is **not** an uncap. `{}` cannot be mistaken for
/// `{"budgetUsdDaily": null}`, so a client bug or a truncated body can never
/// silently lift a cap; axum rejects the body before the handler runs.
#[tokio::test]
async fn an_absent_key_is_rejected_rather_than_read_as_uncapped() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), ROSTER).await;

    let (status, _) = put_budget(&state, "analyst", json!({})).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an empty body must never be read as 'remove the cap'"
    );
    assert_eq!(
        team_row(&state, "analyst").await["budgetUsdDaily"],
        5.0,
        "and nothing was written"
    );
}

/// A cap has to be a real, non-negative number of dollars — the same rule
/// the manifest validator applies, so the console cannot store a value
/// `company.toml` would have rejected.
#[tokio::test]
async fn a_nonsensical_cap_is_refused() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), ROSTER).await;

    let (status, body) = put_budget(&state, "analyst", json!({"budgetUsdDaily": -1.0})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // NaN and ∞ have no JSON literal, so they arrive as raw tokens. Either
    // outcome is a refusal; what must never happen is one being stored,
    // because `spent >= NaN` is false and the cap would enforce nothing
    // while the console rendered it as set.
    for raw in ["{\"budgetUsdDaily\": NaN}", "{\"budgetUsdDaily\": 1e400}"] {
        let request = Request::builder()
            .method("PUT")
            .uri("/api/v1/company/team/analyst/budget")
            .header("cookie", admin_cookie())
            .header("content-type", "application/json")
            .body(Body::from(raw))
            .unwrap();
        let status = router(state.clone())
            .oneshot(request)
            .await
            .unwrap()
            .status();
        assert!(status.is_client_error(), "{raw} → {status}");
    }

    assert_eq!(
        team_row(&state, "analyst").await["budgetUsdDaily"],
        5.0,
        "no refused write left anything behind"
    );
}

/// An unknown teammate 404s rather than storing an override nothing reads.
#[tokio::test]
async fn an_unknown_teammate_is_not_found() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), ROSTER).await;

    let (status, _) = put_budget(&state, "nobody", json!({"budgetUsdDaily": 1.0})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = send(
        &state,
        "DELETE",
        "/api/v1/company/team/nobody/budget",
        None,
        Some(&admin_cookie()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The privilege boundary: a signed-in **member** cannot change a cap, and
/// an unauthenticated caller cannot reach the route at all.
///
/// "A cap that can be raised silently is not much of a cap" — so this is the
/// assertion that makes the enforcement worth having. It is checked on the
/// backend, never on the console's hidden buttons.
#[tokio::test]
async fn a_non_admin_cannot_change_a_cap() {
    use crate::ports::UserRole;

    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), ROSTER).await;
    let member = crate::server::test_support::seed_session(&state, "acme", UserRole::Member).await;

    for (method, body) in [
        ("PUT", Some(json!({"budgetUsdDaily": 999.0}))),
        ("DELETE", None),
    ] {
        let (status, _) = send(
            &state,
            method,
            "/api/v1/company/team/analyst/budget",
            body.clone(),
            Some(&member),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{method} as a member must be refused"
        );

        let (status, _) = send(
            &state,
            method,
            "/api/v1/company/team/analyst/budget",
            body,
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} with no session must be refused"
        );
    }

    assert_eq!(
        team_row(&state, "analyst").await["budgetUsdDaily"],
        5.0,
        "the manifest cap is untouched"
    );
}

/// A teammate created through the console can be given a cap at creation —
/// and that is admin-only, while a budget-less add stays open to any member
/// exactly as it was before #343.
#[tokio::test]
async fn a_new_teammate_can_be_created_with_a_cap() {
    use crate::ports::UserRole;

    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), ROSTER).await;
    let member = crate::server::test_support::seed_session(&state, "acme", UserRole::Member).await;

    // A member may still add a teammate — no permission was taken away.
    let (status, plain) = send(
        &state,
        "POST",
        "/api/v1/company/team",
        Some(json!({"name": "Jamie", "role": "Growth"})),
        Some(&member),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plain}");
    assert!(plain.get("budgetUsdDaily").is_none(), "{plain}");

    // …but not with a budget attached.
    let (status, _) = send(
        &state,
        "POST",
        "/api/v1/company/team",
        Some(json!({"name": "Sam", "role": "Ops", "budgetUsdDaily": 4.0})),
        Some(&member),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "setting a cap is admin-only wherever it happens"
    );

    // An admin can.
    let (status, created) = send(
        &state,
        "POST",
        "/api/v1/company/team",
        Some(json!({"name": "Sam", "role": "Ops", "budgetUsdDaily": 4.0})),
        Some(&admin_cookie()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["budgetUsdDaily"], 4.0, "{created}");
    let sam = created["id"].as_str().unwrap().to_string();
    let row = team_row(&state, &sam).await;
    assert_eq!(
        row["budgetUsdDaily"], 4.0,
        "the cap and the teammate landed in one save: {row}"
    );
    assert!(row["budgetSetBy"].is_string(), "{row}");
}

/// Issue #1989: `POST {scope}/team` refuses a blank name or role, which it
/// used to store.
///
/// This was the only write path in the repository that did not. `PATCH
/// {scope}/team/{agent_id}`, the orchestrator's `add_agent`, `company.toml`
/// and `agents/<id>.toml` all refuse one, so a teammate with an empty role
/// was unreachable by every route except this one — and reachable by this
/// one with a plain `200`.
///
/// Asserted through the wire and then read back off the roster, because the
/// failure this closes is a *stored* record: a blank role interpolates into
/// `persona_prompt` unguarded ("You are Dana, the  at Acme.") and renders as
/// `id — ` in the orchestrator's Team block, neither of which errors and
/// neither of which anyone is told about.
#[tokio::test]
async fn add_member_refuses_a_blank_name_or_role() {
    use crate::ports::UserRole;
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), ROSTER).await;
    let member = crate::server::test_support::seed_session(&state, "acme", UserRole::Member).await;

    // Whitespace as well as empty: `"   "` is what a form sends when
    // somebody tabs through a field, and it stores just as blank.
    for body in [
        json!({"name": "", "role": "Growth"}),
        json!({"name": "   ", "role": "Growth"}),
        json!({"name": "Jamie", "role": ""}),
        json!({"name": "Jamie", "role": "  \t "}),
    ] {
        let (status, answer) = send(
            &state,
            "POST",
            "/api/v1/company/team",
            Some(body.clone()),
            Some(&member),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{body} must be refused, not stored: {answer}"
        );
        assert!(
            answer.to_string().contains("can't be empty"),
            "and refused in the same words `PATCH` uses: {answer}"
        );
    }

    // Nothing landed. The roster is still exactly the manifest's.
    let (status, roster) = send(
        &state,
        "GET",
        "/api/v1/company/team",
        None,
        Some(&admin_cookie()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{roster}");
    assert!(
        roster.as_array().unwrap().iter().all(|row| !row["role"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .is_empty()),
        "no teammate may exist with a blank role: {roster}"
    );

    // And the surrounding whitespace is trimmed off a good one rather than
    // stored, so `" Jamie "` and `"Jamie"` are not two different teammates.
    let (status, created) = send(
        &state,
        "POST",
        "/api/v1/company/team",
        Some(json!({"name": "  Jamie  ", "role": "  Growth  "})),
        Some(&member),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["name"], "Jamie", "{created}");
    assert_eq!(created["role"], "Growth", "{created}");
}

/// Issue #1530: a teammate can be born with a persona override — the
/// create-time path writes it in the same save as the teammate, and the
/// agent detail reads it back as the effective instructions. Takes no
/// permission: any member may add a teammate with instructions.
#[tokio::test]
async fn add_member_with_instructions_persists_the_override() {
    use crate::ports::UserRole;
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), ROSTER).await;
    let member = crate::server::test_support::seed_session(&state, "acme", UserRole::Member).await;

    let (status, created) = send(
        &state,
        "POST",
        "/api/v1/company/team",
        Some(json!({
            "name": "Jamie",
            "role": "Growth",
            "instructions": "Be terse and data-first."
        })),
        Some(&member),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a member may add with instructions: {created}"
    );
    let jamie = created["id"].as_str().unwrap().to_string();

    // Read the detail back, so this is the stored override rather than the
    // handler's own answer.
    let (status, detail) = send(
        &state,
        "GET",
        &format!("/api/v1/company/team/{jamie}"),
        None,
        Some(&admin_cookie()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(
        detail["instructions"], "Be terse and data-first.",
        "{detail}"
    );
    assert_eq!(detail["instructionsOverridden"], true, "{detail}");
}

/// Issue #1674: a setup-created teammate carries its job shape (`focus`) so
/// it is created with the belt that shape was approved with on the review
/// screen, rather than inheriting the whole company default. `research` is
/// the read-only shape: its effective grants hold no `workspace.write`, and
/// a focus-less add still gets the standard company-wide grant.
#[tokio::test]
async fn a_teammate_created_with_a_focus_is_scoped_to_that_focus_belt() {
    use crate::ports::UserRole;
    let home_dir = home();
    let state = state_with_manifest(
        home_dir.path(),
        "[company]\nname = \"Acme\"\n[tools]\n\
         allow = [\"workspace.read\", \"workspace.write\", \"docs.*\", \
         \"files.*\", \"web.*\", \"search\", \"mcp:*\"]\n",
    )
    .await;
    let member = crate::server::test_support::seed_session(&state, "acme", UserRole::Member).await;

    // A Research teammate: reads the workspace and browses, but has no
    // business writing the company's own guidance tree.
    let (status, created) = send(
        &state,
        "POST",
        "/api/v1/company/team",
        Some(json!({
            "name": "Jamie",
            "role": "Researcher",
            "focus": "research",
        })),
        Some(&member),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let jamie = created["id"].as_str().unwrap().to_string();
    let row = team_row(&state, &jamie).await;
    let grants = |field: &str| {
        row["tools"][field]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
            .unwrap_or_default()
    };
    let effective = grants("effective");
    assert!(
        effective.contains(&"workspace.read"),
        "research reads the workspace: {effective:?}"
    );
    assert!(
        !effective.contains(&"workspace.write"),
        "research must not write the workspace it reports on: {effective:?}"
    );
    let requested = grants("requested");
    assert!(
        !requested.contains(&"workspace.write"),
        "the stored belt is the research belt, not the company grant: {requested:?}"
    );

    // A focus-less add keeps the standard company-wide grant — the field
    // takes no permission away from the generic add path.
    let (status, created) = send(
        &state,
        "POST",
        "/api/v1/company/team",
        Some(json!({"name": "Sam", "role": "Generalist"})),
        Some(&member),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let sam = created["id"].as_str().unwrap().to_string();
    let row = team_row(&state, &sam).await;
    let effective = row["tools"]["effective"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
        .unwrap_or_default();
    assert!(
        effective.contains(&"workspace.write"),
        "a focus-less add still inherits the company grant: {effective:?}"
    );
}
