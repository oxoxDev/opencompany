use super::*;
use crate::company::CompanyManifest;
use crate::ports::types::{Actor, ActorKind, CompanyId, CompanyRecord};
use crate::store::fs::{FsCompanyStore, FsEventLog};

fn stores() -> (
    Arc<dyn crate::ports::CompanyStore>,
    Arc<dyn EventLog>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store: Arc<dyn crate::ports::CompanyStore> = Arc::new(FsCompanyStore::new(dir.path()));
    let events: Arc<dyn EventLog> = Arc::new(FsEventLog::new(dir.path()));
    (store, events, dir)
}

fn manifest() -> CompanyManifest {
    toml::from_str("[company]\nname = \"Acme\"\n").expect("valid manifest")
}

async fn seed_company(store: &Arc<dyn crate::ports::CompanyStore>, id: &CompanyId) {
    store
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_desk_hive: Vec::new(),
            overlay_tool_grants: None,
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
            overlay_desk_tools: Default::default(),
            disabled_workflows: Vec::new(),
            template_provenance: None,
            setup: None,
            name_confirmed: false,
            activation_completed_at: None,
            created_at_millis: None,
        })
        .await
        .expect("seed company record");
}

async fn journal_created(
    events: &Arc<dyn EventLog>,
    id: &CompanyId,
    at_millis: u64,
    by: Option<&str>,
) {
    // `EventLog::append` stamps `at_millis` itself (now), so a test that
    // needs a specific timestamp writes directly through the fs backend's
    // append and then rewrites the stored file's timestamp is overkill —
    // instead we drive the window bounds off `now_millis` at call time by
    // asserting relative to whatever `append` actually stamped. See the
    // tests below, which read the timestamp back rather than assume it.
    let _ = at_millis;
    events
        .append(
            id,
            CompanyEvent::WorkflowCreated {
                workflow_id: "wf-1".to_string(),
                name: "My workflow".to_string(),
                by: by.map(|id| Actor {
                    kind: ActorKind::User,
                    id: id.to_string(),
                }),
            },
        )
        .await
        .expect("append");
}

#[tokio::test]
async fn own_attributed_create_inside_the_window_counts() {
    let (store, events, _dir) = stores();
    let id = CompanyId::new("acme");
    seed_company(&store, &id).await;

    let signup = crate::ports::now_millis();
    journal_created(&events, &id, signup, Some("user-1")).await;

    assert!(
        user_saved_workflow_in_week1(&id, &events, "user-1", signup, signup + SEVEN_DAYS_MILLIS)
            .await
            .unwrap(),
        "the user's own attributed create must count"
    );
}

#[tokio::test]
async fn a_teammates_create_does_not_count() {
    // The core per-user proof: company-level would misfire here.
    let (store, events, _dir) = stores();
    let id = CompanyId::new("acme");
    seed_company(&store, &id).await;

    let signup = crate::ports::now_millis();
    journal_created(&events, &id, signup, Some("teammate")).await;

    assert!(
        !user_saved_workflow_in_week1(&id, &events, "user-1", signup, signup + SEVEN_DAYS_MILLIS)
            .await
            .unwrap(),
        "a teammate's create must not activate a different user"
    );
}

#[tokio::test]
async fn an_unattributed_create_does_not_count() {
    // The historical `by: None` gap: never a false positive.
    let (store, events, _dir) = stores();
    let id = CompanyId::new("acme");
    seed_company(&store, &id).await;

    let signup = crate::ports::now_millis();
    journal_created(&events, &id, signup, None).await;

    assert!(
        !user_saved_workflow_in_week1(&id, &events, "user-1", signup, signup + SEVEN_DAYS_MILLIS)
            .await
            .unwrap(),
    );
}

#[tokio::test]
async fn a_create_outside_the_window_does_not_count() {
    let (store, events, _dir) = stores();
    let id = CompanyId::new("acme");
    seed_company(&store, &id).await;

    // Journal now, but claim a signup far enough in the future that the
    // create landed BEFORE the window even opens.
    let future_signup = crate::ports::now_millis() + SEVEN_DAYS_MILLIS * 2;
    journal_created(&events, &id, future_signup, Some("user-1")).await;

    assert!(
        !user_saved_workflow_in_week1(
            &id,
            &events,
            "user-1",
            future_signup,
            future_signup + SEVEN_DAYS_MILLIS
        )
        .await
        .unwrap(),
        "a create that landed before the window opened must not count"
    );
}

#[tokio::test]
async fn a_create_after_the_nominal_window_but_before_the_tick_counts() {
    // The false-nudge gap this fix closes: the scheduler's own tick can
    // land hours after the nominal `signup + 7d` boundary, and a save in
    // that gap is a save all the same.
    let (store, events, _dir) = stores();
    let id = CompanyId::new("acme");
    seed_company(&store, &id).await;

    // Claim a signup far enough in the past that "now" (when this create
    // actually lands) is already outside the nominal 7-day window.
    let signup = crate::ports::now_millis() - SEVEN_DAYS_MILLIS - 60_000;
    journal_created(&events, &id, signup, Some("user-1")).await;
    let evaluated_at = crate::ports::now_millis();

    assert!(
        evaluated_at > signup + SEVEN_DAYS_MILLIS,
        "test setup: the create must land after the nominal window closes"
    );
    assert!(
        user_saved_workflow_in_week1(&id, &events, "user-1", signup, evaluated_at)
            .await
            .unwrap(),
        "a save after the nominal window but before the scheduler's own \
         evaluation instant must still count — the user did save one"
    );
}
