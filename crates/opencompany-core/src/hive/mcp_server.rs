//! The MCP server the company agents speak and reach OpenCompany's tools on
//! (plan `hive-desks`, Phase 3).
//!
//! # Why a server at all
//!
//! `openhuman_embed::Agent` has no seam for an in-process host tool: an agent's
//! tools are OpenHuman's own groups plus the `McpServer`s fixed on its spec.
//! So every tool this crate authors — ledger, tasks, pages, memory, workspace,
//! and the room's speech tools `post` / `broadcast` / `dm` /
//! `complete_episode` / `read` — is served here and reached by the agent as
//! `mcp_call_tool{server: "opencompany", tool, arguments}`.
//!
//! # The wire
//!
//! JSON-RPC 2.0 over Streamable HTTP, on a dedicated loopback listener
//! (`127.0.0.1:0`, see [`McpHost::serve_loopback`]), one route:
//!
//! `POST /internal/mcp/{company}/{runtime_agent_id}` with
//! `Authorization: Bearer <per-agent token>` and
//! `Accept: application/json, text/event-stream`. Methods: `initialize`
//! (answers a `Mcp-Session-Id`), `notifications/initialized` (202, empty),
//! `ping`, `tools/list`, `tools/call`. Every reply is a plain JSON body; the
//! server never opens an SSE stream, and `GET` answers 405 so a client that
//! probes for one learns there is none. The client this matches is
//! OpenHuman's own (`tinymcp::McpHttpClient`), which is also what the tests
//! drive it with.
//!
//! # Attribution and approvals
//!
//! The bearer names the agent; the agent's one in-flight turn
//! ([`InFlightRegistry`]) names the episode, round and conversation. A speech
//! call folds into that turn's outbox; an OpenCompany tool call is decided by
//! the agent's [`ApprovalPolicy`] — allow, deny, or park — and runs under an
//! [`InFlightContext`]. A call parks only on a task that holds an approval
//! claim; without one the policy refuses it and says nobody was asked.

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};

use axum::Router;
use axum::http::StatusCode;
use axum::routing::post;
use openhuman_embed::{AgentSpec, McpAuthConfig, McpServer};
use serde_json::Value;
use tinytools::Tool;

use super::tools::{InFlightRegistry, McpToolAdapter, speech_descriptor, speech_tool_names};
use crate::harness::policy::ApprovalPolicy;
use crate::ports::events::EventLog;
use crate::ports::types::CompanyId;

/// The server name the agents address it by (`mcp_call_tool{server}`), and
/// the mock brain's contract.
pub const SERVER_SLUG: &str = "opencompany";
/// The route prefix; the full path is `{prefix}/{company}/{runtime_agent_id}`.
pub const MCP_PATH_PREFIX: &str = "/internal/mcp";
/// The `Mcp-Session-Id` header, issued on `initialize`.
pub const HEADER_SESSION_ID: &str = "Mcp-Session-Id";
/// Per-request timeout the attached `McpServer` is told to wait: an
/// OpenCompany tool can run a model pass of its own.
const CALL_TIMEOUT_SECS: u64 = 300;

/// One agent the server serves: who it is, what it may call, and what decides
/// its calls.
pub struct McpAgent {
    /// The company the agent belongs to.
    pub company: CompanyId,
    /// The manifest agent id (the speaker id).
    pub agent_id: String,
    /// The runtime agent id the route and the bearer resolve to.
    pub runtime_agent_id: String,
    bearer: String,
    /// The speech tools this agent may call. Every desk seat gets all five;
    /// a non-desk turn still lists them so the catalogue is stable across
    /// surfaces, and the handler refuses `dm` outside an episode.
    pub speech_tools: Vec<String>,
    tools: Vec<McpToolAdapter>,
    /// The approval policy that decides each OpenCompany tool call. `None`
    /// allows everything — for tests over a bare belt only.
    pub policy: Option<Arc<ApprovalPolicy>>,
    /// The agent workspace the served tools sandbox to.
    pub workspace: Option<PathBuf>,
    /// The company journal `read` is served from.
    pub events: Option<Arc<dyn EventLog>>,
}

impl fmt::Debug for McpAgent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpAgent")
            .field("company", &self.company)
            .field("agent_id", &self.agent_id)
            .field("runtime_agent_id", &self.runtime_agent_id)
            .field("tools", &self.tool_names())
            .finish_non_exhaustive()
    }
}

impl McpAgent {
    /// An agent with every speech tool and no OpenCompany tools yet.
    #[must_use]
    pub fn new(
        company: CompanyId,
        agent_id: impl Into<String>,
        runtime_agent_id: impl Into<String>,
        bearer: impl Into<String>,
    ) -> Self {
        Self {
            company,
            agent_id: agent_id.into(),
            runtime_agent_id: runtime_agent_id.into(),
            bearer: bearer.into(),
            speech_tools: speech_tool_names()
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
            tools: Vec::new(),
            policy: None,
            workspace: None,
            events: None,
        }
    }

    /// Mints a fresh bearer: `ocm_` + a v4 UUID.
    #[must_use]
    pub fn mint_bearer() -> String {
        format!("ocm_{}", uuid::Uuid::new_v4().simple())
    }

    /// The OpenCompany tools to serve — the belt minus what OpenHuman runs
    /// natively, which the caller has already split off.
    #[must_use]
    pub fn tools(mut self, tools: Vec<Arc<dyn Tool>>) -> Self {
        self.tools = tools.into_iter().map(McpToolAdapter::new).collect();
        self
    }

    /// Restricts the speech tools this agent may call.
    #[must_use]
    pub fn speech_tools<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.speech_tools = names.into_iter().map(Into::into).collect();
        self
    }

    /// The approval policy that decides this agent's tool calls.
    #[must_use]
    pub fn policy(mut self, policy: Arc<ApprovalPolicy>) -> Self {
        self.policy = Some(policy);
        self
    }

    /// The agent workspace.
    #[must_use]
    pub fn workspace(mut self, workspace: PathBuf) -> Self {
        self.workspace = Some(workspace);
        self
    }

    /// The journal `read` is served from.
    #[must_use]
    pub fn events(mut self, events: Arc<dyn EventLog>) -> Self {
        self.events = Some(events);
        self
    }

    /// The bearer this agent authenticates with.
    #[must_use]
    pub fn bearer(&self) -> &str {
        &self.bearer
    }

    /// The OpenCompany tool names served, in belt order.
    #[must_use]
    pub fn tool_names(&self) -> Vec<String> {
        self.tools
            .iter()
            .map(|tool| tool.name().to_string())
            .collect()
    }

    /// Every tool name the catalogue lists — the `allow_tools` of the
    /// attached `McpServer`.
    #[must_use]
    pub fn allow_tools(&self) -> Vec<String> {
        let mut names = self.speech_tools.clone();
        names.extend(self.tool_names());
        names
    }

    fn tool(&self, name: &str) -> Option<&McpToolAdapter> {
        self.tools.iter().find(|tool| tool.name() == name)
    }

    fn catalogue(&self) -> Vec<Value> {
        let mut tools: Vec<Value> = tinyhivemind::speech::tool_specs()
            .iter()
            .filter(|spec| self.speech_tools.iter().any(|name| name == spec.name))
            .map(speech_descriptor)
            .collect();
        tools.extend(self.tools.iter().map(McpToolAdapter::descriptor));
        tools
    }
}

/// The `dm` recipient rule Phase 4's `DeskHive` supplies (`hive.resolve_dm`).
///
/// TODO(Phase 4): install one on the host from `graph.rs`. Until then the
/// membership snapshot on [`HiveTurn`](super::tools::HiveTurn) is the rule.
pub trait DmResolver: Send + Sync {
    /// `Ok` when `speaker` may address `to` on `desk_id`; `Err(reason)` is
    /// rendered to the seat as `refused: <reason>`.
    fn resolve_dm(
        &self,
        company: &CompanyId,
        desk_id: &str,
        speaker: &str,
        to: &[String],
    ) -> Result<(), String>;
}

/// The server: the agents it serves, the turns in flight, and the listener.
pub struct McpHost {
    agents: RwLock<HashMap<String, Arc<McpAgent>>>,
    in_flight: Arc<InFlightRegistry>,
    addr: OnceLock<SocketAddr>,
    dm_resolver: RwLock<Option<Arc<dyn DmResolver>>>,
}

impl fmt::Debug for McpHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpHost")
            .field("agents", &self.agents.read().map(|a| a.len()).unwrap_or(0))
            .field("addr", &self.addr.get())
            .field("in_flight", &self.in_flight)
            .finish()
    }
}

impl Default for McpHost {
    fn default() -> Self {
        Self {
            agents: RwLock::new(HashMap::new()),
            in_flight: Arc::new(InFlightRegistry::new()),
            addr: OnceLock::new(),
            dm_resolver: RwLock::new(None),
        }
    }
}

/// The one host this process serves every company's agents on.
///
/// Process-wide for the same reason the OpenHuman runtime is
/// ([`openhuman_runtime::global`](crate::harness::openhuman_runtime::global)):
/// an agent is registered on the runtime once, its spec names one endpoint,
/// and every pool in the process — the serving one, a desktop-parity one, a
/// test's — must resolve that endpoint to the same registry of bearers and
/// in-flight turns. Bind it with [`McpHost::serve_loopback`] before the first
/// roster is built; until then [`McpHost::endpoint_for`] is `None` and an agent
/// spec carries no server.
static GLOBAL: OnceLock<Arc<McpHost>> = OnceLock::new();

/// The process-wide host (see [`GLOBAL`]).
#[must_use]
pub fn global() -> Arc<McpHost> {
    GLOBAL.get_or_init(McpHost::new).clone()
}

impl McpHost {
    /// An empty host with no listener.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The in-flight turn registry the driver registers turns on.
    #[must_use]
    pub fn in_flight(&self) -> &Arc<InFlightRegistry> {
        &self.in_flight
    }

    /// Registers (or replaces) an agent, keyed by runtime agent id.
    pub fn register(&self, agent: McpAgent) -> Arc<McpAgent> {
        let agent = Arc::new(agent);
        self.agents
            .write()
            .expect("mcp agents poisoned")
            .insert(agent.runtime_agent_id.clone(), agent.clone());
        agent
    }

    /// Forgets one agent; its bearer stops working at once.
    pub fn unregister(&self, runtime_agent_id: &str) {
        self.agents
            .write()
            .expect("mcp agents poisoned")
            .remove(runtime_agent_id);
    }

    /// Forgets the agent registered under `runtime_agent_id` only if it still
    /// holds `bearer` — a dropped roster entry must not evict the rebuilt one
    /// that has since taken its id.
    pub fn unregister_if_bearer(&self, runtime_agent_id: &str, bearer: &str) {
        let mut agents = self.agents.write().expect("mcp agents poisoned");
        if agents
            .get(runtime_agent_id)
            .is_some_and(|agent| constant_time_eq(agent.bearer.as_bytes(), bearer.as_bytes()))
        {
            agents.remove(runtime_agent_id);
        }
    }

    /// Forgets every agent of `company` — a roster rebuild.
    pub fn unregister_company(&self, company: &CompanyId) {
        self.agents
            .write()
            .expect("mcp agents poisoned")
            .retain(|_, agent| &agent.company != company);
    }

    /// The registered agent, if any.
    #[must_use]
    pub fn agent(&self, runtime_agent_id: &str) -> Option<Arc<McpAgent>> {
        self.agents
            .read()
            .expect("mcp agents poisoned")
            .get(runtime_agent_id)
            .cloned()
    }

    /// Installs Phase 4's `dm` rule.
    pub fn set_dm_resolver(&self, resolver: Arc<dyn DmResolver>) {
        *self.dm_resolver.write().expect("dm resolver poisoned") = Some(resolver);
    }

    /// The loopback address the listener is bound to, once it is.
    #[must_use]
    pub fn addr(&self) -> Option<SocketAddr> {
        self.addr.get().copied()
    }

    /// The endpoint an agent's `McpServer::http` is pointed at.
    #[must_use]
    pub fn endpoint_for(&self, company: &CompanyId, runtime_agent_id: &str) -> Option<String> {
        let addr = self.addr()?;
        Some(format!(
            "http://{addr}{MCP_PATH_PREFIX}/{company}/{runtime_agent_id}"
        ))
    }

    /// Binds `127.0.0.1:0` and serves the router on it. Idempotent: a second
    /// call returns the address the first bound.
    ///
    /// The listener runs on its own thread with its own tokio runtime rather
    /// than on the caller's: the host is process-wide and outlives any one
    /// runtime — a test binary boots one per `#[tokio::test]`, and a listener
    /// spawned on the first would die with it and leave every later turn
    /// dialling a closed port. The same reason the OpenHuman runtime is
    /// booted the way it is.
    pub async fn serve_loopback(self: &Arc<Self>) -> std::io::Result<SocketAddr> {
        self.serve_loopback_blocking()
    }

    /// [`serve_loopback`](Self::serve_loopback) for a caller with no runtime.
    pub fn serve_loopback_blocking(self: &Arc<Self>) -> std::io::Result<SocketAddr> {
        if let Some(addr) = self.addr() {
            return Ok(addr);
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        if let Err(bound) = self.addr.set(addr) {
            // Lost a race with another caller; their listener serves.
            drop(listener);
            return Ok(bound);
        }
        let host = self.clone();
        std::thread::Builder::new()
            .name("opencompany-mcp".to_string())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_name("opencompany-mcp-worker")
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        tracing::error!(
                            "[hive::mcp] MCP listener runtime failed to build: {error}"
                        );
                        return;
                    }
                };
                runtime.block_on(async move {
                    let listener = match tokio::net::TcpListener::from_std(listener) {
                        Ok(listener) => listener,
                        Err(error) => {
                            tracing::error!("[hive::mcp] MCP listener failed to register: {error}");
                            return;
                        }
                    };
                    if let Err(error) = axum::serve(listener, host.router()).await {
                        tracing::error!("[hive::mcp] loopback MCP listener stopped: {error}");
                    }
                });
            })?;
        tracing::info!(%addr, "[hive::mcp] serving the opencompany MCP server on loopback");
        Ok(addr)
    }

    /// The router serving [`MCP_PATH_PREFIX`]`/{company}/{runtime_agent_id}`.
    pub fn router(self: Arc<Self>) -> Router {
        Router::new()
            .route(
                &format!("{MCP_PATH_PREFIX}/{{company}}/{{runtime_agent_id}}"),
                post(handler::handle)
                    .get(|| async { StatusCode::METHOD_NOT_ALLOWED })
                    .delete(|| async { StatusCode::NO_CONTENT }),
            )
            .with_state(self)
    }
}

/// Mounts the MCP routes on an existing router (a test or an operator app
/// that serves everything on one listener). Production uses
/// [`McpHost::serve_loopback`] instead.
pub fn mount(router: Router, host: Arc<McpHost>) -> Router {
    router.merge(host.router())
}

/// What an agent spec needs to reach this server.
#[derive(Clone, Debug)]
pub struct McpAttach {
    /// The agent's endpoint (`McpHost::endpoint_for`).
    pub endpoint: String,
    /// The agent's bearer.
    pub bearer: String,
    /// The tools the agent may see (`McpAgent::allow_tools`).
    pub allow_tools: Vec<String>,
}

impl McpAttach {
    /// The attachment for a registered agent, once the host has an address.
    #[must_use]
    pub fn for_agent(host: &McpHost, agent: &McpAgent) -> Option<Self> {
        Some(Self {
            endpoint: host.endpoint_for(&agent.company, &agent.runtime_agent_id)?,
            bearer: agent.bearer.clone(),
            allow_tools: agent.allow_tools(),
        })
    }
}

/// Attaches the `opencompany` MCP server to an agent spec. The Phase 2
/// `agent_spec_for` calls this once the pool has an [`McpAttach`] for the
/// agent; a spec without it has only OpenHuman's native tools.
#[must_use]
pub fn attach_opencompany_mcp(spec: AgentSpec, ctx: &McpAttach) -> AgentSpec {
    spec.mcp(opencompany_mcp_server(ctx))
}

/// The `McpServer` declaration [`attach_opencompany_mcp`] fixes on the spec:
/// slug [`SERVER_SLUG`], the agent's endpoint and bearer, its allow list.
#[must_use]
pub fn opencompany_mcp_server(ctx: &McpAttach) -> McpServer {
    McpServer::http(SERVER_SLUG, ctx.endpoint.clone())
        .auth(McpAuthConfig::BearerToken {
            token: ctx.bearer.clone(),
        })
        .allow_tools(ctx.allow_tools.clone())
        .timeout_secs(CALL_TIMEOUT_SECS)
        .description("OpenCompany: speak to the desk and use the company's tools")
}

/// Byte-equality that does not stop at the first mismatch.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

mod handler;

#[cfg(test)]
#[path = "mcp_server_tests.rs"]
mod tests;
