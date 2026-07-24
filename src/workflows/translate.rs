//! Translate a company [`WorkflowFile`] into a tinyflows
//! [`WorkflowGraph`](tinyflows::model::WorkflowGraph).
//!
//! OpenCompany's on-disk model is a validated six-kind node/edge graph (see
//! [`crate::company::workflow_file`]); tinyflows' runnable model is a
//! twelve-kind graph. The mapping is mostly one-to-one, with two deliberate
//! choices:
//!
//! * **`output` → [`Transform`](tinyflows::model::NodeKind::Transform)** —
//!   tinyflows has no `output` kind. A `transform` node with no `set` config is
//!   a pure pass-through, which is exactly the terminal "report back" semantics
//!   of an `output` node (its predecessors' items flow through unchanged).
//! * **condition edge labels → `true`/`false` ports** — tinyflows keys a
//!   `condition` node's branch EXCLUSIVELY on the edge `from_port`, which must be
//!   `"true"` or `"false"` (any other value is a hard validation error). The
//!   OpenCompany model carries the branch on an edge `label` (`"yes"`/`"no"`),
//!   so an edge leaving a condition node maps its label to a `true`/`false`
//!   port. Every other edge stays on the default `"main"` port.
//!
//! An **agent** node's roster teammate id becomes the tinyflows `agent_ref` in
//! node config, which the engine's `agent` node routes to the injected
//! `AgentRunner` — that is how a step lands on the harness pool (see
//! [`super::caps`]). `tool_call` and `http_request` nodes are mapped
//! structurally; executing them end-to-end (real tool/HTTP semantics) is a
//! documented follow-on — [`super::caps`] wires them to explicit "not yet wired"
//! capabilities so an unreached node is inert and a reached one fails loudly
//! rather than silently.

use std::collections::HashSet;

use serde_json::{Value, json};
use tinyflows::model::{Edge, Node, NodeKind, WorkflowGraph};

use crate::company::{WorkflowEdgeDef, WorkflowFile, WorkflowNodeDef, WorkflowNodeKind};

/// Translates a validated [`WorkflowFile`] into a tinyflows
/// [`WorkflowGraph`](tinyflows::model::WorkflowGraph) ready for
/// [`tinyflows::compiler::compile`].
///
/// The source file is assumed already validated by
/// [`parse_workflow`](crate::company::workflow_file::parse_workflow) (exactly
/// one trigger, unique node ids, edges reference real nodes), so this is a
/// total, side-effect-free mapping.
pub fn translate(file: &WorkflowFile) -> WorkflowGraph {
    // The ids of every `condition` node, so an edge leaving one can map its
    // label onto the required `true`/`false` branch port.
    let condition_ids: HashSet<&str> = file
        .nodes
        .iter()
        .filter(|n| n.kind == WorkflowNodeKind::Condition)
        .map(|n| n.id.as_str())
        .collect();
    // The ids of every `on_error = "route"` node, so an "error"-labeled edge
    // leaving one maps onto the engine's `error` port (the same mechanism as a
    // condition node's `true`/`false` ports).
    let route_ids: HashSet<&str> = file
        .nodes
        .iter()
        .filter(|n| n.on_error.as_deref() == Some("route"))
        .map(|n| n.id.as_str())
        .collect();

    WorkflowGraph {
        id: Some(file.id.clone()),
        name: file.name.clone(),
        nodes: file.nodes.iter().map(translate_node).collect(),
        edges: file
            .edges
            .iter()
            .map(|edge| translate_edge(edge, &condition_ids, &route_ids))
            .collect(),
        ..WorkflowGraph::default()
    }
}

/// The tinyflows [`NodeKind`] one OpenCompany node kind lowers to. `output` has
/// no tinyflows counterpart — a config-less `transform` is a pure pass-through
/// terminal, exactly the "report back" semantics of an `output` node.
fn tinyflows_kind(kind: WorkflowNodeKind) -> NodeKind {
    match kind {
        WorkflowNodeKind::Trigger => NodeKind::Trigger,
        WorkflowNodeKind::Agent => NodeKind::Agent,
        WorkflowNodeKind::ToolCall => NodeKind::ToolCall,
        WorkflowNodeKind::HttpRequest => NodeKind::HttpRequest,
        WorkflowNodeKind::Condition => NodeKind::Condition,
        WorkflowNodeKind::Output => NodeKind::Transform,
    }
}

/// Maps one OpenCompany node to its tinyflows [`Node`], assembling the engine
/// config in three layers so a node's own config can specialize a step without
/// ever subverting the graph's identity:
///
/// 1. **Derived defaults** — the kind's built-in config (an `agent` node's
///    `prompt`).
/// 2. **User config overlay** — the node's free-form `config`, laid over the
///    defaults (so an author can override the derived `prompt`, add a
///    `tool_call` `slug`/`args`, or shape an `http_request` descriptor).
/// 3. **First-class fields LAST** — `agent_ref` (bound from `agent`, so config
///    can never rebind the node to another teammate), the `tool_call` id
///    placeholder `slug` (only when the overlay carries none), and the
///    engine-read `on_error` / `retry` / `requires_approval` keys.
///
/// A legacy node (no `config`, no typed fields) yields exactly the pre-P1
/// config, so translation of an unchanged file is byte-identical.
fn translate_node(def: &WorkflowNodeDef) -> Node {
    let mut config = serde_json::Map::new();

    // 1. Derived defaults.
    if def.kind == WorkflowNodeKind::Agent {
        config.insert("prompt".to_string(), json!(prompt_for(def)));
    }

    // 2. User config overlay.
    if let Some(Value::Object(user)) = &def.config {
        for (key, value) in user {
            config.insert(key.clone(), value.clone());
        }
    }

    // 3. First-class fields, written last so config cannot shadow them.
    match def.kind {
        WorkflowNodeKind::Agent => {
            if let Some(agent) = def.agent.as_deref().filter(|a| !a.is_empty()) {
                config.insert("agent_ref".to_string(), json!(agent));
            }
        }
        // A `tool_call` needs a `slug` or the engine's node errors; when the
        // config binds none, fall back to the node id as a stable placeholder.
        WorkflowNodeKind::ToolCall => {
            config
                .entry("slug".to_string())
                .or_insert_with(|| json!(def.id));
        }
        _ => {}
    }

    // Per-node error policy the tinyflows engine reads straight off config.
    if let Some(on_error) = def.on_error.as_deref() {
        config.insert("on_error".to_string(), json!(on_error));
    }
    if let Some(retry) = &def.retry {
        config.insert("retry".to_string(), retry_config(retry));
    }
    if let Some(requires_approval) = def.requires_approval {
        config.insert("requires_approval".to_string(), json!(requires_approval));
    }

    Node {
        id: def.id.clone(),
        kind: tinyflows_kind(def.kind),
        type_version: 1,
        name: def.name.clone(),
        config: Value::Object(config),
        ports: Vec::new(),
        position: None,
    }
}

/// Builds the engine's `retry` config object from the typed policy, emitting the
/// exact keys the tinyflows engine reads (`max_attempts` / `backoff_ms` /
/// `backoff`) and omitting any the author left unset (the engine defaults them).
fn retry_config(retry: &crate::company::WorkflowRetryDef) -> Value {
    let mut map = serde_json::Map::new();
    if let Some(max_attempts) = retry.max_attempts {
        map.insert("max_attempts".to_string(), json!(max_attempts));
    }
    if let Some(backoff_ms) = retry.backoff_ms {
        map.insert("backoff_ms".to_string(), json!(backoff_ms));
    }
    if let Some(backoff) = &retry.backoff {
        map.insert("backoff".to_string(), json!(backoff));
    }
    Value::Object(map)
}

/// The instruction handed to an agent node: its summary when present, else its
/// human-readable name.
fn prompt_for(def: &WorkflowNodeDef) -> String {
    def.summary
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(&def.name)
        .to_string()
}

/// Maps one OpenCompany edge to a tinyflows [`Edge`]. Edges leaving a
/// `condition` node carry their branch on `from_port` (`true`/`false`, mapped
/// from the label); an "error"-labeled edge leaving an `on_error = "route"`
/// node carries the `error` port (the engine emits the failure item there);
/// every other edge stays on the default `main` port.
fn translate_edge(
    edge: &WorkflowEdgeDef,
    condition_ids: &HashSet<&str>,
    route_ids: &HashSet<&str>,
) -> Edge {
    let from_port = if condition_ids.contains(edge.from.as_str()) {
        condition_port(edge.label.as_deref())
    } else if route_ids.contains(edge.from.as_str()) && edge.label.as_deref() == Some("error") {
        "error".to_string()
    } else {
        "main".to_string()
    };
    Edge {
        from_node: edge.from.clone(),
        from_port,
        to_node: edge.to.clone(),
        to_port: "main".to_string(),
    }
}

/// Maps a condition edge's label onto the required `true`/`false` branch port.
/// Negative labels (`no`/`false`/`n`) map to `"false"`; everything else
/// (including an absent label) maps to `"true"`.
fn condition_port(label: Option<&str>) -> String {
    let negative = label
        .map(|l| l.trim().to_ascii_lowercase())
        .is_some_and(|l| matches!(l.as_str(), "no" | "false" | "n"));
    if negative { "false" } else { "true" }.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::company::parse_workflow;

    const CAMPAIGN: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/companies/agentic_marketing_agency/workflows/campaign_pipeline.toml"
    ));

    /// The shipped campaign pipeline translates into a graph tinyflows accepts,
    /// exercising every one of the six node kinds.
    #[test]
    fn translates_the_shipped_campaign_pipeline() {
        let file = parse_workflow(CAMPAIGN).expect("campaign parses");
        let graph = translate(&file);

        assert_eq!(graph.id.as_deref(), Some("campaign_pipeline"));
        assert_eq!(graph.name, "Campaign pipeline");
        assert_eq!(graph.nodes.len(), file.nodes.len());
        assert_eq!(graph.edges.len(), file.edges.len());

        // The translated graph is structurally valid for the engine.
        tinyflows::compiler::compile(&graph).expect("translated graph compiles");
    }

    /// Node kinds map across, and an `output` node becomes a pass-through
    /// `transform`.
    #[test]
    fn maps_every_node_kind() {
        let file = parse_workflow(CAMPAIGN).expect("campaign parses");
        let graph = translate(&file);
        let kind = |id: &str| {
            graph
                .nodes
                .iter()
                .find(|n| n.id == id)
                .map(|n| n.kind.clone())
        };

        assert_eq!(kind("brief"), Some(NodeKind::Trigger));
        assert_eq!(kind("strategist"), Some(NodeKind::Agent));
        assert_eq!(kind("gate"), Some(NodeKind::Condition));
        assert_eq!(kind("research"), Some(NodeKind::ToolCall));
        assert_eq!(kind("publish"), Some(NodeKind::HttpRequest));
        // `output` lowers to a pass-through `transform`.
        assert_eq!(kind("done"), Some(NodeKind::Transform));
    }

    /// An agent node carries its roster teammate id as `agent_ref` plus a prompt.
    #[test]
    fn agent_node_carries_agent_ref_and_prompt() {
        let file = parse_workflow(CAMPAIGN).expect("campaign parses");
        let graph = translate(&file);
        let strategist = graph.nodes.iter().find(|n| n.id == "strategist").unwrap();
        assert_eq!(strategist.config["agent_ref"], "brand_strategist");
        assert_eq!(
            strategist.config["prompt"],
            "Turns the brief into an angle + outline."
        );
    }

    /// A condition node's `yes`/`no` labels become `true`/`false` branch ports.
    #[test]
    fn condition_labels_map_to_true_false_ports() {
        let file = parse_workflow(CAMPAIGN).expect("campaign parses");
        let graph = translate(&file);
        let port = |to: &str| {
            graph
                .edges
                .iter()
                .find(|e| e.from_node == "gate" && e.to_node == to)
                .map(|e| e.from_port.clone())
        };
        assert_eq!(port("research").as_deref(), Some("true")); // label "yes"
        assert_eq!(port("copy").as_deref(), Some("false")); // label "no"
    }

    /// Non-condition edges keep the default `main` port.
    #[test]
    fn plain_edges_stay_on_main() {
        let file = parse_workflow(CAMPAIGN).expect("campaign parses");
        let graph = translate(&file);
        let edge = graph
            .edges
            .iter()
            .find(|e| e.from_node == "brief" && e.to_node == "strategist")
            .unwrap();
        assert_eq!(edge.from_port, "main");
        assert_eq!(edge.to_port, "main");
    }

    /// The label mapping is total: negatives → false, everything else → true.
    #[test]
    fn condition_port_mapping() {
        assert_eq!(condition_port(Some("yes")), "true");
        assert_eq!(condition_port(Some("no")), "false");
        assert_eq!(condition_port(Some("TRUE")), "true");
        assert_eq!(condition_port(Some("False")), "false");
        assert_eq!(condition_port(None), "true");
    }

    // --- P1: config overlay + error/retry/approval + error routing ---------

    /// A snapshot pinning that the shipped campaign pipeline (which uses none of
    /// the P1 fields) translates byte-identically to the pre-P1 output: every
    /// node's config equals exactly what the old kind-only mapping produced.
    #[test]
    fn campaign_translation_is_byte_identical_to_legacy() {
        let file = parse_workflow(CAMPAIGN).expect("campaign parses");
        let graph = translate(&file);
        let config = |id: &str| {
            graph
                .nodes
                .iter()
                .find(|n| n.id == id)
                .map(|n| n.config.clone())
                .unwrap()
        };
        assert_eq!(config("brief"), json!({}));
        assert_eq!(
            config("strategist"),
            json!({ "agent_ref": "brand_strategist", "prompt": "Turns the brief into an angle + outline." })
        );
        assert_eq!(config("gate"), json!({}));
        assert_eq!(config("research"), json!({ "slug": "research" }));
        assert_eq!(config("publish"), json!({}));
        assert_eq!(config("done"), json!({}));
    }

    /// A `tool_call` node whose config binds a `slug` keeps that slug; the
    /// node-id placeholder is only a fallback when config carries none.
    #[test]
    fn config_slug_beats_the_node_id_placeholder() {
        let src = r#"
            id = "wf"
            name = "WF"
            [[node]]
            id = "start"
            kind = "trigger"
            name = "Start"
            [[node]]
            id = "call"
            kind = "tool_call"
            name = "Call"
            [node.config]
            slug = "csv_export"
            [node.config.args]
            filename = "out.csv"
            [[edge]]
            from = "start"
            to = "call"
        "#;
        let graph = translate(&parse_workflow(src).expect("parses"));
        let call = graph.nodes.iter().find(|n| n.id == "call").unwrap();
        assert_eq!(call.config["slug"], "csv_export");
        assert_eq!(call.config["args"]["filename"], "out.csv");
    }

    /// A config that tries to spoof `agent_ref` cannot win: the first-class
    /// `agent` field is written last, so the roster binding is authoritative.
    /// (The model would reject this config at parse time; here we build the node
    /// directly to prove the layering order in `translate` itself.)
    #[test]
    fn agent_ref_survives_a_spoofing_config() {
        use crate::company::{WorkflowFile, WorkflowNodeDef, WorkflowNodeKind};
        let file = WorkflowFile {
            id: "wf".into(),
            name: "WF".into(),
            description: None,
            nodes: vec![WorkflowNodeDef {
                id: "worker".into(),
                kind: WorkflowNodeKind::Agent,
                name: "Worker".into(),
                summary: None,
                agent: Some("real".into()),
                config: Some(json!({ "agent_ref": "impostor" })),
                on_error: None,
                retry: None,
                requires_approval: None,
            }],
            edges: Vec::new(),
        };
        let graph = translate(&file);
        assert_eq!(graph.nodes[0].config["agent_ref"], "real");
    }

    /// `on_error` / `retry` / `requires_approval` land as the exact config keys
    /// the tinyflows engine reads.
    #[test]
    fn error_retry_approval_land_as_engine_config_keys() {
        let src = r#"
            id = "wf"
            name = "WF"
            [[node]]
            id = "start"
            kind = "trigger"
            name = "Start"
            requires_approval = true
            [[node]]
            id = "call"
            kind = "tool_call"
            name = "Call"
            on_error = "continue"
            [node.retry]
            max_attempts = 3
            backoff_ms = 250
            backoff = "exponential"
            [[edge]]
            from = "start"
            to = "call"
        "#;
        let graph = translate(&parse_workflow(src).expect("parses"));
        let start = graph.nodes.iter().find(|n| n.id == "start").unwrap();
        assert_eq!(start.config["requires_approval"], true);
        let call = graph.nodes.iter().find(|n| n.id == "call").unwrap();
        assert_eq!(call.config["on_error"], "continue");
        assert_eq!(call.config["retry"]["max_attempts"], 3);
        assert_eq!(call.config["retry"]["backoff_ms"], 250);
        assert_eq!(call.config["retry"]["backoff"], "exponential");
    }

    /// An "error"-labeled edge leaving a routing node maps onto the engine's
    /// `error` port; a non-error edge from the same node stays on `main`.
    #[test]
    fn error_label_maps_to_error_port() {
        let src = r#"
            id = "wf"
            name = "WF"
            [[node]]
            id = "start"
            kind = "trigger"
            name = "Start"
            [[node]]
            id = "call"
            kind = "tool_call"
            name = "Call"
            on_error = "route"
            [node.config]
            slug = "csv_export"
            [[node]]
            id = "ok"
            kind = "output"
            name = "OK"
            [[node]]
            id = "recover"
            kind = "output"
            name = "Recover"
            [[edge]]
            from = "start"
            to = "call"
            [[edge]]
            from = "call"
            to = "ok"
            [[edge]]
            from = "call"
            to = "recover"
            label = "error"
        "#;
        let graph = translate(&parse_workflow(src).expect("parses"));
        let port = |to: &str| {
            graph
                .edges
                .iter()
                .find(|e| e.from_node == "call" && e.to_node == to)
                .map(|e| e.from_port.clone())
        };
        assert_eq!(port("recover").as_deref(), Some("error"));
        assert_eq!(port("ok").as_deref(), Some("main"));
    }
}
