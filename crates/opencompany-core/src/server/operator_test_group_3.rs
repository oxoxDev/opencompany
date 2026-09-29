use super::*;
use crate::company::CompanyManifest;
use crate::ports::types::EventSeq;
use crate::server::router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use super::operator_test_support_1::*;
use super::operator_test_support_2::*;

/// `add_desk_member` must serialize its load-modify-save cycle against
/// `company_write_lock`, exactly like every other console load-modify-save
/// write (`put_logo`, `set_lifecycle`, `patch_company`) — otherwise it can
/// silently revert a concurrent rename: `patch_company` is guarded by
/// `company_write_lock` alone, so a desk write racing in on only the
/// unrelated `serial` cycle lock can load the pre-rename record and save
/// the whole thing back after the rename lands (PR #1875 review finding).
/// Proven the same way `put_logo_serializes_against_the_company_write_lock`
/// proves it: hold the lock externally, drive the real handler through the
/// router, and demand it cannot finish while the lock is held.
#[tokio::test]
async fn add_desk_member_serializes_against_the_company_write_lock() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(&home, desk_manifest()).await;
    let app = router(state);
    let cookie = crate::server::test_support::fixed_cookie("acme");
    let id = CompanyId::new("acme");

    let lock = company_write_lock(&id);
    let guard = lock.lock().await;

    let app_for_task = app.clone();
    let cookie_for_task = cookie.clone();
    let mut task = tokio::spawn(async move {
        app_for_task
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/company/desks/studio/members")
                    .header("cookie", &cookie_for_task)
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"agent_id":"eng"}"#))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    });

    // The handler must be blocked behind the held lock — give it every
    // chance to (wrongly) race ahead before declaring it stuck.
    let raced_ahead = tokio::time::timeout(std::time::Duration::from_millis(200), &mut task)
        .await
        .is_ok();
    assert!(
        !raced_ahead,
        "add_desk_member completed while company_write_lock was held \
         elsewhere — it is not serializing its load-modify-save cycle \
         against concurrent `ops` writers"
    );

    drop(guard);
    let status = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .expect("add_desk_member never resumed after the lock was released")
        .expect("add_desk_member task panicked");
    assert_eq!(status, StatusCode::NO_CONTENT);
}

/// `set_desk_order` must serialize against `company_write_lock` too — same
/// load-modify-save shape and same finding as `add_desk_member`'s own test
/// above (PR #1875 review finding, round 9: the earlier fix covered five
/// handlers but this coverage only proved it for one).
#[tokio::test]
async fn set_desk_order_serializes_against_the_company_write_lock() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(&home, desk_manifest()).await;
    let app = router(state);
    let cookie = crate::server::test_support::fixed_cookie("acme");
    let id = CompanyId::new("acme");
    seed_overlay_eng(&app, &cookie).await;

    let lock = company_write_lock(&id);
    let guard = lock.lock().await;

    let app_for_task = app.clone();
    let cookie_for_task = cookie.clone();
    let mut task = tokio::spawn(async move {
        put_desk_order(
            &app_for_task,
            &cookie_for_task,
            "studio",
            r#"{"ordered_member_ids":["eng","ceo"]}"#,
        )
        .await
    });

    let raced_ahead = tokio::time::timeout(std::time::Duration::from_millis(200), &mut task)
        .await
        .is_ok();
    assert!(
        !raced_ahead,
        "set_desk_order completed while company_write_lock was held \
         elsewhere — it is not serializing its load-modify-save cycle \
         against concurrent `ops` writers"
    );

    drop(guard);
    let status = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .expect("set_desk_order never resumed after the lock was released")
        .expect("set_desk_order task panicked");
    assert_eq!(status, StatusCode::NO_CONTENT);
}

/// `remove_desk_member` must serialize against `company_write_lock` too
/// (PR #1875 review finding, round 9 — see
/// `set_desk_order_serializes_against_the_company_write_lock`'s own doc).
#[tokio::test]
async fn remove_desk_member_serializes_against_the_company_write_lock() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(&home, desk_manifest()).await;
    let app = router(state);
    let cookie = crate::server::test_support::fixed_cookie("acme");
    let id = CompanyId::new("acme");
    seed_overlay_eng(&app, &cookie).await;

    let lock = company_write_lock(&id);
    let guard = lock.lock().await;

    let app_for_task = app.clone();
    let cookie_for_task = cookie.clone();
    let mut task = tokio::spawn(async move {
        app_for_task
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/v1/company/desks/studio/members/eng")
                    .header("cookie", &cookie_for_task)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    });

    let raced_ahead = tokio::time::timeout(std::time::Duration::from_millis(200), &mut task)
        .await
        .is_ok();
    assert!(
        !raced_ahead,
        "remove_desk_member completed while company_write_lock was held \
         elsewhere — it is not serializing its load-modify-save cycle \
         against concurrent `ops` writers"
    );

    drop(guard);
    let status = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .expect("remove_desk_member never resumed after the lock was released")
        .expect("remove_desk_member task panicked");
    assert_eq!(status, StatusCode::NO_CONTENT);
}

/// `create_desk` must serialize against `company_write_lock` too (PR #1875
/// review finding, round 9 — see
/// `set_desk_order_serializes_against_the_company_write_lock`'s own doc).
#[tokio::test]
async fn create_desk_serializes_against_the_company_write_lock() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(&home, desk_manifest()).await;
    let app = router(state);
    let cookie = crate::server::test_support::fixed_cookie("acme");
    let id = CompanyId::new("acme");

    let lock = company_write_lock(&id);
    let guard = lock.lock().await;

    let app_for_task = app.clone();
    let cookie_for_task = cookie.clone();
    let mut task = tokio::spawn(async move {
        app_for_task
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/company/desks")
                    .header("cookie", &cookie_for_task)
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"Growth","members":["eng"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    });

    let raced_ahead = tokio::time::timeout(std::time::Duration::from_millis(200), &mut task)
        .await
        .is_ok();
    assert!(
        !raced_ahead,
        "create_desk completed while company_write_lock was held \
         elsewhere — it is not serializing its load-modify-save cycle \
         against concurrent `ops` writers"
    );

    drop(guard);
    let status = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .expect("create_desk never resumed after the lock was released")
        .expect("create_desk task panicked");
    assert_eq!(status, StatusCode::CREATED);
}

/// `delete_desk` must serialize against `company_write_lock` too (PR #1875
/// review finding, round 9 — see
/// `set_desk_order_serializes_against_the_company_write_lock`'s own doc).
#[tokio::test]
async fn delete_desk_serializes_against_the_company_write_lock() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(&home, desk_manifest()).await;
    let app = router(state);
    let cookie = crate::server::test_support::fixed_cookie("acme");
    let id = CompanyId::new("acme");

    // Create the overlay desk to delete before taking the lock — this
    // test proves serialization on the delete path, not the create path.
    let created = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/desks")
                .header("cookie", &cookie)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"Growth","members":["eng"]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);

    let lock = company_write_lock(&id);
    let guard = lock.lock().await;

    let app_for_task = app.clone();
    let cookie_for_task = cookie.clone();
    let mut task = tokio::spawn(async move {
        app_for_task
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/v1/company/desks/growth")
                    .header("cookie", &cookie_for_task)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    });

    let raced_ahead = tokio::time::timeout(std::time::Duration::from_millis(200), &mut task)
        .await
        .is_ok();
    assert!(
        !raced_ahead,
        "delete_desk completed while company_write_lock was held \
         elsewhere — it is not serializing its load-modify-save cycle \
         against concurrent `ops` writers"
    );

    drop(guard);
    let status = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .expect("delete_desk never resumed after the lock was released")
        .expect("delete_desk task panicked");
    assert_eq!(status, StatusCode::NO_CONTENT);
}

/// A desk that declares no `routing` block still reports the numbers the
/// runtime would use, rather than blanks (plan hive-desks, Phase 4).
///
/// This is the difference the whole DTO exists for: the manifest says
/// nothing, so `declared` is empty — but the desk would still run rounds of
/// a width and under a cap, and a console showing an empty form would be
/// describing a desk that does not exist.
#[tokio::test]
async fn desk_routing_reports_resolved_numbers_for_an_undeclared_block() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let manifest: CompanyManifest = toml::from_str(
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
         [[agent]]\nid = \"a\"\nrole = \"A\"\n\
         [[agent]]\nid = \"b\"\nrole = \"B\"\n\
         [[agent]]\nid = \"c\"\nrole = \"C\"\n\
         [[group_chat]]\nid = \"solvers\"\nname = \"Solvers\"\nmembers = [\"a\", \"b\", \"c\"]\n\
         [[group_chat]]\nid = \"review\"\nname = \"Review\"\nmembers = [\"c\"]\n",
    )
    .unwrap();
    let state = state_with_manifest(&home, manifest).await;
    let app = router(state);
    let cookie = crate::server::test_support::fixed_cookie("acme");

    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/company/desks/solvers/routing")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();

    assert_eq!(body["deskId"], "solvers");
    assert_eq!(body["source"], "default");
    assert_eq!(
        body["effective"]["roundWidth"],
        crate::hive::routing::DEFAULT_ROUND_WIDTH
    );
    assert_eq!(
        body["effective"]["maxRounds"],
        crate::hive::routing::DEFAULT_MAX_ROUNDS
    );
    assert_eq!(
        body["effective"]["turnTimeoutSecs"],
        crate::hive::routing::DEFAULT_TURN_TIMEOUT_SECS
    );
    assert_eq!(body["effective"]["referral"]["enabled"], false);
    assert!(
        matches!(
            body["effective"]["router"].as_str(),
            Some("jev" | "fallback")
        ),
        "{body}"
    );
    // Nothing was declared, so the authored block is empty — which is
    // exactly what distinguishes it from an operator who wrote `5`.
    assert_eq!(body["declared"], serde_json::json!({}));
    let candidates = body["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 3);
    assert_eq!(candidates[0]["agentId"], "a");
    assert_eq!(candidates[0]["sharedWith"], serde_json::json!([]));
    // `c` also sits on the review desk — the seat another desk's round can
    // delay.
    assert_eq!(candidates[2]["sharedWith"], serde_json::json!(["review"]));
}

/// Installing a routing block takes effect, is reported back resolved, and
/// is undone by a reset — without the manifest ever being rewritten.
#[tokio::test]
async fn a_routing_block_installs_and_resets() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let manifest: CompanyManifest = toml::from_str(
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
         [[agent]]\nid = \"a\"\nrole = \"A\"\n\
         [[agent]]\nid = \"b\"\nrole = \"B\"\n\
         [[group_chat]]\nid = \"solvers\"\nname = \"Solvers\"\nmembers = [\"a\", \"b\"]\n\
         [group_chat.routing]\nround_width = 1\n",
    )
    .unwrap();
    let state = state_with_manifest(&home, manifest).await;
    let app = router(state);
    let cookie = crate::server::test_support::fixed_cookie("acme");

    let install = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/company/desks/solvers/routing")
                .header("cookie", &cookie)
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"round_width":2,"max_rounds":3,"referral":{"enabled":true,"max_hops":1,"returns":true}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(install.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(install.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["source"], "overlay");
    assert_eq!(body["declared"]["round_width"], 2);
    assert_eq!(body["declared"]["referral"]["max_hops"], 1);
    assert_eq!(body["effective"]["roundWidth"], 2);
    assert_eq!(body["effective"]["maxRounds"], 3);
    assert_eq!(body["effective"]["referral"]["enabled"], true);
    assert_eq!(body["effective"]["referral"]["maxHops"], 1);
    assert_eq!(body["effective"]["referral"]["returns"], true);

    // The desk list carries the compact summary of what is in force.
    let desks = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/company/desks")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let desks: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(desks.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let solvers = desks
        .as_array()
        .unwrap()
        .iter()
        .find(|desk| desk["id"] == "solvers")
        .unwrap();
    assert_eq!(solvers["routing"]["source"], "overlay");
    assert_eq!(solvers["routing"]["roundWidth"], 2);
    assert_eq!(solvers["routing"]["maxRounds"], 3);

    let reset = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/company/desks/solvers/routing")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reset.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(reset.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    // Back to the blueprint, which declared a width of one.
    assert_eq!(body["source"], "manifest");
    assert_eq!(body["declared"], serde_json::json!({"round_width": 1}));
    assert_eq!(body["effective"]["roundWidth"], 1);
}

/// The runtime refuses exactly what a manifest carrying the same block
/// would be refused for — and in the same words.
#[tokio::test]
async fn installing_a_zero_width_round_is_refused() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let manifest: CompanyManifest = toml::from_str(
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
         [[agent]]\nid = \"a\"\nrole = \"A\"\n\
         [[agent]]\nid = \"b\"\nrole = \"B\"\n\
         [[group_chat]]\nid = \"solvers\"\nname = \"Solvers\"\nmembers = [\"a\", \"b\"]\n",
    )
    .unwrap();
    let state = state_with_manifest(&home, manifest).await;
    let app = router(state);
    let cookie = crate::server::test_support::fixed_cookie("acme");

    for body in [
        r#"{"round_width":0}"#,
        r#"{"turn_timeout_secs":0}"#,
        r#"{"minimum_confidence":7}"#,
        r#"{"referral":{"enabled":true,"reach":"everywhere"}}"#,
    ] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/v1/company/desks/solvers/routing")
                    .header("cookie", &cookie)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::BAD_REQUEST,
            "body {body} was accepted"
        );
    }
}

/// The structural rows a console draws its activity graph from.
///
/// Before these, "who created this desk" and "who moved this seat" were
/// answerable only from a live frame that does not survive a reload.
#[tokio::test]
async fn desk_lifecycle_is_journaled() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(&home, desk_manifest()).await;
    let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();
    let events = runtime.events();
    let app = router(state);
    let cookie = crate::server::test_support::fixed_cookie("acme");

    for (method, uri, body) in [
        (
            "POST",
            "/api/v1/company/team",
            Some(r#"{"name":"Dana","role":"Analyst"}"#),
        ),
        (
            "POST",
            "/api/v1/company/desks",
            Some(r#"{"name":"Growth","members":["eng"]}"#),
        ),
        (
            "POST",
            "/api/v1/company/desks/growth/members",
            Some(r#"{"agent_id":"ceo"}"#),
        ),
        (
            "PUT",
            "/api/v1/company/desks/growth/routing",
            Some(r#"{"round_width":1}"#),
        ),
        ("DELETE", "/api/v1/company/desks/growth/routing", None),
        ("DELETE", "/api/v1/company/desks/growth/members/ceo", None),
        ("DELETE", "/api/v1/company/desks/growth", None),
    ] {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header("cookie", &cookie);
        if body.is_some() {
            req = req.header("content-type", "application/json");
        }
        let res = app
            .clone()
            .oneshot(req.body(body.map_or(Body::empty(), Body::from)).unwrap())
            .await
            .unwrap();
        assert!(
            res.status().is_success(),
            "{method} {uri} answered {}",
            res.status()
        );
    }

    let rows = events
        .read_from(&CompanyId::new("acme"), EventSeq::new(0), 500)
        .await
        .unwrap();
    let kinds: Vec<&str> = rows.iter().map(|row| row.event.kind()).collect();
    assert!(kinds.contains(&"DeskCreated"), "kinds: {kinds:?}");
    assert!(kinds.contains(&"DeskDeleted"), "kinds: {kinds:?}");
    assert!(kinds.contains(&"TeammateAdded"), "kinds: {kinds:?}");
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == "DeskRoutingConfigured")
            .count(),
        2,
        "one row for install and one for reset: {kinds:?}"
    );
    let desk_seat_rows = rows
        .iter()
        .filter(|row| {
            matches!(
                &row.event,
                CompanyEvent::DeskMembersChanged { desk_id, .. } if desk_id != "general"
            )
        })
        .count();
    assert_eq!(
        desk_seat_rows, 2,
        "one row for the add and one for the remove: {kinds:?}"
    );
    assert!(
        rows.iter().any(|row| matches!(
            &row.event,
            CompanyEvent::DeskMembersChanged { desk_id, added, .. }
                if desk_id == "general" && !added.is_empty()
        )),
        "the new teammate joins #general: {kinds:?}"
    );
}

/// Deleting a desk takes its installed routing block with it.
///
/// Left behind, an overlay desk re-created with the same id silently
/// inherits a table nobody installed on it.
#[tokio::test]
async fn deleting_a_desk_drops_its_installed_routing() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(&home, desk_manifest()).await;
    let app = router(state);
    let cookie = crate::server::test_support::fixed_cookie("acme");

    let created = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/desks")
                .header("cookie", &cookie)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"Growth","members":["eng","ceo"]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);

    let installed = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/company/desks/growth/routing")
                .header("cookie", &cookie)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"round_width":1}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(installed.status(), StatusCode::OK);

    let deleted = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/company/desks/growth")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);

    // Re-create the same id; it must come back ungoverned.
    let again = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/desks")
                .header("cookie", &cookie)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"Growth","members":["eng","ceo"]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::CREATED);

    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/company/desks/growth/routing")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["source"], "default");
    assert_eq!(
        body["declared"],
        serde_json::json!({}),
        "a re-created desk inherited a routing block"
    );
}
