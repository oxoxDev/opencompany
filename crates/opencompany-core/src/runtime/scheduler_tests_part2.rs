use super::tests_core::*;

/// A pass that hit a transient store error does NOT latch, so a later pass
/// retries and fires — 0 then 1 across a flaky-once store.
#[tokio::test]
async fn a_failed_pass_does_not_latch() {
    let home_dir = tmp_home();
    let home = home_dir.path().to_path_buf();
    let manifest = scheduled_manifest();
    let schedules = manifest.schedules.clone();
    let flaky = Arc::new(FlakyOnceFires::new(1));
    let rt = Arc::new(
        RuntimeBuilder::new(home, manifest)
            .with_brain(Arc::new(ScheduleBrain))
            .with_schedule_fires(flaky.clone())
            .build()
            .await
            .unwrap(),
    );
    let sid = manifest_schedule_id("0 9 * * MON", "weekly standup");
    // Seed the anchor directly (bypassing the fail budget) so the miss is real.
    flaky.seed(rt.id(), &sid, millis_at(2026, 7, 6, 9, 0) / MINUTE_MS);
    let clock = Arc::new(FakeClock::new(millis_at(2026, 7, 20, 9, 5)));
    let mut scheduler = CompanyScheduler::new(rt.clone(), &schedules, clock).unwrap();

    // First pass: the anchor read errors, so the pass does not complete and
    // must NOT latch.
    assert_eq!(
        scheduler.catch_up().await.unwrap(),
        0,
        "a store error fires nothing and does not latch"
    );
    assert_eq!(fired_count(&rt).await, 0);

    // Second pass: the store now works, so the deferred catch-up fires.
    assert_eq!(
        scheduler.catch_up().await.unwrap(),
        1,
        "the retry makes up the missed fire"
    );
    assert_eq!(fired_count(&rt).await, 1);
}

/// A brain that fails the cycle for one named prompt and answers every other,
/// recording what it was asked, so a test can tell "never reached" apart from
/// "reached and failed".
pub(super) struct FailsOnePrompt {
    fails: String,
    seen: std::sync::Mutex<Vec<String>>,
}

impl FailsOnePrompt {
    fn new(fails: &str) -> Arc<Self> {
        Arc::new(Self {
            fails: fails.to_string(),
            seen: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().expect("seen lock").clone()
    }
}

#[async_trait]
impl Brain for FailsOnePrompt {
    async fn run_cycle(&self, req: CycleRequest, _host: &dyn CycleHost) -> Result<CycleResult> {
        for event in &req.events {
            if let CompanyEvent::ScheduleFired { prompt, .. } = event {
                self.seen.lock().expect("seen lock").push(prompt.clone());
                if *prompt == self.fails {
                    return Err(crate::OpenCompanyError::Config(
                        "this cycle was made to fail".to_string(),
                    ));
                }
            }
        }
        Ok(CycleResult {
            channel_responses: Vec::new(),
            new_traces: vec![CompressedTrace::now(&req.cycle_id, "scheduled")],
            ledger_deltas: Vec::new(),
            token_usage: TokenUsage::default(),
        })
    }
}

/// One schedule's cycle failing must not cost every schedule behind it in the
/// same minute its fire.
///
/// The steady-state loop already isolates a whole failing tick
/// (`if let Err(err) = self.tick()`), which is what made this hard to see: the
/// scheduler kept running, so the symptom was not a dead scheduler but a
/// second schedule that silently never fired — and only in the minutes where
/// the first one happened to fail. A permanently broken first schedule pins
/// every later one off indefinitely.
#[tokio::test]
async fn a_failing_schedule_does_not_cost_the_next_one_its_fire() {
    let home_dir = tmp_home();
    let home = home_dir.path().to_path_buf();
    let manifest: CompanyManifest = toml::from_str(
        r#"
        [company]
        name = "Acme"

        [[agent]]
        id = "ceo"
        role = "Chief"

        [[schedule]]
        cron = "0 9 * * MON"
        prompt = "the one that breaks"

        [[schedule]]
        cron = "0 9 * * MON"
        prompt = "the one behind it"

        [policy]
        mode = "full"
        "#,
    )
    .expect("parse manifest");
    let schedules = manifest.schedules.clone();
    let brain = FailsOnePrompt::new("the one that breaks");
    let rt = Arc::new(
        RuntimeBuilder::new(home, manifest)
            .with_brain(brain.clone())
            .build()
            .await
            .unwrap(),
    );

    let clock = Arc::new(FakeClock::new(millis_at(2026, 7, 13, 9, 0)));
    let mut scheduler = CompanyScheduler::new(rt.clone(), &schedules, clock).unwrap();

    let fired = scheduler
        .tick()
        .await
        .expect("one schedule failing is not a failed tick");

    assert_eq!(
        brain.seen(),
        vec![
            "the one that breaks".to_string(),
            "the one behind it".to_string(),
        ],
        "the tick must go on to the second schedule after the first fails"
    );
    assert_eq!(fired, 1, "the failed one is not counted, the other one is");
}
