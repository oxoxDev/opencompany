//! The ledger routes, end to end over the real router.

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::company::CompanyManifest;
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::runtime::RuntimeBuilder;
use crate::server::router;
use crate::store::FsCompanyStore;
use crate::{AppConfig, AppState};

fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("opencompany-ledgers-")
        .tempdir()
        .expect("tempdir")
}

fn manifest() -> CompanyManifest {
    toml::from_str(
        "[company]\nname = \"Acme\"\n[[agent]]\nid = \"ceo\"\nrole = \"Chief\"\n[policy]\nmode = \"full\"\n",
    )
    .unwrap()
}

async fn state() -> (AppState, tempfile::TempDir) {
    use crate::ports::CompanyStore;
    let dir = home();
    let home = dir.path().to_path_buf();
    let store = FsCompanyStore::new(home.clone());
    let id = CompanyId::new("acme");
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
    let runtime = RuntimeBuilder::new(home, manifest())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;
    (state, dir)
}

async fn send(
    state: &AppState,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", crate::server::test_support::fixed_cookie("acme"));
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
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

fn hazards() -> Value {
    json!({
        "slug": "hazards",
        "title": "Hazards",
        "purpose": "What could go wrong.",
        "derived": "derived/hazards.md",
        "fields": [
            { "name": "id", "role": "id" },
            { "name": "risk", "role": "title" },
            { "name": "status", "role": "status" },
            { "name": "reason", "role": "prose" }
        ],
        "statuses": [
            { "name": "open" },
            { "name": "closed", "closed": true, "needs_reason": true }
        ]
    })
}

#[tokio::test]
async fn the_listing_carries_the_built_ins_with_their_shape() {
    let (state, _home) = state().await;
    let (status, body) = send(&state, "GET", "/api/v1/company/ledgers", None).await;
    assert_eq!(status, StatusCode::OK);
    let slugs: Vec<&str> = body["ledgers"]
        .as_array()
        .expect("ledgers")
        .iter()
        .map(|l| l["slug"].as_str().unwrap())
        .collect();
    assert_eq!(
        &slugs[..3],
        ["tasks", "goals", "decisions"],
        "the built-ins list first, then the seeded baseline: {slugs:?}"
    );
    for global in crate::globals::ledgers() {
        assert!(slugs.contains(&global.slug.as_str()), "{slugs:?}");
    }
    // The console needs the shape to render a form; it must not have to guess.
    let goals = &body["ledgers"][1];
    assert!(goals["fields"].as_array().unwrap().len() > 3);
    // Three, on every ledger, since issue #1512 — so this asserts the ceiling
    // rather than a floor it used to assert in the other direction.
    assert_eq!(goals["statuses"].as_array().unwrap().len(), 3);
    assert_eq!(goals["open"], 0);
    assert_eq!(
        body["remaining"],
        crate::ledger::MAX_DECLARED - crate::globals::ledgers().len(),
        "the baseline's own ledgers count against the cap like any other"
    );
    // The board is listed but marked as written elsewhere, so the console knows
    // not to offer a compose box for it.
    assert_eq!(body["ledgers"][0]["source"], "native");
    assert!(
        body["ledgers"][0]["writtenBy"]
            .as_str()
            .unwrap()
            .contains("spawn_task")
    );
}

#[tokio::test]
async fn a_ledger_round_trips_under_both_scope_forms() {
    let (state, _home) = state().await;

    let (status, created) = send(&state, "POST", "/api/v1/company/ledgers", Some(hazards())).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["slug"], "hazards");

    let (status, entry) = send(
        &state,
        "POST",
        "/api/v1/companies/acme/ledgers/hazards/entries",
        Some(json!({
            "id": "vendor-slip",
            "fields": { "risk": "the vendor misses the date" },
            "status": "open"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{entry}");
    assert_eq!(entry["status"], "open");
    assert_eq!(entry["closed"], false);
    assert_eq!(entry["title"], "the vendor misses the date");

    let (status, read) = send(&state, "GET", "/api/v1/company/ledgers/hazards", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read["matched"], 1);
    assert_eq!(read["entries"][0]["id"], "vendor-slip");
}

/// `read_ledger` used to build `ledger.open`/`ledger.closed` from a second,
/// independent fold (inside `summary`) after already folding once for the
/// rows. A write landing between the two could flip a row's status after the
/// first fold but before the second, so the count disagreed with the rows it
/// was displayed beside — an open row on screen while the badge already
/// counted it closed, until the next refresh.
///
/// Racing a read against a status flip, every response's `open`/`closed`
/// must equal what the rows in that same response actually say — not merely
/// most of the time. This does not assert the fix's code shape; it holds the
/// invariant a two-fold response can violate under a real race.
///
/// Needs real OS-thread concurrency to land the write inside the narrow
/// window between the two folds the old handler had: on the default
/// current-thread runtime the pair never actually overlaps and the race
/// never reproduces. Confirmed against the pre-fix handler (reintroducing
/// `summary(&ctx, spec).await?` in place of `summary_from_counts`) that this
/// fails within the first ~15 rounds; against the fix, 300 rounds passed
/// clean across repeated runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn the_read_count_never_disagrees_with_its_own_rows_under_a_racing_write() {
    let (state, _home) = state().await;
    send(&state, "POST", "/api/v1/company/ledgers", Some(hazards())).await;
    send(
        &state,
        "POST",
        "/api/v1/company/ledgers/hazards/entries",
        Some(json!({ "id": "r1", "fields": { "risk": "a" }, "status": "open" })),
    )
    .await;

    for round in 0..80 {
        // Alternate direction so the race is attempted flipping each way.
        let write_body = if round % 2 == 0 {
            json!({ "id": "r1", "status": "closed", "reason": "handled" })
        } else {
            json!({ "id": "r1", "status": "open" })
        };

        let read_state = state.clone();
        let reader = tokio::spawn(async move {
            send(&read_state, "GET", "/api/v1/company/ledgers/hazards", None).await
        });

        let write_state = state.clone();
        let writer = tokio::spawn(async move {
            send(
                &write_state,
                "POST",
                "/api/v1/company/ledgers/hazards/entries",
                Some(write_body),
            )
            .await
        });

        let (read_result, write_result) = tokio::join!(reader, writer);
        let (read_status, read_body) = read_result.unwrap();
        assert_eq!(read_status, StatusCode::OK, "round {round}: {read_body}");
        let (write_status, write_body) = write_result.unwrap();
        assert_eq!(write_status, StatusCode::OK, "round {round}: {write_body}");

        let rows = read_body["entries"].as_array().expect("entries array");
        let open_rows = rows.iter().filter(|row| row["closed"] == false).count();
        let closed_rows = rows.iter().filter(|row| row["closed"] == true).count();
        assert_eq!(
            read_body["ledger"]["open"], open_rows,
            "round {round}: open count disagrees with the rows beside it: {read_body}"
        );
        assert_eq!(
            read_body["ledger"]["closed"], closed_rows,
            "round {round}: closed count disagrees with the rows beside it: {read_body}"
        );
    }
}

/// **The wire regression, pinned.** `StatusSpec::needs_reason` used to
/// serialize snake_case like every other declaration field, but the console
/// reads `LedgerStatus.needsReason` — camelCase, matching `LedgerSummary`'s
/// own `#[serde(rename_all = "camelCase")]`. The mismatch meant
/// `statusNeedsReason()` always read `undefined` and never fired the console's
/// "ask for a reason before closing" guard on any declared ledger (issue
/// #1266). This reads the same field off the same response shape the
/// frontend receives.
#[tokio::test]
async fn a_status_that_needs_a_reason_carries_camel_case_on_the_wire() {
    let (state, _home) = state().await;
    let (status, created) = send(&state, "POST", "/api/v1/company/ledgers", Some(hazards())).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    let closed_status = created["statuses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "closed")
        .expect("the closed status");
    assert_eq!(closed_status["needsReason"], true, "{closed_status}");
    assert!(
        closed_status.get("needs_reason").is_none(),
        "wire response must not also carry the snake_case key: {closed_status}"
    );
}

/// The console renders the same Markdown the workspace holds, served from the
/// derivation rather than by reading the file back — so a workspace write that
/// failed can never show a stale page.
#[tokio::test]
async fn the_rendered_view_is_the_derived_file() {
    let (state, _home) = state().await;
    send(&state, "POST", "/api/v1/company/ledgers", Some(hazards())).await;
    send(
        &state,
        "POST",
        "/api/v1/company/ledgers/hazards/entries",
        Some(json!({ "id": "r1", "fields": { "risk": "a thing" }, "status": "open" })),
    )
    .await;
    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/company/ledgers/hazards/rendered")
        .header("cookie", crate::server::test_support::fixed_cookie("acme"))
        .body(Body::empty())
        .unwrap();
    let response = router(state.clone()).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("# Hazards"), "{text}");
    assert!(text.contains("r1"), "{text}");
    assert!(text.contains("Do not edit this file"), "{text}");
}

/// A signed-in person is the one principal that may delete.
#[tokio::test]
async fn a_signed_in_person_may_delete_a_row() {
    let (state, _home) = state().await;
    send(&state, "POST", "/api/v1/company/ledgers", Some(hazards())).await;
    send(
        &state,
        "POST",
        "/api/v1/company/ledgers/hazards/entries",
        Some(json!({ "id": "r1", "fields": { "risk": "a" } })),
    )
    .await;

    let (status, _) = send(
        &state,
        "DELETE",
        "/api/v1/company/ledgers/hazards/entries/r1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, read) = send(&state, "GET", "/api/v1/company/ledgers/hazards", None).await;
    assert_eq!(read["matched"], 0);

    // Deleting something that is not there is a 404, not a silent success.
    let (status, _) = send(
        &state,
        "DELETE",
        "/api/v1/company/ledgers/hazards/entries/nothing",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_person_may_retire_a_declared_ledger_but_not_a_built_in() {
    let (state, _home) = state().await;
    send(&state, "POST", "/api/v1/company/ledgers", Some(hazards())).await;

    let (status, body) = send(&state, "DELETE", "/api/v1/company/ledgers/goals", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, _) = send(&state, "DELETE", "/api/v1/company/ledgers/hazards", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, list) = send(&state, "GET", "/api/v1/company/ledgers", None).await;
    assert_eq!(
        list["ledgers"].as_array().unwrap().len(),
        3 + crate::globals::ledgers().len(),
        "the three built-ins and the seeded baseline remain"
    );
}

/// Refused at the write. The console's compose box is what turns this into a
/// prompt for the reason rather than a rejected save.
#[tokio::test]
async fn closing_without_a_reason_is_a_400_that_says_so() {
    let (state, _home) = state().await;
    send(&state, "POST", "/api/v1/company/ledgers", Some(hazards())).await;
    let (status, body) = send(
        &state,
        "POST",
        "/api/v1/company/ledgers/hazards/entries",
        Some(json!({ "id": "r1", "status": "closed" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        format!("{body}").contains("reason"),
        "the refusal must say what is missing: {body}"
    );

    let (status, _) = send(
        &state,
        "POST",
        "/api/v1/company/ledgers/hazards/entries",
        Some(json!({ "id": "r1", "status": "closed", "reason": "the vendor delivered" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// The shape the global `learnings` ledger has: a required prose field and the
/// check that reports it.
fn findings() -> Value {
    json!({
        "slug": "findings",
        "title": "Findings",
        "purpose": "What we found out.",
        "derived": "derived/findings.md",
        "fields": [
            { "name": "id", "role": "id", "required": true },
            { "name": "finding", "role": "title", "required": true },
            { "name": "status", "role": "status", "required": true },
            { "name": "evidence", "role": "prose", "required": true },
            { "name": "reason", "role": "prose" }
        ],
        "statuses": [
            { "name": "noted" },
            { "name": "adopted", "closed": true, "needs_reason": true }
        ],
        "checks": ["required-field", "known-status", "closed-needs-reason"]
    })
}

/// The route half of the write/read contract: a row the ledger would report as
/// unreadable is refused with a 400 that names the field, rather than accepted
/// with a 200 and then listed under the read's faults.
#[tokio::test]
async fn a_row_missing_a_required_field_is_a_400_rather_than_a_fault_on_read() {
    let (state, _home) = state().await;
    send(&state, "POST", "/api/v1/company/ledgers", Some(findings())).await;

    let (status, body) = send(
        &state,
        "POST",
        "/api/v1/company/ledgers/findings/entries",
        Some(json!({
            "id": "L-BAD-1",
            "status": "noted",
            "fields": { "finding": "a row with no evidence field" }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        format!("{body}").contains("evidence"),
        "the refusal must name the field: {body}"
    );

    // Refused means nothing landed — so the read has neither the row nor a
    // fault about it, which is the contradiction this closes.
    let (status, body) = send(
        &state,
        "GET",
        "/api/v1/company/ledgers/findings/entries",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["entries"].as_array().map(Vec::len), Some(0), "{body}");
    assert!(body["faults"].is_null(), "{body}");

    let (status, _) = send(
        &state,
        "POST",
        "/api/v1/company/ledgers/findings/entries",
        Some(json!({
            "id": "L-BAD-1",
            "status": "noted",
            "fields": { "finding": "a row with evidence", "evidence": "it happened three times" }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// The board is readable through the surface and not writable by it.
#[tokio::test]
async fn the_board_cannot_be_written_through_the_ledger_routes() {
    let (state, _home) = state().await;
    let (status, body) = send(
        &state,
        "POST",
        "/api/v1/company/ledgers/tasks/entries",
        Some(json!({ "id": "t1", "fields": { "title": "x" } })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(format!("{body}").contains("spawn_task"), "{body}");
}

/// A caller that guessed learns the real names from the failure, in one turn.
#[tokio::test]
async fn an_unknown_slug_answers_with_the_real_ones() {
    let (state, _home) = state().await;
    let (status, body) = send(&state, "GET", "/api/v1/company/ledgers/taks", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let message = format!("{body}");
    assert!(message.contains("tasks"), "{message}");
    assert!(message.contains("goals"), "{message}");
}

#[tokio::test]
async fn a_read_narrows_by_status_and_search() {
    let (state, _home) = state().await;
    send(&state, "POST", "/api/v1/company/ledgers", Some(hazards())).await;
    send(
        &state,
        "POST",
        "/api/v1/company/ledgers/hazards/entries",
        Some(json!({ "id": "vendor", "fields": { "risk": "supplier slips" }, "status": "open" })),
    )
    .await;
    send(
        &state,
        "POST",
        "/api/v1/company/ledgers/hazards/entries",
        Some(json!({ "id": "hiring", "status": "closed", "reason": "role filled" })),
    )
    .await;

    let (_, open) = send(
        &state,
        "GET",
        "/api/v1/company/ledgers/hazards?status=open",
        None,
    )
    .await;
    assert_eq!(open["matched"], 1);
    assert_eq!(open["entries"][0]["id"], "vendor");

    let (_, found) = send(
        &state,
        "GET",
        "/api/v1/company/ledgers/hazards?q=SUPPLIER",
        None,
    )
    .await;
    assert_eq!(found["matched"], 1);

    let (status, _) = send(
        &state,
        "GET",
        "/api/v1/company/ledgers/hazards?sort=newest",
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an unknown sort is refused"
    );
}

/// A declaration that collides is refused with the reason, not silently
/// dropped into a listing the caller then cannot find it in.
#[tokio::test]
async fn a_colliding_declaration_is_refused() {
    let (state, _home) = state().await;
    send(&state, "POST", "/api/v1/company/ledgers", Some(hazards())).await;
    let (status, body) = send(&state, "POST", "/api/v1/company/ledgers", Some(hazards())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(format!("{body}").contains("hazards"), "{body}");

    let mut shadow = hazards();
    shadow["slug"] = json!("tasks");
    let (status, body) = send(&state, "POST", "/api/v1/company/ledgers", Some(shadow)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(format!("{body}").contains("built-in"), "{body}");
}

/// The declaration reaches the workspace, and the workspace refuses to let
/// anybody edit what it wrote.
#[tokio::test]
async fn the_derived_file_appears_in_the_workspace_and_is_read_only() {
    let (state, _home) = state().await;
    send(&state, "POST", "/api/v1/company/ledgers", Some(hazards())).await;

    let (_, tree) = send(&state, "GET", "/api/v1/company/workspace", None).await;
    let nodes = tree.as_array().expect("tree");
    let file = nodes
        .iter()
        .find(|node| node["name"] == "hazards.md")
        .expect("the ledger's file is in the tree");
    let id = file["id"].as_str().unwrap();

    let (status, body) = send(
        &state,
        "PUT",
        &format!("/api/v1/company/workspace/file/{id}"),
        Some(json!({ "content": "my own version" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(format!("{body}").contains("hazards"), "{body}");
}

/// A row's byline names the signed-in person, not their opaque id (issue
/// #1263). `seed_fixed_admin` seeds a user whose id is a generated internal
/// id but whose email is `harness-admin@example.test` — exactly the id/email
/// split the console needs to show something a reader recognizes.
#[tokio::test]
async fn a_row_s_byline_names_the_person_by_email_not_id() {
    let (state, _home) = state().await;
    send(&state, "POST", "/api/v1/company/ledgers", Some(hazards())).await;

    let (status, entry) = send(
        &state,
        "POST",
        "/api/v1/company/ledgers/hazards/entries",
        Some(json!({
            "id": "vendor-slip",
            "fields": { "risk": "the vendor misses the date" },
            "status": "open"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{entry}");
    assert_eq!(entry["updatedBy"]["kind"], "human");
    assert_eq!(entry["updatedBy"]["label"], "harness-admin@example.test");
    let id = entry["updatedBy"]["id"]
        .as_str()
        .expect("updatedBy carries an id");
    assert!(!id.is_empty());
    assert_ne!(
        id, "harness-admin@example.test",
        "the id and the label must no longer be the same opaque value"
    );
    assert_eq!(entry["openedBy"]["label"], "harness-admin@example.test");

    let (status, read) = send(&state, "GET", "/api/v1/company/ledgers/hazards", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        read["entries"][0]["updatedBy"]["label"],
        "harness-admin@example.test"
    );
}
