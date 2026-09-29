use std::sync::Arc;

use serde_json::{Value, json};

use super::agent::copilot_persona;
use super::tests_copilot_unit::DESC_GRAPH;
use super::workflow_build_fixtures_tests::*;
use super::workflow_build_shared_tests::*;
use super::*;

// ---------------------------------------------------------------------------
// The copilot agent — pass tier over the native tool-calling loop (issue #840)
// ---------------------------------------------------------------------------

/// The fixture's canonical good graph: a scheduled trigger → `maya` drafts.
fn good_workflow() -> Value {
    json!({
        "name": "Weekly digest",
        "description": "Draft and send the weekly digest.",
        "nodes": [
            { "id": "start", "kind": "trigger", "name": "Every Monday", "schedule": "0 9 * * 1", "requires_approval": true },
            { "id": "draft", "kind": "agent", "name": "Draft", "agent": "maya", "requires_approval": false }
        ],
        "edges": [{ "from": "start", "to": "draft" }]
    })
}

/// The happy path over the NATIVE dispatcher: the agent proposes a valid graph,
/// the propose tool accepts it under host authority, and the caller returns
/// `Graph` — with the host-minted (deduped) id/name, stripped approval gating,
/// and the surviving schedule.
#[tokio::test]
async fn a_description_drafts_a_graph_via_the_agent() {
    let model = NativeCopilotModel::scripting(vec![
        propose_step("email the weekly digest", good_workflow()),
        NativeStep::done("Proposed the weekly digest workflow for your review."),
    ]);
    let (_home, runtime) = runtime_with_agent(model.clone(), None).await;
    seed_workflow(&runtime, "weekly-digest", "Weekly digest").await;

    let outcome = draft_workflow_from_description(&runtime, "email the weekly digest every Monday")
        .await
        .expect("the drafter runs");
    let (summary, spec) = match outcome {
        DescriptionDraftOutcome::Graph { summary, spec, .. } => (summary, spec),
        DescriptionDraftOutcome::NotAutomatable(reason) => panic!("expected a graph: {reason}"),
    };
    assert!(summary.contains("digest"), "summary: {summary}");
    assert_eq!(spec.id, "weekly-digest-2", "host mints + dedups the id");
    assert_eq!(spec.name, "Weekly digest 2", "host dedups the name");
    assert!(spec.nodes.iter().all(|n| n.requires_approval.is_none()));
    assert_eq!(spec.nodes[0].schedule.as_deref(), Some("0 9 * * 1"));
    assert!(model.calls() >= 1, "the model ran at least once");
}

/// Issue #1931 regression: the vendored `openhuman` runtime compiles in a
/// `workflows` toolpack that claims the bare name `propose_workflow`, owned only
/// by openhuman's OWN `workflow_builder`/`flow_discovery` agents — and withholds
/// any tool sharing that name from every OTHER agent's advertised belt,
/// regardless of who actually registered it (`strip_packed_from_visible` matches
/// by name alone). That silently dropped this copilot's propose tool from the
/// model's very first request: the model still called it (from the system
/// prompt), got back `unknown tool`, and gave up narrating prose instead of
/// drafting a graph — so every downstream assertion about the drafted graph
/// failed, none of them naming the real cause.
///
/// This pins the invariant that would have caught it directly: the FIRST model
/// request of a turn must advertise all three of the copilot's own tools,
/// including the propose tool, by name — not a downstream side effect of it
/// being missing.
#[tokio::test]
async fn the_first_model_request_advertises_all_three_copilot_tools() {
    let model = NativeCopilotModel::scripting(vec![
        propose_step("email the weekly digest", good_workflow()),
        NativeStep::done("Proposed the weekly digest workflow for your review."),
    ]);
    let (_home, runtime) = runtime_with_agent(model.clone(), None).await;
    seed_workflow(&runtime, "weekly-digest", "Weekly digest").await;

    let _ = draft_workflow_from_description(&runtime, "email the weekly digest every Monday").await;

    let seen = model.seen_tool_names();
    let first_request_tools = seen.first().expect("the model was invoked at least once");
    for tool in [
        "list_effective_tools",
        "check_workflow",
        "propose_company_workflow",
    ] {
        assert!(
            first_request_tools.iter().any(|name| name == tool),
            "the first model request must advertise `{tool}`; got {first_request_tools:?}"
        );
    }
}

/// Issue #1042 regression: drafting the SAME description twice must draft a graph
/// BOTH times, and the second turn must NOT replay the first turn's session
/// transcript. Before the per-turn workspace fix, the copilot agent's stable
/// per-company `workspace_dir` let the second turn's fresh, empty-history agent
/// discover the first turn's persisted transcript and resume it — so the model saw
/// its own prior draft and refused ("I already drafted this last turn"), leaving
/// the dialog empty. The fix mints a unique workspace per turn, so each turn's
/// resume scan finds nothing. This asserts both the OUTCOME (both are `Graph`) and
/// the MECHANISM (the second turn's first invoke carries only system + user, no
/// replayed assistant/tool turn).
#[tokio::test]
async fn repeating_a_description_does_not_replay_the_prior_turn() {
    // Each turn is one propose (accepted) then a closing reply. Scripting both
    // turns' steps explicitly — rather than relying on the exhausted-script repeat
    // — so the SECOND turn genuinely proposes a graph too, isolating the transcript
    // replay as the only thing that could make it refuse.
    let model = NativeCopilotModel::scripting(vec![
        propose_step("email the weekly digest", good_workflow()),
        NativeStep::done("Proposed the weekly digest workflow for your review."),
        propose_step("email the weekly digest", good_workflow()),
        NativeStep::done("Proposed the weekly digest workflow for your review."),
    ]);
    let (_home, runtime) = runtime_with_agent(model.clone(), None).await;
    seed_workflow(&runtime, "weekly-digest", "Weekly digest").await;

    const DESC: &str = "email the weekly digest every Monday";

    let first = draft_workflow_from_description(&runtime, DESC)
        .await
        .expect("the first draft runs");
    if let DescriptionDraftOutcome::NotAutomatable(reason) = &first {
        panic!("the first draft must be a graph, got not-automatable: {reason}");
    }

    // The number of invokes the first turn consumed — the next recorded invoke is
    // the SECOND turn's first invoke.
    let invokes_before_second = model.calls();

    let second = draft_workflow_from_description(&runtime, DESC)
        .await
        .expect("the second draft runs");
    if let DescriptionDraftOutcome::NotAutomatable(reason) = &second {
        panic!(
            "the repeated draft must ALSO be a graph, not a replay-driven refusal, got \
             not-automatable: {reason}"
        );
    }

    // The mechanism: the second turn opened on a FRESH conversation — only the
    // system prompt and this turn's user message. A replayed prior transcript would
    // inject the first turn's assistant (propose) + tool-result messages here.
    let seen = model.seen_messages();
    let second_turn_first_invoke = &seen[invokes_before_second];
    let replayed_prior_turn = second_turn_first_invoke
        .iter()
        .any(|m| matches!(m, Message::Assistant(_) | Message::Tool(_)));
    assert!(
        !replayed_prior_turn,
        "the second turn's first invoke must not replay the prior turn's transcript; saw \
         {} messages: {:?}",
        second_turn_first_invoke.len(),
        second_turn_first_invoke
            .iter()
            .map(|m| match m {
                Message::System(_) => "system",
                Message::User(_) => "user",
                Message::Assistant(_) => "assistant",
                Message::Tool(_) => "tool",
                Message::Custom(_) => "custom",
            })
            .collect::<Vec<_>>()
    );
}

/// The agent finishes without proposing — it judged the work a one-off — so the
/// caller folds to not-automatable carrying the agent's own stated reason.
#[tokio::test]
async fn an_honest_decline_folds_to_not_automatable() {
    let model = NativeCopilotModel::scripting(vec![NativeStep::done(
        "This is a one-off task, better done once by hand than built into a reusable workflow.",
    )]);
    let (_home, runtime) = runtime_with_agent(model, None).await;

    let outcome = draft_workflow_from_description(&runtime, "do a one-off thing")
        .await
        .expect("the drafter runs");
    match outcome {
        DescriptionDraftOutcome::NotAutomatable(reason) => {
            assert!(
                reason.contains("one-off") || reason.contains("by hand"),
                "reason: {reason}"
            );
        }
        DescriptionDraftOutcome::Graph { .. } => panic!("expected not-automatable"),
    }
}

/// A first propose that fails a host gate (a delivery request with no `output`
/// node) comes back to the agent as gate sentences; its second propose adds the
/// output node and is accepted — recovery inside one turn, ending in `Graph`.
#[tokio::test]
async fn the_agent_recovers_after_a_failing_propose() {
    // No delivery node → the delivery gate fires for an "email me" request.
    let bad = json!({
        "name": "Digest",
        "nodes": [
            { "id": "t", "kind": "trigger", "name": "Monday", "schedule": "0 9 * * 1" },
            { "id": "a", "kind": "agent", "name": "Draft and email", "agent": "maya",
              "summary": "email the digest to the owner" }
        ],
        "edges": [{ "from": "t", "to": "a" }]
    });
    let good = json!({
        "name": "Digest",
        "nodes": [
            { "id": "t", "kind": "trigger", "name": "Monday", "schedule": "0 9 * * 1" },
            { "id": "a", "kind": "agent", "name": "Draft", "agent": "maya" },
            { "id": "o", "kind": "output", "name": "Send", "destination": { "kind": "owner" } }
        ],
        "edges": [{ "from": "t", "to": "a" }, { "from": "a", "to": "o" }]
    });
    let model = NativeCopilotModel::scripting(vec![
        propose_step("email the digest", bad),
        propose_step("email the digest", good),
        NativeStep::done("Fixed the delivery and proposed it."),
    ]);
    let (_home, runtime) = runtime_with_agent(model.clone(), None).await;

    let outcome =
        draft_workflow_from_description(&runtime, "email me the weekly digest every monday")
            .await
            .expect("the drafter runs");
    assert!(
        matches!(outcome, DescriptionDraftOutcome::Graph { .. }),
        "the agent's corrected propose is accepted"
    );
    assert!(model.calls() >= 2, "the agent re-proposed after the gate");
}

/// A description that names a teammate by ROLE ("the writer") drafts with the
/// host resolver's id rewrite and an operator-facing note explaining it — the
/// note rides through to `DescriptionDraftOutcome::Graph`.
#[tokio::test]
async fn a_role_named_agent_resolves_with_a_note() {
    let by_role = json!({
        "name": "Draft",
        "nodes": [
            { "id": "t", "kind": "trigger", "name": "Start" },
            { "id": "a", "kind": "agent", "name": "Write", "agent": "Writer" }
        ],
        "edges": [{ "from": "t", "to": "a" }]
    });
    let model = NativeCopilotModel::scripting(vec![
        propose_step("draft an update", by_role),
        NativeStep::done("Proposed it."),
    ]);
    let (_home, runtime) = runtime_with_agent(model, None).await;

    let outcome = draft_workflow_from_description(&runtime, "have the writer draft an update")
        .await
        .expect("the drafter runs");
    match outcome {
        DescriptionDraftOutcome::Graph { spec, notes, .. } => {
            assert_eq!(spec.nodes[1].agent.as_deref(), Some("maya"));
            assert!(notes.iter().any(|n| n.contains("maya")), "notes: {notes:?}");
        }
        DescriptionDraftOutcome::NotAutomatable(reason) => panic!("expected a graph: {reason}"),
    }
}

/// An agent that keeps checking without ever proposing exhausts its
/// tool-iteration budget, and the caller folds to not-automatable naming the step
/// budget — never a silent empty graph. Each check is a DISTINCT call (a
/// differently-named draft) so openhuman's identical-repeat loop guard does not
/// stop the turn early; the run reaches `set_max_tool_iterations` instead.
#[tokio::test]
async fn a_cap_hit_folds_to_not_automatable() {
    let steps: Vec<NativeStep> = (0..10)
        .map(|i| {
            NativeStep::call(
                "check_workflow",
                json!({
                    "workflow": {
                        "name": format!("Draft {i}"),
                        "nodes": [
                            { "id": "t", "kind": "trigger", "name": "Start" },
                            { "id": "a", "kind": "agent", "name": format!("Step {i}"), "agent": "maya" }
                        ],
                        "edges": [{ "from": "t", "to": "a" }]
                    }
                }),
            )
        })
        .collect();
    let model = NativeCopilotModel::scripting(steps);
    let (_home, runtime) = runtime_with_agent(model, None).await;

    let outcome = draft_workflow_from_description(&runtime, "email the weekly digest")
        .await
        .expect("the drafter runs");
    match outcome {
        DescriptionDraftOutcome::NotAutomatable(reason) => {
            assert!(reason.contains("step budget"), "reason: {reason}");
        }
        DescriptionDraftOutcome::Graph { .. } => panic!("a cap hit must not draft a graph"),
    }
}

/// A zero-usage turn (the offline path — no tokens, no charge) meters nothing:
/// `record_workflow_build_usage`'s zero guard means no sample reaches the meter.
#[tokio::test]
async fn a_zero_usage_turn_meters_nothing() {
    let model = NativeCopilotModel::scripting(vec![
        propose_step("digest", good_workflow()),
        NativeStep::done("done"),
    ]);
    let meter = Arc::new(RecordingUsageMeter::default());
    let (_home, runtime) = runtime_with_agent(model, Some(meter.clone())).await;

    let outcome = draft_workflow_from_description(&runtime, "email the weekly digest")
        .await
        .expect("the drafter runs");
    assert!(matches!(outcome, DescriptionDraftOutcome::Graph { .. }));
    assert!(
        meter.samples().is_empty(),
        "a zero-usage turn records no sample"
    );
}

/// A CHARGED turn records a usage sample carrying the backend-charged
/// `cost_usd` — the load-bearing property of building the copilot as a real
/// OpenHuman `Agent` (a token-only tinyflows runner would drop cost to zero).
#[tokio::test]
async fn a_charged_turn_records_cost_usd() {
    let model = NativeCopilotModel::scripting(vec![
        propose_step("digest", good_workflow()),
        NativeStep::done("done"),
    ])
    .with_charge(120, 40, 0.0042);
    let meter = Arc::new(RecordingUsageMeter::default());
    let (_home, runtime) = runtime_with_agent(model, Some(meter.clone())).await;

    let outcome = draft_workflow_from_description(&runtime, "email the weekly digest")
        .await
        .expect("the drafter runs");
    assert!(matches!(outcome, DescriptionDraftOutcome::Graph { .. }));

    let samples = meter.samples();
    assert_eq!(
        samples.len(),
        1,
        "one metered sample for the turn: {samples:?}"
    );
    assert!(
        samples[0].cost_usd > 0.0,
        "a charged turn pins a non-zero cost_usd: {:?}",
        samples[0]
    );
    assert!(samples[0].input_tokens > 0, "the token counts survive too");
}

/// The copilot persona names ONLY node kinds inside `DESCRIPTION_NODE_KINDS` —
/// the prompt↔gate coupling: a kind the prompt teaches that the propose gate does
/// not accept (or vice versa) would silently leak. Backticked kinds are the
/// prompt's node-kind references.
#[test]
fn the_persona_names_only_supported_node_kinds() {
    let persona = copilot_persona();
    // Every OC workflow node kind; any the persona backticks must be authorable.
    let all_kinds = [
        "trigger",
        "agent",
        "tool_call",
        "http_request",
        "condition",
        "output",
        "switch",
        "merge",
        "split_out",
        "transform",
        "output_parser",
        "sub_workflow",
    ];
    for kind in all_kinds {
        if persona.contains(&format!("`{kind}`")) {
            assert!(
                DESCRIPTION_NODE_KINDS.contains(&kind),
                "the persona names node kind `{kind}`, which is NOT authorable \
                 (DESCRIPTION_NODE_KINDS = {DESCRIPTION_NODE_KINDS:?})"
            );
        }
    }
    // And every authorable kind IS present, so the contract is actually taught.
    for kind in DESCRIPTION_NODE_KINDS {
        assert!(
            persona.contains(kind),
            "the persona must name the authorable kind `{kind}`"
        );
    }
}

/// The prompt grounds the model in the company's real state: the operator's
/// description verbatim, the roster ids, the existing workflow names, and the
/// granted tool slugs — and the system prompt carries the `tool_call` rule the
/// card builder has no need for.
#[tokio::test]
async fn the_description_prompt_renders_the_company_state_verbatim() {
    let (_home, runtime) = runtime_with(ScriptedModel::replying(DESC_GRAPH)).await;
    seed_workflow(&runtime, "existing-one", "Existing One").await;
    let company = gather_company_evidence(&runtime).await.unwrap();
    // `None` wiring — this fixture asserts the rendering, so it wants the widest
    // honest slug set (the grant filter alone), not a deployment-narrowed one.
    let slugs = crate::company::workflow_effective_tool_slugs(&company.record, None);

    let description = "email the weekly digest every Monday morning";
    let prompt = description_evidence_prompt(&company, &slugs, &[], description);
    assert!(
        prompt.contains(description),
        "the description appears verbatim"
    );
    assert!(prompt.contains("`maya`"), "the roster id is rendered");
    assert!(
        prompt.contains("Existing One"),
        "existing names are rendered"
    );
    assert!(
        prompt.contains("`web_fetch`"),
        "granted tool slugs are rendered: {prompt}"
    );
    // Issue #813: the tool line carries the honest capability + required args, not
    // a bare slug — so the model does not reach for a tool that cannot do the job.
    assert!(
        prompt.contains("cannot search for a URL"),
        "the web_fetch capability line is rendered: {prompt}"
    );
    assert!(
        prompt.contains("(args: url)"),
        "web_fetch's required arg is rendered: {prompt}"
    );
    // Issue #840: the copilot's system prompt is the ported persona plus the
    // shared graph contract. It names the three tools it drives, states the
    // delivery invariant, shows an `output` node with a destination in the schema
    // example, and carries the roster-copy rule and the SAFETY stance.
    let system = copilot_persona();
    assert!(
        system.contains("list_effective_tools")
            && system.contains("check_workflow")
            && system.contains("propose_company_workflow"),
        "the persona names its three tools: {system}"
    );
    assert!(
        system.contains("an `agent` node cannot send"),
        "the delivery invariant is stated: {system}"
    );
    assert!(
        system.contains("\"kind\": \"output\"") && system.contains("\"destination\""),
        "the schema example includes an output node with a destination: {system}"
    );
    assert!(
        system.contains("copied EXACTLY"),
        "the roster-copy rule is stated: {system}"
    );
    assert!(
        system.contains("SAFETY"),
        "the SAFETY stance is present: {system}"
    );
}

#[tokio::test]
async fn the_description_prompt_excludes_capability_filtered_tools() {
    let (_home, runtime) = runtime_with(ScriptedModel::replying(DESC_GRAPH)).await;
    let company = gather_company_evidence(&runtime).await.unwrap();
    let mut record = company.record.clone();
    record.manifest.tools.allow.push("search".to_string());
    // The live resolver reads a plan whose namespace key set is the callable
    // tier. Omitting `web` must remove every web slug from prompt grounding,
    // even when the company grants the namespace.
    let capability_filter = crate::harness::capability_budget::resolve_filter(
        &crate::harness::capability_budget::CapabilityPlan {
            period: crate::harness::capability_budget::BudgetPeriod::Daily,
            budgets: [
                ("shell".to_string(), u64::MAX),
                ("code".to_string(), u64::MAX),
                ("search".to_string(), u64::MAX),
            ]
            .into_iter()
            .collect(),
            total_budget: None,
        },
        Some(&EmptyUsageMeter),
        &record.id,
        crate::ports::now_millis(),
    )
    .await;
    let wired: std::collections::BTreeSet<&'static str> =
        crate::workflows::caps::WORKFLOW_TOOL_NAMESPACES
            .into_iter()
            .filter(|namespace| {
                !matches!(
                    &capability_filter,
                    crate::harness::toolbelt::CapabilityFilter::DenyNamespaces(denied)
                        if denied.contains(namespace)
                )
            })
            .collect();
    let effective = crate::company::workflow_effective_tool_slugs(&record, Some(&wired));
    let unwired = crate::company::workflow_granted_but_unwired_tool_slugs(&record, Some(&wired));
    assert!(!effective.iter().any(|slug| slug == "web_fetch"));
    assert!(effective.iter().any(|slug| slug == "web_search"));
    assert!(unwired.iter().any(|slug| slug == "web_fetch"));

    let evidence = CompanyEvidence { record, ..company };
    let prompt = description_evidence_prompt(&evidence, &effective, &unwired, "search the web");
    assert!(!prompt.contains("web_fetch —"));
    assert!(prompt.contains("granted but not wired"));
    assert!(prompt.contains("web_fetch"));
    assert!(prompt.contains("if the task needs one, say so"));
}

/// A company with no teammates and no granted tools renders the guiding lines
/// that keep the model from authoring an `agent` or `tool_call` node it cannot
/// ground.
#[tokio::test]
async fn the_description_prompt_names_an_empty_roster_and_toolset() {
    let (_home, runtime) = runtime_with(ScriptedModel::replying(DESC_GRAPH)).await;
    let base = gather_company_evidence(&runtime).await.unwrap();
    // Reuse the gathered record but blank the roster / names for the render.
    let empty = CompanyEvidence {
        roster: Vec::new(),
        existing_names: Vec::new(),
        existing_ids: HashSet::new(),
        ..base
    };
    let prompt = description_evidence_prompt(&empty, &[], &[], "do the thing");
    assert!(prompt.contains("no teammates"), "{prompt}");
    assert!(prompt.contains("no callable tools are wired"), "{prompt}");
    assert!(prompt.contains("(none yet)"), "{prompt}");
}
