use super::*;
use crate::ports::types::CompanyId;
use tinyflows::model::Node;

/// A company record whose `[policy]` is the only thing under test.
///
/// `always_approve` is written explicitly on every call — including as an
/// empty list. [`DEFAULT_ALWAYS_APPROVE`](crate::company::DEFAULT_ALWAYS_APPROVE)
/// is empty as of issue #684, so letting it default would no longer decide
/// anything behind these tests' backs; writing it out stays the rule
/// anyway, because a test that borrows a shipped default tests the default
/// rather than the mechanism, and this module's subject is the tier.
fn company(mode: &str, always_approve: &[&str]) -> CompanyRecord {
    let always = always_approve
        .iter()
        .map(|entry| format!("\"{entry}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let manifest = toml::from_str(&format!(
        r#"
[company]
name = "Acme"

[policy]
mode = "{mode}"
always_approve = [{always}]

[[agent]]
id = "ceo"
role = "Chief Executive"
description = "Runs Acme."
"#
    ))
    .expect("valid manifest");
    CompanyRecord {
        general_channel: Default::default(),
        overlay_desk_hive: Vec::new(),
        overlay_retired_agents: Vec::new(),
        overlay_agent_edits: Vec::new(),
        id: CompanyId::new("acme"),
        manifest,
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
    }
}

fn tool_node(id: &str, slug: &str) -> Node {
    Node {
        id: id.to_string(),
        kind: NodeKind::ToolCall,
        type_version: 1,
        name: String::new(),
        config: json!({ "slug": slug }),
        ports: Vec::new(),
        position: None,
    }
}

fn kind_node(id: &str, kind: NodeKind) -> Node {
    Node {
        id: id.to_string(),
        kind,
        type_version: 1,
        name: String::new(),
        config: json!({}),
        ports: Vec::new(),
        position: None,
    }
}

fn graph(nodes: Vec<Node>) -> WorkflowGraph {
    WorkflowGraph {
        id: Some("wf".to_string()),
        nodes,
        ..WorkflowGraph::default()
    }
}

fn gate_ids(graph: &WorkflowGraph) -> Vec<&str> {
    graph
        .nodes
        .iter()
        .filter(|n| n.config.get("requires_approval") == Some(&json!(true)))
        .map(|n| n.id.as_str())
        .collect()
}

/// The defect itself: on the default `supervised` mode a `shell` node runs
/// with no operator card. It must now stop.
#[tokio::test]
async fn a_consequential_call_gates_under_supervised() {
    let mut g = graph(vec![tool_node("run-it", "shell")]);
    let gated = apply_policy_gates(
        &mut g,
        &company("supervised", &[]),
        "wf",
        "run-1",
        &GrantSet::default(),
    )
    .await;

    assert_eq!(gate_ids(&g), ["run-it"]);
    assert_eq!(gated.len(), 1);
    assert_eq!(gated[0].slug, "shell");
    assert_eq!(gated[0].node_id, "run-it");
    assert!(
        gated[0].reason.contains("shell"),
        "the card must name the tool: {}",
        gated[0].reason
    );
}

/// `full` autonomy is the operator saying "don't ask me". The graph must
/// come out byte-identical, or this change would gate companies that opted
/// out of gating.
#[tokio::test]
async fn full_autonomy_leaves_the_graph_untouched() {
    let before = graph(vec![tool_node("run-it", "shell")]);
    let mut after = before.clone();
    let gated = apply_policy_gates(
        &mut after,
        &company("full", &[]),
        "wf",
        "run-1",
        &GrantSet::default(),
    )
    .await;

    assert!(gated.is_empty());
    assert_eq!(after.nodes[0].config, before.nodes[0].config);
}

/// #674's boundary condition, at the seam rather than in a unit test.
///
/// The `full` company above runs an authored `shell` node without asking,
/// because the operator passed the `[tools].allow` grant, authored the node
/// and saw the command. Template that command from an upstream node's output
/// and they saw a *shape*: the content arrives at run time from data they
/// never read, so the node is judged as an agent call and gates.
///
/// Without this the split is defeated by one line of authoring, and the
/// judgement unit tests cannot catch it — they prove `judge` answers
/// correctly, not that this pass hands it the arguments it must see.
#[tokio::test]
async fn a_shell_node_templated_from_upstream_output_gates_under_full() {
    let mut node = tool_node("run-it", "shell");
    node.config = json!({ "slug": "shell", "args": { "command": "=previous.output" } });
    let mut g = graph(vec![node]);

    let gated = apply_policy_gates(
        &mut g,
        &company("full", &[]),
        "wf",
        "run-1",
        &GrantSet::default(),
    )
    .await;

    assert_eq!(gate_ids(&g), vec!["run-it"]);
    assert_eq!(gated.len(), 1, "{gated:?}");
    assert_eq!(gated[0].slug, "shell");
    assert!(
        gated[0].reason.contains("arbitrary code"),
        "the card must say what it is asking about: {}",
        gated[0].reason
    );
}

/// The same condition on the other gated node kind: an `http_request` node
/// with a literal URL does not gate under `full`
/// (`an_http_request_node_does_not_gate_under_full`), and one whose
/// destination is decided by the run does.
///
/// This is the pair that shows the rule is about the *arguments* and not
/// about `tool_call` nodes specifically.
#[tokio::test]
async fn an_http_request_node_with_a_templated_url_gates_under_full() {
    let mut node = tool_node("fetch", "unused");
    node.kind = NodeKind::HttpRequest;
    node.config = json!({ "method": "POST", "url": "=item.endpoint" });
    let mut g = graph(vec![node]);

    let gated = apply_policy_gates(
        &mut g,
        &company("full", &[]),
        "wf",
        "run-1",
        &GrantSet::default(),
    )
    .await;

    assert_eq!(gated.len(), 1, "{gated:?}");
    assert_eq!(
        gated[0].target.as_deref(),
        Some("POST (destination resolved at run time)"),
        "an operator approving an outbound request with no host shown \
         should be told that is what they are doing"
    );
}

/// A pure read of the agent's own workspace is `Reach::Nothing`, and a
/// metered read is `Reach::Money` — neither parks under supervision. Gating
/// either would stop runs that have nothing to decide, and for `web_search`
/// it would be worse than useless: consent for it happens once, at grant
/// time, via an explicit `search` grant a `*` cannot confer.
#[tokio::test]
async fn a_plain_read_and_a_metered_read_do_not_gate() {
    let mut g = graph(vec![
        // `file_read` is a genuine `Reach::Nothing` read of the agent's own
        // workspace. `read_workspace_state` is NOT one — issue #459
        // reclassified it to `Reach::Consequence` because it shells out to
        // git under an agent-writable `.git/config`, so it parks under
        // supervision by design (see `consequence.rs` +
        // `reading_workspace_state_is_classified_with_shell_because_it_runs_git`).
        tool_node("read", "file_read"),
        tool_node("search", "web_search"),
    ]);
    let gated = apply_policy_gates(
        &mut g,
        &company("supervised", &[]),
        "wf",
        "run-1",
        &GrantSet::default(),
    )
    .await;

    assert!(gated.is_empty(), "{gated:?}");
    assert!(gate_ids(&g).is_empty());
}

#[test]
fn tinyflows_parallel_control_nodes_do_not_describe_outward_calls() {
    for kind in [
        NodeKind::Spawn,
        NodeKind::Scatter,
        NodeKind::Gather,
        NodeKind::Gate,
    ] {
        let node = kind_node("control", kind.clone());
        assert_eq!(call_of(&node), None, "{kind:?}");
    }
}

/// The three newest additions to `NodeKind` (tinyflows 0.8.x), classified
/// directly against `call_of` rather than only through `apply_policy_gates`
/// — a regression lock on the two new match arms added alongside
/// `Capabilities.approvals`.
///
/// `Approval` and `Void` join `tinyflows_parallel_control_nodes_do_not_describe_outward_calls`
/// in reaching nothing on this path (`Approval`'s own doc comment on the
/// match arm says why: it is a stub, like `Code`/`Memory`/`Shell`, not a
/// classified capability call); `Trigger` was already exhaustive-matched to
/// `None` and gets the same direct check for symmetry.
#[test]
fn the_newest_node_kinds_reach_nothing_on_this_path() {
    for kind in [NodeKind::Approval, NodeKind::Trigger, NodeKind::Void] {
        let node = kind_node("n", kind.clone());
        assert_eq!(call_of(&node), None, "{kind:?}");
    }
}

/// The two node kinds `call_of` *does* classify, checked directly rather
/// than only through the higher-level `apply_policy_gates` tests above —
/// so a future new `None` arm accidentally shadowing one of these two would
/// fail here even if a specific gating test happened not to exercise it.
#[test]
fn tool_call_and_http_request_are_the_two_classified_kinds() {
    let tool = tool_node("run-it", "shell");
    assert!(call_of(&tool).is_some(), "{tool:?}");

    let mut http = tool_node("fetch", "unused");
    http.kind = NodeKind::HttpRequest;
    http.config = json!({ "method": "GET", "url": "https://example.com" });
    assert!(call_of(&http).is_some(), "{http:?}");
}

/// `always_approve` outranks the tier, exactly as it does on the agent
/// path — so a company can gate a metered read it wants to be asked about
/// even though `supervised` alone would let it through.
#[tokio::test]
async fn always_approve_gates_a_call_the_tier_would_allow() {
    let mut g = graph(vec![tool_node("search", "web_search")]);
    let gated = apply_policy_gates(
        &mut g,
        &company("full", &["web_search"]),
        "wf",
        "run-1",
        &GrantSet::default(),
    )
    .await;

    assert_eq!(gate_ids(&g), ["search"]);
    assert!(gated[0].reason.contains("always-approve"), "{gated:?}");
}

/// An author may add a gate the policy does not require; they may not
/// remove one it does. Fail-closed in the one direction that matters.
#[tokio::test]
async fn an_authored_opt_out_cannot_remove_a_policy_gate() {
    let mut node = tool_node("run-it", "shell");
    node.config = json!({ "slug": "shell", "requires_approval": false });
    let mut g = graph(vec![node]);

    apply_policy_gates(
        &mut g,
        &company("supervised", &[]),
        "wf",
        "run-1",
        &GrantSet::default(),
    )
    .await;

    assert_eq!(g.nodes[0].config["requires_approval"], json!(true));
}

/// An `agent` node is never touched: its gated calls already park through
/// #395's drain, and gating it here would park the same call twice.
///
/// This test previously also asserted that an `http_request` node is left
/// alone. Issue #614 is precisely that this was wrong, so that half moved to
/// [`an_http_request_node_gates_under_supervised`] with the opposite
/// expectation — recorded here rather than silently inverted.
#[tokio::test]
async fn an_agent_node_is_never_gated_here() {
    let mut agent = tool_node("think", "shell");
    agent.kind = NodeKind::Agent;
    let mut transform = tool_node("shape", "shell");
    transform.kind = NodeKind::Transform;
    let mut g = graph(vec![agent, transform]);

    let gated = apply_policy_gates(
        &mut g,
        &company("supervised", &[]),
        "wf",
        "run-1",
        &GrantSet::default(),
    )
    .await;

    assert!(gated.is_empty(), "{gated:?}");
    assert!(gate_ids(&g).is_empty());
}

/// Issue #614's defect: an `http_request` node reached an external address
/// on a `supervised` company with no card. It runs through
/// [`GuardedHttpClient`](super::super::caps), never `ToolInvoker`, so #460's
/// fix did not reach it.
#[tokio::test]
async fn an_http_request_node_gates_under_supervised() {
    let mut node = tool_node("fetch", "unused");
    node.kind = NodeKind::HttpRequest;
    node.config = json!({ "method": "post", "url": "https://api.example.com/v1/pay?token=s3cret" });
    let mut g = graph(vec![node]);

    let gated = apply_policy_gates(
        &mut g,
        &company("supervised", &[]),
        "wf",
        "run-1",
        &GrantSet::default(),
    )
    .await;

    assert_eq!(gate_ids(&g), ["fetch"]);
    assert_eq!(gated[0].slug, "http_request");
    // Method and host, uppercased — and NOT the path or the query, which is
    // where tokens live. This string goes to the durable journal.
    assert_eq!(gated[0].target.as_deref(), Some("POST api.example.com"));
    assert!(
        !gated[0].target.as_deref().unwrap().contains("s3cret"),
        "the card must not carry the query string"
    );
}

/// `full` autonomy leaves an `http_request` node alone, the same as a
/// `tool_call` one — the operator opted out of being asked.
#[tokio::test]
async fn an_http_request_node_does_not_gate_under_full() {
    let mut node = tool_node("fetch", "unused");
    node.kind = NodeKind::HttpRequest;
    node.config = json!({ "url": "https://api.example.com/x" });
    let before = node.config.clone();
    let mut g = graph(vec![node]);

    let gated = apply_policy_gates(
        &mut g,
        &company("full", &[]),
        "wf",
        "run-1",
        &GrantSet::default(),
    )
    .await;

    assert!(gated.is_empty(), "{gated:?}");
    assert_eq!(g.nodes[0].config, before);
}

/// A URL the run has not resolved yet is the common authoring case
/// (`url = "=item.endpoint"`). The node still gates — the call is no less
/// consequential — and the card says the destination is not knowable yet
/// rather than either claiming `=item.endpoint` is a host or going silent.
/// An operator approving an outbound request with no host shown should be
/// told that is what they are doing.
#[tokio::test]
async fn an_unresolved_url_gates_and_says_the_destination_is_not_known_yet() {
    let mut node = tool_node("fetch", "unused");
    node.kind = NodeKind::HttpRequest;
    node.config = json!({ "method": "GET", "url": "=item.endpoint" });
    let mut g = graph(vec![node]);

    let gated = apply_policy_gates(
        &mut g,
        &company("supervised", &[]),
        "wf",
        "run-1",
        &GrantSet::default(),
    )
    .await;

    assert_eq!(gate_ids(&g), ["fetch"]);
    let target = gated[0].target.as_deref().expect("the absence is stated");
    assert!(target.starts_with("GET "), "{target}");
    assert!(target.contains("run time"), "{target}");
    assert!(
        !target.contains("item.endpoint"),
        "an unresolved expression is not a host: {target}"
    );
}

/// The default method matters: an `http_request` node with no `method` is a
/// GET, and the card should say so rather than leaving it blank.
#[test]
fn a_target_defaults_to_get_and_drops_path_query_and_fragment() {
    assert_eq!(
        http_target(&json!({ "url": "https://h.test/a/b?q=1#f" })).as_deref(),
        Some("GET h.test")
    );
    assert_eq!(
        http_target(&json!({ "method": "delete", "url": "http://h.test:8080/x" })).as_deref(),
        Some("DELETE h.test:8080")
    );
    // No `url` key at all: nothing was authored, so there is nothing to
    // name and nothing to explain.
    assert_eq!(http_target(&json!({ "method": "GET" })), None);
    // Authored but unusable — no scheme, or a scheme with an empty host.
    // The method is still known, and the missing host is stated.
    for config in [
        json!({ "url": "not-a-url" }),
        json!({ "url": "https:///only-path" }),
    ] {
        let target = http_target(&config).expect("authored, so the absence is stated");
        assert!(target.contains("run time"), "{target}");
    }
}

/// A URL's userinfo never reaches the card (CWE-200).
///
/// `https://token:secret@api.example.com/v1` puts a credential in the
/// authority, and this string is written to the durable approval journal,
/// rendered on the Approvals page and kept after the decision — the exact
/// test the path and query already failed. It also names the wrong thing:
/// a reader scanning `POST token:secret@api.example.com` sees the token
/// before the host the decision actually turns on.
#[test]
fn a_target_never_carries_url_userinfo() {
    for (url, expected) in [
        (
            "https://token:secret@api.example.com/v1",
            "POST api.example.com",
        ),
        ("https://user@api.example.com", "POST api.example.com"),
        // A `@` in the credential itself: the host is after the LAST one.
        (
            "https://user:p@ss@api.example.com:8443/x",
            "POST api.example.com:8443",
        ),
    ] {
        let target =
            http_target(&json!({ "method": "POST", "url": url })).expect("a host is nameable here");
        assert_eq!(target, expected, "{url}");
        assert!(!target.contains('@'), "{url} → {target}");
        assert!(!target.contains("secret"), "{url} → {target}");
    }
}

/// A slugless `tool_call` has no call to classify; the engine's own node
/// reports it. Gating it would turn a clear authoring error into a card an
/// operator cannot act on.
#[tokio::test]
async fn a_slugless_tool_call_is_left_alone() {
    let mut node = tool_node("broken", "shell");
    node.config = json!({});
    let mut g = graph(vec![node]);

    let gated = apply_policy_gates(
        &mut g,
        &company("supervised", &[]),
        "wf",
        "run-1",
        &GrantSet::default(),
    )
    .await;

    assert!(gated.is_empty());
    assert!(gate_ids(&g).is_empty());
}

/// The load-bearing assumption of gating at translate time: for every tool
/// a `tool_call` node can actually reach, the verdict is decided by the
/// tool NAME alone. Node `args` may still carry unresolved `=`-expressions
/// when this pass runs, so a tool whose classification depends on its
/// arguments (`composio_execute` is the existing one) would be classified
/// against a template and could be gated wrongly in either direction.
///
/// None are reachable today — `WORKFLOW_TOOL_NAMESPACES` is `shell` /
/// `code` / `web` / `search`, and `composio` is not in it. This fails the
/// moment that stops being true, rather than letting a call slip the gate
/// silently. Same stance `web_search_is_still_a_priced_call` takes in
/// `consequence.rs`.
#[test]
fn every_reachable_workflow_tool_is_classified_by_name_alone() {
    use crate::policy::consequence_of;

    // Every tool the invoker wires, across all four reachable namespaces.
    for slug in [
        "shell",
        "read_workspace_state",
        "apply_patch",
        "git_operations",
        "csv_export",
        "web_fetch",
        "http_request",
        "curl",
        "image_info",
        "web_search",
    ] {
        let bare = consequence_of(slug, &json!({}));
        for args in [
            json!({ "action": "GMAIL_SEND_EMAIL" }),
            json!({ "amount_usd": 500.0 }),
            json!({ "command": "=item.cmd" }),
        ] {
            let with_args = consequence_of(slug, &args);
            assert_eq!(
                bare.reach, with_args.reach,
                "`{slug}` changes reach with args {args} — it can no longer be gated at \
                 translate time, where args may still be unresolved templates"
            );
        }
    }
}

// --- issue #846: an authored gate's card names its call too ------------

/// A node the **author** gated is described, not just identified.
///
/// This is the whole of #846's third defect. `policy_gates` only ever
/// produced a `GatedCall` for a node the company's policy stopped, so on a
/// `full`-tier company — where the policy stops nothing — every workflow
/// card carried a node id and the engine's resume payload and named neither
/// the tool nor the host. #375 fixed exactly this on the chat surface by
/// carrying the call's own arguments; this asks `call_of` the same question
/// for a gate nobody's policy raised.
#[test]
fn an_authored_gate_is_described_from_the_graph() {
    let g = graph(vec![Node {
        config: json!({
            "slug": "web_fetch",
            "args": { "url": "https://www.bbc.com/sport?token=secret" },
            "requires_approval": true,
        }),
        ..tool_node("fetch_bbc", "web_fetch")
    }]);

    let described = describe_call(&g, "fetch_bbc").expect("a tool_call node is describable");
    assert_eq!(described.node_id, "fetch_bbc");
    assert_eq!(described.slug, "web_fetch");
    assert_eq!(
        described.args["url"],
        "https://www.bbc.com/sport?token=secret"
    );
    // Host only. This string is journalled and kept after the decision, so
    // it must never carry the query — where a token is a routine thing to
    // find. The full arguments travel on `args`, redacted downstream by the
    // shared projection.
    assert_eq!(described.target.as_deref(), Some("www.bbc.com"));
    // Nobody wrote a reason, and the card must not invent a policy-shaped
    // one for a decision no policy made.
    assert!(described.reason.is_empty());
}

/// A gate on a node that calls nothing is described as such.
///
/// An authored `requires_approval` on a `transform` is a genuine "stop and
/// look at this" with no call behind it, and a card that invented one would
/// be worse than a card that says nothing.
#[test]
fn a_gate_on_a_node_that_calls_nothing_is_not_described() {
    let g = graph(vec![Node {
        kind: NodeKind::Transform,
        ..tool_node("review", "unused")
    }]);
    assert!(describe_call(&g, "review").is_none());
    assert!(describe_call(&g, "no-such-node").is_none());
}

/// A `tool_call` whose URL is still an unresolved template names no host.
///
/// Node arguments may still be `=`-expressions when this runs — the module
/// docs record that — and a card that printed `=item.json.url` as a
/// destination would be worse than one that prints none.
#[test]
fn an_unresolved_url_yields_no_host() {
    let g = graph(vec![Node {
        config: json!({ "slug": "web_fetch", "args": { "url": "=item.json.url" } }),
        ..tool_node("fetch", "web_fetch")
    }]);
    let described = describe_call(&g, "fetch").expect("still describable");
    assert_eq!(described.slug, "web_fetch");
    assert!(described.target.is_none(), "{:?}", described.target);
}

/// Userinfo is not mistaken for a host.
///
/// `https://user:pw@evil.test/` must name `evil.test`, not `user`. Shared
/// with `http_target` through `host_of` so the two surfaces cannot disagree.
#[test]
fn userinfo_is_not_mistaken_for_the_host() {
    assert_eq!(
        tool_target(&json!({ "url": "https://user:pw@evil.test/x?q=1" })).as_deref(),
        Some("evil.test")
    );
}
