//! A session that once declared `read` resumes under a belt built without an
//! event log.

use super::built_in_test_fixtures::*;
use super::*;

#[tokio::test]
async fn a_session_that_declared_read_still_runs_without_an_event_log() {
    let mut rec = record();
    rec.id = CompanyId::new(format!("read-retention-{}", uuid::Uuid::new_v4().simple()));

    let mut fx = fixture();
    fx.deps.events = Some(Arc::new(crate::hive::test_support::MemoryLog::default()));
    let pool = HarnessPool::new();
    pool.ensure(&rec, &fx.deps).await.expect("ensure");
    pool.run(
        &rec.id,
        "ceo",
        "first",
        &fx.deps,
        crate::runtime::delegation::ChatTarget::default(),
    )
    .await
    .expect("a turn with `read` on the belt");
    drop(pool);

    let fx = fixture();
    assert!(fx.deps.events.is_none());
    let pool = HarnessPool::new();
    pool.ensure(&rec, &fx.deps).await.expect("ensure");
    pool.run(
        &rec.id,
        "ceo",
        "second",
        &fx.deps,
        crate::runtime::delegation::ChatTarget::default(),
    )
    .await
    .expect("the resumed session runs");
    let agent = pool.agent(&rec.id, "ceo").await.expect("ceo");
    assert!(
        agent
            .tools()
            .iter()
            .any(|tool| tool.name() == crate::hive::tools::READ_TOOL),
        "`read` stays on the belt so a recorded declaration can be answered"
    );
}
