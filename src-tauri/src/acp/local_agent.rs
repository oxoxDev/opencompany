//! `LocalAcpAgent`: the `transport = "local"` implementation of the host
//! crate's [`AcpAgent`] port (issue #1245) — a real coding CLI, spawned once
//! per declared local-acp harness and driven over stdio through the existing
//! [`AcpClient`].
//!
//! ## One process, many sessions
//!
//! A harness can serve more than one teammate, but [`AcpClient::spawn`] opens
//! one subprocess with one global update sink — ACP's `session/update`
//! notifications are not routed per caller, only tagged with the `sessionId`
//! they belong to. So this buffers every notification by `sessionId` as it
//! arrives, and a `prompt` call drains only its own session's buffer after
//! `session/prompt` returns rather than reading whatever the sink last saw.
//!
//! ## Permission requests: copied from `buzz-agent`, not bridged to the queue
//!
//! An earlier draft routed ACP `session/request_permission` calls through
//! `ApprovalRequestQueue` and, until that landed, refused every request by
//! default. `buzz-agent` (`crates/buzz-acp`) answers a much simpler question
//! instead — trust the CLI's own permission mode, and auto-approve whatever
//! it still asks about — and that is what this does too, via
//! [`AutoApprovingFiles`]. There is no human-approval queue in the loop here;
//! an ACP-run teammate's own CLI is the trust boundary, the same as it is for
//! a developer running that CLI interactively themselves.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use opencompany::Result;
use opencompany::error::OpenCompanyError;
use opencompany::ports::acp::{AcpAgent, AcpAgentFactory, AcpTurn, AcpUpdate};
use opencompany::ports::types::CompanyId;
use serde_json::Value;
use tokio::sync::Mutex as AsyncMutex;

use crate::acp::client::{AcpClient, AutoApprovingFiles, ClientHandler, ConfinedFiles};
use crate::acp::confine::Confinement;
use crate::acp::discovery::HARNESSES;

/// Per-CLI startup model env var, confirmed live against the real adapter
/// (issue #1245's live smoke test) — not guessed. `None` means this build has
/// no known startup env var for that CLI, and [`LocalAcpAgent::session_for`]
/// falls back to the ACP-native `session/set_config_option` path instead —
/// also confirmed live, for `codex-acp` specifically (its `configOptions`
/// model entry accepts a set; no env var candidate tried had any effect).
fn model_env_var(agent: &str) -> Option<&'static str> {
    match agent {
        "claude" => Some("ANTHROPIC_MODEL"),
        "goose" => Some("GOOSE_MODEL"),
        _ => None,
    }
}

/// One spawned local-transport ACP harness, serving every teammate bound to
/// it.
pub struct LocalAcpAgent {
    command: &'static str,
    args: Vec<String>,
    env: Vec<(String, String)>,
    /// The desired model, kept regardless of whether an env var already
    /// carries it — [`Self::session_for`] falls back to
    /// `session/set_config_option` when [`model_env_var`] returned `None` at
    /// construction, so this is the only record of what was actually asked
    /// for in that case.
    model: Option<String>,
    /// The company's agent-workspace root (`HarnessDeps::workspace_root`).
    /// Each session roots at `workspace_root/<company>/<agent>/workspace`,
    /// mirroring `harness::built_in::build::agent_workspace` exactly, so an
    /// ACP-run teammate's files land in the same conventional place a
    /// `built_in`-run one's would.
    workspace_root: PathBuf,
    client: AsyncMutex<Option<Arc<AcpClient>>>,
    /// `session_key` (`"{company}::{agent_id}"`) → ACP `sessionId`.
    sessions: AsyncMutex<HashMap<String, String>>,
    /// `session/update` notifications, demultiplexed by ACP `sessionId` —
    /// see the module docs for why this exists at all.
    pending_updates: Arc<StdMutex<HashMap<String, Vec<Value>>>>,
}

impl LocalAcpAgent {
    /// `agent` is one of `ACP_AGENTS` (the manifest already validated this).
    /// `model`, when set, is forwarded via that agent's own startup lever
    /// when this build knows one.
    pub fn new(agent: &str, model: Option<&str>, workspace_root: PathBuf) -> Result<Self> {
        let def = HARNESSES.iter().find(|h| h.id == agent).ok_or_else(|| {
            OpenCompanyError::Config(format!("no local ACP harness definition for `{agent}`"))
        })?;

        let mut env = Vec::new();
        if let (Some(model), Some(var)) = (model, model_env_var(agent)) {
            env.push((var.to_string(), model.to_string()));
        }

        Ok(Self {
            command: def.command,
            args: def.args.iter().map(|a| a.to_string()).collect(),
            env,
            model: model.map(str::to_string),
            workspace_root,
            client: AsyncMutex::new(None),
            sessions: AsyncMutex::new(HashMap::new()),
            pending_updates: Arc::new(StdMutex::new(HashMap::new())),
        })
    }

    /// The spawned client, spawning it on first call.
    async fn client(&self) -> Result<Arc<AcpClient>> {
        let mut guard = self.client.lock().await;
        if let Some(client) = guard.as_ref() {
            return Ok(client.clone());
        }

        std::fs::create_dir_all(&self.workspace_root).map_err(|error| {
            OpenCompanyError::Config(format!(
                "could not create ACP workspace root {}: {error}",
                self.workspace_root.display()
            ))
        })?;
        let confinement = Confinement::new(&self.workspace_root)
            .map_err(|error| OpenCompanyError::Config(format!("acp workspace: {error}")))?;
        // Auto-approves permission requests by kind — see the module docs.
        let handler: Arc<dyn ClientHandler> = Arc::new(AutoApprovingFiles::new(
            ConfinedFiles::new(confinement, None),
        ));

        let pending = Arc::clone(&self.pending_updates);
        let sink: crate::acp::client::UpdateSink = Arc::new(move |update: Value| {
            let session_id = update["sessionId"].as_str().unwrap_or_default().to_string();
            pending
                .lock()
                .unwrap()
                .entry(session_id)
                .or_default()
                .push(update);
        });

        let args: Vec<&str> = self.args.iter().map(String::as_str).collect();
        let env: Vec<(&str, &str)> = self
            .env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let client = AcpClient::spawn(
            self.command,
            &args,
            &self.workspace_root,
            &env,
            handler,
            sink,
        )
        .await
        .map_err(|error| {
            OpenCompanyError::Config(format!("could not start `{}`: {error}", self.command))
        })?;
        client
            .initialize()
            .await
            .map_err(|error| OpenCompanyError::Config(format!("acp initialize: {error}")))?;

        let client = Arc::new(client);
        *guard = Some(client.clone());
        Ok(client)
    }

    /// The per-(company, agent) session directory, created if it does not
    /// exist yet — mirrors `harness::built_in::build::agent_workspace`.
    fn session_root(&self, company: &CompanyId, agent_id: &str) -> Result<PathBuf> {
        let dir = self
            .workspace_root
            .join(company.as_ref())
            .join(agent_id)
            .join("workspace");
        std::fs::create_dir_all(&dir).map_err(|error| {
            OpenCompanyError::Config(format!(
                "could not create ACP session workspace {}: {error}",
                dir.display()
            ))
        })?;
        Ok(dir)
    }

    /// This session's cached ACP `sessionId`, opening one if none exists yet.
    ///
    /// A fresh session is where model steering happens when no startup env
    /// var carries it ([`model_env_var`] returned `None` for this agent):
    /// `session/new`'s own response is inspected for a `configOptions` entry
    /// with `category: "model"` whose options include the desired value, and
    /// if found, `session/set_config_option` applies it before this session
    /// is used for anything. Confirmed live to be per-session state (not
    /// global), which is exactly the granularity wanted — a session opened
    /// here is one (company, agent) pair for its whole life.
    async fn session_for(
        &self,
        client: &AcpClient,
        session_key: &str,
        root: &Path,
    ) -> Result<String> {
        let mut sessions = self.sessions.lock().await;
        if let Some(id) = sessions.get(session_key) {
            return Ok(id.clone());
        }

        let raw = client
            .call(
                "session/new",
                serde_json::json!({ "cwd": root.display().to_string(), "mcpServers": [] }),
            )
            .await
            .map_err(|error| OpenCompanyError::Config(format!("acp session/new: {error}")))?;
        let id = raw["sessionId"]
            .as_str()
            .ok_or_else(|| {
                OpenCompanyError::Config("acp session/new returned no sessionId".to_string())
            })?
            .to_string();

        // `self.env` carries the model only when `new()` found a known env
        // var for this agent — non-empty means the spawn already handled it,
        // so the fallback below must not also fire (redundant at best, and
        // this session's model would otherwise be decided by whichever of
        // the two APIs the adapter honors last). No matching `config_id`
        // falls through the same way: either an env var already carried it
        // at spawn, or this build has no lever for this agent at all (issue
        // #1245's documented codex gap, before this fallback existed) —
        // either way, silently doing nothing here is correct, not a missed
        // error.
        if self.env.is_empty()
            && let Some(model) = &self.model
            && let Some(config_id) = model_config_id(&raw, model)
        {
            client
                .call(
                    "session/set_config_option",
                    serde_json::json!({
                        "sessionId": id,
                        "configId": config_id,
                        "value": model,
                    }),
                )
                .await
                .map_err(|error| {
                    OpenCompanyError::Config(format!(
                        "acp session/set_config_option (model `{model}`): {error}"
                    ))
                })?;
        }

        sessions.insert(session_key.to_string(), id.clone());
        Ok(id)
    }

    /// `session_key` is `"{company}::{agent_id}"` — recovers `agent_id` by
    /// stripping the company prefix, since `AcpAgent::prompt` does not carry
    /// it separately. Agent ids are `snake_case` (manifest-validated) and
    /// cannot themselves contain `::`, so this split is unambiguous.
    fn agent_id_of<'a>(company: &CompanyId, session_key: &'a str) -> &'a str {
        session_key
            .strip_prefix(company.as_ref())
            .and_then(|rest| rest.strip_prefix("::"))
            .unwrap_or(session_key)
    }
}

/// Finds the `configId` to set to reach `desired_model`, from a fresh
/// `session/new` response's `configOptions` — the entry whose `category` is
/// `"model"` and whose `options` include a `value` matching `desired_model`.
/// `None` when nothing matches: either this adapter advertises no such
/// option, or it does but not for this exact value.
///
/// Accepts both `configId` (the ACP spec's own name) and `id` — confirmed
/// live that `codex-acp` emits `id`, matching the same quirk documented for
/// `claude-agent-acp` in `harness::acp::run_turn`.
///
/// `pub` (not private) so `tests/acp_live_smoke.rs` can pin this parsing
/// against a captured real response without a live spawn — the one part of
/// the fallback that can be tested deterministically and in CI.
pub fn model_config_id(session_new_result: &Value, desired_model: &str) -> Option<String> {
    session_new_result["configOptions"]
        .as_array()?
        .iter()
        .find_map(|opt| {
            if opt.get("category").and_then(|c| c.as_str()) != Some("model") {
                return None;
            }
            let matches = opt
                .get("options")
                .and_then(|o| o.as_array())
                .is_some_and(|options| {
                    options
                        .iter()
                        .any(|o| o.get("value").and_then(|v| v.as_str()) == Some(desired_model))
                });
            if !matches {
                return None;
            }
            opt.get("configId")
                .or_else(|| opt.get("id"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
}

/// Translates one raw `session/update` notification into this crate's
/// [`AcpUpdate`], or `None` for a kind that is dropped rather than
/// approximated (`plan`, `available_commands_update`, …) — see
/// `harness::acp::run_turn`'s own module docs for the mapping table this
/// mirrors.
fn parse_update(raw: &Value) -> Option<AcpUpdate> {
    let update = raw.get("update")?;
    match update.get("sessionUpdate")?.as_str()? {
        "agent_message_chunk" => Some(AcpUpdate::MessageChunk(
            update["content"]["text"].as_str()?.to_string(),
        )),
        "agent_thought_chunk" => Some(AcpUpdate::ThoughtChunk),
        "tool_call" => Some(AcpUpdate::ToolCall {
            id: update["toolCallId"].as_str()?.to_string(),
            title: update["title"].as_str().unwrap_or_default().to_string(),
        }),
        "tool_call_update" => Some(AcpUpdate::ToolCallUpdate {
            id: update["toolCallId"].as_str()?.to_string(),
            status: update["status"].as_str().unwrap_or_default().to_string(),
            result: update
                .get("content")
                .and_then(|c| c.as_array())
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|b| b["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("")
                }),
        }),
        _ => None,
    }
}

#[async_trait]
impl AcpAgent for LocalAcpAgent {
    async fn prompt(
        &self,
        company: &CompanyId,
        session_key: &str,
        message: &str,
    ) -> Result<AcpTurn> {
        let client = self.client().await?;
        let agent_id = Self::agent_id_of(company, session_key);
        let root = self.session_root(company, agent_id)?;
        let session_id = self.session_for(&client, session_key, &root).await?;

        // Clear any stale buffer before the turn starts, so the drain below
        // sees exactly this turn's updates and nothing left over from one
        // that timed out or was cancelled without being read.
        self.pending_updates.lock().unwrap().remove(&session_id);

        let stop_reason = client
            .prompt(&session_id, message)
            .await
            .map_err(|error| OpenCompanyError::Config(format!("acp prompt: {error}")))?;

        let raw = self
            .pending_updates
            .lock()
            .unwrap()
            .remove(&session_id)
            .unwrap_or_default();
        let updates = raw.iter().filter_map(parse_update).collect();
        Ok(AcpTurn {
            updates,
            stop_reason,
        })
    }

    async fn cancel(&self, company: &CompanyId, session_key: &str) -> Result<()> {
        let session_id = {
            let sessions = self.sessions.lock().await;
            sessions.get(session_key).cloned()
        };
        let Some(session_id) = session_id else {
            // No session ever opened for this (company, agent) — nothing to
            // cancel, and asking a client that may not exist yet would spawn
            // one just to tell it to stop.
            return Ok(());
        };
        let client = { self.client.lock().await.clone() };
        let Some(client) = client else {
            return Ok(());
        };
        let _ = company; // carried for symmetry with `prompt`; not needed here
        client
            .cancel(&session_id)
            .await
            .map_err(|error| OpenCompanyError::Config(format!("acp cancel: {error}")))
    }
}

/// Builds a fresh [`LocalAcpAgent`] per call — no caching, matching
/// `harness::lanes::built_in_lane`'s own precedent of building a fresh pool
/// on every `RuntimeBuilder::build`. A rebuild is rare (a manifest or
/// inference-settings change), and the old agent's subprocess is killed on
/// drop (`AcpClient::kill_on_drop`), so nothing leaks.
pub struct LocalAcpAgentFactory;

impl AcpAgentFactory for LocalAcpAgentFactory {
    fn build(
        &self,
        agent: &str,
        model: Option<&str>,
        workspace_root: &Path,
    ) -> Result<Arc<dyn AcpAgent>> {
        Ok(Arc::new(LocalAcpAgent::new(
            agent,
            model,
            workspace_root.to_path_buf(),
        )?))
    }
}
