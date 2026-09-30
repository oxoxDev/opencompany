use super::*;

#[test]
fn chat_outputs_reject_metadata_for_the_wrong_kind() {
    let workspace = r#"{"kind":"workspace-node","targetId":"node-1","title":"Draft"}"#;
    let artifact = r#"{"kind":"artifact","targetId":"artifact-1","title":"Brief","taskId":"task-1","version":2}"#;
    assert!(serde_json::from_str::<ChatOutput>(workspace).is_ok());
    assert!(serde_json::from_str::<ChatOutput>(artifact).is_ok());

    for invalid in [
        r#"{"kind":"workspace-node","targetId":"node-1","title":"Draft","taskId":"task-1"}"#,
        r#"{"kind":"workspace-node","targetId":"node-1","title":"Draft","version":2}"#,
        r#"{"kind":"artifact","targetId":"artifact-1","title":"Brief"}"#,
        r#"{"kind":"artifact","targetId":"artifact-1","title":"Brief","taskId":"task-1"}"#,
        r#"{"kind":"artifact","targetId":"artifact-1","title":"Brief","version":2}"#,
    ] {
        assert!(
            serde_json::from_str::<ChatOutput>(invalid).is_err(),
            "{invalid}"
        );
    }
}

/// The answers must survive the **blob**, not merely the record.
///
/// `CompanyRecord` gained a `setup` field and the fs store round-tripped it
/// for free, because it serialises the whole record. SQLite and MongoDB do
/// not: they rebuild a record field by field from `OverlayBlob`, so anything
/// missing there is dropped silently on the way back out — losing exactly the
/// answers Phase 2 builds workflows from, on exactly the backends a hosted
/// tenant runs, and nowhere else. `--all-features` compilation is what
/// surfaced it; this is what keeps it surfaced.
#[test]
fn the_setup_answers_survive_the_overlay_blob() {
    let answers = crate::company::setup::SetupAnswers {
        industry: "E-commerce — homeware".into(),
        team_hint: "someone on dispatch".into(),
        automate: "meta ads, order dispatch".into(),
    };
    let mut record = CompanyRecord {
        general_channel: Default::default(),
        overlay_desk_hive: Vec::new(),
        overlay_retired_agents: Vec::new(),
        overlay_agent_edits: Vec::new(),
        id: CompanyId::new("acme"),
        manifest: toml::from_str("[company]\nname = \"Acme\"\n").expect("manifest"),
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
        setup: Some(answers.clone()),
        name_confirmed: false,
        activation_completed_at: None,
        created_at_millis: None,
    };

    let json = serde_json::to_string(&OverlayBlob::from_record(&record)).expect("serialize");
    let parsed = OverlayBlob::parse(&json).expect("parse");
    assert_eq!(
        parsed.setup,
        Some(answers),
        "the answers were dropped by the blob the SQL backends rebuild from"
    );

    // A company that never went through setup carries nothing, and a row
    // written before the field existed still loads.
    record.setup = None;
    let json = serde_json::to_string(&OverlayBlob::from_record(&record)).expect("serialize");
    assert_eq!(OverlayBlob::parse(&json).expect("parse").setup, None);
    assert_eq!(
        OverlayBlob::parse("{\"agents\":[]}")
            .expect("legacy row")
            .setup,
        None
    );
}

/// Issue #1741: `SecretValue` derived `Serialize`, so
/// `serde_json::to_value` over anything holding one emitted the plaintext
/// credential. Unlike the `Debug` surface — patched five separate times on
/// the *enclosing* structs, each time after somebody noticed a live key in
/// a log line — no test anywhere caught the serialize side.
///
/// The guard lives on `SecretValue` itself, so the assertions below are
/// deliberately made through containers the type knows nothing about: a
/// struct with a plain `#[derive(Serialize)]` standing in for the next
/// config struct somebody writes, plus `Option`, `Vec`, a map value, and
/// `#[serde(flatten)]` (a genuinely different serde code path), across
/// both `to_string` and `to_value` (also different code paths in
/// `serde_json`). Regress the impl to a derive and every arm fails.
#[test]
fn secret_value_redacts_in_debug_and_serialize() {
    use std::collections::BTreeMap;

    // Obviously fake, and distinctive enough that a substring hit is a real
    // hit. Same sentinel as the four existing planted-secret tests.
    const FAKE_SECRET: &str = "NOT-A-REAL-KEY-planted-for-tests";

    // Case-**insensitive**. A leak that arrives lowercased, uppercased, or
    // case-mangled on the way out is still a leak, and an exact-case search
    // reads it as clean — which is how a sibling change shipped a
    // leak-detection test that passed a deliberate leak.
    fn leaks(rendering: &str) -> bool {
        rendering
            .to_ascii_lowercase()
            .contains(&FAKE_SECRET.to_ascii_lowercase())
    }

    // Sanity: the detector detects. Without this the whole test could be
    // vacuous and read as green.
    assert!(
        leaks(&format!("token={}", FAKE_SECRET.to_ascii_lowercase())),
        "the leak detector cannot see a lowercased sentinel; every \
         assertion below would be vacuous"
    );

    /// The next config struct somebody writes: derives `Serialize` and
    /// `Debug` with no idea a secret is in there.
    #[derive(Debug, Serialize)]
    struct UnsuspectingConfig {
        bind: String,
        token: SecretValue,
        optional: Option<SecretValue>,
        many: Vec<SecretValue>,
        by_name: BTreeMap<String, SecretValue>,
        // No map-*key* arm: `SecretValue` derives neither `Ord` nor
        // `Hash`, so it cannot occupy a key position in any std map. That
        // is worth keeping — a credential is not an identity to index by.
        #[serde(flatten)]
        nested: NestedSecrets,
    }

    /// Flattened into the outer struct, so serde uses `FlatMapSerializer`
    /// instead of the ordinary struct serializer.
    #[derive(Debug, Serialize)]
    struct NestedSecrets {
        inner: SecretValue,
    }

    let secret = SecretValue(FAKE_SECRET.to_string());
    let config = UnsuspectingConfig {
        bind: "127.0.0.1:8080".to_string(),
        token: secret.clone(),
        optional: Some(secret.clone()),
        many: vec![secret.clone(), secret.clone()],
        by_name: BTreeMap::from([("github".to_string(), secret.clone())]),
        nested: NestedSecrets {
            inner: secret.clone(),
        },
    };

    // --- Serialize, both serde_json entry points -----------------------
    let as_string = serde_json::to_string(&config).expect("serialize");
    assert!(
        !leaks(&as_string),
        "plaintext reached to_string: {as_string}"
    );

    let as_value = serde_json::to_value(&config).expect("to_value");
    let value_text = as_value.to_string();
    assert!(
        !leaks(&value_text),
        "plaintext reached to_value: {value_text}"
    );

    // The bare type, not just embedded in something.
    let bare = serde_json::to_string(&secret).expect("serialize bare");
    assert!(
        !leaks(&bare),
        "plaintext reached a bare serialization: {bare}"
    );
    assert_eq!(bare, format!("\"{SECRET_REDACTED}\""));

    // Redaction is *visible*, not a silently dropped field: an operator
    // reading a dump can tell a secret was there and was withheld.
    assert!(
        as_string.contains(SECRET_REDACTED),
        "the marker is missing, so the field vanished silently: {as_string}"
    );
    // Everything non-secret still serializes normally — the guard is
    // scoped to the secret, not to the struct.
    assert!(as_string.contains("127.0.0.1:8080"), "{as_string}");

    // --- Debug, plain and alternate ------------------------------------
    for rendering in [format!("{config:?}"), format!("{config:#?}")] {
        assert!(
            !leaks(&rendering),
            "plaintext reached a Debug rendering: {rendering}"
        );
        assert!(rendering.contains(SECRET_REDACTED), "{rendering}");
    }
    // On the type itself, so an enclosing struct's *derived* Debug is safe
    // and the container stops having to remember.
    assert_eq!(
        format!("{secret:?}"),
        format!("SecretValue({SECRET_REDACTED})")
    );

    // --- The persistence door is still open ----------------------------
    // Every secret-store backend writes `expose()` and reads back through
    // the constructor; none of them touch serde. That path must keep
    // returning the plaintext or storing a credential stops working.
    assert_eq!(secret.expose(), FAKE_SECRET);
    assert_eq!(SecretValue(secret.expose().to_string()), secret);

    // --- Deserialization keeps working ---------------------------------
    // Reading a secret *in* never leaks one, so `Deserialize` stays
    // derived: a config or stored shape may name a `SecretValue` field.
    let loaded: SecretValue =
        serde_json::from_str(&format!("\"{FAKE_SECRET}\"")).expect("deserialize");
    assert_eq!(loaded.expose(), FAKE_SECRET);

    // The asymmetry is deliberate, and asserted so nobody discovers it in
    // production: a serde round-trip yields the marker, which fails closed
    // at the point of use rather than carrying a live credential onward.
    let round_tripped: SecretValue = serde_json::from_str(&bare).expect("round-trip");
    assert_eq!(round_tripped.expose(), SECRET_REDACTED);
    assert_ne!(round_tripped, secret);
}

fn round_trip<T>(value: &T) -> T
where
    T: Serialize + for<'de> Deserialize<'de>,
{
    let json = serde_json::to_string(value).expect("serialize");
    serde_json::from_str(&json).expect("deserialize")
}

/// The additive proof this repo asks of every new journal field: a message
/// carrying no mentions must serialize **byte-for-byte** as it did before
/// the field existed, so no stored record migrates and the cross-backend
/// round-trip needs no special case.
#[test]
fn a_message_with_no_mentions_serializes_as_it_did_before_the_field() {
    let event = CompanyEvent::OperatorMessage {
        text: "hello".to_string(),
        by: None,
        chat: None,
        parent: None,
        deliverable: None,
        mentions: Vec::new(),
        attachments: Vec::new(),
    };
    let json = serde_json::to_string(&event).expect("serialize");
    assert_eq!(json, r#"{"kind":"OperatorMessage","text":"hello"}"#);
}

/// The same for a reply, whose `mention_depth` is a `u8` and would
/// otherwise serialize as a literal `0` on every reply ever written.
#[test]
fn a_reply_with_no_mentions_serializes_as_it_did_before_the_fields() {
    let event = CompanyEvent::AgentReply {
        audience: Vec::new(),
        episode: None,
        chat_id: "general-channel".to_string(),
        agent_id: "ceo".to_string(),
        text: "hi".to_string(),
        steps: Vec::new(),
        task_id: None,
        outputs: Vec::new(),
        parent: None,
        mentions: Vec::new(),
        mention_depth: 0,
    };
    let json = serde_json::to_string(&event).expect("serialize");
    assert_eq!(
        json,
        r#"{"kind":"AgentReply","chat_id":"general-channel","agent_id":"ceo","text":"hi"}"#
    );
}

/// And the other direction: a record written before either field existed
/// still loads, which is what `#[serde(default)]` is there for.
#[test]
fn a_message_journaled_before_mentions_existed_still_loads() {
    let stored = r#"{"kind":"OperatorMessage","text":"hello"}"#;
    let event: CompanyEvent = serde_json::from_str(stored).expect("deserialize");
    match event {
        CompanyEvent::OperatorMessage { mentions, .. } => assert!(mentions.is_empty()),
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn a_mention_round_trips_with_its_target_and_span() {
    let mention = Mention {
        target: MentionTarget::Agent {
            id: "engineer".to_string(),
        },
        text: "@engineer".to_string(),
        offset: 4,
        quiet: false,
    };
    assert_eq!(round_trip(&mention), mention);
    // `quiet` is omitted when false, so an ordinary mention stays small on
    // the wire and in the journal.
    let json = serde_json::to_string(&mention).expect("serialize");
    assert!(!json.contains("quiet"), "{json}");
}

#[test]
fn every_mention_target_round_trips() {
    for target in [
        MentionTarget::Agent {
            id: "engineer".to_string(),
        },
        MentionTarget::User {
            id: "u1".to_string(),
        },
        MentionTarget::Desk {
            id: "engineering".to_string(),
        },
        MentionTarget::Everyone,
    ] {
        assert_eq!(round_trip(&target), target);
    }
}

// ── Issue #174: cycle usage carries cost, and folds ─────────────────────

/// A cycle with nothing to report writes nothing, and any single non-zero
/// field makes it real usage — including a token-less charge.
#[test]
fn token_usage_is_zero_only_when_every_field_is() {
    assert!(TokenUsage::default().is_zero());
    for usage in [
        TokenUsage {
            input: 1,
            ..TokenUsage::default()
        },
        TokenUsage {
            output: 1,
            ..TokenUsage::default()
        },
        TokenUsage {
            cached_input: 1,
            ..TokenUsage::default()
        },
        TokenUsage {
            cost_usd: 0.0001,
            ..TokenUsage::default()
        },
    ] {
        assert!(!usage.is_zero(), "{usage:?} is real usage");
    }
}

/// Several model passes in one cycle accumulate into one total.
#[test]
fn token_usage_folds_passes_together() {
    let mut total = TokenUsage::default();
    total.fold(&TokenUsage {
        input: 100,
        output: 20,
        cached_input: 10,
        cost_usd: 0.01,
    });
    total.fold(&TokenUsage {
        input: 50,
        output: 5,
        cached_input: 0,
        cost_usd: 0.02,
    });
    assert_eq!(total.input, 150);
    assert_eq!(total.output, 25);
    assert_eq!(total.cached_input, 10);
    assert!((total.cost_usd - 0.03).abs() < 1e-9);
}

/// A bogus peer value must never wrap the meter into a huge or tiny number.
#[test]
fn token_usage_fold_saturates_instead_of_overflowing() {
    let mut total = TokenUsage {
        input: u64::MAX,
        output: u64::MAX,
        cached_input: u64::MAX,
        cost_usd: 0.0,
    };
    total.fold(&TokenUsage {
        input: 10,
        output: 10,
        cached_input: 10,
        cost_usd: 0.0,
    });
    assert_eq!(total.input, u64::MAX);
    assert_eq!(total.output, u64::MAX);
    assert_eq!(total.cached_input, u64::MAX);
}

/// The cost fields are additive on the wire: a peer that predates them still
/// decodes, and an all-zero usage still serializes them for a peer that has
/// them.
#[test]
fn token_usage_decodes_a_payload_without_the_cost_fields() {
    let legacy: TokenUsage = serde_json::from_str(r#"{"input":7,"output":3}"#).unwrap();
    assert_eq!(legacy.input, 7);
    assert_eq!(legacy.output, 3);
    assert_eq!(legacy.cached_input, 0);
    assert_eq!(legacy.cost_usd, 0.0);
    assert_eq!(round_trip(&legacy), legacy);
}

/// The `TurnStep` wire shape is camelCase with snake_case enum values:
/// `{kind, status, label, detail?, elapsedMs?}`. Locks the contract the
/// console `TurnStep` mirror in `frontend/src/api/types.ts` depends on.
#[test]
fn turn_step_wire_shape_is_camel_case_with_snake_case_enums() {
    let step = TurnStep {
        kind: TurnStepKind::ToolCall,
        status: TurnStepStatus::Error,
        label: "Searching the web".to_string(),
        detail: Some("brave · search".to_string()),
        elapsed_ms: Some(1234),
        ..TurnStep::default()
    };
    let json = serde_json::to_value(&step).unwrap();
    assert_eq!(json["kind"], "tool_call");
    assert_eq!(json["status"], "error");
    assert_eq!(json["label"], "Searching the web");
    assert_eq!(json["detail"], "brave · search");
    assert_eq!(json["elapsedMs"], 1234);
    assert_eq!(round_trip(&step), step);
}

/// A step with no detail/elapsed omits both keys, and every kind/status
/// value serializes to its documented snake_case token.
#[test]
fn turn_step_omits_absent_fields_and_covers_every_variant() {
    let bare = TurnStep {
        kind: TurnStepKind::Thinking,
        status: TurnStepStatus::Ok,
        label: "Thinking".to_string(),
        detail: None,
        elapsed_ms: None,
        ..TurnStep::default()
    };
    let json = serde_json::to_value(&bare).unwrap();
    assert_eq!(json["kind"], "thinking");
    assert_eq!(json["status"], "ok");
    assert!(json.get("detail").is_none(), "absent detail is omitted");
    assert!(json.get("elapsedMs").is_none(), "absent elapsed is omitted");

    assert_eq!(serde_json::to_value(TurnStepKind::Note).unwrap(), "note");
    assert_eq!(
        serde_json::to_value(TurnStepStatus::Running).unwrap(),
        "running"
    );
}

/// `OutboundMessage.steps` is additive: an empty timeline is omitted from
/// the wire entirely (so every prior producer round-trips byte-identically),
/// and a legacy `{channel, text}` payload still loads with an empty `steps`.
#[test]
fn outbound_message_steps_are_additive_and_omitted_when_empty() {
    let no_steps = OutboundMessage {
        message_id: None,
        task_id: None,
        outputs: Vec::new(),
        channel: "operator".to_string(),
        agent: None,
        text: "hi".to_string(),
        steps: Vec::new(),
        reply_to: None,
        mentions: Vec::new(),
    };
    let json = serde_json::to_string(&no_steps).unwrap();
    assert_eq!(json, r#"{"channel":"operator","text":"hi"}"#);

    let legacy: OutboundMessage =
        serde_json::from_str(r#"{"channel":"operator","text":"hi"}"#).unwrap();
    assert!(legacy.steps.is_empty());

    let with_steps = OutboundMessage {
        message_id: None,
        task_id: None,
        outputs: Vec::new(),
        channel: "operator".to_string(),
        agent: None,
        text: "done".to_string(),
        steps: vec![TurnStep {
            kind: TurnStepKind::Note,
            status: TurnStepStatus::Error,
            label: "MCP: brave unavailable".to_string(),
            detail: Some("server rejected the call".to_string()),
            elapsed_ms: None,
            ..TurnStep::default()
        }],
        reply_to: None,
        mentions: Vec::new(),
    };
    assert_eq!(round_trip(&with_steps), with_steps);
}

/// Issue #246: `OutboundMessage.task_id` is additive on exactly the same
/// terms as `steps` above — a bubble that opened no card must serialize
/// byte-for-byte as it did before the field existed, and a payload written
/// before it existed must still load. Without both halves every already-
/// stored response would change shape the moment this field shipped.
#[test]
fn outbound_message_task_id_is_additive_and_omitted_when_absent() {
    let no_card = OutboundMessage {
        message_id: None,
        task_id: None,
        outputs: Vec::new(),
        channel: "operator".to_string(),
        agent: None,
        text: "hi".to_string(),
        steps: Vec::new(),
        reply_to: None,
        mentions: Vec::new(),
    };
    assert_eq!(
        serde_json::to_string(&no_card).unwrap(),
        r#"{"channel":"operator","text":"hi"}"#,
        "a bubble that opened no card keeps the pre-#246 wire form"
    );

    let legacy: OutboundMessage =
        serde_json::from_str(r#"{"channel":"operator","text":"hi"}"#).unwrap();
    assert!(legacy.task_id.is_none());

    let with_card = OutboundMessage {
        message_id: None,
        task_id: Some("t-42".to_string()),
        outputs: Vec::new(),
        channel: "operator".to_string(),
        agent: None,
        text: "opened one".to_string(),
        steps: Vec::new(),
        reply_to: None,
        mentions: Vec::new(),
    };
    assert_eq!(round_trip(&with_card), with_card);
    assert!(
        serde_json::to_string(&with_card)
            .unwrap()
            .contains(r#""taskId":"t-42""#),
        "the console reads the card off a camelCase key"
    );
}

/// `AgentReply.steps` is additive the same way: a reply journaled before
/// the field existed loads with an empty timeline, and a tool-less reply
/// omits the key so its on-disk form is byte-identical to the legacy log.
#[test]
fn agent_reply_steps_are_additive_and_omitted_when_empty() {
    let legacy: CompanyEvent = serde_json::from_str(
        r#"{"kind":"AgentReply","chat_id":"main","agent_id":"ceo","text":"hi"}"#,
    )
    .expect("a pre-steps AgentReply still loads");
    match &legacy {
        CompanyEvent::AgentReply { steps, .. } => assert!(steps.is_empty()),
        other => panic!("expected AgentReply, got {other:?}"),
    }

    // A tool-less reply serializes without the `steps` key.
    let tool_less = CompanyEvent::AgentReply {
        audience: Vec::new(),
        episode: None,
        mentions: Vec::new(),
        mention_depth: 0,
        parent: None,
        task_id: None,
        outputs: Vec::new(),
        chat_id: "general-channel".to_string(),
        agent_id: "ceo".to_string(),
        text: "hi".to_string(),
        steps: Vec::new(),
    };
    let json = serde_json::to_value(&tool_less).unwrap();
    assert!(json.get("steps").is_none());

    // A reply with a timeline round-trips it.
    let with_steps = CompanyEvent::AgentReply {
        audience: Vec::new(),
        episode: None,
        mentions: Vec::new(),
        mention_depth: 0,
        parent: None,
        task_id: None,
        outputs: Vec::new(),
        chat_id: "general-channel".to_string(),
        agent_id: "ceo".to_string(),
        text: "done".to_string(),
        steps: vec![TurnStep {
            kind: TurnStepKind::ToolCall,
            status: TurnStepStatus::Ok,
            label: "Reading messages".to_string(),
            detail: None,
            elapsed_ms: Some(12),
            ..TurnStep::default()
        }],
    };
    let back: CompanyEvent =
        serde_json::from_str(&serde_json::to_string(&with_steps).unwrap()).unwrap();
    assert_eq!(back, with_steps);
}
