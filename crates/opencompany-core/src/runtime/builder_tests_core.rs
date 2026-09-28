pub(super) use super::*;
pub(super) use crate::ports::types::{CompanyId, CompressedTrace, ToolCall};
pub(super) use crate::runtime::journal::ExecutedEffect;

#[derive(Clone)]
pub(super) struct TestMemoryScopes {
    pub(super) context: Arc<dyn ContextStore>,
}

#[async_trait::async_trait]
impl crate::store::MemoryScopes for TestMemoryScopes {
    fn agent_context(&self, _agent_id: &str) -> Arc<dyn ContextStore> {
        self.context.clone()
    }

    fn desk_context(&self, _desk_id: &str) -> Arc<dyn ContextStore> {
        self.context.clone()
    }

    async fn archived_traces(&self, _company: &CompanyId) -> Result<Vec<CompressedTrace>> {
        Ok(Vec::new())
    }
}

pub(super) fn tmp_home(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("tempdir")
}

/// A company's journal file, as a previous host left it: `keys` committed in
/// order, at the bundle path the fs journal has always used.
pub(super) async fn seed_filesystem_journal(home: &std::path::Path, id: &CompanyId, keys: &[&str]) {
    let journal = RuntimeJournal::new(Bundle::new(home.to_path_buf(), id).journal_jsonl());
    for (n, key) in keys.iter().enumerate() {
        journal
            .record_executed(
                key,
                ExecutedEffect {
                    kind: "filing.submit".into(),
                    amount_usd: None,
                    task_id: Some("t-1".into()),
                    at_millis: 1_000 + n as u64,
                    irreversible: true,
                },
            )
            .await
            .expect("seed a legacy journal line");
    }
}

/// A declaration file every template author would write, used by the
/// seeding tests below.
#[cfg(test)]
pub(super) const PIPELINE_LEDGER: &str = r#"
title = "Deal pipeline"
purpose = "Every deal in flight and why a lost one was lost."

[[field]]
name = "deal"
role = "id"
required = true

[[field]]
name = "stage"
role = "status"
required = true

[[status]]
name = "qualifying"

[[status]]
name = "won"
closed = true
needs_reason = true
"#;

pub(super) fn parse(toml_src: &str) -> CompanyManifest {
    toml::from_str(toml_src).expect("valid manifest")
}

pub(super) fn seed_policy(mode: &str, always: &[&str], under: Option<f64>) -> Policy {
    Policy {
        mode: mode.to_string(),
        always_approve: always.iter().map(|s| s.to_string()).collect(),
        auto_approve_under_usd: under,
        approval_ttl_hours: None,
    }
}

pub(super) fn held_override(mode: &str) -> PolicyOverride {
    use crate::ports::types::{Actor, ActorKind};
    PolicyOverride {
        mode: Some(mode.to_string()),
        always_approve: None,
        auto_approve_under_usd: None,
        approval_ttl_hours: None,
        set_by: Actor {
            kind: ActorKind::User,
            id: "admin-1".to_string(),
        },
        at_millis: 1_700_000_000_000,
    }
}

/// A `[tools]` block granting exactly `allow`.
pub(super) fn seed_tools(allow: &[&str]) -> Tools {
    let mut tools = Tools {
        provider: crate::company::TOOL_PROVIDERS[0].to_string(),
        allow: allow.iter().map(|g| g.to_string()).collect(),
        web_allowed_domains: Vec::new(),
        composio: Default::default(),
        search_daily_calls: None,
        max_delegation_depth: None,
    };
    tools.allow.shrink_to_fit();
    tools
}

/// A console grant of `added`, as the write route would have stored it.
pub(super) fn held_grants(added: &[&str]) -> ToolGrantsOverride {
    use crate::ports::types::{Actor, ActorKind};
    ToolGrantsOverride {
        added: added.iter().map(|g| g.to_string()).collect(),
        set_by: Actor {
            kind: ActorKind::User,
            id: "admin-1".to_string(),
        },
        at_millis: 1_700_000_000_000,
    }
}

/// A bodiless overlay stub — `merge_enabled_workflows` only reads the id.
pub(super) fn overlay(id: &str) -> OverlayWorkflow {
    OverlayWorkflow {
        id: id.to_string(),
        toml: String::new(),
    }
}

// --- Issue #208: two-build rebuild semantics over one home dir ----------

/// A seed manifest with the roster the create-path draft below references.
pub(super) fn wf_manifest(extra: &str) -> CompanyManifest {
    parse(&format!(
        "[company]\nname=\"Acme\"\n[policy]\nmode=\"full\"\n\
         [[agent]]\nid=\"assistant\"\nrole=\"Assistant\"\n{extra}"
    ))
}

/// The minimal valid three-node graph the create path accepts, mirroring
/// `workflow_create`'s own `valid_draft`.
pub(super) fn wf_draft(id: &str, name: &str) -> crate::company::RawWorkflow {
    use crate::company::{RawEdge, RawNode, RawWorkflow};
    let node = |id: &str, kind: &str, name: &str, agent: Option<&str>| RawNode {
        id: id.to_string(),
        kind: kind.to_string(),
        name: name.to_string(),
        summary: None,
        agent: agent.map(str::to_string),
        schedule: None,
        config: None,
        on_error: None,
        retry: None,
        requires_approval: None,
        repeatable: None,
        destination: None,
        postcondition: None,
        verify: None,
    };
    RawWorkflow {
        id: id.to_string(),
        name: name.to_string(),
        description: Some("A tiny graph.".to_string()),
        owner_desk: None,
        nodes: vec![
            node("start", "trigger", "Start", None),
            node("worker", "agent", "Worker", Some("assistant")),
            node("done", "output", "Report", None),
        ],
        edges: vec![
            RawEdge {
                from: "start".to_string(),
                to: "worker".to_string(),
                label: None,
            },
            RawEdge {
                from: "worker".to_string(),
                to: "done".to_string(),
                label: Some("ok".to_string()),
            },
        ],
    }
}

pub(super) fn openhuman_manifest() -> CompanyManifest {
    parse(
        r#"
        [company]
        name = "Acme"
        [[agent]]
        id = "ceo"
        role = "Chief"
        [tools]
        provider = "openhuman"
        allow = ["email.*"]
        [channels.email]
        provider = "openhuman"
        "#,
    )
}

/// Spawns an in-process OpenAI-compatible stub that answers every
/// chat-completion with `marker`, so a harness turn can run without a real
/// inference backend. Mirrors the provider-test helper of the same name.
#[cfg(feature = "openhuman")]
pub(super) async fn spawn_stub(marker: &'static str) -> String {
    use axum::routing::post;
    use axum::{Json, Router};

    let app = Router::new().route(
        "/chat/completions",
        post(move || async move {
            Json(serde_json::json!({
                "choices": [{ "message": { "role": "assistant", "content": marker } }],
                "usage": { "prompt_tokens": 1, "completion_tokens": 1 }
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}
