//! Shared fixtures and helpers for the `team_agent` test files split out of
//! the original single inline `#[cfg(test)] mod tests { ... }` module. Every
//! item here was duplicated verbatim across the split siblings (or used
//! unqualified from a sibling that never defined it); this file is the one
//! copy they all import from now, declared from `team_agent.rs` with
//! `#[cfg(test)] #[path = "team_agent_test_support.rs"]` like every other
//! `*_test_support.rs` in this directory.

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::company::CompanyManifest;
use crate::ports::CompanyStore;
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::runtime::RuntimeBuilder;
use crate::server::router;
use crate::store::FsCompanyStore;
use crate::{AppConfig, AppState};

/// A company whose grants actually bite: `ceo` asks for one tool the company
/// does not allow, `writer` asks for nothing at all, and `hermit` sits on no
/// desk. Each of those is a different arm of the resolution under test.
pub(super) const ROSTER: &str = r#"
[company]
name = "Acme"
[policy]
mode = "full"
[tools]
allow = ["workspace", "workspace.*", "composio"]

[[agent]]
id = "ceo"
role = "Chief Executive"
description = "Sets direction and delegates."
tier = "orchestrator"
tools = ["workspace.read", "email.send"]

[[agent]]
id = "writer"
role = "Writer"

[[agent]]
id = "hermit"
role = "Hermit"

[[group_chat]]
id = "content"
name = "Content desk"
members = ["writer", "ceo"]
"#;

/// [`ROSTER`], plus a declared `[[harness]]` set (issue #1245's
/// harness-picker follow-up): `laptop` is a `local` ACP harness and the
/// **default**, so a fresh overlay teammate — which names no harness of
/// its own — lands there and a model override on it is meaningful. Tests
/// that need to exercise the harness picker itself declare a second,
/// non-default `built_in` entry (`main`) to switch *away* from.
pub(super) const ACP_ROSTER: &str = r#"
[company]
name = "Acme"
[policy]
mode = "full"
[tools]
allow = ["workspace", "workspace.*", "composio"]

[[agent]]
id = "ceo"
role = "Chief Executive"
description = "Sets direction and delegates."
tier = "orchestrator"
tools = ["workspace.read", "email.send"]

[[harness]]
id = "main"
kind = "built_in"

[[harness]]
id = "laptop"
kind = "acp"
default = true

[harness.acp]
transport = "local"
agent = "claude"
"#;

pub(super) fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("oc-agent-detail-")
        .tempdir()
        .expect("tempdir")
}

pub(super) async fn state_with_manifest(home: &std::path::Path, manifest_toml: &str) -> AppState {
    let manifest: CompanyManifest = toml::from_str(manifest_toml).unwrap();
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

pub(super) async fn send(
    state: &AppState,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", crate::server::test_support::fixed_cookie("acme"));
    let request = match &body {
        Some(value) => builder
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(value).unwrap()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

pub(super) async fn draft_for(state: &AppState, agent: &str, body: Value) -> (StatusCode, Value) {
    send(
        state,
        "POST",
        &format!("/api/v1/company/team/{agent}/draft"),
        Some(body),
    )
    .await
}

pub(super) async fn get_agent(state: &AppState, agent: &str) -> (StatusCode, Value) {
    send(state, "GET", &format!("/api/v1/company/team/{agent}"), None).await
}

pub(super) async fn patch_agent(state: &AppState, agent: &str, body: Value) -> (StatusCode, Value) {
    send(
        state,
        "PATCH",
        &format!("/api/v1/company/team/{agent}"),
        Some(body),
    )
    .await
}

/// Drives the route as a specific principal. The harness signs every other
/// request in as an admin, which is exactly why this exists: an
/// authority check verified only as an admin passes identically against no
/// check at all.
pub(super) async fn send_as(
    state: &AppState,
    method: &str,
    uri: &str,
    body: Option<Value>,
    cookie: String,
) -> (StatusCode, Value) {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", cookie);
    let request = match &body {
        Some(value) => builder
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(value).unwrap()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

/// Adds a teammate through the console's own route and returns its id.
pub(super) async fn add_overlay(state: &AppState, name: &str, role: &str) -> String {
    let (status, created) = send(
        state,
        "POST",
        "/api/v1/company/team",
        Some(json!({"name": name, "role": role, "description": "Original."})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    created["id"].as_str().unwrap().to_string()
}

/// Seeds one `inference/providers` row directly on `agent`'s secret
/// store, for the pin-validation tests (keys rework, issue #2306, slice
/// 3a) — the same pattern `server::ops::inference`'s own tests use.
pub(super) async fn seed_provider(state: &AppState, slug: &str, enabled: bool) {
    use crate::company::inference::store;

    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).expect("company registered");
    store::put_provider(
        &id,
        runtime.secrets().as_ref(),
        store::ProviderDraft {
            slug: slug.to_string(),
            label: slug.to_string(),
            kind: "custom".to_string(),
            base_url: "http://127.0.0.1:9/v1".to_string(),
            models: std::collections::BTreeMap::new(),
            enabled,
        },
    )
    .await
    .unwrap();
}

pub(super) fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("expected an array, got {value}"))
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

/// A meter that reports one fixed spend, so the ceiling can be seen holding
/// rather than only described.
pub(super) struct FixedMeter(pub(super) u64);

#[async_trait::async_trait]
impl crate::ports::UsageMeter for FixedMeter {
    async fn record(
        &self,
        _company: &CompanyId,
        _sample: &crate::ports::usage::UsageSample,
    ) -> crate::Result<()> {
        Ok(())
    }

    async fn query(
        &self,
        _company: &CompanyId,
        _since_millis: u64,
    ) -> crate::Result<Vec<crate::ports::usage::UsageSample>> {
        Ok(vec![crate::ports::usage::UsageSample {
            at_millis: crate::ports::now_millis(),
            agent: crate::metering::UNATTRIBUTED_AGENT.to_string(),
            provider: "managed".to_string(),
            input_tokens: self.0,
            output_tokens: 0,
            cached_input_tokens: 0,
            cost_usd: 0.0,
            kind: crate::ports::usage::SampleKind::AuthoringCall,
            run_id: None,
            model: None,
        }])
    }
}

/// A meter that cannot answer. The gate is deliberately **not** fail-closed
/// here: a metering outage that silently disabled a working copilot would
/// be the worse failure, and it is the same call the harness makes.
pub(super) struct FailingMeter;

#[async_trait::async_trait]
impl crate::ports::UsageMeter for FailingMeter {
    async fn record(
        &self,
        _company: &CompanyId,
        _sample: &crate::ports::usage::UsageSample,
    ) -> crate::Result<()> {
        Ok(())
    }

    async fn query(
        &self,
        _company: &CompanyId,
        _since_millis: u64,
    ) -> crate::Result<Vec<crate::ports::usage::UsageSample>> {
        Err(crate::error::OpenCompanyError::Store("no meter".into()))
    }
}

pub(super) fn plan_with(total_tokens: Option<u64>) -> crate::company::Plan {
    crate::company::Plan {
        name: Some("starter".to_string()),
        total_tokens,
        ..Default::default()
    }
}

/// The smallest legal GIF: a 1x1 image. Small enough to embed here, and
/// real enough that the decoder — rather than believing the part's declared
/// type — accepts it.
pub(super) const TINY_GIF: &[u8] = b"GIF89a\x01\x00\x01\x00\x00\xff\x00,\x00\x00\x00\x00\
\x01\x00\x01\x00\x00\x02\x00;";

/// A PNG whose header claims a 65535×65535 frame in a body of a few dozen
/// bytes — the decompression bomb the dimension caps exist for. The
/// signature and IHDR are enough for both the sniff and the size read.
pub(super) fn bomb_png() -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.extend_from_slice(&13u32.to_be_bytes());
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&65535u32.to_be_bytes());
    v.extend_from_slice(&65535u32.to_be_bytes());
    v.extend_from_slice(&[8, 6, 0, 0, 0]);
    v
}

/// Posts `bytes` to the avatar upload route as a `file` part named `name`.
pub(super) async fn upload_avatar(
    state: &AppState,
    name: &str,
    bytes: &[u8],
) -> (StatusCode, Value) {
    const BOUNDARY: &str = "----ocavatartest";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; \
             filename=\"{name}\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/company/avatars")
        .header("cookie", crate::server::test_support::fixed_cookie("acme"))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body))
        .unwrap();
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Posts `bytes` to the generic workspace upload route as a `file` part
/// named `name`, declaring `mime` as its `Content-Type`. The declared type
/// is what the store keeps — the referent check must not trust it, and this
/// helper exists to prove that.
pub(super) async fn upload_workspace_binary(
    state: &AppState,
    name: &str,
    mime: &str,
    bytes: &[u8],
) -> (StatusCode, Value) {
    const BOUNDARY: &str = "----ocworkspacetest";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; \
             filename=\"{name}\"\r\nContent-Type: {mime}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/company/workspace/upload")
        .header("cookie", crate::server::test_support::fixed_cookie("acme"))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body))
        .unwrap();
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
