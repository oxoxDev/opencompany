use super::*;

use axum::body::{Body, to_bytes};
use axum::http::Request;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::company::CompanyManifest;
use crate::ports::CompanyStore;
use crate::ports::types::CompanyId;
use crate::runtime::RuntimeBuilder;
use crate::server::router;
use crate::store::FsCompanyStore;
use crate::{AppConfig, AppState};

/// A company whose `[tools].allow` is the catch-all plus one named grant.
/// The catch-all is the point: it covers `shell`/`code`/`web` and confers
/// none of the five this route deals in, which is the manifest shape #1796
/// was reported against.
const MANIFEST: &str = "[company]\nname = \"Acme\"\n\
     [[agent]]\nid = \"ceo\"\nrole = \"Chief\"\n\
     [tools]\nallow = [\"*\", \"search\"]\n";

fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("oc-tool-grants-")
        .tempdir()
        .expect("tempdir")
}

async fn state(home: &std::path::Path) -> AppState {
    let manifest: CompanyManifest = toml::from_str(MANIFEST).unwrap();
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

async fn call(
    state: &AppState,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", crate::server::test_support::fixed_cookie("acme"))
        .header("content-type", "application/json");
    let request = match body {
        Some(value) => request.body(Body::from(value.to_string())).unwrap(),
        None => request.body(Body::empty()).unwrap(),
    };
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

const URI: &str = "/api/v1/company/tools/grants";

/// The read surface before anything is granted: the manifest's list, an
/// empty `added`, and the closed list of what this page may add. The
/// console renders a control off `grantable`, so an empty one would be the
/// dead end #1796 is about.
#[tokio::test]
async fn get_reports_the_manifest_grants_and_what_may_be_added() {
    let dir = home();
    let state = state(dir.path()).await;
    let (status, body) = call(&state, "GET", URI, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["allow"], json!(["*", "search"]));
    assert_eq!(body["manifestAllow"], json!(["*", "search"]));
    assert_eq!(body["added"], json!([]));
    assert_eq!(
        body["grantable"],
        json!([
            "chargebee",
            "composio",
            "hosting",
            "mcp_registry",
            "paypal",
            "search"
        ])
    );
    assert!(body["setBy"].is_null());
}

/// The whole point of the issue: granting `chargebee` from the console
/// makes the company grant it, and the grant is attributed.
#[tokio::test]
async fn granting_a_namespace_widens_the_effective_allow_list() {
    let dir = home();
    let state = state(dir.path()).await;
    let (status, body) = call(&state, "PUT", URI, Some(json!({"namespace": "chargebee"}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["allow"], json!(["*", "search", "chargebee"]));
    assert_eq!(body["added"], json!(["chargebee"]));
    assert_eq!(body["manifestAllow"], json!(["*", "search"]));
    assert!(body["setBy"].is_string(), "a grant must be attributed");
    assert!(body["setAtMillis"].is_number());
}

/// The grant reaches the readers that decide whether an agent gets the
/// tools — not just this route's own view of it. `grants_chargebee_explicit`
/// over the stored manifest is exactly what `/billing/chargebee` and the
/// harness tool wiring each ask, so asserting it here is asserting that
/// "Connected" and "usable" have stopped disagreeing.
#[tokio::test]
async fn a_console_grant_is_visible_to_the_harness_grant_check() {
    let dir = home();
    let state = state(dir.path()).await;
    let (status, _) = call(&state, "PUT", URI, Some(json!({"namespace": "chargebee"}))).await;
    assert_eq!(status, StatusCode::OK);

    let store = FsCompanyStore::new(dir.path().to_path_buf());
    let record = store
        .load(&CompanyId::new("acme"))
        .await
        .unwrap()
        .expect("the company is stored");
    assert!(
        crate::company::grants_chargebee_explicit(&record.manifest.tools.allow),
        "the stored manifest must grant it: {:?}",
        record.manifest.tools.allow
    );
    assert!(
        crate::company::grants_chargebee_explicit(&record.effective_tool_allow()),
        "and so must the effective list"
    );
}

/// `mcp_registry` is grantable here too (a hosted tenant, whose manifest
/// is a read-only boot snapshot, has no other way to confer it once the
/// harness starts requiring the explicit grant), and the grant reaches the
/// same reader the harness gate uses.
#[tokio::test]
async fn granting_mcp_registry_is_visible_to_the_harness_grant_check() {
    let dir = home();
    let state = state(dir.path()).await;
    let (status, body) = call(
        &state,
        "PUT",
        URI,
        Some(json!({"namespace": "mcp_registry"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["allow"], json!(["*", "search", "mcp_registry"]));
    assert_eq!(body["added"], json!(["mcp_registry"]));

    let store = FsCompanyStore::new(dir.path().to_path_buf());
    let record = store
        .load(&CompanyId::new("acme"))
        .await
        .unwrap()
        .expect("the company is stored");
    assert!(
        crate::company::grants_mcp_registry_explicit(&record.manifest.tools.allow),
        "the stored manifest must grant it: {:?}",
        record.manifest.tools.allow
    );
    assert!(
        crate::company::grants_mcp_registry_explicit(&record.effective_tool_allow()),
        "and so must the effective list"
    );
}

/// The catch-all does not confer these namespaces, and this route does not
/// quietly change that: before the grant, `chargebee` is refused by the very
/// check the harness makes, `*` notwithstanding.
#[tokio::test]
async fn the_catch_all_still_confers_nothing_before_the_grant() {
    let dir = home();
    let _state = state(dir.path()).await;
    let store = FsCompanyStore::new(dir.path().to_path_buf());
    let record = store
        .load(&CompanyId::new("acme"))
        .await
        .unwrap()
        .expect("the company is stored");
    assert!(record.manifest.tools.allow.iter().any(|g| g == "*"));
    assert!(!crate::company::grants_chargebee_explicit(
        &record.effective_tool_allow()
    ));
}

/// The closed list is a boundary, not a hint. `shell` has no connect page
/// and no credential form, so a settings page that could confer it would be
/// the general capability-widening surface the seed-wins rule forbids.
#[tokio::test]
async fn a_namespace_outside_the_closed_list_is_refused() {
    let dir = home();
    let state = state(dir.path()).await;
    let (status, _) = call(&state, "PUT", URI, Some(json!({"namespace": "shell"}))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (_, body) = call(&state, "GET", URI, None).await;
    assert_eq!(body["added"], json!([]), "nothing may be stored");
}

/// Even a namespace smuggled into the stored overlay confers nothing: the
/// closed list is enforced again at resolution, so a row that reached the
/// store under version skew cannot hand an agent a shell.
#[tokio::test]
async fn a_smuggled_namespace_confers_nothing_at_resolution() {
    let dir = home();
    let state = state(dir.path()).await;
    let store = FsCompanyStore::new(dir.path().to_path_buf());
    let id = CompanyId::new("acme");
    let mut record = store.load(&id).await.unwrap().unwrap();
    record.manifest.tools.allow = vec!["files".to_string()];
    record.overlay_tool_grants = Some(ToolGrantsOverride {
        added: vec!["shell".to_string()],
        set_by: Actor {
            kind: ActorKind::User,
            id: "someone".to_string(),
        },
        at_millis: 1,
    });
    store.save(&record).await.unwrap();

    let stored = store.load(&id).await.unwrap().unwrap();
    assert_eq!(
        stored.effective_tool_allow(),
        vec!["files".to_string()],
        "the stored `shell` must be dropped, not resolved"
    );
    let (_, body) = call(&state, "GET", URI, None).await;
    assert_eq!(body["allow"], json!(["files"]));
}

/// Granting what version control already grants stores nothing. Otherwise
/// the console would claim credit for a seed grant and a later `DELETE`
/// would look like it had revoked one — which this layer cannot do.
#[tokio::test]
async fn granting_a_seed_namespace_stores_no_override() {
    let dir = home();
    let state = state(dir.path()).await;
    let (status, body) = call(&state, "PUT", URI, Some(json!({"namespace": "search"}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["added"], json!([]));
    assert_eq!(body["allow"], json!(["*", "search"]));
    assert!(body["setBy"].is_null());
}

/// Two grants accumulate rather than replacing each other — an operator who
/// wires PayPal after Chargebee has not thereby un-wired Chargebee.
#[tokio::test]
async fn grants_accumulate() {
    let dir = home();
    let state = state(dir.path()).await;
    call(&state, "PUT", URI, Some(json!({"namespace": "chargebee"}))).await;
    let (_, body) = call(&state, "PUT", URI, Some(json!({"namespace": "paypal"}))).await;
    assert_eq!(body["added"], json!(["chargebee", "paypal"]));
    assert_eq!(body["allow"], json!(["*", "search", "chargebee", "paypal"]));
}

/// Granting the same namespace twice is idempotent, not a duplicate entry —
/// and the second call does not re-attribute the first.
///
/// A retried `PUT` (a lost response, a second admin making sure) must not
/// move `setBy`/`setAtMillis` onto whoever asked last. A capability widening
/// that records the wrong person is barely better than one that records
/// nobody, and the grant happened once.
#[tokio::test]
async fn granting_twice_is_idempotent_and_keeps_the_original_attribution() {
    let dir = home();
    let state = state(dir.path()).await;

    // Seeded rather than granted twice in a row on purpose. Two live calls
    // share one `now_millis()` at this speed and one signed-in admin, so
    // comparing their responses would pass whether or not the second call
    // re-stamped anything — a test that cannot fail. A distinct actor and an
    // obviously-old timestamp make the re-attribution visible.
    let store = FsCompanyStore::new(dir.path().to_path_buf());
    let id = CompanyId::new("acme");
    let mut record = store.load(&id).await.unwrap().unwrap();
    record.overlay_tool_grants = Some(ToolGrantsOverride {
        added: vec!["hosting".to_string()],
        set_by: Actor {
            kind: ActorKind::User,
            id: "the-operator-who-actually-granted-it".to_string(),
        },
        at_millis: 1_700_000_000_000,
    });
    record.manifest.tools.allow = record.effective_tool_allow();
    store.save(&record).await.unwrap();

    let (status, body) = call(&state, "PUT", URI, Some(json!({"namespace": "hosting"}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["added"], json!(["hosting"]), "duplicated the entry");
    assert_eq!(body["allow"], json!(["*", "search", "hosting"]));
    assert_eq!(body["setBy"], "the-operator-who-actually-granted-it");
    assert_eq!(body["setAtMillis"], 1_700_000_000_000u64);

    // And nothing was written, so the stored record says the same.
    let after = store.load(&id).await.unwrap().unwrap();
    let held = after.overlay_tool_grants.expect("still granted");
    assert_eq!(held.set_by.id, "the-operator-who-actually-granted-it");
    assert_eq!(held.at_millis, 1_700_000_000_000);
}

/// A single withdrawal leaves the other console grants alone, and actually
/// removes the namespace from the folded manifest every reader consults —
/// a revocation that only cleared the overlay would leave the tool wired.
#[tokio::test]
async fn one_namespace_can_be_withdrawn() {
    let dir = home();
    let state = state(dir.path()).await;
    call(&state, "PUT", URI, Some(json!({"namespace": "chargebee"}))).await;
    call(&state, "PUT", URI, Some(json!({"namespace": "paypal"}))).await;
    let (status, body) = call(
        &state,
        "DELETE",
        "/api/v1/company/tools/grants?namespace=paypal",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["added"], json!(["chargebee"]));
    assert_eq!(body["allow"], json!(["*", "search", "chargebee"]));

    let store = FsCompanyStore::new(dir.path().to_path_buf());
    let record = store.load(&CompanyId::new("acme")).await.unwrap().unwrap();
    assert!(
        !crate::company::grants_paypal_explicit(&record.manifest.tools.allow),
        "the withdrawal must reach the list the harness reads: {:?}",
        record.manifest.tools.allow
    );
}

/// **A `DELETE` naming a namespace the SEED grants must change nothing.**
///
/// The obvious implementation subtracts the requested namespace from the
/// folded `[tools].allow`, which quietly strips a manifest grant this layer
/// has no authority over — the company then loses the tool until its next
/// rebuild re-reads `company.toml`, and on a hosted tenant that is the next
/// restart. `search` is in this fixture's manifest precisely so this can be
/// asserted against a real seed grant rather than a hypothetical one.
#[tokio::test]
async fn withdrawing_a_seed_namespace_leaves_it_granted() {
    let dir = home();
    let state = state(dir.path()).await;
    let (status, body) = call(
        &state,
        "DELETE",
        "/api/v1/company/tools/grants?namespace=search",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["allow"], json!(["*", "search"]), "{body}");
    assert_eq!(body["manifestAllow"], json!(["*", "search"]));

    let store = FsCompanyStore::new(dir.path().to_path_buf());
    let record = store.load(&CompanyId::new("acme")).await.unwrap().unwrap();
    assert!(
        crate::company::grants_search_explicit(&record.manifest.tools.allow),
        "the seed grant must survive: {:?}",
        record.manifest.tools.allow
    );
}

/// The same, with a console grant also standing: withdrawing the seed's
/// namespace must leave BOTH the seed grant and the unrelated console grant
/// exactly where they were.
#[tokio::test]
async fn withdrawing_a_seed_namespace_leaves_the_console_grants_alone() {
    let dir = home();
    let state = state(dir.path()).await;
    call(&state, "PUT", URI, Some(json!({"namespace": "chargebee"}))).await;
    let (_, body) = call(
        &state,
        "DELETE",
        "/api/v1/company/tools/grants?namespace=search",
        None,
    )
    .await;
    assert_eq!(body["allow"], json!(["*", "search", "chargebee"]), "{body}");
    assert_eq!(body["added"], json!(["chargebee"]));
}

/// A bare `DELETE` clears every console grant and restores the manifest's
/// own list byte for byte — including the seed grants, which this layer
/// never had the power to remove.
#[tokio::test]
async fn clearing_restores_the_manifest_list() {
    let dir = home();
    let state = state(dir.path()).await;
    call(&state, "PUT", URI, Some(json!({"namespace": "chargebee"}))).await;
    call(&state, "PUT", URI, Some(json!({"namespace": "composio"}))).await;
    let (status, body) = call(&state, "DELETE", URI, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["added"], json!([]));
    assert_eq!(body["allow"], json!(["*", "search"]));
    assert!(body["setBy"].is_null());
}

/// Clearing what was never granted is a no-op, not a 404: the caller's
/// intent is already satisfied.
#[tokio::test]
async fn clearing_nothing_is_a_no_op() {
    let dir = home();
    let state = state(dir.path()).await;
    let (status, body) = call(&state, "DELETE", URI, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["allow"], json!(["*", "search"]));
}

/// Widening what the company's agents can reach is an admin action. A
/// signed-in non-admin reads the list and cannot move it.
#[tokio::test]
async fn a_non_admin_may_read_but_not_grant() {
    let dir = home();
    let state = state(dir.path()).await;
    crate::server::test_support::seed_fixed_member(&state, "acme").await;
    let request = Request::builder()
        .method("PUT")
        .uri(URI)
        .header("cookie", crate::server::test_support::member_cookie("acme"))
        .header("content-type", "application/json")
        .body(Body::from(json!({"namespace": "chargebee"}).to_string()))
        .unwrap();
    let response = router(state.clone()).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

/// **A grant must not claim "next turn" on a runtime that cannot deliver it.**
///
/// This test used to assert the opposite, and was wrong for the reason
/// Codex caught on #1832: the fixture boots on the echo brain — no
/// inference credential, and `openhuman` is absent from the default build —
/// so it is on a NON-harness cognition path, and `HarnessPool::ensure` (the
/// only live grant refresh) never runs for it. `AppState::default` wires no
/// rebuilder either.
///
/// That is exactly the configuration where storing the record and reporting
/// the harness timing tells an operator the integration now reaches their
/// teammates while its belts stay as they were at boot — #1796 reproduced
/// one layer inside its own fix. The response must say restart.
#[tokio::test]
async fn a_grant_on_a_non_harness_runtime_reports_restart_not_next_turn() {
    let dir = home();
    let state = state(dir.path()).await;
    let (status, body) = call(&state, "PUT", URI, Some(json!({"namespace": "hosting"}))).await;
    assert_eq!(status, StatusCode::OK);

    // The grant is stored and durable regardless: the timing is a statement
    // about reach, not about whether the write landed.
    assert_eq!(body["added"], json!(["hosting"]));
    assert_eq!(body["allow"], json!(["*", "search", "hosting"]));

    assert_eq!(
        body["takesEffect"], TAKES_EFFECT_RESTART,
        "an echo-brain company with no rebuilder cannot honour the next-turn promise"
    );
    assert_ne!(body["takesEffect"], TAKES_EFFECT);
}

/// A withdrawal reports the same way, and needs it more: a revocation the
/// runtime has not picked up is a capability the operator believes they
/// removed and the agents still hold.
#[tokio::test]
async fn a_withdrawal_on_a_non_harness_runtime_reports_restart_too() {
    let dir = home();
    let state = state(dir.path()).await;
    call(&state, "PUT", URI, Some(json!({"namespace": "hosting"}))).await;
    let (status, body) = call(&state, "DELETE", URI, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["added"], json!([]));
    assert_eq!(body["takesEffect"], TAKES_EFFECT_RESTART);
}

/// A plain read makes no claim about a write, so it keeps the live-refresh
/// wording. Only a write that had to reach a runtime reports what happened.
#[tokio::test]
async fn the_read_route_reports_the_live_timing() {
    let dir = home();
    let state = state(dir.path()).await;
    let (_, body) = call(&state, "GET", URI, None).await;
    assert_eq!(body["takesEffect"], TAKES_EFFECT);
}

/// Storing nothing reaches no runtime, so it makes no claim about one:
/// granting what version control already grants keeps the read wording and
/// does not rebuild anything.
#[tokio::test]
async fn a_no_op_grant_makes_no_runtime_claim() {
    let dir = home();
    let state = state(dir.path()).await;
    let (_, body) = call(&state, "PUT", URI, Some(json!({"namespace": "search"}))).await;
    assert_eq!(body["added"], json!([]));
    assert_eq!(body["takesEffect"], TAKES_EFFECT);
}

// --- Lock-release-before-rebuild (PR #1875 review finding, CodeRabbit) --

/// A rebuilder that always succeeds, so `apply_to_runtime` actually reaches
/// `rebuild_company` instead of short-circuiting on `can_rebuild_in_place()
/// == false` — the default `state()` fixture has none wired, which is why
/// every other test above reads `TAKES_EFFECT_RESTART` rather than
/// exercising the rebuild path at all.
struct AlwaysRebuilds {
    home: std::path::PathBuf,
}

#[async_trait::async_trait]
impl crate::runtime::RuntimeRebuilder for AlwaysRebuilds {
    async fn rebuild(
        &self,
        _state: &AppState,
        request: crate::runtime::RebuildRequest,
    ) -> crate::Result<crate::company::runtime::CompanyRuntime> {
        RuntimeBuilder::new(self.home.clone(), request.manifest)
            .with_id(request.id)
            .with_handover(request.handover)
            .build()
            .await
    }
}

async fn state_with_rebuilder(home: &std::path::Path) -> AppState {
    state(home)
        .await
        .with_rebuilder(std::sync::Arc::new(AlwaysRebuilds {
            home: home.to_path_buf(),
        }))
}

/// `add_grant` drops `company_write_lock` before `apply_to_runtime` might
/// call into `rebuild_company` — which now takes that same non-reentrant
/// lock itself, so a task still holding it across the call would deadlock
/// against its own rebuild. Nothing proved that until this test; proven
/// the same way `rebuild_company_serializes_against_the_company_write_lock`
/// (`src/runtime/rebuild.rs`) proves the equivalent property one layer
/// down: hold the lock externally, drive the real request through the
/// router, and demand it completes only once the lock is released.
#[tokio::test]
async fn add_grant_does_not_deadlock_against_its_own_rebuild() {
    let dir = home();
    let state = state_with_rebuilder(dir.path()).await;

    let lock = company_write_lock(&CompanyId::new("acme"));
    let guard = lock.lock().await;

    let state_for_task = state.clone();
    let mut task = tokio::spawn(async move {
        call(
            &state_for_task,
            "PUT",
            URI,
            Some(json!({"namespace": "hosting"})),
        )
        .await
    });

    // The request must be blocked behind the held lock — give it every
    // chance to (wrongly) race ahead before declaring it stuck.
    let raced_ahead = tokio::time::timeout(std::time::Duration::from_millis(200), &mut task)
        .await
        .is_ok();
    assert!(
        !raced_ahead,
        "add_grant completed while company_write_lock was held elsewhere — it is not \
         serializing its save against a concurrent writer"
    );

    drop(guard);
    let (status, body) = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .expect(
            "add_grant never resumed after the lock was released — it deadlocked against \
             its own rebuild_company call",
        )
        .expect("task panicked");
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// Same property, for `clear_grants` (PR #1875 review finding, CodeRabbit).
#[tokio::test]
async fn clear_grants_does_not_deadlock_against_its_own_rebuild() {
    let dir = home();
    let state = state_with_rebuilder(dir.path()).await;
    // Seeded before the lock is held externally, so the withdrawal below
    // actually changes the manifest and reaches `apply_to_runtime`.
    call(&state, "PUT", URI, Some(json!({"namespace": "hosting"}))).await;

    let lock = company_write_lock(&CompanyId::new("acme"));
    let guard = lock.lock().await;

    let state_for_task = state.clone();
    let mut task = tokio::spawn(async move { call(&state_for_task, "DELETE", URI, None).await });

    let raced_ahead = tokio::time::timeout(std::time::Duration::from_millis(200), &mut task)
        .await
        .is_ok();
    assert!(
        !raced_ahead,
        "clear_grants completed while company_write_lock was held elsewhere — it is not \
         serializing its save against a concurrent writer"
    );

    drop(guard);
    let (status, body) = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .expect(
            "clear_grants never resumed after the lock was released — it deadlocked \
             against its own rebuild_company call",
        )
        .expect("task panicked");
    assert_eq!(status, StatusCode::OK, "{body}");
}
