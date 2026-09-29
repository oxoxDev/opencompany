use super::*;
use crate::economy::client::{JsonRpcResponse, MockTinyplaceClient, RegistryReceipt};
use crate::ports::types::CompanyRecord;
use crate::store::FsCompanyStore;

fn signer() -> Arc<LocalSigner> {
    Arc::new(LocalSigner::generate())
}

/// A store rooted at a fresh tempdir, seeded with an empty-ledger record so
/// `remaining_budget` can read it back.
async fn seeded_store(company: &CompanyId) -> (tempfile::TempDir, Arc<dyn CompanyStore>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FsCompanyStore::new(dir.path().to_path_buf());
    let manifest =
        toml::from_str("[company]\nname = \"Acme\"\nhandle = \"acme\"\n").expect("manifest");
    store
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_desk_hive: Vec::new(),
            overlay_retired_agents: Vec::new(),
            id: company.clone(),
            manifest,
            ledger: Vec::new(),
            lifecycle: "running".to_string(),
            overlay_agents: Vec::new(),
            overlay_desk_members: Vec::new(),
            overlay_desk_order: Vec::new(),
            overlay_desks: Vec::new(),
            overlay_workflows: Vec::new(),
            overlay_budgets: Vec::new(),
            overlay_agent_edits: Vec::new(),
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
        .expect("save");
    (dir, Arc::new(store))
}

fn challenge(amount: &str) -> X402Challenge {
    X402Challenge {
        amount: amount.to_string(),
        recipient: "Recipient".into(),
        asset: "USDC".into(),
        network: "solana".into(),
    }
}

fn identity(company: &CompanyId) -> CompanyIdentity {
    CompanyIdentity {
        company: company.clone(),
        handle: "acme".to_string(),
    }
}

async fn ledger_of(store: &Arc<dyn CompanyStore>, company: &CompanyId) -> Vec<LedgerEntry> {
    store.load(company).await.unwrap().unwrap().ledger
}

#[tokio::test]
async fn registration_402_then_budget_check_then_complete() {
    let company = CompanyId::new("acme");
    let (_dir, store) = seeded_store(&company).await;
    let sk = signer();
    let mock = Arc::new(
        MockTinyplaceClient::new()
            .with_register_name(PaidOutcome::PaymentRequired(challenge("25.00")))
            .with_register_paid(RegistryReceipt {
                id: "reg-1".into(),
                addr: AgentAddr("acme.addr".into()),
                fee_usd: 25.0,
            }),
    );
    let economy = TinyplaceEconomy::new(
        mock.clone(),
        sk,
        store.clone(),
        company.clone(),
        Some(200.0),
    )
    .going_public(true);

    let state = economy
        .ensure_registered(&identity(&company))
        .await
        .unwrap();
    assert_eq!(
        state,
        RegistrationState::Registered {
            addr: AgentAddr("acme.addr".into())
        }
    );

    let ledger = ledger_of(&store, &company).await;
    assert_eq!(ledger.len(), 1, "one registry.fee row");
    assert_eq!(ledger[0].kind, "registry.fee");
    assert_eq!(ledger[0].amount_usd, -25.0);
    assert_eq!(mock.count("register_name_paid"), 1);
}

#[tokio::test]
async fn registration_over_budget_rejected() {
    let company = CompanyId::new("acme");
    let (_dir, store) = seeded_store(&company).await;
    let mock = Arc::new(
        MockTinyplaceClient::new()
            .with_register_name(PaidOutcome::PaymentRequired(challenge("25.00"))),
    );
    let economy = TinyplaceEconomy::new(
        mock.clone(),
        signer(),
        store.clone(),
        company.clone(),
        Some(10.0),
    )
    .going_public(true);

    let err = economy
        .ensure_registered(&identity(&company))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "budget_exceeded");
    assert!(
        ledger_of(&store, &company).await.is_empty(),
        "ledger untouched"
    );
    assert_eq!(
        mock.count("register_name_paid"),
        0,
        "never completed the paid call"
    );
}

#[tokio::test]
async fn ensure_registered_private_returns_unregistered() {
    let company = CompanyId::new("acme");
    let (_dir, store) = seeded_store(&company).await;
    // resolve returns not_found; going_public is left false.
    let mock = Arc::new(MockTinyplaceClient::new());
    let economy =
        TinyplaceEconomy::new(mock.clone(), signer(), store, company.clone(), Some(200.0));

    let state = economy
        .ensure_registered(&identity(&company))
        .await
        .unwrap();
    assert_eq!(state, RegistrationState::Unregistered);
    assert_eq!(
        mock.count("register_name"),
        0,
        "private company never claims"
    );
}

#[tokio::test]
async fn pay_fails_closed_when_over_scope() {
    let company = CompanyId::new("acme");
    let (_dir, store) = seeded_store(&company).await;
    let mock = Arc::new(MockTinyplaceClient::new());
    let economy = TinyplaceEconomy::new(mock.clone(), signer(), store, company, None);

    let quote = Quote {
        quote_id: "q1".into(),
        to: AgentAddr("Vendor".into()),
        amount_usd: 30.0,
    };
    let budget = BudgetScope {
        remaining_usd: 20.0,
        label: "vendor-scope".into(),
    };
    let err = economy.pay(&quote, &budget).await.unwrap_err();
    assert_eq!(err.code(), "budget_exceeded");
    assert_eq!(mock.settle_calls(), 0, "no settle before the budget check");
    assert_eq!(mock.verify_calls(), 0, "no verify before the budget check");
}

#[tokio::test]
async fn pay_success_journals_receipt() {
    let company = CompanyId::new("acme");
    let (_dir, store) = seeded_store(&company).await;
    let mock = Arc::new(MockTinyplaceClient::new().with_verify(true, None));
    let economy = TinyplaceEconomy::new(
        mock.clone(),
        signer(),
        store.clone(),
        company.clone(),
        Some(100.0),
    );

    let quote = Quote {
        quote_id: "q1".into(),
        to: AgentAddr("Vendor".into()),
        amount_usd: 15.0,
    };
    let budget = BudgetScope {
        remaining_usd: 50.0,
        label: "vendor-scope".into(),
    };
    let receipt = economy.pay(&quote, &budget).await.unwrap();
    assert_eq!(receipt.quote_id, "q1");
    assert_eq!(receipt.amount_usd, 15.0);
    assert_eq!(mock.settle_calls(), 1);

    let ledger = ledger_of(&store, &company).await;
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0].kind, "x402.out");
    assert_eq!(ledger[0].amount_usd, -15.0);
}

#[tokio::test]
async fn pay_rejects_when_verification_fails() {
    let company = CompanyId::new("acme");
    let (_dir, store) = seeded_store(&company).await;
    let mock = Arc::new(MockTinyplaceClient::new().with_verify(false, Some("bad sig".into())));
    let economy =
        TinyplaceEconomy::new(mock.clone(), signer(), store.clone(), company.clone(), None);

    let quote = Quote {
        quote_id: "q1".into(),
        to: AgentAddr("Vendor".into()),
        amount_usd: 15.0,
    };
    let budget = BudgetScope {
        remaining_usd: 50.0,
        label: "s".into(),
    };
    let err = economy.pay(&quote, &budget).await.unwrap_err();
    assert_eq!(err.code(), "tinyplace_verify_failed");
    assert_eq!(mock.settle_calls(), 0, "never settle an unverified auth");
    assert!(ledger_of(&store, &company).await.is_empty());
}

#[tokio::test]
async fn send_task_402_pays_under_budget() {
    let company = CompanyId::new("acme");
    let (_dir, store) = seeded_store(&company).await;
    let mock = Arc::new(
        MockTinyplaceClient::new()
            .with_send_task(PaidOutcome::PaymentRequired(challenge("12.00")))
            .with_send_task_paid(JsonRpcResponse::ok(
                "t1",
                serde_json::json!({ "id": "task-9" }),
            )),
    );
    let economy = TinyplaceEconomy::new(
        mock.clone(),
        signer(),
        store.clone(),
        company.clone(),
        Some(100.0),
    );

    let handle = economy
        .send_a2a_task(
            &AgentAddr("Vendor".into()),
            A2aTask {
                skill: "seo.audit".into(),
                input: serde_json::json!({ "site": "x" }),
            },
        )
        .await
        .unwrap();
    assert_eq!(handle, A2aTaskHandle("task-9".into()));

    let ledger = ledger_of(&store, &company).await;
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0].kind, "x402.out");
    assert_eq!(ledger[0].amount_usd, -12.0);
}

#[tokio::test]
async fn send_task_402_over_budget_rejected() {
    let company = CompanyId::new("acme");
    let (_dir, store) = seeded_store(&company).await;
    let mock = Arc::new(
        MockTinyplaceClient::new().with_send_task(PaidOutcome::PaymentRequired(challenge("80.00"))),
    );
    let economy = TinyplaceEconomy::new(
        mock.clone(),
        signer(),
        store.clone(),
        company.clone(),
        Some(50.0),
    );

    let err = economy
        .send_a2a_task(
            &AgentAddr("Vendor".into()),
            A2aTask {
                skill: "seo.audit".into(),
                input: serde_json::json!({}),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "budget_exceeded");
    assert_eq!(mock.count("send_task_paid"), 0);
    assert!(ledger_of(&store, &company).await.is_empty());
}

fn card(handle: &str) -> AgentCard {
    AgentCard {
        handle: handle.to_string(),
        ..Default::default()
    }
}

/// Polls until the outbox drains, bounded so a broken replayer fails the
/// test instead of hanging it.
async fn drained_within(outbox: &Arc<Outbox>, budget: Duration) -> bool {
    let step = Duration::from_millis(10);
    for _ in 0..(budget.as_millis() / step.as_millis()).max(1) {
        if outbox.is_empty() {
            return true;
        }
        tokio::time::sleep(step).await;
    }
    outbox.is_empty()
}

/// Issue #454, the reachability test that matters: an economy built through
/// the **production spawn path** may degrade offline, and the card it queued
/// is genuinely sent once the network returns.
///
/// A test that calls `flush_outbox` by hand would prove the drain works. It
/// would not prove the drain is ever *reached* — which is the entire defect,
/// since the pre-#454 drain worked fine and had no caller outside its own
/// test module. So nothing here touches the flush surface: the replayer's
/// own timer is what has to do it.
#[tokio::test]
async fn a_spawned_replayer_queues_offline_then_actually_sends() {
    let company = CompanyId::new("acme");
    let (_dir, store) = seeded_store(&company).await;
    let mock = Arc::new(MockTinyplaceClient::new());
    mock.set_reachable(false);
    let economy = Arc::new(TinyplaceEconomy::new(
        mock.clone(),
        signer(),
        store,
        company.clone(),
        None,
    ));
    spawn_outbox_replayer(&economy, Duration::from_millis(20));

    economy
        .publish_card(&identity(&company), &card("acme"))
        .await
        .expect("an offline publish degrades once a replayer is attached");
    assert_eq!(economy.outbox().len(), 1, "the card is queued");
    assert_eq!(mock.count("put_agent"), 1, "one refused attempt so far");

    // The network comes back. There is no reconnect signal in the client
    // seam, so the next interval tick is the drain.
    mock.set_reachable(true);
    assert!(
        drained_within(economy.outbox(), Duration::from_secs(5)).await,
        "the replayer never drained the outbox"
    );
    assert!(
        mock.count("put_agent") >= 2,
        "the queued card was replayed onto the wire, not merely dropped"
    );
}

/// The other side of the same invariant: with no replayer attached, an
/// offline publish is an **error** and queues nothing. This is what a
/// constructor path that forgets to spawn the replayer inherits — a visible
/// failure instead of a card dropped behind an `Ok(())`.
#[tokio::test]
async fn offline_publish_without_a_replayer_errors_and_queues_nothing() {
    let company = CompanyId::new("acme");
    let (_dir, store) = seeded_store(&company).await;
    let mock = Arc::new(MockTinyplaceClient::new());
    mock.set_reachable(false);
    // The bare constructor: no `spawn_outbox_replayer`.
    let economy = TinyplaceEconomy::new(mock.clone(), signer(), store, company.clone(), None);
    assert!(!economy.has_replayer());

    let err = economy
        .publish_card(&identity(&company), &card("acme"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "tinyplace_unreachable");
    assert!(
        economy.outbox().is_empty(),
        "nothing is queued when nothing would drain it"
    );
}

/// Newest wins: replay only ever needs the current card, so a second offline
/// publish replaces the first rather than stacking behind it.
#[tokio::test]
async fn a_newer_offline_card_replaces_the_queued_one() {
    let company = CompanyId::new("acme");
    let (_dir, store) = seeded_store(&company).await;
    let mock = Arc::new(MockTinyplaceClient::new());
    mock.set_reachable(false);
    let economy = Arc::new(TinyplaceEconomy::new(
        mock.clone(),
        signer(),
        store,
        company.clone(),
        None,
    ));
    // The production interval, so the replayer's timer cannot fire inside
    // this test: what is asserted is the queue, not the drain.
    spawn_outbox_replayer(&economy, OUTBOX_REPLAY_INTERVAL);

    let identity = identity(&company);
    economy.publish_card(&identity, &card("old")).await.unwrap();
    economy.publish_card(&identity, &card("new")).await.unwrap();

    assert_eq!(economy.outbox().len(), 1, "the outbox stays bounded");
    assert_eq!(
        economy.outbox().take(),
        Some(OutboxAction::PublishCard(card("new"))),
        "the newest card is what replay would send"
    );
}

/// Issue #454: an offline task send errors and queues **nothing**. The ghost
/// copy it used to push was unreachable state at best and a budget-less
/// background double-send at worst.
#[tokio::test]
async fn unreachable_send_task_errors_without_queueing() {
    let company = CompanyId::new("acme");
    let (_dir, store) = seeded_store(&company).await;
    let mock = Arc::new(MockTinyplaceClient::new());
    mock.set_reachable(false);
    let economy = TinyplaceEconomy::new(mock.clone(), signer(), store, company, None);

    let err = economy
        .send_a2a_task(
            &AgentAddr("Vendor".into()),
            A2aTask {
                skill: "seo.audit".into(),
                input: serde_json::json!({}),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "tinyplace_unreachable");
    assert!(
        economy.outbox().is_empty(),
        "a paid task is never deferred for background replay"
    );
}

#[tokio::test]
async fn ensure_registered_already_ours_short_circuits() {
    let company = CompanyId::new("acme");
    let (_dir, store) = seeded_store(&company).await;
    let sk = signer();
    let mine = AgentAddr(sk.agent_id());
    let mock = Arc::new(MockTinyplaceClient::new().with_resolve(Some(mine.clone())));
    let economy = TinyplaceEconomy::new(mock.clone(), sk, store, company.clone(), Some(200.0))
        .going_public(true);

    let state = economy
        .ensure_registered(&identity(&company))
        .await
        .unwrap();
    assert_eq!(state, RegistrationState::Registered { addr: mine });
    assert_eq!(mock.count("register_name"), 0, "no claim when already ours");
}
